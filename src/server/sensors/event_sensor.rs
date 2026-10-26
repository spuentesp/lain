//! Event sensor (§6.7, stretch goal).
//!
//! Detects message-bus and scheduler subscriptions / publications:
//!
//! - kafkajs / confluent-kafka (Node/TypeScript): `producer.send({ topic })`
//!   and `consumer.run({ topic })` / `consumer.subscribe({ topics: [..] })`.
//! - confluent_kafka (Python): `consumer.subscribe(["topic"])` — the
//!   literal topic list on the `subscribe` call.
//! - aiokafka (Python): `AIOKafkaProducer(...)`, `KafkaConsumer(...)`,
//!   `.send(topic=..., ...)`.
//! - rdkafka (Rust): `FutureProducer::send(record, ...)` where the
//!   `record` carries the topic name in its `topic` field, plus
//!   `StreamConsumer::subscribe(&[...])`.
//! - kafka-go (Go): `producer.SendMessage(&kafka.Message{Topic: "..."})`
//!   and `consumer.SubscribeTopics("...")`.
//! - pika (Python): `basic_publish(routing_key=...)`,
//!   `basic_consume(queue=...)`, `queue_declare(queue=...)` — mint
//!   `amqp/<queue>` topics (broker derived from the framework id).
//! - amqplib (Node/TS): `channel.publish(exchange, routingKey, ...)`
//!   and `channel.consume(queue, ...)` — same `amqp/<queue>` shape.
//! - Celery (Python): `@app.task` and `@shared_task` decorators make the
//!   decorated function a scheduled topic named after its qualified
//!   module path.
//! - NestJS (TypeScript): `@Cron('0 0 * * *')` decorator marks the
//!   following function as a scheduled topic.
//!
//! Each `(broker, topic)` pair mints ONE canonical `Topic` hub node per
//! repo (id anchored on [`TOPIC_ANCHOR`] with the label alone — no path,
//! no line) and each detected site hangs a `Produces` (publishers) or
//! `Consumes` (subscribers) edge with `SourceSite` provenance off that
//! hub. Topic names are extracted from string literals and same-file
//! `const` / `let` constant assignments (§6.7 "same-file constants").
//! Dynamic topic names are NOT silently invisible: a producer-side
//! dynamic name is dropped (no `(broker, name)` to anchor a hub on —
//! a hub with the sentinel would falsely advertise a provider that
//! never resolves); a consumer-side dynamic name emits a synthetic
//! per-site `Function` node carrying a `TopicConsumerFact` whose
//! `name` is [`TOPIC_DYNAMIC_SENTINEL`] (`<dynamic>`), and
//! `TopicDispatch` returns `Unresolved { reason: DynamicTopic, .. }`
//! for it — the joiner pushes that call onto
//! `coverage.unnormalized` next to the existing HTTP `Unnormalized`
//! arm. See `docs/protocol-coverage.md`, "Dynamic topic names".
//!
//! After site emission the sensor also walks payload schema files
//! (`*.avsc`, `*.schema.json`): each parses into a `Schema` node with
//! `Field` children (the same shape `grpc_provider_sensor` emits for
//! proto messages), and a schema that names an already-emitted topic
//! gets the `Topic → Schema` `PayloadSchema` edge that feeds the
//! payload arm of the contract join (`field_join.rs`).
//!
//! Adding another bus or job queue (MQTT, NATS, Redis, SQS, …) is a
//! detection-pattern addition only — the hub model is broker-generic
//! (`Topic { broker, name }`). The recipe, with a real pattern-entry
//! template and the tests to mirror, lives in
//! `docs/protocol-coverage.md` ("Recipe: adding a bus without new
//! design"), which also names the config-extraction follow-up.
//!
//! The sensor runs at phase 2 so `Topic` nodes exist before the
//! joiner binds consumer services to producer services (§7.7). It
//! publishes through `replace_sensor_output(EventSensor, …)` so a
//! rescan retracts only its own previous output (§6.1).

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{SourceSite, UnresolvedReason};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::env_sensor::{self, EnvLookup, ScalarBindingIndex};
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::payload_schema::{parse_avro_schema, parse_json_schema};
use crate::server::sensors::util::{self, Lang};
use crate::server::sensors::util_tokenize::strip_line_comment;
use crate::server::sensors::ScanReport;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type TopicIdioms = (
    Vec<util::CompiledIdiom>,
    Vec<util::CompiledIdiom>,
    Vec<util::CompiledIdiom>,
);

