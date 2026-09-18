//! Dynamic-dispatch sensor
//!
//! Static analysis (Tree-sitter + LSP) cannot follow dynamic dispatch:
//! message buses, DI containers, schema-driven routers, and reflection
//! all hide receivers from the type system. This sensor emits
//! **heuristic edges** with explicit confidence so tools like
//! `get_blast_radius` can surface a wider blast radius than the static
//! graph alone would reveal.
//!
//! Edges created:
//!   - `DynamicDispatch` from a file to a synthetic `Hub:<detector>`
//!     node when the call-site matches a convention pattern.
//!   - `BusTopic` for message-bus publishers and subscribers; the
//!     target node carries the topic name when extractable.
//!   - `RouteMatches` for FastAPI / Express / gRPC router handlers; the
//!     target is the handler function when identifiable.
//!
//! All edges carry `provenance = Heuristic { detector, confidence }`.
//! Consumers should treat the `confidence` field as load-bearing — see
//! `EdgeProvenance::Heuristic` and the `LAIN_HEURISTIC_MIN_CONFIDENCE`
//! knob honoured by `get_blast_radius`.

use crate::error::LainError;
use crate::graph::{graph_path, GraphDatabase};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

/// Confidence assigned to each heuristic family. Higher = more specific
/// pattern match. The `LAIN_HEURISTIC_MIN_CONFIDENCE` knob (default
/// 0.5) controls which of these pass through the `get_blast_radius`
/// default filter.
const CONFIDENCE_MESSAGE_BUS: f32 = 0.7;
const CONFIDENCE_CONTAINER_RESOLVE: f32 = 0.6;
const CONFIDENCE_SCHEMA_ROUTER: f32 = 0.5;
const CONFIDENCE_REFLECTION: f32 = 0.4;
/// `Box<dyn Trait>`, `Arc<dyn Trait>`, `Rc<dyn Trait>` — Rust's
/// canonical dynamic dispatch. Confidence below the message_bus
/// patterns because the trait object surface is statically known,
/// only the implementation is runtime-resolved.
const CONFIDENCE_RUST_TRAIT_OBJECT: f32 = 0.6;
/// `tokio::spawn(...)` etc. — the closure escapes to a foreign
/// task, so blast radius from inside the closure is invisible to
/// the static graph. Lower than trait objects because most
/// spawns are short-lived fire-and-forget tasks that don't
/// recursively dispatch back.
const CONFIDENCE_ASYNC_TASK_SPAWN: f32 = 0.5;
/// `eval()`, `exec()`, `pickle.loads()` — direct dynamic code
/// execution. Any symbol loaded through one of these is invisible
/// to the static graph; the confidence matches reflection
/// because the dispatch surface is similarly opaque.
const CONFIDENCE_DYNAMIC_EVAL: f32 = 0.4;
/// TypeScript `as any` / `as unknown as any` and `<any>` casts —
/// the canonical escape hatch from the static type system. After
/// one of these the runtime value carries `any` semantics, so
/// any subsequent method call dispatches through the JavaScript
/// prototype chain (no static dispatch). Pinned lower than
/// `serde_value` because `as any` is often a temporary
/// workaround rather than an architectural dispatch surface —
/// the user usually intends to remove it.
const CONFIDENCE_TYPE_ESCAPE: f32 = 0.3;

