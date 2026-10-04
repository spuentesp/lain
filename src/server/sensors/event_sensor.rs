//! Event sensor (§6.7, stretch goal).
//!
//! Detects message-bus and scheduler subscriptions / publications:
//!
//! - kafkajs / confluent-kafka (Node/TypeScript): `producer.send({ topic })`
//!   and `consumer.run({ topic })` / `consumer.subscribe({ topics: [..] })`.
//! - aiokafka (Python): `AIOKafkaProducer(...)`, `KafkaConsumer(...)`,
//!   `.send(topic=..., ...)`.
//! - rdkafka (Rust): `FutureProducer::send(record, ...)` where the
//!   `record` carries the topic name in its `topic` field, plus
//!   `StreamConsumer::subscribe(&[...])`.
//! - kafka-go (Go): `producer.SendMessage(&kafka.Message{Topic: "..."})`
//!   and `consumer.SubscribeTopics("...")`.
//! - Celery (Python): `@app.task` and `@shared_task` decorators make the
//!   decorated function a scheduled topic named after its qualified
//!   module path.
//! - NestJS (TypeScript): `@Cron('0 0 * * *')` decorator marks the
//!   following function as a scheduled topic.
//!
//! Each detected site emits a `Topic` node plus a `Produces` (publishers)
//! or `Consumes` (subscribers) edge. Topic names are extracted from
//! string literals and same-file `const` / `let` constant assignments
//! (§6.7 "same-file constants"). Dynamic topic names are recorded but
//! skipped from the graph so they surface in `coverage.unnormalized`.
//!
//! The sensor runs at phase 2 so `Topic` nodes exist before the
//! joiner binds consumer services to producer services (§7.7). It
//! publishes through `replace_sensor_output(EventSensor, …)` so a
//! rescan retracts only its own previous output (§6.1).

use crate::error::LainError;
use crate::federation::contracts::model::SourceSite;
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const DEFAULT_BROKER: &str = "kafka";

/// What the regex extractor finds in a single file. `topic` is the
/// literal name when extractable; `dynamic` means the topic arg is a
/// non-literal expression (`x`, `f()`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectedSite {
    kind: SiteKind,
    topic: Option<String>,
    broker: String,
    line: u32,
    /// Optional enclosing-symbol name resolved against the graph; only
    /// set when the sensor found one (`Function` / `Method` covering
    /// the line).
    owner_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SiteKind {
    Produces,
    Consumes,
    Scheduled,
}

// ─── Top-level sensor ────────────────────────────────────────────────

pub struct EventSensor;

// Topic events ride on the `EntryPoints` bucket so counts surface in
// `run_all` without growing the enum. Phase 2 (§6.7 / §7.7): needs the
// joiner's service table to know which repo a `Topic` belongs to.
crate::server::sensors::register_sensor!(
    EventSensor,
    "event",
    EntryPoints,
    2,
    scan_workspace_event
);

// ─── Workspace scan ──────────────────────────────────────────────────

pub fn scan_workspace_event(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    let any_ext = |p: &Path| {
        Some(
            p.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string(),
        )
    };
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, any_ext) {
        let ext = ext.as_str();
        let path_str = graph_path(root, &path);
        let constants = collect_same_file_constants(&content, ext);
        let sites = detect_sites(&content, ext, &constants);
        let (nodes, edges) = emit_sites(&sites, &path_str, namespace, graph);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }

    graph.replace_sensor_output(SensorOwner::EventSensor, &all_nodes, &all_edges)?;
    Ok(all_nodes.len())
}

/// Resolve the enclosing function/method for a given `(path, line)`.
/// Returns the function's id and a fresh node id when no function
/// exists yet in the graph.
fn resolve_function_id(
    graph: &GraphDatabase,
    graph_path_str: &str,
    name_hint: &str,
    line: u32,
    namespace: &RepoNamespace,
) -> String {
    if let Some(sym) = crate::server::sensors::util::enclosing_symbol(graph, graph_path_str, line) {
        return sym.id.clone();
    }
    // No existing function indexed: mint a synthetic one anchored at
    // the site line so the `Produces` / `Consumes` edge has a real
    // source.
    GraphNode::generate_id(
        &NodeType::Function,
        graph_path_str,
        if name_hint.is_empty() {
            "event_site"
        } else {
            name_hint
        },
        Some(line),
        namespace,
    )
}