#[cfg(test)]
thread_local! {
    static IDIOM_SET_COMPILE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const DEFAULT_BROKER: &str = "kafka";

/// Hub identity anchor — the `path` component of the hub's ID only.
/// A `Topic` hub's id is
/// `generate_id(Topic, TOPIC_ANCHOR, "{broker}/{topic}", None, ns)` —
/// the label alone — so every publish/subscribe site in the repo
/// converges on ONE node per `(broker, topic)`. Site provenance lives
/// on the `Produces`/`Consumes` edges (`site: SourceSite { path, line }`),
/// one edge per site. The node RECORD keeps `path` = a real site file
/// (see `emit_sites`): `prune_orphans` drops nodes whose path is not a
/// tracked file. `tests/event_hub_e2e.rs` pins this literal.
const TOPIC_ANCHOR: &str = "<topics>";

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
impl crate::server::sensors::Sensor for EventSensor {
    fn name(&self) -> &'static str {
        "event"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::EntryPoints
    }
    fn phase(&self) -> u8 {
        2
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_event(graph, root, namespace)
    }
    fn scan_with_report(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<ScanReport, LainError> {
        scan_workspace_event_with_report(graph, root, namespace)
    }
}
inventory::submit!(crate::server::sensors::SensorEntry(&EventSensor));

// ─── Workspace scan ──────────────────────────────────────────────────

pub fn scan_workspace_event(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    scan_workspace_event_with_report(graph, root, namespace).map(|r| r.emitted)
}

/// Like [`scan_workspace_event`], and also reports the producer sites whose
/// topic could not be resolved, as `UnresolvedReason::DynamicTopic`, for the
/// coverage ledger. Consumer sites are not counted here: they reach the
/// joiner through the dynamic-topic sentinel node and are reported there.
pub fn scan_workspace_event_with_report(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<ScanReport, LainError> {
    if graph.is_read_only() {
        return Ok(ScanReport::default());
    }

    // Load once per scan so every site uses the same repo-local patterns.
    let patterns = Patterns::with_overrides(root)?;
    let mut idioms: Vec<(Lang, TopicIdioms)> = Vec::new();

    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();
    // Hub ids minted in this scan. One `Topic` node per (broker, topic)
    // per repo — pushed once no matter how many sites reference it
    // (per REPO scan, not per file: two files publishing one topic
    // share the hub).
    let mut emitted_topic_ids: BTreeSet<String> = BTreeSet::new();
    let mut producer_gaps = ProducerGaps::default();
    // The env sensor runs first (phase 0), so its scalar channel is populated.
    let env = env_sensor::scalar_bindings_for(root);
    // Go package scope is read from every `.go` file before the site walk, so a
    // reference to another package's value resolves regardless of walk order.
    let go_files: Vec<(std::path::PathBuf, String)> = util::scan_files(root, |p| {
        (p.extension().and_then(|e| e.to_str()) == Some("go")).then_some(())
    })
    .map(|(p, c, _)| (p, c))
    .collect();
    let go_scopes = go_constant_scopes(root, &go_files, &env);

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
        let Some(lang) = ext_to_lang(ext) else {
            continue;
        };
        let index = idioms
            .iter()
            .position(|(cached_lang, _)| *cached_lang == lang)
            .unwrap_or_else(|| {
                #[cfg(test)]
                IDIOM_SET_COMPILE_COUNT.with(|count| count.set(count.get() + 1));
                idioms.push((lang, util::topic_idioms_for(&patterns, lang)));
                idioms.len() - 1
            });
        let path_str = graph_path(root, &path);
        let mut constants = collect_same_file_constants(&content, ext, &env);
        let mut gaps = collect_env_gaps(&content, ext, &env);
        if ext == "go" {
            if let Some(scope) = go_scopes.get(&dir_key(root, &path)) {
                for (key, value) in &scope.values {
                    constants
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                for (key, reason) in &scope.gaps {
                    gaps.entry(key.clone()).or_insert(*reason);
                }
            }
        }
        let mut unresolved_producers: Vec<(u32, Option<String>)> = Vec::new();
        let sites = detect_sites(
            &content,
            ext,
            &constants,
            &idioms[index].1,
            &mut unresolved_producers,
        );
        for (line, identifier) in unresolved_producers {
            let reason = identifier
                .as_ref()
                .and_then(|id| gaps.get(id))
                .copied()
                .unwrap_or(UnresolvedReason::DynamicTopic);
            producer_gaps.record(reason, format!("{path_str}:{line}"));
        }
        let (nodes, edges) =
            emit_sites(&sites, &path_str, namespace, graph, &mut emitted_topic_ids);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }

    // Deterministic hub path. A hub is minted at whichever site the
    // walk reaches first, but walk order is not stable — and both
    // `prune_orphans` (path-keyed) and the joiner's `assign_services`
    // (path-prefix keyed) read this field. Rewrite each hub's path to
    // the min `site.path` among its own `Produces`/`Consumes` legs:
    // deterministic, and still a real file the orphan sweep keeps.
    let mut hub_paths: BTreeMap<String, String> = BTreeMap::new();
    for e in &all_edges {
        if !matches!(e.edge_type, EdgeType::Produces | EdgeType::Consumes) {
            continue;
        }
        let Some(site) = &e.site else { continue };
        hub_paths
            .entry(e.target_id.clone())
            .and_modify(|p| {
                if site.path < *p {
                    *p = site.path.clone();
                }
            })
            .or_insert_with(|| site.path.clone());
    }
    for n in &mut all_nodes {
        if n.node_type == NodeType::Topic {
            if let Some(p) = hub_paths.get(&n.id) {
                n.path = p.clone();
            }
        }
    }

    // Payload schemas ride the same EventSensor batch, scanned after
    // site emission so a schema's `topic` can bind to a `Topic` node
    // minted in this very scan.
    scan_payload_schemas(&mut all_nodes, &mut all_edges, root, namespace);

    graph.replace_sensor_output(SensorOwner::EventSensor, &all_nodes, &all_edges)?;
    Ok(ScanReport {
        emitted: all_nodes.len(),
        error: None,
        unresolved: producer_gaps.into_records(),
    })
}

/// Producer sites whose topic did not resolve, by the reason each earns: an
/// environment variable that is ambiguous or unmapped, or a dynamic topic.
/// Each reason keeps a count and the first few `path:line` ids, the same way
/// the SQL sensor keeps its unresolved samples.
#[derive(Default)]
struct ProducerGaps {
    by_reason: BTreeMap<UnresolvedReason, (usize, Vec<String>)>,
}

impl ProducerGaps {
    const MAX_SAMPLES: usize = 5;

    fn record(&mut self, reason: UnresolvedReason, site: String) {
        let (count, samples) = self.by_reason.entry(reason).or_default();
        *count += 1;
        if samples.len() < Self::MAX_SAMPLES {
            samples.push(site);
        }
    }

    fn into_records(self) -> Vec<UnresolvedRecord> {
        self.by_reason
            .into_iter()
            .map(|(reason, (count, sample_ids))| UnresolvedRecord {
                reason,
                count,
                sample_ids,
            })
            .collect()
    }
}

// ─── Payload schemas (Avro / JSON Schema → Topic → Schema) ───────────

/// Walk the workspace for payload schema files (`*.avsc`,
/// `*.schema.json`), parse them with the `payload_schema` parsers, and
/// emit the node set `grpc_provider_sensor.rs:180-250` emits for proto
/// messages: a `Schema` node carrying `ContractFact::Schema { direction:
/// Payload }`, one `Field` child per parsed field carrying
/// `ContractFact::Field`, and a `HasField` edge per field. When the
/// schema names a topic whose `Topic` node was already emitted in this
/// scan (`{DEFAULT_BROKER}/{topic}`), the `Topic → Schema`
/// `PayloadSchema` edge is minted too — without a `Topic` endpoint
/// there is no edge (`insert_edges_batch` would drop it silently,
/// `graph/mod.rs:1156`).
///
/// Everything is pushed into the caller's batch, which goes to a single
/// `replace_sensor_output(EventSensor, …)`: nodes are upserted before
/// edges there, so both endpoints of every edge materialize first. The
/// `Topic` lookup reads the pending batch (not the graph) because a
/// previous scan's `Topic` nodes are retracted in that same call —
/// binding to one would mint an edge toward a stale id.
///
/// Ownership: `derive_owner` routes `.avsc` / `*.schema.json`
/// `Schema`/`Field` nodes to `SensorOwner::EventSensor`, so these
/// nodes are retracted with the rest of this sensor's output and every
/// `ContractFact` here stays single-owner (`sensors/AGENTS.md`).
fn scan_payload_schemas(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    root: &Path,
    namespace: &RepoNamespace,
) -> usize {
    use crate::federation::contracts::model::{ContractFact as CF, Direction};
    use std::ffi::OsStr;

    let is_payload_file = |p: &Path| {
        let is_avsc = p.extension() == Some(OsStr::new("avsc"));
        let is_schema_json = p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".schema.json"));
        (is_avsc || is_schema_json).then_some(())
    };

    let mut minted = 0;
    for (path, content, ()) in util::scan_files(root, is_payload_file) {
        let parsed = if path.extension() == Some(OsStr::new("avsc")) {
            parse_avro_schema(&content)
        } else {
            parse_json_schema(&content)
        };
        let Some(ps) = parsed else { continue };

        let path_str = graph_path(root, &path);
        let line = ps.fields.first().map_or(1, |f| f.line);
        let schema_id = GraphNode::generate_id(
            &NodeType::Schema,
            &path_str,
            &ps.name,
            Some(line),
            namespace,
        );
        let mut schema_node = GraphNode::new(NodeType::Schema, ps.name.clone(), path_str.clone());
        schema_node.id = schema_id.clone();
        schema_node.line_start = Some(line);
        schema_node.line_end = Some(line);
        schema_node.contract = vec![CF::Schema {
            direction: Direction::Payload,
        }];
        nodes.push(schema_node);

        for f in &ps.fields {
            let field_name = f.path.to_string();
            let field_id = GraphNode::generate_id(
                &NodeType::Field,
                &path_str,
                &field_name,
                Some(f.line),
                namespace,
            );
            let mut field_node = GraphNode::new(NodeType::Field, field_name, path_str.clone());
            field_node.id = field_id.clone();
            field_node.line_start = Some(f.line);
            field_node.line_end = Some(f.line);
            field_node.contract = vec![CF::Field(f.meta.clone())];
            nodes.push(field_node);
            edges.push(GraphEdge {
                edge_type: EdgeType::HasField,
                source_id: schema_id.clone(),
                target_id: field_id,
                weight: Some(1.0),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Static {
                    source: crate::schema::StaticSource::Regex,
                }),
                site: None,
                detail: None,
            });
        }

        if let Some(topic_name) = &ps.topic {
            let topic_label = format!("{DEFAULT_BROKER}/{topic_name}");
            // One hub per (broker, topic) per repo (`TOPIC_ANCHOR`), so
            // this is a single `PayloadSchema` edge — the fan-out over
            // per-site nodes is gone. The lookup reads the pending
            // batch (not the graph) because a previous scan's `Topic`
            // nodes are retracted in the same `replace_sensor_output`
            // call — binding to one would mint an edge toward a stale
            // id. Both endpoints are in this batch, so no edge dangles.
            if let Some(topic_id) = nodes
                .iter()
                .find(|n| n.node_type == NodeType::Topic && n.name == topic_label)
                .map(|n| n.id.clone())
            {
                edges.push(GraphEdge {
                    edge_type: EdgeType::PayloadSchema,
                    source_id: topic_id,
                    target_id: schema_id.clone(),
                    weight: Some(1.0),
                    cross_repo: false,
                    provenance: Some(EdgeProvenance::Static {
                        source: crate::schema::StaticSource::Regex,
                    }),
                    site: None,
                    detail: None,
                });
            }
        }
        minted += 1;
    }
    minted
}

