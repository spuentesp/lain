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
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{self, Lang};
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
    let lang = match ext_to_lang(ext) {
        Some(l) => l,
        None => return out,
    };
    let patterns = Patterns::patterns();
    let (producers, consumers, scheduled) = util::topic_idioms_for(patterns, lang);
    let lines: Vec<&str> = content.lines().collect();
    for (idx, raw) in lines.iter().enumerate() {
        let line_num = idx as u32 + 1;
        // Strip line comments. Without this, a commented-out
        // `producer.send({ topic: 'foo' })` would still match the
        // regex and emit a phantom Topic.
        let line = strip_line_comment(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for m in util::walk_idioms(&producers, line, line_num) {
            if let Some(value) = resolve_idiom_capture(&m, constants) {
                out.push(DetectedSite {
                    kind: SiteKind::Produces,
                    topic: Some(value),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
        for m in util::walk_idioms(&consumers, line, line_num) {
            if let Some(value) = resolve_idiom_capture(&m, constants) {
                out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(value),
                    broker: DEFAULT_BROKER.into(),
                    line: line_num,
                    owner_name: None,
                });
            }
        }
        for m in util::walk_idioms(&scheduled, line, line_num) {
            // The walker returns the spec (or a Celery-marker
            // match with no spec). Project to `(topic, owner_name,
            // broker)` based on the framework id.
            let (topic, owner_name, broker) = schedule_value(&m, &lines, idx, ext, constants);
            out.push(DetectedSite {
                kind: SiteKind::Scheduled,
                topic,
                broker,
                line: line_num,
                owner_name,
            });
        }
    }
    out
}

/// Map a file extension to the Tier-1 `Lang` the walker uses for
/// per-language idiom selection. Returns `None` for extensions no
/// event-sensor walker handles (the caller skips them).
fn ext_to_lang(ext: &str) -> Option<Lang> {
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
        "py" => Some(Lang::Python),
        "rs" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        _ => None,
    }
}

/// Resolve the walker's capture to a topic name. The walker
/// tracks which slot (`literal` vs `identifier`) matched; the
/// caller (event_sensor) interprets them per the pre-Tier-1
/// rules:
/// - `literal` set: the source had a quoted string, the value
///   is the literal contents. Use as-is.
/// - `identifier` set: the source had a bare identifier (e.g.
///   `topic: TOPIC_NAME`); try to resolve it against the
///   same-file constants table. If the constant is missing,
///   return `None` (a dynamic topic — the emitter drops it
///   and the joiner surfaces it in `coverage.unnormalized`).
fn resolve_idiom_capture(
    m: &util::IdiomMatch,
    constants: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(lit) = m.literal.as_deref() {
        return Some(lit.to_string());
    }
    if let Some(ident) = m.identifier.as_deref() {
        if let Some(resolved) = constants.get(ident) {
            return Some(resolved.clone());
        }
        // Bare identifier that doesn't resolve to a constant:
        // a dynamic topic. The original Rust detector's
        // `extract_object_property_identifier` + `resolve_topic_name`
        // chain returned `None` here too, and the emitter's
        // `site.topic.clone() → match None → continue` filtered
        // it out. Tier 1 preserves that behaviour: a
        // `producer.send({ topic: <bare> })` where `<bare>` is
        // not a same-file constant emits no Topic node.
        return None;
    }
    None
}

/// Project a `Scheduled` walker match into the `(topic, owner_name,
/// broker)` triple the emitter expects. The exact behaviour depends
/// on the framework:
/// - NestJS `@Cron('spec')` — the spec is the topic; the function
///   name is the owner. The walker captured the spec; the
///   lookahead finds the function name.
/// - Celery `@app.task` / `@shared_task` — no spec; the topic is
///   the function name. The walker captured `None` for the spec;
///   the lookahead finds the function name.
///
/// The Celery case is the one with no spec — when the walker
/// yields a match with no `literal` slot, we fall back to the
/// function-lookahead populating the topic.
fn schedule_value(
    m: &util::IdiomMatch,
    lines: &[&str],
    idx: usize,
    ext: &str,
    _constants: &BTreeMap<String, String>,
) -> (Option<String>, Option<String>, String) {
    let spec = m.literal.clone();
    // Lookahead for the decorated function (5 lines, stop at the
    // next decorator / function-decl / blank-line skip). The
    // shape of the function declaration varies by language.
    let owner_name = lookahead_for_function(lines, idx + 1, ext);
    let (broker, topic) = match m.framework_id.as_str() {
        "celery-task" => ("celery".to_string(), owner_name.clone().or(spec)),
        // NestJS `@Cron` and any other `Scheduled` entry the YAML
        // grows in the future.
        _ => (
            "schedule".to_string(),
            spec.clone().or_else(|| owner_name.clone()),
        ),
    };
    (topic, owner_name, broker)
}

/// Look at the next few lines after a `@decorator` and return
/// the name of the function the decorator decorates. Stops at a
/// blank line, a non-blank non-decorator non-function line, or
/// the next decorator. Returns `None` if the lookahead cannot
/// resolve a function name.
fn lookahead_for_function(lines: &[&str], start: usize, ext: &str) -> Option<String> {
    for ahead in lines.iter().skip(start).take(5) {
        let ahead = ahead.trim();
        if ahead.is_empty() {
            continue;
        }
        // The comment marker depends on the language. Mirrors
        // the per-language dispatch in `detect_sites`.
        if (ext == "py" && ahead.starts_with('#')) || (ext != "py" && ahead.starts_with("//")) {
            continue;
        }
        // Stop at the next decorator — it's not the function.
        if ahead.starts_with('@') {
            return None;
        }
        let name = match ext {
            "py" => {
                if ahead.starts_with("def ") || ahead.starts_with("async def ") {
                    parse_def_name(ahead)
                } else {
                    return None;
                }
            }
            _ => {
                if ahead.starts_with("function ")
                    || ahead.starts_with("async function ")
                    || ahead.starts_with("export ")
                {
                    parse_function_name(ahead)
                } else {
                    return None;
                }
            }
        };
        return name;
    }
    None
}

// ─── Per-language string extractors ───────────────────────────────────
//
// The pre-Tier-1 `event_sensor` carried ~200 lines of bespoke
// per-language extractors (kafkajs / aiokafka / rdkafka / kafka-go /
// Celery / NestJS). Tier 1 collapsed all of them into
// `frameworks.yaml` regexes consumed by the generic walker in
// `crate::server::sensors::util::walk_idioms`. The only
// per-language helper still needed is the function-lookahead
// (the language's "function declaration" shape), which lives in
// `lookahead_for_function` and the two `parse_function_name` /
// `parse_def_name` helpers below.

/// Parse `function foo(…)` / `async function foo(…)` /
/// `export function foo(…)` / `export const foo = …` /
/// `function $foo(…)` to extract the name.
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

/// Parse `def foo(…)` / `async def foo(…)` to extract the name.
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

// ─── Emission ────────────────────────────────────────────────────────

/// Emit one `Topic` node plus a `Produces`/`Consumes` edge per site.
///
/// Edge-source invariant: the source is either an already-indexed
/// enclosing symbol, or the per-site synthetic node that the consumer
/// block below emits — never a third, unmaterialized id.
/// `insert_edges_batch` drops an edge whose endpoints aren't in the
/// graph (`graph/mod.rs:1156`), so an unmaterialized source does not
/// fail loudly: the edge simply vanishes.
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

        // The synthetic per-site node. Two jobs, and it is emitted when
        // either one needs it (the block below):
        //
        // 1. Edge anchor — the `Produces`/`Consumes` edge source when
        //    no enclosing function is indexed. `insert_edges_batch`
        //    drops an edge whose endpoints aren't in the graph
        //    (`graph/mod.rs:1156`) without failing, so an unmaterialized
        //    source makes the edge vanish rather than error.
        // 2. Fact carrier for subscription sites.
        //
        // When a symbol IS indexed the edge rides the symbol instead —
        // that is what kept peer sensors' edges alive (see
        // `util::SQL_READ_PREFIX`) — and a producer site then needs
        // neither job, so no node is emitted for it.
        let id_name = format!(
            "{}{graph_path}:{}",
            crate::server::sensors::util::TOPIC_READ_PREFIX,
            site.line
        );
        let site_node_id = GraphNode::generate_id(
            &NodeType::Function,
            graph_path,
            &id_name,
            Some(site.line),
            namespace,
        );

        // Edge source: the enclosing function when one is indexed (so
        // call-chain traversal is unchanged), else the site's own
        // synthetic node — which we materialize below, so the edge
        // never dangles.
        let source_id =
            crate::server::sensors::util::enclosing_symbol(graph, graph_path, site.line)
                .map(|sym| sym.id.clone())
                .unwrap_or_else(|| site_node_id.clone());

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

        // Emit the synthetic node when it is either the edge anchor or
        // the fact carrier. A producer site whose enclosing function IS
        // indexed needs neither — its edge rides the symbol — so no
        // node is emitted for it.
        let is_subscription = matches!(site.kind, SiteKind::Consumes | SiteKind::Scheduled);
        let is_edge_anchor = source_id == site_node_id;
        if !is_subscription && !is_edge_anchor {
            continue;
        }
        // ONLY subscription sites carry the fact. A `Produces` site is
        // not a subscriber — emitting one puts a false consumer in
        // `ContractIndex`, and false consumers hide real ones.
        let fact = is_subscription.then(|| {
            let kind = match site.kind {
                SiteKind::Scheduled => TopicConsumerKind::Scheduled,
                _ => TopicConsumerKind::Subscription,
            };
            CF::TopicConsumer(TopicConsumerFact {
                broker: site.broker.clone(),
                name: topic_name.clone(),
                kind,
            })
        });
        // Two sites on one physical line share the synthetic node
        // (`topic-read:<path>:<line>`), and `GraphNode.contract` holds a
        // single fact, so at most one of them can be recorded. Which one
        // must not depend on the order the walk happened to see them in:
        // a `Produces` site must never block a consumer's fact from
        // landing. A later subscription can therefore fill an empty slot
        // but never overwrite a fact already recorded.
        if let Some(existing) = nodes.iter_mut().find(|n| n.id == site_node_id) {
            if existing.contract.is_none() {
                existing.contract = fact;
            }
        } else {
            let mut consumer_node = crate::server::sensors::util::synthetic_site_node(
                id_name, graph_path, site.line, namespace,
            );
            consumer_node.contract = fact;
            nodes.push(consumer_node);
        }
    }

    (nodes, edges)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::ContractFact;
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

    /// A pure producer must NOT emit a `TopicConsumer` fact. It
    /// produces; claiming it subscribes is a false consumer in
    /// `ContractIndex` — and false consumers are the noise that hides
    /// real ones.
    #[test]
    fn a_producer_site_emits_no_topic_consumer_fact() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "async function publish() {\n  await producer.send({ topic: 'orders.created', messages: [] });\n}\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let consumers = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| matches!(n.contract, Some(ContractFact::TopicConsumer(_))))
            .count();
        assert_eq!(
            consumers, 0,
            "a producer site must not emit a TopicConsumer fact — it produces, \
             it does not subscribe"
        );
    }

    /// Two defects share one cause: the consumer-fact block runs for
    /// every site kind and de-dupes on `source_id`.
    ///
    /// 1. A producer site registers `TopicConsumer` for the topic it
    ///    *produces* — a false consumer in `ContractIndex`.
    /// 2. When the enclosing symbol is indexed, both sites share one
    ///    `source_id`, so the first site's fact wins the de-dupe and
    ///    the other site's real topic is **dropped** — a discovered
    ///    consumer disappearing, this codebase's worst failure.
    ///
    /// Minted explicitly (no tree-sitter indexer in `scan_workspace_event`)
    /// so case 2 is actually exercised.
    #[test]
    fn a_function_that_produces_and_consumes_keeps_the_consumer_fact() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("relay.py"),
            "def relay():\n    producer.send(topic='orders.created', value={})\n    c1 = KafkaConsumer('orders.shipped')\n    c2 = KafkaConsumer('orders.delivered')\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        let mut fn_node = GraphNode::new(NodeType::Function, "relay".into(), "relay.py".into());
        fn_node.line_start = Some(1);
        fn_node.line_end = Some(4);
        fn_node.id = GraphNode::generate_id(&NodeType::Function, "relay.py", "relay", Some(1), &ns);
        graph.upsert_node(fn_node).expect("insert enclosing symbol");

        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let consumed: Vec<String> = graph
            .get_all_nodes()
            .into_iter()
            .filter_map(|n| match n.contract {
                Some(ContractFact::TopicConsumer(f)) => Some(f.name),
                _ => None,
            })
            .collect();
        assert!(
            !consumed.contains(&"orders.created".to_string()),
            "a producer must not register as a consumer of its own topic — \
             got {consumed:?}"
        );
        assert!(
            consumed.contains(&"orders.shipped".to_string()),
            "the orders.shipped consumer fact was dropped — got {consumed:?}"
        );
        assert!(
            consumed.contains(&"orders.delivered".to_string()),
            "a second topic site in the same function lost its fact to the \
             source_id de-dupe — got {consumed:?}"
        );
    }

    /// Two topic sites on ONE physical line share the synthetic node
    /// (`topic-read:<path>:<line>`), and `GraphNode.contract` holds a
    /// single fact — so at most one of them can be recorded. But which
    /// one must not depend on the order the regex walk happened to see
    /// them in: a `Produces` site processed first used to claim the
    /// de-dupe slot with NO fact, and the real consumer on the same
    /// line then lost its fact entirely.
    #[test]
    fn a_consumer_fact_is_never_lost_to_a_same_line_producer() {
        for (label, line) in [
            (
                "producer first",
                "    producer.send(topic='orders.created', value={}); c = KafkaConsumer('orders.shipped')\n",
            ),
            (
                "consumer first",
                "    c = KafkaConsumer('orders.shipped'); producer.send(topic='orders.created', value={})\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("relay.py"), format!("def relay():\n{line}")).unwrap();
            let graph = empty_db();
            let ns = RepoNamespace::for_test();
            scan_workspace_event(&graph, dir.path(), &ns).unwrap();

            let consumed: Vec<String> = graph
                .get_all_nodes()
                .into_iter()
                .filter_map(|n| match n.contract {
                    Some(ContractFact::TopicConsumer(f)) => Some(f.name),
                    _ => None,
                })
                .collect();
            assert!(
                consumed.contains(&"orders.shipped".to_string()),
                "{label}: the consumer's fact must survive sharing a line \
                 with a producer — got {consumed:?}"
            );
        }
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