// ─── Detection (regex-first, language-dispatched) ────────────────────

/// `const NAME = "value";` (TS / JS / Go) and `NAME = "value"` at
/// module level (Python). Keys must be identifiers and values must be
/// string literals. Both single- and double-quoted forms are
/// supported.
fn collect_same_file_constants(content: &str, ext: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        let (lhs, rhs) = match trimmed.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        let rhs = rhs.trim().trim_end_matches(';').trim();
        let value = match extract_string_literal(rhs) {
            Some(v) => v,
            None => continue,
        };
        // Strip TS/JS keywords; Python has no keyword on bare assignments.
        let kw_stripped = lhs
            .trim()
            .strip_prefix("const ")
            .or_else(|| lhs.trim().strip_prefix("let "))
            .or_else(|| lhs.trim().strip_prefix("var "));
        let name = match kw_stripped {
            Some(n) => n.trim().to_string(),
            None => {
                if ext == "py" {
                    let n = lhs.trim();
                    if !n.is_empty() && is_ident(n) && !n.contains(' ') {
                        n.to_string()
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
        };
        if name.is_empty() || !is_ident(&name) {
            continue;
        }
        out.insert(name, value);
    }
    out
}

fn extract_string_literal(s: &str) -> Option<String> {
    crate::server::sensors::util_tokenize::extract_string_literal(s, 0).map(|(_, lit)| lit)
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip a line comment marker. `//` covers TypeScript, JavaScript,
/// Rust, Go; `#` covers Python. The cut is at the first occurrence
/// outside of any string literal. Cheap approximation: we cut at the
/// first `//` or `#` that isn't inside a quote — correct for the
/// event-sensor's input shape (topic arg always comes before any
/// comment marker on the line).
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match in_string {
            Some(q) if b == q => {
                // Closing quote — only honor if not escaped.
                if i > 0 && bytes[i - 1] == b'\\' {
                    i += 1;
                    continue;
                }
                in_string = None;
            }
            Some(_) => {}
            None if b == b'"' || b == b'\'' || b == b'`' => {
                in_string = Some(b);
            }
            None if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            None if b == b'#' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

fn detect_sites(
    content: &str,
    ext: &str,
    constants: &BTreeMap<String, String>,
) -> Vec<DetectedSite> {
    let mut out: Vec<DetectedSite> = Vec::new();
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            detect_ts_sites(&mut out, content, constants);
        }
        "py" => {
            detect_py_sites(&mut out, content, constants);
        }
        "rs" => {
            detect_rust_sites(&mut out, content, constants);
        }
        "go" => {
            detect_go_sites(&mut out, content, constants);
        }
        _ => {}
    }
    out
}

fn detect_ts_sites(
    out: &mut Vec<DetectedSite>,
    content: &str,
    constants: &BTreeMap<String, String>,
) {
    // kafkajs / confluent-kafka: `producer.send({ topic: 'foo' })` and
    // `consumer.run({ topics: ['foo'] })`.
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx as u32 + 1;
        // Strip line comments. Without this, a commented-out
        // `producer.send({ topic: 'foo' })` would still match the
        // regex and emit a phantom Topic.
        let line = strip_line_comment(line);
        let trimmed = line.trim();

        // Producer pattern.
        if trimmed.contains(".send(") || trimmed.contains(".send (") {
            if let Some(topic) = extract_object_property(line, "topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            } else if let Some(name) = extract_object_property_identifier(line, "topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: resolve_topic_name(&name, constants),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }

        // Consumer pattern: `consumer.run({ topics: ['foo'] })` or
        // `consumer.subscribe({ topics: [...] })`.
        if trimmed.contains(".run(") || trimmed.contains(".subscribe(") {
            if let Some(topic) = extract_array_string_member(line) {
                out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }

        // Celery-style scheduled task: `@Cron('...')` decorator on
        // a function — the next non-empty line is the function it
        // decorates.
        if let Some(spec) = extract_decorator_arg(trimmed, "Cron") {
            let mut owner_name: Option<String> = None;
            for ahead in content.lines().skip(idx + 1).take(5) {
                let ahead = ahead.trim();
                if ahead.is_empty() || ahead.starts_with("//") {
                    continue;
                }
                if ahead.starts_with("function ")
                    || ahead.starts_with("async function ")
                    || ahead.starts_with("export ")
                {
                    owner_name = parse_function_name(ahead);
                    break;
                }
                if ahead.starts_with("@") {
                    break;
                }
            }
            out.push(DetectedSite {
                kind: SiteKind::Scheduled,
                topic: Some(spec),
                broker: "schedule".into(),
                line: line_num,
                owner_name,
            });
        }
    }
}

fn detect_py_sites(
    out: &mut Vec<DetectedSite>,
    content: &str,
    constants: &BTreeMap<String, String>,
) {
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx as u32 + 1;
        let line = strip_line_comment(line);
        let trimmed = line.trim();

        // aiokafka: `KafkaConsumer(topic)` constructor or
        // `producer.send_and_wait(topic, ...)`.
        if trimmed.contains("AIOKafkaProducer(") || trimmed.contains("KafkaProducer(") {
            if let Some(topic) = extract_positional_string_arg(trimmed, "AIOKafkaProducer")
                .or_else(|| extract_positional_string_arg(trimmed, "KafkaProducer"))
            {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
        if trimmed.contains("AIOKafkaConsumer(") || trimmed.contains("KafkaConsumer(") {
            if let Some(topic) = extract_positional_string_arg(trimmed, "AIOKafkaConsumer")
                .or_else(|| extract_positional_string_arg(trimmed, "KafkaConsumer"))
            {
                out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
        // `.send_and_wait("topic", ...)` or `.send(topic="foo", ...)`.
        if trimmed.contains(".send_and_wait(") || trimmed.contains(".send(") {
            if let Some(topic) = extract_first_string_arg(trimmed) {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            } else if let Some(name) = extract_kwarg_identifier(trimmed, "topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: resolve_topic_name(&name, constants),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }

        // Celery `@app.task` and `@shared_task` decorators.
        if let Some(spec) = extract_py_decorator_task(trimmed) {
            let mut owner_name: Option<String> = None;
            for ahead in content.lines().skip(idx + 1).take(5) {
                let ahead = ahead.trim();
                if ahead.is_empty() || ahead.starts_with("#") {
                    continue;
                }
                if ahead.starts_with("def ") || ahead.starts_with("async def ") {
                    owner_name = parse_def_name(ahead);
                    break;
                }
                if ahead.starts_with("@") {
                    break;
                }
            }
            // For Celery the broker is `celery` and the topic is the
            // module path / task name. We use the function name as a
            // stable identifier when present.
            out.push(DetectedSite {
                kind: SiteKind::Scheduled,
                topic: owner_name.clone().or(Some(spec)),
                broker: "celery".into(),
                line: line_num,
                owner_name,
            });
        }
    }
}

fn detect_rust_sites(
    out: &mut Vec<DetectedSite>,
    content: &str,
    constants: &BTreeMap<String, String>,
) {
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx as u32 + 1;
        let line = strip_line_comment(line);
        let trimmed = line.trim();

        // rdkafka producer: `FutureRecord::to("topic")` is the
        // canonical publish shape — the topic name is the first
        // argument. Detect before the `::send(` shape so a single-line
        // `let record = FutureRecord::to("...")` form is also covered.
        if trimmed.contains("FutureRecord::to(") {
            if let Some(topic) = extract_first_string_arg(trimmed) {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            } else if let Some(name) = extract_first_ident_arg(trimmed) {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: resolve_topic_name(&name, constants),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }

        // rdkafka: `FutureProducer::send(record, ...)` where the
        // record's topic is a string literal in
        // `FutureRecord { topic: ... }`.
        if trimmed.contains("::send(") || trimmed.contains(".send(") {
            // The topic name appears as a string literal in the
            // `FutureRecord` struct's `topic` field.
            if let Some(topic) = extract_object_property(line, "topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            } else if let Some(name) = extract_object_property_identifier(line, "topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: resolve_topic_name(&name, constants),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }

        // StreamConsumer::subscribe(&["foo", "bar"]) — single-element
        // arrays only, to keep the regex simple.
        if trimmed.contains("::subscribe(") || trimmed.contains(".subscribe(") {
            if let Some(topic) = extract_array_string_member(line) {
                out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
    }
}

fn detect_go_sites(
    out: &mut Vec<DetectedSite>,
    content: &str,
    constants: &BTreeMap<String, String>,
) {
    for (idx, line) in content.lines().enumerate() {
        let line_num = idx as u32 + 1;
        let line = strip_line_comment(line);
        let trimmed = line.trim();

        // kafka-go: `producer.SendMessage(&kafka.Message{Topic: "foo"})`
        // or `writer.WriteMessages(ctx, kafka.Message{Topic: "foo"})`.
        // Match `SendMessage` as a prefix so `SendMessages(` (the
        // segmentio/kafka-go WriteMessages sibling) is also covered.
        if trimmed.contains("SendMessage") || trimmed.contains("WriteMessages(") {
            if let Some(topic) = extract_struct_field(line, "Topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            } else if let Some(name) = extract_struct_field_identifier(line, "Topic") {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: resolve_topic_name(&name, constants),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
        // `consumer.SubscribeTopics("foo")`.
        if trimmed.contains("SubscribeTopics(") {
            if let Some(topic) = extract_first_string_arg(trimmed) {
                out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(topic),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
    }
}

// ─── Per-language string extractors ───────────────────────────────────

/// Look for `{ key: "<value>" }` patterns. Returns the literal value.
fn extract_object_property(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let idx = line.find(&needle)?;
    let rest = &line[idx + needle.len()..];
    let rhs = rest.trim_start();
    extract_string_literal(rhs)
}

fn extract_object_property_identifier(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let idx = line.find(&needle)?;
    let rest = &line[idx + needle.len()..];
    let rhs = rest.trim_start();
    let ident: String = rhs
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

fn extract_struct_field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let idx = line.find(&needle)?;
    let rest = &line[idx + needle.len()..];
    let rhs = rest.trim_start();
    extract_string_literal(rhs)
}

fn extract_struct_field_identifier(line: &str, key: &str) -> Option<String> {
    extract_object_property_identifier(line, key)
}

/// Look for the first quoted string in the line that appears inside
/// `[ ... ]`. Returns it.
fn extract_array_string_member(line: &str) -> Option<String> {
    let lbracket = line.find('[')?;
    let rbracket = line.rfind(']')?;
    let inside = &line[lbracket..=rbracket];
    extract_string_literal(inside.trim_matches(|c: char| c == '[' || c == ']').trim())
}

fn extract_first_string_arg(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let close = line.rfind(')').unwrap_or(line.len());
    let inside = &line[open + 1..close];
    extract_string_literal(inside.trim())
}

/// Like `extract_first_string_arg` but for a bare identifier arg
/// (e.g. `FutureRecord::to(TOPIC)` where `TOPIC` is a constant).
fn extract_first_ident_arg(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let close = line.rfind(')').unwrap_or(line.len());
    let inside = &line[open + 1..close];
    let ident: String = inside
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() || !is_ident(&ident) {
        None
    } else {
        Some(ident)
    }
}

fn extract_positional_string_arg(line: &str, ctor: &str) -> Option<String> {
    let needle = format!("{ctor}(");
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    extract_string_literal(after.trim())
}

fn extract_kwarg_identifier(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    let ident: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

fn extract_decorator_arg(line: &str, name: &str) -> Option<String> {
    let needle = format!("@{name}(");
    let idx = line.find(&needle)?;
    let after = &line[idx + needle.len()..];
    extract_string_literal(after.trim())
}

fn extract_py_decorator_task(line: &str) -> Option<String> {
    // `@app.task` or `@shared_task` decorators. The function name
    // comes from the next line; the "topic" here is just a sentinel
    // string (`"celery:<name>"`) so it sorts deterministically. The
    // joiner uses the `owner_name` to look up the actual function.
    if line.starts_with("@app.task") || line.starts_with("@shared_task") {
        Some("celery:task".into())
    } else {
        None
    }
}

fn parse_function_name(line: &str) -> Option<String> {
    let after = line
        .trim_start_matches("export ")
        .trim_start_matches("async function")
        .trim_start_matches("function")
        .trim_start();
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn parse_def_name(line: &str) -> Option<String> {
    let after = line
        .trim_start_matches("async def")
        .trim_start_matches("def")
        .trim_start();
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn resolve_topic_name(ident: &str, constants: &BTreeMap<String, String>) -> Option<String> {
    constants.get(ident).cloned()
}

// ─── Emission ────────────────────────────────────────────────────────

fn emit_sites(
    sites: &[DetectedSite],
    graph_path: &str,
    namespace: &RepoNamespace,
    graph: &GraphDatabase,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    use crate::federation::contracts::model::{
        ContractFact as CF, TopicConsumerFact, TopicConsumerKind,
    };

    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut emitted_topic_ids: BTreeSet<String> = BTreeSet::new();
    // De-dupe the consumer-side function nodes we re-emit. We only
    // need to write the `TopicConsumer` contract fact once per
    // function.
    let mut emitted_consumer_facts: BTreeSet<String> = BTreeSet::new();

    for site in sites {
        let topic_name = match site.topic.clone() {
            Some(n) => n,
            None => {
                // Dynamic topic — `coverage.unnormalized` materializes
                // this in the index; nothing to emit.
                continue;
            }
        };

        // One `Topic` node per unique `(broker, name)` per file.
        let topic_label = format!("{}/{}", site.broker, topic_name);
        let topic_id = GraphNode::generate_id(
            &NodeType::Topic,
            graph_path,
            &topic_label,
            Some(site.line),
            namespace,
        );
        if emitted_topic_ids.insert(topic_id.clone()) {
            let mut node =
                GraphNode::new(NodeType::Topic, topic_label.clone(), graph_path.to_string());
            node.id = topic_id.clone();
            node.line_start = Some(site.line);
            node.contract = Some(CF::Provider(
                crate::federation::contracts::model::ProviderFact {
                    method: crate::federation::contracts::model::HttpMethod::Any,
                    template: topic_name.clone(),
                    handler: None,
                    operation_id: None,
                    origin: crate::federation::contracts::model::ProviderOrigin::Code,
                },
            ));
            nodes.push(node);
        }

        // Resolve the enclosing function id. We always emit a node
        // here so the graph has a stable handle, even when no
        // Function/Method was indexed before.
        let owner_hint = site.owner_name.clone().unwrap_or_default();
        let source_id = resolve_function_id(graph, graph_path, &owner_hint, site.line, namespace);

        let edge_type = match site.kind {
            SiteKind::Produces => EdgeType::Produces,
            SiteKind::Consumes | SiteKind::Scheduled => EdgeType::Consumes,
        };

        edges.push(GraphEdge {
            edge_type,
            source_id: source_id.clone(),
            target_id: topic_id.clone(),
            weight: Some(1.0),
            cross_repo: false,
            provenance: Some(EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            }),
            site: Some(SourceSite {
                path: graph_path.to_string(),
                line: site.line,
            }),
            detail: None,
        });

        // The consumer side: emit one `TopicConsumer` contract fact
        // on the source function. We attach it to the same source_id
        // so the joiner can resolve `consumer → topic` per function.
        // The fact is what `ContractJoiner::run` matches against
        // producer-side `Topic` nodes.
        if emitted_consumer_facts.insert(source_id.clone()) {
            let mut consumer_node = GraphNode::new(
                NodeType::Function,
                if owner_hint.is_empty() {
                    topic_label.clone()
                } else {
                    owner_hint.clone()
                },
                graph_path.to_string(),
            );
            consumer_node.id = source_id.clone();
            consumer_node.line_start = Some(site.line);
            let kind = match site.kind {
                SiteKind::Scheduled => TopicConsumerKind::Scheduled,
                _ => TopicConsumerKind::Subscription,
            };
            consumer_node.contract = Some(CF::TopicConsumer(TopicConsumerFact {
                broker: site.broker.clone(),
                name: topic_name.clone(),
                kind,
            }));
            nodes.push(consumer_node);
        }
    }

    (nodes, edges)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType, RepoNamespace};

    fn empty_db() -> GraphDatabase {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("graph.bin");
        GraphDatabase::new(&path).unwrap()
    }

    fn write_cwd_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    /// kafkajs `producer.send({ topic: 'orders.created' })` must emit
    /// a `Topic` node plus a `Produces` edge.
    #[test]
    fn kafkajs_producer_emits_produces_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "async function publish() {\n  await producer.send({ topic: 'orders.created', messages: [] });\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        let n = scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        assert!(n >= 1, "expected at least 1 node, got {n}");

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "expected Topic kafka/orders.created, got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );

        let produces: Vec<_> = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::Produces)
            .collect();
        assert!(!produces.is_empty(), "expected at least one Produces edge");
    }

    /// kafkajs consumer: `consumer.run({ topics: ['orders.created'] })`.
    #[test]
    fn kafkajs_consumer_emits_consumes_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("billing.ts"),
            "async function start() {\n  await consumer.run({ topics: ['orders.created'], eachMessage: handler });\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let consumes: Vec<_> = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::Consumes)
            .collect();
        assert!(!consumes.is_empty());
        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "expected kafka/orders.created"
        );
    }

    /// aiokafka producer: `await producer.send_and_wait('orders.created', payload)`.
    #[test]
    fn aiokafka_producer_emits_produces_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.py"),
            "async def publish(producer):\n    await producer.send_and_wait('orders.created', payload)\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    /// aiokafka `KafkaConsumer('orders.created')` constructor pattern.
    #[test]
    fn aiokafka_consumer_constructor_emits_consumes_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("billing.py"),
            "def start():\n    consumer = KafkaConsumer('orders.created', bootstrap_servers='localhost:9092')\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let consumes = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::Consumes)
            .count();
        assert!(consumes >= 1);
    }

    /// Celery `@app.task` decorator emits a scheduled topic.
    #[test]
    fn celery_task_emits_scheduled_topic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tasks.py"),
            "@app.task\ndef process_order(order_id):\n    pass\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(!topics.is_empty(), "expected at least one scheduled Topic");
    }

    /// NestJS `@Cron('0 0 * * *')` emits a scheduled topic.
    #[test]
    fn nestjs_cron_emits_scheduled_topic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("scheduler.ts"),
            "@Cron('0 0 * * *')\nasync function rollup() {}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(!topics.is_empty(), "expected at least one scheduled Topic");
    }

    /// kafka-go: `producer.SendMessage(&kafka.Message{Topic: "orders.created"})`.
    #[test]
    fn kafka_go_producer_emits_produces_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.go"),
            "func publish(w *kafka.Writer) error {\n    return w.SendMessages(&kafka.Message{Topic: \"orders.created\", Value: []byte(\"x\")})\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    /// rdkafka: `FutureRecord::to("orders.created")` is the canonical
    /// publish shape. The detector pulls the topic name out of the
    /// first argument.
    #[test]
    fn rdkafka_producer_emits_produces_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.rs"),
            "fn publish(p: FutureProducer) {\n    let record = FutureRecord::to(\"orders.created\").payload(b\"x\");\n    p.send(record, Duration::from_secs(1));\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "rdkafka FutureRecord::to must emit a Topic; got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    /// Same-file constants: `const FOO = 'orders.created';`
    /// then `producer.send({ topic: FOO })` resolves to `orders.created`.
    #[test]
    fn ts_constant_topic_name_is_resolved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "const TOPIC_NAME = 'orders.created';\nasync function publish() {\n  await producer.send({ topic: TOPIC_NAME, messages: [] });\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topics.iter().any(|t| t.name == "kafka/orders.created"),
            "TOPIC_NAME constant must resolve; got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    /// Dynamic topic arg (`topic: variable`) is skipped — the joiner
    /// surfaces those calls in `coverage.unnormalized`.
    #[test]
    fn dynamic_topic_arg_does_not_emit_topic_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "async function publish(topic) {\n  await producer.send({ topic: topic, messages: [] });\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topics = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .count();
        assert_eq!(topics, 0, "dynamic topic must not emit a Topic node");
    }

    /// `replace_sensor_output` retracts prior output: a second scan
    /// after the source is changed must not leave stale nodes behind.
    #[test]
    fn rescan_retracts_stale_topic_nodes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.ts"),
            "producer.send({ topic: 'orders.created' });\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        assert!(
            graph
                .get_all_nodes()
                .into_iter()
                .any(|n| n.node_type == NodeType::Topic),
            "first scan should produce a Topic"
        );

        // Rewrite the file with a different topic.
        std::fs::write(
            dir.path().join("a.ts"),
            "producer.send({ topic: 'orders.updated' });\n",
        )
        .unwrap();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        let names: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .map(|n| n.name)
            .collect();
        assert!(
            !names.iter().any(|n| n == "kafka/orders.created"),
            "stale Topic must be retracted; got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "kafka/orders.updated"),
            "new Topic must be present; got {names:?}"
        );
    }

    /// A file with no event-side surface must not emit any Topic or
    /// Produces/Consumes edge.
    #[test]
    fn benign_file_emits_no_topic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "# nothing to see here\n").unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        let n = scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn extract_string_literal_handles_double_and_single_quotes() {
        assert_eq!(
            extract_string_literal("\"orders.created\","),
            Some("orders.created".into())
        );
        assert_eq!(
            extract_string_literal("'orders.created'"),
            Some("orders.created".into())
        );
        assert_eq!(extract_string_literal("foo"), None);
    }

    #[test]
    fn collect_constants_extracts_const_and_let() {
        let src = "const A = 'foo';\nlet B = \"bar\";\nconst c = 'baz';\n";
        let constants = collect_same_file_constants(src, "ts");
        assert_eq!(constants.get("A"), Some(&"foo".to_string()));
        assert_eq!(constants.get("B"), Some(&"bar".to_string()));
        assert_eq!(constants.get("c"), Some(&"baz".to_string()));
    }

    #[test]
    fn collect_constants_for_python_module_level_assignments() {
        let src = "TOPIC = \"orders.created\"\n";
        let constants = collect_same_file_constants(src, "py");
        assert_eq!(constants.get("TOPIC"), Some(&"orders.created".to_string()));
    }

    // Ensure the read/scan helpers don't emit anything for a file
    // whose only AST-level input is a comment or an unrelated string.
    #[test]
    fn negative_coverage_only_comments_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.ts"),
            "// producer.send({ topic: 'orders.created' })\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        let topics = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .count();
        assert_eq!(topics, 0, "commented-out code must not emit Topics");
    }

    // Silence unused-import warnings when the file is compiled in a
    // subset of feature flags.
    #[allow(dead_code)]
    fn _silence_unused(_g: &GraphDatabase) {
        let _ = write_cwd_file;
        let _n = GraphNode::new(NodeType::Topic, "x".into(), "y".into());
    }
}