// ─── Detection (regex-first, language-dispatched) ────────────────────

/// `const NAME = "value";` (TS / JS / Go), `NAME = "value"` at
/// module level (Python), and `[modifiers] Type NAME = "value";` (Java). Keys must be identifiers and values must be
/// string literals. Both single- and double-quoted forms are
/// supported.
fn collect_same_file_constants(
    content: &str,
    ext: &str,
    env: &ScalarBindingIndex,
) -> BTreeMap<String, String> {
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
        let Some(name) = constant_name(lhs, ext) else {
            continue;
        };
        // A literal is its own value. An environment read takes the value the
        // env channel resolved; a conflict yields no constant, and a missing
        // variable falls back to the literal default written in the code.
        let value = match extract_string_literal(rhs) {
            Some(v) => v,
            None => match env_fallback(rhs, env) {
                Some(v) => v,
                None => continue,
            },
        };
        out.insert(name, value);
    }
    out
}

/// The directory a file sits in, relative to the workspace root. Go packages
/// are directories, so this is the package scope.
fn dir_key(root: &Path, path: &Path) -> String {
    path.parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One Go package's scope: the values its exported names resolve to, and the
/// names whose environment read the env channel cannot give a value.
#[derive(Debug, Clone, Default)]
struct GoScope {
    values: BTreeMap<String, String>,
    gaps: BTreeMap<String, UnresolvedReason>,
}

/// Go package scope for every directory with `.go` files: its own exported
/// package-level names, plus those of every other package under
/// `package.Name` (the import alias a caller uses). A package name shared by
/// two directories is not qualified at all, so the reference stays unresolved.
fn go_constant_scopes(
    root: &Path,
    files: &[(std::path::PathBuf, String)],
    env: &ScalarBindingIndex,
) -> BTreeMap<String, GoScope> {
    let mut own: BTreeMap<String, GoScope> = BTreeMap::new();
    for (path, content) in files {
        let dir = dir_key(root, path);
        let (values, gaps) = go_package_values(content, &dir, files, root, env);
        let scope = own.entry(dir).or_default();
        scope.values.extend(values);
        scope.gaps.extend(gaps);
    }
    let mut bases: BTreeMap<&str, usize> = BTreeMap::new();
    for dir in own.keys() {
        *bases
            .entry(dir.rsplit('/').next().unwrap_or(""))
            .or_default() += 1;
    }
    let mut qualified = GoScope::default();
    for (dir, scope) in &own {
        let base = dir.rsplit('/').next().unwrap_or("");
        if bases.get(base).copied().unwrap_or(0) != 1 {
            continue;
        }
        for (name, value) in &scope.values {
            qualified
                .values
                .insert(format!("{base}.{name}"), value.clone());
        }
        for (name, reason) in &scope.gaps {
            qualified.gaps.insert(format!("{base}.{name}"), *reason);
        }
    }
    own.into_iter()
        .map(|(dir, mut scope)| {
            for (key, value) in &qualified.values {
                scope
                    .values
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
            for (key, reason) in &qualified.gaps {
                scope.gaps.entry(key.clone()).or_insert(*reason);
            }
            (dir, scope)
        })
        .collect()
}

/// Exported `Name = value` declarations in one Go file. A value is a string
/// literal, or a call to a function in the same package that returns one. A
/// call that reads the environment without a value is a gap, not a value.
fn go_package_values(
    content: &str,
    dir: &str,
    files: &[(std::path::PathBuf, String)],
    root: &Path,
    env: &ScalarBindingIndex,
) -> (BTreeMap<String, String>, BTreeMap<String, UnresolvedReason>) {
    static DECL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static CALL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let decl = DECL.get_or_init(|| {
        regex::Regex::new(r"^\s*(?:var\s+)?([A-Z]\w*)\s*=\s*(.+?)\s*$").expect("go decl")
    });
    let call = CALL.get_or_init(|| regex::Regex::new(r"^([a-z_]\w*)\(\)$").expect("go call"));
    let mut values = BTreeMap::new();
    let mut gaps = BTreeMap::new();
    for line in content.lines() {
        let Some(c) = decl.captures(line) else {
            continue;
        };
        let name = c[1].to_string();
        let rhs = c[2].trim();
        if let Some(value) = extract_string_literal(rhs) {
            values.insert(name, value);
        } else if let Some(f) = call.captures(rhs) {
            match go_function_value(&f[1], dir, files, root, env) {
                Some(Ok(value)) => {
                    values.insert(name, value);
                }
                Some(Err(reason)) => {
                    gaps.insert(name, reason);
                }
                None => {}
            }
        }
    }
    (values, gaps)
}

/// What a Go function returns when it is a simple environment lookup with a
/// fallback. `None` when the function is not one. Otherwise the env channel's
/// value for the variable it reads; when the channel has none, the last string
/// literal the function returns. Failing both, the reason it cannot resolve.
fn go_function_value(
    name: &str,
    dir: &str,
    files: &[(std::path::PathBuf, String)],
    root: &Path,
    env: &ScalarBindingIndex,
) -> Option<Result<String, UnresolvedReason>> {
    let header = format!("func {name}()");
    let body = files
        .iter()
        .filter(|(p, _)| dir_key(root, p) == dir)
        .find_map(|(_, content)| {
            let mut found = false;
            let mut body: Vec<&str> = Vec::new();
            for line in content.lines() {
                if !found {
                    found = line.starts_with(&header);
                    continue;
                }
                if line == "}" {
                    return Some(body.join("\n"));
                }
                body.push(line);
            }
            None
        })?;
    let (var, _) = env_sensor::env_read_in(&body)?;
    let fallback = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("return "))
        .filter_map(extract_string_literal)
        .next_back();
    Some(match env.lookup(&var) {
        EnvLookup::Resolved(value) => Ok(value),
        EnvLookup::Ambiguous => Err(UnresolvedReason::EnvAmbiguous),
        EnvLookup::Unmapped => fallback.ok_or(UnresolvedReason::EnvUnmapped),
    })
}

/// Constants whose environment read the env channel cannot give a value to,
/// with the reason: `EnvAmbiguous` when the sources disagree, `EnvUnmapped`
/// when no source names the variable and the code gives no default.
fn collect_env_gaps(
    content: &str,
    ext: &str,
    env: &ScalarBindingIndex,
) -> BTreeMap<String, UnresolvedReason> {
    let mut out: BTreeMap<String, UnresolvedReason> = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = trimmed.split_once('=') else {
            continue;
        };
        let rhs = rhs.trim().trim_end_matches(';').trim();
        let Some(name) = constant_name(lhs, ext) else {
            continue;
        };
        if extract_string_literal(rhs).is_some() {
            continue;
        }
        let Some((var, default)) = env_sensor::env_read_in(rhs) else {
            continue;
        };
        match env.lookup(&var) {
            EnvLookup::Resolved(_) => {}
            EnvLookup::Ambiguous => {
                out.insert(name, UnresolvedReason::EnvAmbiguous);
            }
            EnvLookup::Unmapped => {
                if default.is_none() {
                    out.insert(name, UnresolvedReason::EnvUnmapped);
                }
            }
        }
    }
    out
}