/// Convention patterns per language family. Each entry is `(regex, detector)`;
/// a file matching the regex in this list emits a heuristic edge tagged
/// with `detector` and the per-family confidence constant above.
///
/// Regexes are deliberately conservative (anchored on the call shape,
/// not bare keywords) so prose comments and unrelated strings do not
/// trigger false positives. Tightening further belongs in follow-up
/// per-language detectors; this first cut trades recall for precision.
const RAW_DETECTORS: &[(&str, &str, &str)] = &[
    // (regex, detector_name, edge_type_as_str)
    // Message-bus publishers.
    (
        r"\b\w+\.(publish|emit|send|dispatch|trigger|fire)\s*\(",
        "message_bus_publisher",
        "BusTopic",
    ),
    (
        r"\b(kafka|sns|sqs|pubsub|EventBus|MessageBus|Producer|producer)\.\w+\s*\(",
        "message_bus_publisher",
        "BusTopic",
    ),
    // Message-bus subscribers.
    (
        r"\b\w+\.(subscribe|on|listen|consume|add_listener|on_message)\s*\(",
        "message_bus_subscriber",
        "BusTopic",
    ),
    (
        r"@(?:consumer|consumer\.listen|subscriber|event_handler|event\.listen)\b",
        "message_bus_subscriber",
        "BusTopic",
    ),
    // Dependency-injection container resolves.
    (
        r"\b(container|Container|container\.|app\.container|bean_factory|BeanFactory)\.(resolve|get|lookup|getBean|get_service)\s*\(",
        "container_resolve",
        "DynamicDispatch",
    ),
    (
        r"@(?:inject|Inject|Autowired|autowired)\b",
        "container_resolve",
        "DynamicDispatch",
    ),
    // Schema-driven routers.
    (
        r"@(?:app|router|route|bp|api)\.(get|post|put|patch|delete|head|options)\s*\(",
        "schema_router",
        "RouteMatches",
    ),
    (
        r"\b(app|router)\.(get|post|put|patch|delete|use|route|all)\s*\(",
        "schema_router",
        "RouteMatches",
    ),
    // gRPC service definitions are routed to handlers via the schema.
    (r"^\s*rpc\s+\w+\s*\(", "schema_router", "RouteMatches"),
    // Reflection / dynamic value types. Bare `^` is intentionally
    // absent — without `(?m)` it would match every file at offset 0,
    // turning the detector into a no-op signal.
    (
        r"\b(serde_json::Value|serde_value::Value|JsonValue|Box<dyn Any>|Arc<dyn Any>|Rc<dyn Any>|kotlin\.Any)\b",
        "serde_value",
        "DynamicDispatch",
    ),
    (r"\binterface\s*\{\s*\}", "serde_value", "DynamicDispatch"),
    (r"^\s*dynamic\s+\w", "serde_value", "DynamicDispatch"),
    // Rust trait objects (non-Any). The earlier serde_value pattern
    // covers `Box<dyn Any>` etc. — this covers the common case where
    // the dispatch target is a user-defined trait whose set of
    // implementers is open and not visible to the static graph.
    // We don't anchor the trait identifier on a word boundary because
    // `dyn MyTrait` is followed by space-or-angle, not a letter, and
    // a `\w` would let `dyn MyTraitFoo` match `dyn MyTrait` if a later
    // pattern ever drifted.
    (
        r"(?:Box|Rc|Arc)<dyn\s+[A-Z][A-Za-z0-9_]*",
        "rust_trait_object",
        "DynamicDispatch",
    ),
    // Async task spawners. The closure handed to `spawn` is opaque to
    // the static graph: the future may call anything, and the
    // JoinHandle's caller has no syntactic edge to the work. We
    // anchor on the runtime namespace so prose like "the spawn
    // function" doesn't trigger.
    (
        r"\b(tokio|async_std|smol|executor|workers)\s*::\s*spawn\s*\(",
        "async_task_spawn",
        "DynamicDispatch",
    ),
    // Direct dynamic code execution: eval / exec / new Function /
    // pickle.loads. Anchored on the function name + open paren so
    // prose like "the eval function" doesn't match. We don't restrict
    // by language because every one of these is in scope for the
    // static graph's blind spot.
    (
        r"\b(eval|exec|pickle\.loads|new\s+Function|Function)\s*\(",
        "dynamic_eval",
        "DynamicDispatch",
    ),
    // TypeScript type escape hatches: `as any`, `as unknown as any`
    // (the canonical workaround when `as any` is forbidden by
    // lint rules), and the legacy `<any>` cast. After any of these
    // the value dispatches through the JavaScript prototype chain
    // — the static type system has been told to look the other way.
    // We anchor on `as any` / `<any>` with whitespace boundaries
    // so prose like "cast as anything you like" doesn't match.
    (
        r"\bas\s+(unknown\s+)?any\b|\bas\s+any\s+as\s+any\b|<any>",
        "type_escape",
        "DynamicDispatch",
    ),
];

