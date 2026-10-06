//! Protocol-first sensors
//!
//! Scans spec files to enrich existing graph nodes with cross-runtime
//! API surface information (gRPC, HTTP, GraphQL, WebSocket, etc.).
//!
//! Sensors are registered via [`inventory::submit!`] at static-init
//! time and iterated by [`run_all`]. Adding a new sensor means
//! one `register_sensor!` line (the `Sensor` impl plus its
//! `inventory::submit!`) in the sensor's module — no central registry to edit, no
//! `dispatch_tool_call`-style match ladder to grow. See
//! [`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md#sensor-pattern-one-concern-per-file-one-trait-shared).

pub mod codeowners_sensor;
pub mod dynamic_dispatch_sensor;
pub mod entry_point_sensor;
pub mod env_sensor;
pub mod event_sensor;
pub mod field_access_sensor;
pub mod graphql_consumer_sensor;
pub mod graphql_provider_sensor;
pub mod graphql_resolver_link_sensor;
pub mod graphql_sensor;
pub mod grpc_consumer_sensor;
pub mod grpc_handler_link_sensor;
pub mod grpc_provider_sensor;
pub mod http_client_sensor;
pub mod http_sensor;
pub mod openapi_line_index;
pub mod openapi_schema;
pub mod openapi_sensor;
pub mod patterns;
pub mod proto_sensor;
pub mod sql_sensor;
pub mod util;
pub mod util_tokenize;
pub mod websocket_sensor;

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::RepoNamespace;
pub use graphql_sensor::GraphQlOperation;
pub use grpc_provider_sensor::GrpcProvider;
pub use http_sensor::HttpRoute;
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
    pub dynamic_dispatch: usize,
    // Contract-federation counts (§6.1). The corresponding sensors
    // land in later PRs (6, 9, 16); the buckets are reserved here so
    // `SensorCountField` and `run_all` carry them today. Until their
    // sensors are wired in, `run_all` populates these as zero.
    pub http_clients: usize,
    pub fields: usize,
    pub field_reads: usize,
    pub entry_points: usize,
    // Phase D (spec §7): the SQL-tables sensor writes one count
    // per `Table` node it mints. The bucket lives next to
    // `entry_points` because both are "consumers of contract
    // surfaces" that ride the same per-repo scan.
    pub sql_tables: usize,
}

impl SensorCounts {
    pub fn total(&self) -> usize {
        self.http_routes
            + self.openapi
            + self.proto
            + self.graphql
            + self.websocket
            + self.dynamic_dispatch
            + self.http_clients
            + self.fields
            + self.field_reads
            + self.entry_points
            + self.sql_tables
    }

    pub(crate) fn add(&mut self, field: SensorCountField, n: usize) {
        match field {
            SensorCountField::HttpRoutes => self.http_routes += n,
            SensorCountField::Openapi => self.openapi += n,
            SensorCountField::Proto => self.proto += n,
            SensorCountField::Graphql => self.graphql += n,
            SensorCountField::Websocket => self.websocket += n,
            SensorCountField::DynamicDispatch => self.dynamic_dispatch += n,
            SensorCountField::HttpClients => self.http_clients += n,
            SensorCountField::Fields => self.fields += n,
            SensorCountField::FieldReads => self.field_reads += n,
            SensorCountField::EntryPoints => self.entry_points += n,
            SensorCountField::SqlTables => self.sql_tables += n,
        }
    }

    pub fn as_map(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        if self.http_routes > 0 {
            map.insert("http_routes".to_string(), self.http_routes as u64);
        }
        if self.openapi > 0 {
            map.insert("openapi".to_string(), self.openapi as u64);
        }
        if self.proto > 0 {
            map.insert("proto".to_string(), self.proto as u64);
        }
        if self.graphql > 0 {
            map.insert("graphql".to_string(), self.graphql as u64);
        }
        if self.websocket > 0 {
            map.insert("websocket".to_string(), self.websocket as u64);
        }
        if self.dynamic_dispatch > 0 {
            map.insert("dynamic_dispatch".to_string(), self.dynamic_dispatch as u64);
        }
        if self.http_clients > 0 {
            map.insert("http_clients".to_string(), self.http_clients as u64);
        }
        if self.fields > 0 {
            map.insert("fields".to_string(), self.fields as u64);
        }
        if self.field_reads > 0 {
            map.insert("field_reads".to_string(), self.field_reads as u64);
        }
        if self.entry_points > 0 {
            map.insert("entry_points".to_string(), self.entry_points as u64);
        }
        if self.sql_tables > 0 {
            map.insert("sql_tables".to_string(), self.sql_tables as u64);
        }
        map
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
    DynamicDispatch,
    // Reserved for the contract-federation sensors that land in PR 6,
    // 9, and 16. `run_all` carries the enum values now so the count
    // types are honest before the sensors arrive.
    HttpClients,
    Fields,
    FieldReads,
    EntryPoints,
    // Phase D (spec §7): the SQL-tables sensor bucket.
    SqlTables,
}

/// Default-delegating per-sensor report returned by
/// [`Sensor::scan_with_report`]. The shape is intentionally minimal:
/// `emitted` is the legacy `scan()` count, `error` is the sensor's
/// error message if it failed. Sensors that opt into richer
/// per-(sensor, lang) reporting override `scan_with_report` and
/// return a richer report (TLA+: `files_analyzed[s][lang]`,
/// `unresolved[r][s]`, etc.).
///
/// The `unknown()` constructor is the marker that a sensor has not
/// yet migrated to per-file reporting; the coverage ledger treats
/// that as a coverage gap per spec §4.2 ("unmigrated sensors are
/// reported as `unknown`, never as `clean`").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanReport {
    pub emitted: usize,
    pub error: Option<String>,
}