/// The name a constant declaration binds, by language. Returns `None` when the
/// left-hand side is not a plain declaration of an identifier.
fn constant_name(lhs: &str, ext: &str) -> Option<String> {
    let lhs = lhs.trim();
    let name = match ext {
        "py" => lhs.to_string(),
        "kt" | "kts" => lhs
            .split(':')
            .next()?
            .split_whitespace()
            .last()?
            .to_string(),
        "java" | "cs" => {
            // `[modifiers] Type NAME =`: the name is the last token.
            let tokens: Vec<&str> = lhs.split_whitespace().collect();
            if tokens.len() < 2 {
                return None;
            }
            tokens[tokens.len() - 1].to_string()
        }
        _ => lhs
            .strip_prefix("const ")
            .or_else(|| lhs.strip_prefix("let "))
            .or_else(|| lhs.strip_prefix("var "))?
            .trim()
            .to_string(),
    };
    (!name.is_empty() && is_ident(&name)).then_some(name)
}

/// The value of an environment read in `rhs`, as the env channel sees it, or
/// the literal default when the channel has no value for the variable.
fn env_fallback(rhs: &str, env: &ScalarBindingIndex) -> Option<String> {
    let (var, default) = env_sensor::env_read_in(rhs)?;
    match env.lookup(&var) {
        EnvLookup::Resolved(value) => Some(value),
        EnvLookup::Ambiguous => None,
        EnvLookup::Unmapped => default,
    }
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

fn detect_sites(
    content: &str,
    ext: &str,
    constants: &BTreeMap<String, String>,
    idioms: &TopicIdioms,
    unresolved_producers: &mut Vec<(u32, Option<String>)>,
) -> Vec<DetectedSite> {
    let mut out: Vec<DetectedSite> = Vec::new();
    let (producers, consumers, scheduled) = idioms;
    // An entry with `requires` applies only to files that contain its marker
    // (the Go client's import path, say), so a bare `Topic:` field elsewhere
    // is not read as a producer.
    let applies =
        |i: &util::CompiledIdiom| i.requires.as_deref().is_none_or(|r| content.contains(r));
    let producers: Vec<util::CompiledIdiom> =
        producers.iter().filter(|i| applies(i)).cloned().collect();
    let consumers: Vec<util::CompiledIdiom> =
        consumers.iter().filter(|i| applies(i)).cloned().collect();
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
            // A producer whose topic cannot be resolved stays a `topic: None`
            // site: `emit_sites` drops it (no hub to anchor on). The scan counts
            // it into the coverage report, under the reason its identifier earns.
            let topic = resolve_idiom_capture(&m, constants);
            if topic.is_none() {
                unresolved_producers.push((line_num, m.identifier.clone()));
            }
            out.push(DetectedSite {
                kind: SiteKind::Produces,
                topic,
                broker: broker_for(&m).into(),
                line: line_num,
                owner_name: None,
            });
        }
        for m in util::walk_idioms(&consumers, line, line_num) {
            match resolve_idiom_capture(&m, constants) {
                Some(value) => out.push(DetectedSite {
                    kind: SiteKind::Consumes,
                    topic: Some(value),
                    broker: broker_for(&m).into(),
                    line: line_num,
                    owner_name: None,
                }),
                None => {
                    // Dynamic topic on the consumer side: the walker
                    // matched a subscribe/consume call but the topic
                    // arg could not be resolved (parameter, f-string,
                    // identifier without a same-file const, ...).
                    // `emit_sites` emits a synthetic per-site node
                    // carrying `TopicConsumerFact { name:
                    // TOPIC_DYNAMIC_SENTINEL, .. }` and the joiner
                    // surfaces it in `coverage.unnormalized` with
                    // `reason: DynamicTopic`. Producer-side dynamic
                    // topics are still dropped at `detect_sites` —
                    // there is no `(broker, name)` to anchor a hub on
                    // and a hub with the sentinel would falsely
                    // advertise a provider that never resolves.
                    out.push(DetectedSite {
                        kind: SiteKind::Consumes,
                        topic: None,
                        broker: broker_for(&m).into(),
                        line: line_num,
                        owner_name: None,
                    });
                }
            }
        }
        for m in util::walk_idioms(scheduled, line, line_num) {
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

/// Broker label for a produce/consume idiom match. The AMQP clients
/// (`pika*` in Python, `amqplib*` in TS/JS) mint `amqp/<queue>`
/// topics; every other framework keeps the `kafka/` default. Mirrors
/// `schedule_value`'s framework→value mapping: the `framework_id`
/// carries which YAML entry matched, so the broker follows the
/// framework, never the filename. The joiner's `default_broker_for`
/// parses the prefix back into `ContractKey::Topic { broker, .. }`
/// (`joiner/endpoints.rs:224`), so no joiner change is needed.
fn broker_for(m: &util::IdiomMatch) -> &str {
    m.broker.as_deref().unwrap_or(DEFAULT_BROKER)
}

/// Map a file extension to the Tier-1 `Lang` the walker uses for
/// per-language idiom selection. Returns `None` for extensions no
/// event-sensor walker handles (the caller skips them).
fn ext_to_lang(ext: &str) -> Option<Lang> {
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
        "py" => Some(Lang::Python),
        "rs" => Some(Lang::Rust),
        "cs" => Some(Lang::CSharp),
        "kt" | "kts" => Some(Lang::Kotlin),
        "go" => Some(Lang::Go),
        "rb" => Some(Lang::Ruby),
        "java" => Some(Lang::Java),
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
///   return `None` (a dynamic topic). On the consumer side the
///   emitter falls through to the dynamic-topic path: a synthetic
///   per-site node carrying `TopicConsumerFact { name:
///   TOPIC_DYNAMIC_SENTINEL, .. }`, which the joiner surfaces in
///   `coverage.unnormalized` with `reason: DynamicTopic`. The
///   producer side stays a pure drop — there is no
///   `(broker, name)` to anchor a hub on.
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

/// Emit the canonical `Topic` hub (one node per `(broker, topic)` per
/// repo) plus one `Produces`/`Consumes` edge per site — the edge's
/// `site` field is the per-site provenance.
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
    emitted_topic_ids: &mut BTreeSet<String>,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    use crate::federation::contracts::model::{
        ContractFact as CF, TopicConsumerFact, TopicConsumerKind,
    };

    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();

    for site in sites {
        // The discriminator for the "dynamic topic" path. Producer
        // sites with a non-literal name stay dropped (we have no `Topic`
        // hub to hang the `Produces` edge off — `prune_orphans` would
        // kill a hub without a real site path anyway). Consumer sites
        // with a non-literal name ARE recorded: a subscriber whose
        // topic argument is dynamic is a coverage gap the operator
        // cannot see otherwise, and the synthetic per-site node below
        // carries a `ContractFact::TopicConsumer` with the dynamic
        // sentinel as its `name`. `TopicDispatch` recognises the
        // sentinel and returns
        // `Unresolved { reason: DynamicTopic, target_service: None }`,
        // which the joiner pushes onto `coverage.unnormalized` — see
        // `joiner/mod.rs:390` (the `Unnormalized` arm) and the new
        // sibling arm for `DynamicTopic`. The `Produces` side stays a
        // pure drop, same as today.
        let dynamic_topic =
            site.topic.is_none() && matches!(site.kind, SiteKind::Consumes | SiteKind::Scheduled);
        let topic_name = match site.topic.clone() {
            Some(n) => n,
            None if !dynamic_topic => continue,
            None => crate::server::sensors::util::TOPIC_DYNAMIC_SENTINEL.to_string(),
        };

        // One canonical hub per (broker, name) per repo: identity is
        // the label alone (id anchored on `TOPIC_ANCHOR`, no line), so
        // every site — in this file or any other — converges on the
        // same node. The scan-scoped set pushes the node once; a hub
        // already emitted contributes only the site's edge below.
        //
        // Skipped for the dynamic-topic case: there is no real
        // `(broker, name)` to anchor on, and a hub with the
        // `TOPIC_DYNAMIC_SENTINEL` name would falsely advertise a
        // provider that never resolves. The synthetic per-site node
        // below is still emitted so the `TopicConsumerFact` reaches
        // the joiner.
        let topic_label = format!("{}/{}", site.broker, topic_name);
        let topic_id = if dynamic_topic {
            String::new()
        } else {
            GraphNode::generate_id(
                &NodeType::Topic,
                TOPIC_ANCHOR,
                &topic_label,
                None,
                namespace,
            )
        };
        if !dynamic_topic && emitted_topic_ids.insert(topic_id.clone()) {
            // `path` is a real site file, NOT the id anchor:
            // `prune_orphans` (ingestion orphan sweep) drops every node
            // whose `path` is not a git-tracked file, and a dropped hub
            // takes its `Produces`/`Consumes` edges with it. Same
            // convention as `sql_sensor`'s `Table` nodes (file-anchored
            // record, line-less id). The scan-wide pass in
            // `scan_workspace_event` then rewrites it to the MIN site
            // path among the hub's legs so it does not depend on walk
            // order. `line_start` stays `None` like the id — the site
            // lines live on the edges' `site` field.
            let mut node =
                GraphNode::new(NodeType::Topic, topic_label.clone(), graph_path.to_string());
            node.id = topic_id.clone();
            node.contract = vec![CF::Provider(
                crate::federation::contracts::model::ProviderFact {
                    method: crate::federation::contracts::model::HttpMethod::Any,
                    template: topic_name.clone(),
                    handler: None,
                    operation_id: None,
                    origin: crate::federation::contracts::model::ProviderOrigin::Code,
                },
            )];
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

        // Dynamic-topic case has no hub to hang the edge on — the
        // synthetic per-site node carries the `TopicConsumerFact`, and
        // `TopicDispatch` decides the verdict from the sentinel. Skip
        // the edge emission here; without a `target_id`, the edge
        // would dangle and `insert_edges_batch` would drop it silently.
        if !dynamic_topic {
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
        }

        // Emit the synthetic node when it is either the edge anchor or
        // the fact carrier. A producer site whose enclosing function IS
        // indexed needs neither — its edge rides the symbol — so no
        // node is emitted for it.
        let is_subscription = matches!(site.kind, SiteKind::Consumes | SiteKind::Scheduled);
        let is_edge_anchor = source_id == site_node_id;
        if !is_subscription && !is_edge_anchor {
            continue;
        }
        // ONLY subscription sites carry a fact. A `Produces` site is
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
        // (`topic-read:<path>:<line>`). `GraphNode.contract` is a list,
        // so each subscription records its own fact and none of them is
        // lost; a `Produces` site contributes none at all and the node
        // exists purely as the edge anchor. An exact duplicate is not
        // appended twice — one site repeating the same topic must not
        // double-count the consumer.
        if let Some(existing) = nodes.iter_mut().find(|n| n.id == site_node_id) {
            if let Some(f) = fact {
                if !existing.contract.contains(&f) {
                    existing.contract.push(f);
                }
            }
        } else {
            let mut consumer_node = crate::server::sensors::util::synthetic_site_node(
                id_name, graph_path, site.line, namespace,
            );
            if let Some(f) = fact {
                consumer_node.contract.push(f);
            }
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

    #[test]
    fn workspace_compiles_event_idioms_once_per_language_and_reloads_on_rescan() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..16 {
            std::fs::write(
                dir.path().join(format!("producer{index}.ts")),
                format!("producer.send({{ topic: 'orders.{index}' }});"),
            )
            .unwrap();
        }
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        IDIOM_SET_COMPILE_COUNT.with(|count| count.set(0));
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        assert_eq!(IDIOM_SET_COMPILE_COUNT.with(|count| count.get()), 1);
        assert_eq!(
            graph
                .all_nodes()
                .iter()
                .filter(|n| n.node_type == NodeType::Topic)
                .count(),
            16
        );
        std::fs::write(dir.path().join("producer0.ts"), "// removed producer").unwrap();
        IDIOM_SET_COMPILE_COUNT.with(|count| count.set(0));
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();
        assert_eq!(IDIOM_SET_COMPILE_COUNT.with(|count| count.get()), 1);
        assert_eq!(
            graph
                .all_nodes()
                .iter()
                .filter(|n| n.node_type == NodeType::Topic)
                .count(),
            15
        );
    }

    #[test]
    fn escaped_backslash_before_comment_does_not_emit_phantom_topic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("producer.ts"),
            r#"const path = "C:\\"; // producer.send({ topic: 'ghost' });
producer.send({ topic: 'orders.created' });
"#,
        )
        .unwrap();
        let graph = empty_db();
        scan_workspace_event(&graph, dir.path(), &RepoNamespace::for_test()).unwrap();
        let topics: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .map(|n| n.name)
            .collect();
        assert_eq!(topics, vec!["kafka/orders.created".to_string()]);
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

    /// confluent_kafka (Python) `consumer.subscribe(["order.events"])`
    /// — the enterprise-mesh notify-worker shape. The constructor
    /// `Consumer({...})` carries no topic; the literal topic list on
    /// `subscribe` is the only evidence, and it must yield a
    /// `Consumes` edge on `kafka/<topic>`.
    #[test]
    fn confluent_kafka_subscribe_emits_consumes_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.py"),
            "from confluent_kafka import Consumer\n\n\
             CONSUMER_CONFIG = {\"group.id\": \"notify-worker\", \"bootstrap.servers\": \"kafka:9092\"}\n\n\
             def build_consumer():\n\
             \x20   consumer = Consumer(CONSUMER_CONFIG)\n\
             \x20   consumer.subscribe([\"order.events\"])\n\
             \x20   return consumer\n",
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
            topics.iter().any(|t| t.name == "kafka/order.events"),
            "confluent_kafka subscribe([...]) must emit Topic kafka/order.events; \
             got {:?}",
            topics.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        let consumes = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::Consumes)
            .count();
        assert!(consumes >= 1, "expected at least one Consumes edge");
        let fact_carrier = graph.get_all_nodes().into_iter().find(|n| {
            n.contract
                .iter()
                .any(|f| matches!(f, ContractFact::TopicConsumer(t) if t.name == "order.events"))
        });
        assert!(
            fact_carrier.is_some(),
            "a TopicConsumer fact for order.events must be recorded"
        );
    }

    /// pika (Python AMQP) `ch.basic_consume(queue=...)` must mint a
    /// typed `amqp/<queue>` topic — NOT `kafka/<queue>`. Before the
    /// AMQP patterns existed this shape produced zero sensor facts
    /// (the enterprise fixture's ledger-svc blind spot); the anchor
    /// miner's heuristic queue anchor is not a sensor edge.
    #[test]
    fn pika_basic_consume_emits_amqp_consumes_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ledger.py"),
            "import pika\n\
             ch = conn.channel()\n\
             ch.queue_bind(queue=\"order-events\", exchange=\"x\")\n\
             ch.basic_consume(queue=\"order-events\", on_message_callback=cb)\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topic = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Topic && n.name == "amqp/order-events")
            .expect("amqp topic node (NOT kafka/order-events)");
        assert!(
            graph
                .all_edges()
                .iter()
                .any(|e| e.edge_type == EdgeType::Consumes && e.target_id == topic.id),
            "pika basic_consume must emit a Consumes edge to the amqp topic"
        );
    }

    /// `ch.basic_consume(queue=QUEUE_NAME)` where `QUEUE_NAME` is a
    /// bare identifier without a same-file `const` (or imported
    /// from another module — the constants table is per-file).
    /// The walker yields `IdiomMatch { literal: None, identifier:
    /// Some("QUEUE_NAME") }`, `resolve_idiom_capture` returns `None`
    /// because the constants table does not name it, and the
    /// emitter falls through to the dynamic-topic path: a
    /// synthetic per-site `Function` node carrying a
    /// `TopicConsumerFact` whose `name` is `TOPIC_DYNAMIC_SENTINEL`,
    /// no `Topic` hub (the literal-name form would have produced
    /// `amqp/<QUEUE_NAME>`, but there is no `QUEUE_NAME` to anchor
    /// on). Pinned end-to-end through the sensor — the joiner test
    /// `topic_join_dynamic_sentinel_is_unnormalized` exercises the
    /// dispatch path with a hand-built node, not the walker.
    #[test]
    fn dynamic_topic_on_consumer_emits_sentinel_synthetic_node_and_no_topic_hub() {
        use crate::federation::contracts::model::{
            ContractFact, TopicConsumerFact, TopicConsumerKind,
        };
        use crate::server::sensors::util::TOPIC_DYNAMIC_SENTINEL;
        let dir = tempfile::tempdir().unwrap();
        // `QUEUE_NAME` is a bare identifier with no same-file const
        // — the constants table has no entry for it, so
        // `resolve_idiom_capture` returns `None` for the consumer
        // walker.
        std::fs::write(
            dir.path().join("ledger.py"),
            "import pika\n\
             ch = conn.channel()\n\
             ch.basic_consume(queue=QUEUE_NAME, on_message_callback=cb)\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topic_nodes: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .collect();
        assert!(
            topic_nodes.is_empty(),
            "a dynamic-topic consumer must NOT mint a Topic hub (no broker/name to anchor); got {topic_nodes:?}"
        );

        let synthetic: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| {
                n.node_type == NodeType::Function
                    && n.name.starts_with(crate::server::sensors::util::TOPIC_READ_PREFIX)
                    && n.contract.iter().any(|f| {
                        matches!(f, ContractFact::TopicConsumer(TopicConsumerFact { name, broker: b, .. })
                            if name == TOPIC_DYNAMIC_SENTINEL && b == "amqp")
                    })
            })
            .collect();
        assert_eq!(
            synthetic.len(),
            1,
            "the consumer-side dynamic topic must emit exactly one synthetic per-site node carrying the sentinel fact; got {synthetic:?}"
        );
        // Exactly one fact on the node (the subscription).
        let contract = &synthetic[0].contract;
        assert_eq!(
            contract.len(),
            1,
            "the sentinel fact is the only contract fact on the synthetic node; got {contract:?}"
        );
        match &contract[0] {
            ContractFact::TopicConsumer(TopicConsumerFact { broker, name, kind }) => {
                assert_eq!(broker, "amqp", "pika broker label, not kafka");
                assert_eq!(name, TOPIC_DYNAMIC_SENTINEL);
                assert!(
                    matches!(kind, TopicConsumerKind::Subscription),
                    "basic_consume is a subscription"
                );
            }
            other => panic!("expected TopicConsumer with sentinel, got {other:?}"),
        }
    }

    /// pika `channel.queue_bind(queue=..., exchange=..., routing_key=...)`
    /// alone must mint a typed `amqp/<queue>` topic + Consumes edge.
    /// This is the acceptance fixtures' ONLY AMQP idiom (enterprise_
    /// {scale,mesh}'s ledger-svc binds its queue and never calls
    /// `basic_consume` / `basic_publish` / `queue_declare`), so an
    /// unrecognised `queue_bind` leaves the repo with zero sensor
    /// facts.
    #[test]
    fn pika_queue_bind_emits_amqp_consumes_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ledger.py"),
            "import pika\n\
             ch = conn.channel()\n\
             ch.queue_bind(queue=\"order-events\", exchange=\"platform\", routing_key=\"order-events\")\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let topic = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Topic && n.name == "amqp/order-events")
            .expect("queue_bind must mint the amqp topic node (NOT kafka/order-events)");
        assert!(
            graph
                .all_edges()
                .iter()
                .any(|e| e.edge_type == EdgeType::Consumes && e.target_id == topic.id),
            "pika queue_bind must emit a Consumes edge to the amqp topic"
        );
    }

    /// The broker label comes from the matched framework id, not from
    /// the filename or the DEFAULT_BROKER constant: a pika-shaped
    /// Python file mints `amqp/<queue>` while the aiokafka idiom in a
    /// sibling file still mints `kafka/<topic>`.
    #[test]
    fn broker_derives_from_framework_not_filename() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/a.py"),
            "ch.basic_publish(exchange='x', routing_key='q1', body=b)\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/b.py"), "producer.send('t1', msg)\n").unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let names: Vec<String> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::Topic)
            .map(|n| n.name)
            .collect();
        assert!(
            names.iter().any(|n| n == "amqp/q1"),
            "pika publish -> amqp/q1; got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "kafka/t1"),
            "kafka send -> kafka/t1; got {names:?}"
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
            .filter(|n| {
                n.contract
                    .iter()
                    .any(|f| matches!(f, ContractFact::TopicConsumer(_)))
            })
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
            .flat_map(|n| {
                n.contract
                    .iter()
                    .filter_map(|f| match f {
                        ContractFact::TopicConsumer(t) => Some(t.name.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
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
                .flat_map(|n| {
                n.contract
                .iter()
                .filter_map(|f| match f {
                    ContractFact::TopicConsumer(t) => Some(t.name.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
            })
                .collect();
            assert!(
                consumed.contains(&"orders.shipped".to_string()),
                "{label}: the consumer's fact must survive sharing a line \
                 with a producer — got {consumed:?}"
            );
        }
    }

    /// The point of `contract` being a list: two subscriptions to
    /// different topics on one physical line share the synthetic node
    /// (`topic-read:<path>:<line>`), and each must be recorded. When
    /// `contract` held a single fact the second topic's consumer
    /// vanished — a discovered consumer silently disappearing.
    #[test]
    fn two_subscriptions_on_one_line_are_both_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("relay.py"),
            "def relay():\n    c = KafkaConsumer('orders.shipped'); d = KafkaConsumer('orders.delivered')\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let consumed: Vec<String> = graph
            .get_all_nodes()
            .into_iter()
            .flat_map(|n| n.contract)
            .filter_map(|f| match f {
                ContractFact::TopicConsumer(t) => Some(t.name),
                _ => None,
            })
            .collect();
        assert!(
            consumed.contains(&"orders.shipped".to_string()),
            "first subscription lost: {consumed:?}"
        );
        assert!(
            consumed.contains(&"orders.delivered".to_string()),
            "second subscription on the same line must not be dropped — got {consumed:?}"
        );
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

    /// Dynamic topic arg (`topic: variable`) is dropped silently —
    /// no Topic node is emitted (nothing else records the call today).
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
        let constants = collect_same_file_constants(src, "ts", &ScalarBindingIndex::default());
        assert_eq!(constants.get("A"), Some(&"foo".to_string()));
        assert_eq!(constants.get("B"), Some(&"bar".to_string()));
        assert_eq!(constants.get("c"), Some(&"baz".to_string()));
    }

    #[test]
    fn collect_constants_for_python_module_level_assignments() {
        let src = "TOPIC = \"orders.created\"\n";
        let constants = collect_same_file_constants(src, "py", &ScalarBindingIndex::default());
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

    /// An Avro schema that names a topic bound to an already-emitted
    /// `Topic` must produce the full payload node set — `Schema` +
    /// `Field` + `HasField` (mirroring `grpc_provider_sensor`'s proto
    /// message output) — plus the `Topic → Schema` `PayloadSchema`
    /// edge. The topic comes from a producer site in the same scan, so
    /// the edge endpoint is materialized in the same sensor batch.
    #[test]
    fn avro_schema_with_topic_emits_payload_schema_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("schemas")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("schemas/order.avsc"),
            r#"{
        "type": "record", "name": "Order", "topic": "order.events",
        "fields": [{"name": "customer_id", "type": "string"}]
    }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/producer.py"),
            "producer.send('order.events', msg)\n",
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        let schema = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Schema && n.name.contains("Order"))
            .expect("schema node");
        let topic = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Topic && n.name.ends_with("order.events"))
            .expect("topic node");
        assert!(
            graph.all_edges().iter().any(|e| {
                e.edge_type == EdgeType::PayloadSchema
                    && e.source_id == topic.id
                    && e.target_id == schema.id
            }),
            "Topic -> PayloadSchema edge missing"
        );
        let field = graph
            .get_all_nodes()
            .into_iter()
            .find(|n| n.node_type == NodeType::Field && n.name == "customer_id")
            .expect("field node");
        assert!(
            graph.all_edges().iter().any(|e| {
                e.edge_type == EdgeType::HasField
                    && e.source_id == schema.id
                    && e.target_id == field.id
            }),
            "Schema -> HasField -> Field edge missing"
        );
    }

    /// A schema with no `topic` still emits its `Schema` node — the
    /// payload definition is real — but no `PayloadSchema` edge: with
    /// no `Topic` node there is nothing to bind, and `insert_edges_batch`
    /// would silently drop a dangling edge anyway (`graph/mod.rs:1156`).
    #[test]
    fn avro_schema_without_topic_emits_schema_but_no_payload_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/audit.avsc"),
            r#"{
        "type": "record", "name": "Audit",
        "fields": [{"name": "actor", "type": "string"}]
    }"#,
        )
        .unwrap();
        let graph = empty_db();
        let ns = RepoNamespace::for_test();
        scan_workspace_event(&graph, dir.path(), &ns).unwrap();

        assert!(
            graph
                .get_all_nodes()
                .into_iter()
                .any(|n| n.node_type == NodeType::Schema && n.name.contains("Audit")),
            "the Audit schema node must be emitted even without a topic"
        );
        assert!(
            !graph
                .all_edges()
                .iter()
                .any(|e| e.edge_type == EdgeType::PayloadSchema),
            "no topic -> nothing to bind; a dangling PayloadSchema edge would lie"
        );
    }

    // Silence unused-import warnings when the file is compiled in a
    // subset of feature flags.
    #[allow(dead_code)]
    fn _silence_unused(_g: &GraphDatabase) {
        let _ = write_cwd_file;
        let _n = GraphNode::new(NodeType::Topic, "x".into(), "y".into());
    }
}