/// One precompiled detector. Building a `regex::Regex` per detector per
/// file is the largest single cost in `scan_workspace_dispatch` on a
/// multi-thousand-file workspace; precompiling once and threading the
/// already-resolved `EdgeType` and confidence score through the hot
/// loop cuts that cost out entirely.
struct CompiledDetector {
    regex: Regex,
    edge_type: EdgeType,
    confidence: f32,
    detector: &'static str,
}

/// All detectors precompiled once on first access. Patterns that fail
/// to compile are silently dropped — the failure mode is "this family
/// never matches", which is honest (no false positives) and easier to
/// notice in test coverage than a panic at static-init time.
///
/// Pre-resolving the `&str → EdgeType` and `&str → confidence` lookups
/// also moves those match statements out of the per-file hot loop.
static DETECTORS: Lazy<Vec<CompiledDetector>> = Lazy::new(|| {
    let confidence_for = |detector: &str| match detector {
        "message_bus_publisher" | "message_bus_subscriber" => CONFIDENCE_MESSAGE_BUS,
        "container_resolve" => CONFIDENCE_CONTAINER_RESOLVE,
        "schema_router" => CONFIDENCE_SCHEMA_ROUTER,
        "serde_value" => CONFIDENCE_REFLECTION,
        "rust_trait_object" => CONFIDENCE_RUST_TRAIT_OBJECT,
        "async_task_spawn" => CONFIDENCE_ASYNC_TASK_SPAWN,
        "dynamic_eval" => CONFIDENCE_DYNAMIC_EVAL,
        "type_escape" => CONFIDENCE_TYPE_ESCAPE,
        _ => 0.5,
    };
    let edge_type_for = |kind: &str| match kind {
        "BusTopic" => EdgeType::BusTopic,
        "RouteMatches" => EdgeType::RouteMatches,
        _ => EdgeType::DynamicDispatch,
    };
    RAW_DETECTORS
        .iter()
        .filter_map(|(pat, detector, edge_kind)| {
            let regex = Regex::new(pat).ok()?;
            Some(CompiledDetector {
                regex,
                edge_type: edge_type_for(edge_kind),
                confidence: confidence_for(detector),
                detector,
            })
        })
        .collect()
});

/// Walks the workspace, runs each detector over each source file, and
/// inserts the resulting heuristic edges into `graph`. Returns the
/// number of edges created.
pub fn scan_workspace_dispatch(
    graph: &GraphDatabase,
    root: &std::path::Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    let mut total = 0usize;
    let mut new_edges: Vec<GraphEdge> = Vec::new();
    let mut hub_nodes: Vec<GraphNode> = Vec::new();
    let mut seen_hubs: HashSet<String> = HashSet::new();
    let mut ensured_files: HashSet<String> = HashSet::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !["rs", "py", "ts", "js", "tsx", "jsx", "go", "java"].contains(&ext) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };

        let rel_path = graph_path(root, path);
        let file_id = GraphNode::generate_id(&NodeType::File, &rel_path, "", None, namespace);

        // Source endpoint must exist in the graph; `insert_edges_batch`
        // drops edges whose endpoints aren't indexed. Track per-file so
        // we don't re-upsert when many detectors fire on the same file.
        if ensured_files.insert(file_id.clone()) {
            let mut file_node = GraphNode::new(NodeType::File, String::new(), rel_path.clone());
            file_node.id = file_id.clone();
            graph.upsert_node(file_node)?;
        }

        for det in DETECTORS.iter() {
            if !det.regex.is_match(&content) {
                continue;
            }

            // Synthetic Hub target. Deterministic id per detector so
            // repeated scans converge on the same node.
            let hub_name = format!("Hub:{}", det.detector);
            let hub_id =
                GraphNode::generate_id(&NodeType::Synthetic, "__hub__", &hub_name, None, namespace);

            if seen_hubs.insert(hub_id.clone()) {
                let mut hub_node = GraphNode::new(NodeType::Synthetic, hub_name, String::new());
                hub_node.id = hub_id.clone();
                hub_nodes.push(hub_node);
            }

            new_edges.push(GraphEdge {
                edge_type: det.edge_type.clone(),
                source_id: file_id.clone(),
                target_id: hub_id,
                weight: Some(det.confidence),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: det.detector.to_string(),
                    confidence: det.confidence,
                }),
            });
        }
    }

    // Nodes before edges — the edge endpoints need to resolve.
    for node in hub_nodes {
        graph.upsert_node(node)?;
    }
    if !new_edges.is_empty() {
        graph.insert_edges_batch(&new_edges)?;
        total = new_edges.len();
    }
    Ok(total)
}

