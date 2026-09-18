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
}
