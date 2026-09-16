//! Protocol-first sensors
//!
//! Scans spec files to enrich existing graph nodes with cross-runtime
//! API surface information (gRPC, HTTP, GraphQL, WebSocket, etc.).
//!
//! Sensors are registered via [`inventory::submit!`] at static-init
//! time and iterated by [`run_all`]. Adding a new sensor means
//! `impl Sensor for XxxSensor` + one `inventory::submit!` line in the
//! sensor's module — no central registry to edit, no
//! `dispatch_tool_call`-style match ladder to grow. See
//! [`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md#sensor-pattern-one-concern-per-file-one-trait-shared).

pub mod graphql_sensor;
pub mod http_sensor;
pub mod openapi_sensor;
pub mod proto_sensor;
pub mod websocket_sensor;

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::RepoNamespace;
pub use graphql_sensor::GraphQlOperation;
pub use http_sensor::HttpRoute;
pub use openapi_sensor::OpenApiOperation;
pub use proto_sensor::ProtoService;
use std::path::Path;
pub use websocket_sensor::WebSocketEndpoint;

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

    fn add(&mut self, field: SensorCountField, n: usize) {
        match field {
            SensorCountField::HttpRoutes => self.http_routes += n,
            SensorCountField::Openapi => self.openapi += n,
            SensorCountField::Proto => self.proto += n,
            SensorCountField::Graphql => self.graphql += n,
            SensorCountField::Websocket => self.websocket += n,
        }
    }
}

/// Which `SensorCounts` field this sensor's count contributes to.
/// Lets each sensor pick its bucket via the trait without `run_all`
/// needing a per-sensor match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorCountField {
    HttpRoutes,
    Openapi,
    Proto,
    Graphql,
    Websocket,
}

/// A registered protocol sensor. Each impl contributes its `scan`
/// results to one bucket of [`SensorCounts`] and is discovered by
/// `run_all` via the `inventory` collection.
pub trait Sensor: Send + Sync {
    fn name(&self) -> &'static str;
    fn count_field(&self) -> SensorCountField;
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError>;
}

/// Inventory wrapper so each sensor can `inventory::submit!(SensorEntry(&…))`.
pub struct SensorEntry(pub &'static (dyn Sensor + 'static));
inventory::collect!(SensorEntry);

/// Run every registered protocol sensor over `root`, returning how
/// many nodes/edges each contributed.
///
/// Each sensor is independent: one failing is logged and skipped
/// rather than aborting ingestion, because a malformed `.proto` in a
/// corner of the tree must not cost the caller their call graph.
///
/// `namespace` is threaded through to every `GraphNode::generate_id`
/// call so two federation repos with identical `(type, path, name)`
/// route entries (e.g. `GET /health`) mint distinct ids and don't
/// silently overwrite each other on merge.
pub fn run_all(graph: &GraphDatabase, root: &Path, namespace: &RepoNamespace) -> SensorCounts {
    let mut counts = SensorCounts::default();
    for entry in inventory::iter::<SensorEntry>() {
        let sensor = entry.0;
        match sensor.scan(graph, root, namespace) {
            Ok(n) => counts.add(sensor.count_field(), n),
            Err(e) => tracing::warn!("{} sensor failed for {:?}: {e}", sensor.name(), root),
        }
    }
    counts
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

        let counts = run_all(&graph, &dir, &crate::schema::RepoNamespace::for_test());
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

        assert_eq!(
            run_all(&graph, &dir, &crate::schema::RepoNamespace::for_test()).total(),
            0
        );
    }
}