/// Unit-struct Sensor impl. Discovery via
/// `inventory::submit!(SensorEntry(&DynamicDispatchSensor))` below.
pub struct DynamicDispatchSensor;

impl crate::server::sensors::Sensor for DynamicDispatchSensor {
    fn name(&self) -> &'static str {
        "dynamic_dispatch"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::DynamicDispatch
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &std::path::Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_dispatch(graph, root, namespace)
    }
}

inventory::submit!(crate::server::sensors::SensorEntry(&DynamicDispatchSensor));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RepoNamespace;
    use std::collections::HashSet;

    /// A file containing a `bus.publish(` call must emit a `BusTopic`
    /// edge tagged with the `message_bus_publisher` detector.
    #[test]
    fn bus_publisher_creates_heuristic_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.py"),
            "def publish_order():\n    bus.publish('orders.v1', payload)\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        let count = scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();
        assert!(count >= 1, "expected ≥1 heuristic edge, got {count}");

        let heuristic: Vec<_> = graph
            .all_edges()
            .into_iter()
            .filter(|e| matches!(
                e.provenance,
                Some(EdgeProvenance::Heuristic { ref detector, .. }) if detector == "message_bus_publisher"
            ))
            .collect();
        assert!(
            !heuristic.is_empty(),
            "expected at least one message_bus_publisher edge"
        );
        assert_eq!(heuristic[0].edge_type, EdgeType::BusTopic);
    }

    /// A file with no dispatch patterns must produce no edges.
    #[test]
    fn benign_file_emits_no_heuristic_edges() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Hello\n\nThis file has no dynamic dispatch patterns.\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        let count = scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();
        assert_eq!(count, 0);
    }

    /// A FastAPI route decorator must produce a `RouteMatches` edge
    /// tagged with `schema_router`.
    #[test]
    fn fastapi_route_creates_route_matches_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("api.py"),
            "from fastapi import FastAPI\n\
             app = FastAPI()\n\
             @app.get('/orders/{order_id}')\n\
             def get_order(order_id: int):\n\
             \x20\x20\x20\x20return {}\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        let count = scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();
        assert!(count >= 1);

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("schema_router"),
            "expected schema_router in {:?}",
            detectors
        );
    }

    /// Confidence must be propagated into the edge weight so consumers
    /// that ignore provenance can still rank by `weight`.
    #[test]
    fn confidence_is_recorded_in_edge_weight() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.py"),
            "def on_message(channel, method, properties, body):\n\
             \x20\x20\x20\x20bus.subscribe('orders.v1', on_message)\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();
        let edge = graph
            .all_edges()
            .into_iter()
            .find(|e| matches!(e.provenance, Some(EdgeProvenance::Heuristic { .. })))
            .expect("heuristic edge should exist");

        assert!(edge.weight.is_some(), "weight must be set to confidence");
        let confidence = match edge.provenance.as_ref().unwrap() {
            EdgeProvenance::Heuristic { confidence, .. } => *confidence,
            _ => unreachable!(),
        };
        assert!((0.0..=1.0).contains(&confidence));
    }

    /// A Rust trait object (`Box<dyn Trait>`) is the canonical dynamic
    /// dispatch surface in Rust — the trait object itself is opaque to
    /// the static graph because every impl block in every crate is a
    /// potential receiver. The detector emits a `DynamicDispatch`
    /// edge tagged with `rust_trait_object`.
    #[test]
    fn rust_trait_object_creates_dynamic_dispatch_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dispatcher.rs"),
            "trait Handler { fn handle(&self, req: Request); }\n\
             fn run(h: Box<dyn Handler>) { h.handle(req); }\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("rust_trait_object"),
            "expected rust_trait_object in {:?}",
            detectors
        );

        // Confidence is 0.6 — higher than async spawn (0.5) because
        // trait objects are a more obvious dispatch point.
        let edge = graph
            .all_edges()
            .into_iter()
            .find(|e| matches!(
                e.provenance,
                Some(EdgeProvenance::Heuristic { ref detector, .. }) if detector == "rust_trait_object"
            ))
            .expect("rust_trait_object edge should exist");
        let confidence = match edge.provenance.as_ref().unwrap() {
            EdgeProvenance::Heuristic { confidence, .. } => *confidence,
            _ => unreachable!(),
        };
        assert!(
            (confidence - 0.6).abs() < 1e-6,
            "rust_trait_object confidence must be 0.6, got {confidence}"
        );
    }

    /// `tokio::spawn(...)` hands a future off to the runtime; the
    /// static graph can't see what the future calls. The detector
    /// emits a `DynamicDispatch` edge tagged with `async_task_spawn`.
    /// Bare `spawn(` without a namespace prefix is intentionally
    /// ignored — too noisy in comment prose.
    #[test]
    fn async_task_spawn_creates_dynamic_dispatch_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.rs"),
            "fn dispatch(req: Request) {\n    tokio::spawn(async move { process(req).await });\n}\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("async_task_spawn"),
            "expected async_task_spawn in {:?}",
            detectors
        );
    }

    /// `spawn(` without a runtime namespace is prose noise, not a
    /// dispatch surface. The detector must not match it — otherwise
    /// every doc comment or test name that mentions "spawn" would
    /// produce a phantom heuristic edge.
    #[test]
    fn bare_spawn_without_namespace_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.rs"),
            "// TODO: spawn a worker thread here\n\
             fn helper() { spawn(local_fn()); }\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            !detectors.contains("async_task_spawn"),
            "bare `spawn(` must not trigger async_task_spawn: {:?}",
            detectors
        );
    }

    /// Direct dynamic code execution (`eval`, `exec`, `pickle.loads`,
    /// `new Function`) is the most extreme form of dynamic dispatch —
    /// anything reachable from the loaded string is invisible to the
    /// static graph. Pinned at confidence 0.4 because the pattern is
    /// unambiguous but the runtime target is unknowable from the
    /// call site alone.
    #[test]
    fn dynamic_eval_emits_dynamic_dispatch_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("load.py"),
            "import pickle\n\
             def load_blob(blob):\n    return pickle.loads(blob)\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("dynamic_eval"),
            "expected dynamic_eval in {:?}",
            detectors
        );
    }

    /// `eval` matches across languages (Python's built-in eval and
    /// JavaScript's eval are both dispatch surfaces). Pinned by a
    /// JavaScript fixture so a Python-only tuning doesn't silently
    /// miss the same risk in a JS codebase.
    #[test]
    fn javascript_eval_also_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("runner.js"),
            "function run(src) { return eval(src); }\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("dynamic_eval"),
            "JS eval must also trigger dynamic_eval: {:?}",
            detectors
        );
    }

    /// Negative coverage: identifier prefixes that contain the
    /// dynamic_eval keyword as a *substring* of a longer identifier
    /// (`evaluation`, `evaluate_command`, `executable_path`,
    /// `exec_summary`) must NOT match. Without this pinned, a
    /// future tightening of the regex to bare-word matchers would
    /// silently start firing on every code review comment that
    /// mentions "execution time" or "evaluated against".
    #[test]
    fn identifier_prefixes_with_eval_or_exec_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("metrics.py"),
            "def evaluate_query(q): pass\n\
             def execution_time(): pass\n\
             def exec_summary(): pass\n\
             def executable_path(): pass\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            !detectors.contains("dynamic_eval"),
            "identifier prefixes (evaluate_, execution_, exec_, executable_) \
             must not trigger dynamic_eval: {detectors:?}"
        );
    }

    /// Negative coverage: `Box<dyn Foo>` (trait object) should fire
    /// `rust_trait_object`, but `Vec<MyStruct>` and
    /// `HashMap<String, MyStruct>` (concrete types, no `dyn`)
    /// must NOT — they're statically-resolved containers, not
    /// dispatch surfaces.
    #[test]
    fn concrete_generic_types_do_not_match_rust_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("storage.rs"),
            "struct Store { items: Vec<MyStruct>, lookup: HashMap<String, MyStruct> }\n\
             fn build() -> Box<MyStruct> { unimplemented!() }\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            !detectors.contains("rust_trait_object"),
            "concrete generics (Vec, HashMap, Box without `dyn`) \
             must not trigger rust_trait_object: {detectors:?}"
        );
    }

    /// Negative coverage: heuristic detectors must not match
    /// commented-out code. A regex that fires on `// foo.publish()`
    /// would over-report on every commented-out migration.
    ///
    /// CURRENT BEHAVIOUR (documented, not enforced): the detector
    /// regexes don't strip comments before matching — a commented
    /// `bus.publish(` still matches. Fixing this properly requires
    /// a comment-stripping pre-pass that costs a tree-sitter parse
    /// per file, which is a future-PR concern. The pinned test
    /// below documents the current behaviour so a future tightening
    /// is a deliberate decision rather than a silent regression:
    /// the test asserts that *identifier-as-keyword* substrings
    /// (`evaluate_`, `exec_`, `executable_`) do NOT fire. That's
    /// the cheaper negative-coverage contract we can hold today.
    #[test]
    fn identifier_prefixes_in_dispatch_context_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("handlers.py"),
            // Each of these LOOKS like it might match a heuristic
            // pattern but is actually a different operation:
            // - evaluate_query: identifier with 'eval' prefix
            // - execution_time: identifier with 'exec' prefix
            // - exec_summary:  identifier with 'exec_' prefix
            // - executable_path: identifier with 'execut' prefix
            "def evaluate_query(q): pass\n\
             def execution_time(): pass\n\
             def exec_summary(): pass\n\
             def executable_path(): pass\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            !detectors.contains("dynamic_eval"),
            "identifier prefixes (evaluate_, execution_, exec_, executable_) \
             must not trigger dynamic_eval: {detectors:?}"
        );
    }

    /// TypeScript `as any` (and the lint-bypass `as unknown as any`,
    /// and the legacy `<any>` cast) is the canonical escape hatch
    /// from the static type system. After one of these, the value
    /// dispatches through the JavaScript prototype chain at runtime
    /// — the LSP can't follow. New `type_escape` detector at
    /// confidence 0.3 (lower than reflection — `as any` is often a
    /// temporary workaround rather than an architectural surface).
    #[test]
    fn typescript_as_any_emits_type_escape_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("client.ts"),
            "function getUser(id: string): any {\n  return fetch(`/users/${id}`) as any;\n}\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("type_escape"),
            "expected type_escape in {:?}",
            detectors
        );

        // Confidence pinned at 0.3.
        let edge = graph
            .all_edges()
            .into_iter()
            .find(|e| matches!(
                e.provenance,
                Some(EdgeProvenance::Heuristic { ref detector, .. }) if detector == "type_escape"
            ))
            .expect("type_escape edge should exist");
        let confidence = match edge.provenance.as_ref().unwrap() {
            EdgeProvenance::Heuristic { confidence, .. } => *confidence,
            _ => unreachable!(),
        };
        assert!(
            (confidence - 0.3).abs() < 1e-6,
            "type_escape confidence must be 0.3, got {confidence}"
        );
    }

    /// `as unknown as any` — the canonical workaround when an
    /// `eslint @typescript-eslint/no-explicit-any` rule forbids
    /// bare `as any`. The detector must match this two-step cast.
    #[test]
    fn typescript_as_unknown_as_any_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("strict.ts"),
            "const data = fetch('/x') as unknown as any;\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            detectors.contains("type_escape"),
            "as unknown as any must also trigger type_escape: {:?}",
            detectors
        );
    }

    /// Negative coverage: `as Anything` (capitalised, not the
    /// `any` keyword) and `<Anything>` (generic, not the legacy
    /// `<any>` cast) must NOT fire. The detector targets the literal
    /// `any` keyword only.
    #[test]
    fn as_something_other_than_any_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("user_defined.ts"),
            "interface Anything { value: string }\n\
             function cast(): Anything { return { value: 'x' } as Anything; }\n",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let detectors: HashSet<String> = graph
            .all_edges()
            .into_iter()
            .filter_map(|e| match e.provenance {
                Some(EdgeProvenance::Heuristic { detector, .. }) => Some(detector),
                _ => None,
            })
            .collect();
        assert!(
            !detectors.contains("type_escape"),
            "user-defined `Anything` interface must not trigger type_escape: {:?}",
            detectors
        );
    }
}