impl ScanReport {
    /// The sensor did not opt into per-file reporting. Equivalent
    /// to `Default::default()`; the dedicated constructor exists so
    /// the call sites read clearly.
    pub fn unknown() -> Self {
        Self::default()
    }
}

/// A registered protocol sensor. Each impl contributes its `scan`
/// results to one bucket of [`SensorCounts`] and is discovered by
/// `run_all` via the `inventory` collection.
///
/// [`Self::phase`] orders sensors across passes (§6.1): phase 0 runs
/// first (HTTP, OpenAPI, proto, GraphQL, WebSocket, dynamic
/// dispatch — anything that needs the static resolve phase done),
/// phase 1 runs after (HTTP client, entry points — they read
/// symbols sensors produced), and phase 2 runs last
/// (`field_access_sensor`, which needs `SendsHttp` and `Calls`
/// resolved by the joiner). Within a phase, sensors sort by name so
/// ordering is deterministic across runs.
pub trait Sensor: Send + Sync {
    fn name(&self) -> &'static str;
    fn count_field(&self) -> SensorCountField;
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError>;
    /// Execution phase (§6.1). Lower phases run first; within a
    /// phase, sensors run in lexicographic name order.
    fn phase(&self) -> u8 {
        0
    }
    /// Default-delegating per-sensor report used by the coverage
    /// ledger (`run_all_with_reports`). The default calls
    /// [`Self::scan`] and packs the legacy count into `emitted`;
    /// sensors that opt into richer per-(sensor, lang) reporting
    /// override this method.
    fn scan_with_report(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<ScanReport, LainError> {
        let n = self.scan(graph, root, namespace)?;
        Ok(ScanReport {
            emitted: n,
            error: None,
        })
    }
}

/// Inventory wrapper so each sensor can `inventory::submit!(SensorEntry(&…))`.
pub struct SensorEntry(pub &'static (dyn Sensor + 'static));
inventory::collect!(SensorEntry);

/// Declare a sensor: the `Sensor` impl plus its `inventory` registration.
/// `$scan` is any `Fn(&GraphDatabase, &Path, &RepoNamespace) ->
/// Result<usize, LainError>`; `$phase` defaults to 0.
macro_rules! register_sensor {
    ($ty:ident, $name:literal, $count:ident, $scan:expr) => {
        $crate::server::sensors::register_sensor!($ty, $name, $count, 0, $scan);
    };
    ($ty:ident, $name:literal, $count:ident, $phase:literal, $scan:expr) => {
        impl $crate::server::sensors::Sensor for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn count_field(&self) -> $crate::server::sensors::SensorCountField {
                $crate::server::sensors::SensorCountField::$count
            }
            fn phase(&self) -> u8 {
                $phase
            }
            fn scan(
                &self,
                graph: &$crate::graph::GraphDatabase,
                root: &::std::path::Path,
                namespace: &$crate::schema::RepoNamespace,
            ) -> ::std::result::Result<usize, $crate::error::LainError> {
                let scan: fn(
                    &$crate::graph::GraphDatabase,
                    &::std::path::Path,
                    &$crate::schema::RepoNamespace,
                ) -> ::std::result::Result<usize, $crate::error::LainError> = $scan;
                scan(graph, root, namespace)
            }
        }
        inventory::submit!($crate::server::sensors::SensorEntry(&$ty));
    };
}
pub(crate) use register_sensor;

/// Run every registered protocol sensor over `root`, returning how
/// many nodes/edges each contributed.
///
/// Sensors are sorted by `(phase, name)` so a later phase can rely
/// on the side effects of an earlier one (HTTP client reads the
/// routes `http_sensor` just emitted). Within a phase, name order
/// keeps output deterministic — the §8.3 determinism test enforces
/// this.
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
    let (counts, _reports) = run_all_with_reports(graph, root, namespace);
    counts
}

/// Shared inventory iteration behind [`run_all`] and the coverage
/// ledger's `run_all_with_coverage`. Walks every registered
/// [`Sensor`], sorts by `(phase, name)`, and asks each one for a
/// [`ScanReport`] via [`Sensor::scan_with_report`]. Returns the
/// aggregated [`SensorCounts`] and one `(name, report)` pair per
/// sensor (the coverage ledger consumes the latter).
///
/// A failing sensor contributes a `ScanReport { error: Some(...) }`
/// with `emitted = 0` — the inventory iteration never aborts on a
/// single sensor failure (a malformed `.proto` in a corner of the
/// tree must not cost the caller their call graph).
pub(crate) fn run_all_with_reports(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> (SensorCounts, Vec<(&'static str, ScanReport)>) {
    let mut counts = SensorCounts::default();
    let mut reports = Vec::new();
    let mut entries: Vec<&SensorEntry> = inventory::iter::<SensorEntry>().collect();
    entries.sort_by(|a, b| {
        let pa = a.0.phase();
        let pb = b.0.phase();
        pa.cmp(&pb).then_with(|| a.0.name().cmp(b.0.name()))
    });
    for entry in entries {
        let sensor = entry.0;
        let report = match sensor.scan_with_report(graph, root, namespace) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("{} sensor failed for {:?}: {e}", sensor.name(), root);
                ScanReport {
                    emitted: 0,
                    error: Some(e.to_string()),
                }
            }
        };
        counts.add(sensor.count_field(), report.emitted);
        reports.push((sensor.name(), report));
    }
    (counts, reports)
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
