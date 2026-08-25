//! Protocol-first sensors
//!
//! Scans spec files to enrich existing graph nodes with cross-runtime
//! API surface information (gRPC, HTTP, GraphQL, WebSocket, etc.).

pub mod http_sensor;
pub mod proto_sensor;
pub mod openapi_sensor;
pub mod graphql_sensor;
pub mod websocket_sensor;

pub use http_sensor::HttpRoute;
pub use proto_sensor::ProtoService;
pub use openapi_sensor::OpenApiOperation;
pub use graphql_sensor::GraphQlOperation;
pub use websocket_sensor::WebSocketEndpoint;
use crate::graph::GraphDatabase;
use std::path::Path;

/// Run every protocol sensor over `root`, returning how many nodes/edges
/// each contributed.
///
/// The sensors were written, compiled (all but `http_sensor`, which was
/// not even declared as a module) and never called: no path in
/// `ingest/` referenced this module, so `HttpRoute`, `CallsHttp` and
/// `Implements` could not appear in any graph. `describe_schema`
/// advertised those types anyway and `get_cross_runtime_callers` — whose
/// entire job is reading them — answered nothing for every symbol in
/// every codebase.
///
/// Each sensor is independent: one failing is logged and skipped rather
/// than aborting ingestion, because a malformed `.proto` in a corner of
/// the tree must not cost the caller their call graph.
pub fn run_all(graph: &GraphDatabase, root: &Path) -> SensorCounts {
    let mut counts = SensorCounts::default();

    match http_sensor::scan_workspace_routes(graph, root) {
        Ok(n) => counts.http_routes = n,
        Err(e) => tracing::warn!("http sensor failed for {:?}: {e}", root),
    }
    match openapi_sensor::scan_workspace(graph, root) {
        Ok(n) => counts.openapi = n,
        Err(e) => tracing::warn!("openapi sensor failed for {:?}: {e}", root),
    }
    match proto_sensor::scan_workspace(graph, root) {
        Ok(n) => counts.proto = n,
        Err(e) => tracing::warn!("proto sensor failed for {:?}: {e}", root),
    }
    match graphql_sensor::scan_workspace(graph, root) {
        Ok(n) => counts.graphql = n,
        Err(e) => tracing::warn!("graphql sensor failed for {:?}: {e}", root),
    }
    match websocket_sensor::scan_workspace(graph, root) {
        Ok(n) => counts.websocket = n,
        Err(e) => tracing::warn!("websocket sensor failed for {:?}: {e}", root),
    }

    counts
}

/// What [`run_all`] contributed, per sensor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SensorCounts {
    pub http_routes: usize,
    pub openapi: usize,
    pub proto: usize,
    pub graphql: usize,
    pub websocket: usize,
}

impl SensorCounts {
    pub fn total(&self) -> usize {
        self.http_routes + self.openapi + self.proto + self.graphql + self.websocket
    }
}

#[cfg(test)]
mod run_all_tests {
    use super::*;
    use crate::schema::{EdgeType, GraphNode, NodeType};

    /// End to end through the entry point ingestion calls: a Go route and
    /// its handler must become an `HttpRoute` node joined to the handler
    /// by `CallsHttp` — the edge `get_cross_runtime_callers` reads.
    #[test]
    fn run_all_produces_the_cross_runtime_types() {
        let dir = std::env::temp_dir().join("lain_sensors_run_all");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("routes.go"), "r.GET(\"/api/users\", listUsers)\n").unwrap();

        let db = std::env::temp_dir().join("lain_sensors_run_all_db");
        let _ = std::fs::remove_dir_all(&db);
        let graph = GraphDatabase::new(&db).unwrap();
        graph
            .upsert_node(GraphNode::new(
                NodeType::Function,
                "listUsers".into(),
                dir.join("routes.go").to_string_lossy().to_string(),
            ))
            .unwrap();

        let counts = run_all(&graph, &dir);
        assert_eq!(counts.http_routes, 1, "the Go route must be picked up");

        let routes = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.node_type == NodeType::HttpRoute)
            .count();
        assert_eq!(routes, 1);

        let calls_http = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::CallsHttp)
            .count();
        assert_eq!(calls_http, 1, "route must be linked to its handler");
    }

    /// A tree with nothing to find must be harmless, not an error.
    #[test]
    fn run_all_on_an_empty_tree_is_a_no_op() {
        let dir = std::env::temp_dir().join("lain_sensors_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "nothing here\n").unwrap();

        let db = std::env::temp_dir().join("lain_sensors_empty_db");
        let _ = std::fs::remove_dir_all(&db);
        let graph = GraphDatabase::new(&db).unwrap();

        assert_eq!(run_all(&graph, &dir).total(), 0);
    }
}
