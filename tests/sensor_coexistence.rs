//! Two sensors that share a node shape must both survive `run_all`.
//!
//! Ownership bugs are invisible in single-sensor tests and fatal in
//! production: `replace_sensor_output(owner, …)` retracts *every* node
//! with that owner, so an unrelated sensor running later silently
//! deletes an earlier sensor's output. These tests drive the whole
//! `run_all` pipeline over a workspace where two sensors collide, which
//! is the only place the bug shows.
//!
//! Regressions these pin:
//! - `graphql_consumer`'s `FieldRef` nodes were owned by
//!   `SensorOwner::FieldAccessSensor`, so `field_access_sensor` (phase 2,
//!   later) retracted them on every run.
//! - `GraphqlProvider`/`GraphqlConsumer`/`GraphqlResolverLink` all mapped
//!   to one `SensorOwner::GraphqlSensor`, so the consumer's rescan wiped
//!   the provider's Schema/Field nodes and `HasField`/`ResponseSchema`
//!   edges. Same for the gRPC family.
//! - `WebSocketConsumer` rides on an `HttpClientCall` node, so the
//!   node-type catch-all claimed it and `http_client_sensor` deleted it.
//! - `.json` Schema/Field nodes were routed to `SensorOwner::EventSensor`,
//!   so `event_sensor` retracted `openapi.json` schemas every scan.
//! - `sql_sensor` wrote `TableConsumer` onto the enclosing symbol node
//!   while `event_sensor` wrote `TopicConsumer` onto the same node id —
//!   one `Option<ContractFact>` per node, event runs later, SQL reader lost.

use lain::graph::{sensor_owner_of, GraphDatabase, SensorOwner};
use lain::schema::{NodeType, RepoNamespace};
use lain::server::federation::contracts::model::ContractFact;
use lain::server::sensors::run_all;
use std::fs;
use std::path::{Path, PathBuf};

/// A fresh temp workspace directory, empty and uniquely named.
fn workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sensor_coexistence_{}_{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create workspace");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(p, body).expect("write fixture file");
}

fn scan(dir: &Path) -> GraphDatabase {
    let graph = GraphDatabase::new(&dir.join("db.bin")).expect("open graph");
    run_all(&graph, dir, &RepoNamespace::for_test(), "svc");
    graph
}

fn nodes_of(graph: &GraphDatabase, ty: NodeType) -> Vec<lain::schema::GraphNode> {
    graph.get_nodes_by_types(&[ty]).expect("list nodes")
}

/// Every contract fact in the graph satisfying `pred`.
fn facts(graph: &GraphDatabase, pred: impl Fn(&ContractFact) -> bool) -> Vec<ContractFact> {
    graph
        .get_all_nodes()
        .into_iter()
        .filter_map(|n| n.contract)
        .filter(|c| pred(c))
        .collect()
}

// ─── 1. GraphQL provider + consumer in one workspace ─────────────────
//
// `schema.graphql` is the provider side; a `gql` template in TypeScript
// is the consumer side; a resolver file links them. All three GraphQL
// sensors fire, and the consumer runs after the provider.

const GRAPHQL_SCHEMA: &str = r#"
type Order {
  id: ID!
  customer_id: String!
  total: Int!
}

type Query {
  orders: [Order!]!
}
"#;

const GRAPHQL_CONSUMER: &str = r#"
import { gql } from "@apollo/client";

export const ORDERS = gql`
  query Orders {
    orders {
      id
      customer_id
    }
  }
`;
"#;

const GRAPHQL_RESOLVER: &str = r#"
export const resolvers = {
  Query: {
    orders: () => [],
  },
};
"#;

#[test]
fn graphql_provider_schemas_survive_a_full_run_all() {
    let ws = workspace("graphql");
    write(&ws, "schema.graphql", GRAPHQL_SCHEMA);
    write(&ws, "src/queries.ts", GRAPHQL_CONSUMER);
    write(&ws, "src/resolvers.ts", GRAPHQL_RESOLVER);

    let graph = scan(&ws);

    let schemas = nodes_of(&graph, NodeType::Schema);
    assert!(
        !schemas.is_empty(),
        "graphql_provider's Schema nodes were retracted by a later sensor"
    );
    let fields = nodes_of(&graph, NodeType::Field);
    assert!(
        !fields.is_empty(),
        "graphql_provider's Field nodes were retracted by a later sensor"
    );

    let refs = nodes_of(&graph, NodeType::FieldRef);
    assert!(
        !refs.is_empty(),
        "graphql_consumer's FieldRef nodes were retracted by field_access"
    );

    // And the ownership must actually be per-sensor, or the next
    // `run_all` will clobber again. Owned by the *emitter*
    // (graphql_provider_sensor), which is the sensor that calls
    // `replace_sensor_output` — `SensorOwner::GraphqlSensor` is passed
    // by nobody, so nodes it owned were never retracted.
    for n in nodes_of(&graph, NodeType::Schema) {
        assert_eq!(
            sensor_owner_of(&n),
            Some(SensorOwner::GraphqlProviderSensor),
            "Schema node {} should be owned by the sensor that emits it",
            n.id
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

// ─── 2. WebSocket consumer alongside the HTTP client sensor ──────────
//
// `WebSocketConsumer` rides on an `HttpClientCall` node. If the
// node-type catch-all claims it, `http_client_sensor`'s
// `replace_sensor_output(HttpClientSensor, …)` deletes it on every scan.

const WS_CONSUMER: &str = r#"
const socket = new WebSocket("wss://orders.internal/ws");
socket.onmessage = (ev) => console.log(ev.data);
"#;

#[test]
fn websocket_consumers_survive_http_client_rescan() {
    let ws = workspace("websocket");
    write(&ws, "src/socket.ts", WS_CONSUMER);
    // A plain HTTP call too, so `http_client_sensor` definitely runs and
    // performs its `replace_sensor_output`.
    write(
        &ws,
        "src/api.ts",
        r#"fetch("https://api.example.com/users").then(r => r.json());"#,
    );

    let graph = scan(&ws);

    let ws_consumers: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.contract, Some(ContractFact::WebSocketConsumer(_))))
        .collect();
    assert!(
        !ws_consumers.is_empty(),
        "WebSocketConsumer nodes vanished after run_all"
    );

    for n in &ws_consumers {
        assert_eq!(
            sensor_owner_of(n),
            Some(SensorOwner::WebSocketSensor),
            "WebSocketConsumer node {} must be owned by the WebSocket sensor, \
             not by the HTTP client sensor's catch-all",
            n.id
        );
    }

    // Re-scan: a second `run_all` must not retract them either.
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");
    let after: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.contract, Some(ContractFact::WebSocketConsumer(_))))
        .collect();
    assert_eq!(
        after.len(),
        ws_consumers.len(),
        "a second run_all retracted WebSocketConsumer nodes"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ─── 3. OpenAPI JSON spec must not be owned by the event sensor ──────

const OPENAPI_JSON: &str = r#"{
  "openapi": "3.0.0",
  "info": { "title": "ping", "version": "1.0" },
  "paths": {
    "/ping": {
      "get": {
        "operationId": "ping",
        "responses": {
          "200": {
            "description": "ok",
            "content": {
              "application/json": {
                "schema": {
                  "type": "object",
                  "properties": { "ok": { "type": "boolean" } }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

#[test]
fn openapi_json_spec_schemas_survive_event_sensor() {
    let ws = workspace("openapi_json");
    write(&ws, "openapi.json", OPENAPI_JSON);
    // Give the event sensor something to do so its
    // `replace_sensor_output(EventSensor, …)` actually runs.
    write(
        &ws,
        "src/pub.js",
        r#"consumer.subscribe({ topics: ["orders.created"] });"#,
    );

    let graph = scan(&ws);

    let schemas = nodes_of(&graph, NodeType::Schema);
    assert!(
        !schemas.is_empty(),
        "openapi.json schemas were retracted by event_sensor"
    );
    for n in &schemas {
        let owner = sensor_owner_of(n);
        assert_ne!(
            owner,
            Some(SensorOwner::EventSensor),
            "openapi.json Schema node {} must not be owned by the event sensor",
            n.id
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

// ─── 4. gRPC provider + consumer in one workspace ────────────────────
//
// Same collision as GraphQL, different family: `grpc_provider` (phase 0)
// mints Schema/Field nodes from the `.proto`; `grpc_consumer` (phase 1)
// and `grpc_handler_link` (phase 1) run later and must not retract them.

const PROTO: &str = r#"
syntax = "proto3";

package orders;

message Order {
  string id = 1;
  string customer_id = 2;
}

service Orders {
  rpc GetOrder (Order) returns (Order);
}
"#;

#[test]
fn grpc_provider_schemas_survive_a_full_run_all() {
    let ws = workspace("grpc");
    write(&ws, "proto/orders.proto", PROTO);
    write(
        &ws,
        "src/client.py",
        r#"
import orders_pb2
stub.GetOrder(orders_pb2.Order(id="1"))
"#,
    );

    let graph = scan(&ws);

    let schemas = nodes_of(&graph, NodeType::Schema);
    assert!(
        !schemas.is_empty(),
        "grpc_provider's Schema nodes were retracted by a later sensor"
    );
    for n in &schemas {
        assert_eq!(
            sensor_owner_of(n),
            Some(SensorOwner::GrpcProviderSensor),
            "proto Schema node {} must be owned by the sensor that emits it \
             (grpc_provider) so a rescan can retract it when the message is gone",
            n.id
        );
    }
    let _ = fs::remove_dir_all(&ws);
}

/// The staleness half: `grpc_provider_sensor` emits `.proto`
/// `Schema`/`Field` nodes but `sensor_owner_of` maps them to
/// `ProtoSensor` — and `proto_sensor` never calls
/// `replace_sensor_output` at all. So nothing retracts them: delete a
/// message from a `.proto`, rescan, and the schema stays forever.
///
/// Same "stale reader" class as the sql/topic consumer bugs, in the
/// accumulation direction.
#[test]
fn deleting_a_proto_message_retracts_its_schema() {
    let ws = workspace("proto_schema_stale");
    write(&ws, "proto/orders.proto", PROTO);
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Schema)
        .into_iter()
        .filter(|n| n.name == "Order")
        .count();
    assert_eq!(before, 1, "one Order schema expected, got {before}");

    // Delete the message; keep the file (so it is not an orphan sweep
    // case — this must be the sensor's own retraction).
    write(
        &ws,
        "proto/orders.proto",
        r#"
syntax = "proto3";

package orders;

service Orders {
  rpc GetOrder (Order) returns (Order);
}
"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let after = nodes_of(&graph, NodeType::Schema)
        .into_iter()
        .filter(|n| n.name == "Order")
        .count();
    assert_eq!(
        after, 0,
        "a message deleted from a .proto must have its Schema retracted on rescan"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// The same staleness for `.graphql`: `graphql_provider_sensor` emits
/// `Schema`/`Field` nodes but `sensor_owner_of` mapped them to
/// `SensorOwner::GraphqlSensor`, which no sensor ever passes to
/// `replace_sensor_output`. Nothing retracted them — delete a type from
/// the SDL and its schema stayed forever.
#[test]
fn deleting_a_graphql_type_retracts_its_schema() {
    let ws = workspace("graphql_schema_stale");
    write(&ws, "schema.graphql", GRAPHQL_SCHEMA);
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Schema)
        .into_iter()
        .filter(|n| n.name == "Order")
        .count();
    assert_eq!(before, 1, "one Order schema expected, got {before}");

    // Delete the type; keep the file (so it is the sensor's own
    // retraction, not the orphan sweep).
    write(
        &ws,
        "schema.graphql",
        r#"
type Query {
  orders: [ID!]!
}
"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let after = nodes_of(&graph, NodeType::Schema)
        .into_iter()
        .filter(|n| n.name == "Order")
        .count();
    assert_eq!(
        after, 0,
        "a type deleted from a .graphql must have its Schema retracted on rescan"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// The same staleness for fact-less `Module` nodes: `proto_sensor`
/// upserted them and nothing retracted them, so renaming a service left
/// the old node in the graph forever.
#[test]
fn deleting_a_proto_service_retracts_its_module() {
    let ws = workspace("proto_module_stale");
    write(
        &ws,
        "proto/orders.proto",
        r#"
syntax = "proto3";

package orders;

service Orders {
  rpc GetOrder (Order) returns (Order);
}
"#,
    );
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Module).len();
    assert!(
        before >= 1,
        "expected at least one Module node, got {before}"
    );

    // Rename the service; keep the file so this is the sensor's own
    // retraction, not the orphan sweep.
    write(
        &ws,
        "proto/orders.proto",
        r#"
syntax = "proto3";

package orders;

service Billing {
  rpc GetInvoice (Invoice) returns (Invoice);
}
"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let stale = nodes_of(&graph, NodeType::Module)
        .into_iter()
        .filter(|n| n.name.contains("Orders"))
        .count();
    assert_eq!(
        stale, 0,
        "a service renamed away in a .proto must have its Module retracted on rescan"
    );
    let _ = fs::remove_dir_all(&ws);
}

/// The twin for fact-less `Interface` nodes from `graphql_sensor`.
#[test]
fn deleting_a_graphql_operation_retracts_its_interface() {
    let ws = workspace("graphql_iface_stale");
    write(&ws, "schema.graphql", "query orders {\n  id\n}\n");
    let graph = scan(&ws);

    let before = nodes_of(&graph, NodeType::Interface).len();
    assert!(
        before >= 1,
        "expected at least one Interface node, got {before}"
    );

    write(&ws, "schema.graphql", "query invoices {\n  id\n}\n");
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let stale = nodes_of(&graph, NodeType::Interface)
        .into_iter()
        .filter(|n| n.name.contains("orders"))
        .count();
    assert_eq!(
        stale, 0,
        "an operation removed from a .graphql must have its Interface retracted on rescan"
    );
    let _ = fs::remove_dir_all(&ws);
}

// ─── 5. WebSocket providers must be retracted on rescan ──────────────
//
// `sensor_owner_of` returns `WebSocketSensor`, but no sensor passed that
// owner to `replace_sensor_output`, so provider nodes were upserted and
// never removed — stale routes persisted forever.

#[test]
fn stale_websocket_providers_are_retracted_on_rescan() {
    let ws = workspace("ws_stale");
    write(
        &ws,
        "src/server.js",
        r#"app.ws("/ws/v1", (sock) => { sock.on("message", () => {}); });"#,
    );

    let graph = scan(&ws);
    let before: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.contract, Some(ContractFact::WebSocketProvider(_))))
        .collect();
    assert!(!before.is_empty(), "expected a WebSocketProvider node");

    // Rewrite the file with a different route, then rescan. The v1
    // provider must be gone — not merely shadowed by v2.
    write(
        &ws,
        "src/server.js",
        r#"app.ws("/ws/v2", (sock) => { sock.on("message", () => {}); });"#,
    );
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let after: Vec<_> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.contract, Some(ContractFact::WebSocketProvider(_))))
        .collect();
    assert!(
        !after
            .iter()
            .any(|n| n.name.contains("v1") || n.name.contains("/ws/v1")),
        "stale WebSocketProvider survived the rescan: {:?}",
        after.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(&ws);
}

// ─── 6. sql × event: one function that both subscribes and reads SQL ──
//
// `sql_sensor` (phase 1) used to write `TableConsumer` onto the
// enclosing symbol node; `event_sensor` (phase 2) writes
// `TopicConsumer` onto the **same** symbol node id (both resolve
// through `util::enclosing_symbol`). `GraphNode.contract` is a
// single `Option<ContractFact>`, so event always ran last and
// won — the SQL reader vanished from `ContractIndex.consumers`
// after every scan (the I3 hole this branch exists to close).
// `sql_sensor` now emits `TableConsumer` on a synthetic
// `sql-read:<site>` Function node, mirroring `rpc-call:` /
// `graphql-call:`.

const BOTH_TOPIC_AND_SQL: &str = r#"
def sync_shipments():
    consumer = KafkaConsumer("orders.created")
    cursor.execute("SELECT id FROM shipments")
    return consumer
"#;

#[test]
fn a_topic_and_sql_reader_keeps_both_consumer_facts() {
    // One function does both: `KafkaConsumer("orders.created")` and
    // `cursor.execute("SELECT id FROM shipments")`.
    let ws = workspace("topic_and_sql");
    write(&ws, "src/jobs.py", BOTH_TOPIC_AND_SQL);
    let graph = scan(&ws);

    let tables = facts(&graph, |c| matches!(c, ContractFact::TableConsumer(_)));
    assert!(
        !tables.is_empty(),
        "the SQL reader's TableConsumer was clobbered by the topic consumer"
    );
    let topics = facts(&graph, |c| matches!(c, ContractFact::TopicConsumer(_)));
    assert!(!topics.is_empty(), "the topic consumer must survive too");
    let _ = fs::remove_dir_all(&ws);
}

// ─── Task 9 — BFS depth-gate regression tests
//
//     The mutation harness reports 4 survivors in
//     `src/server/graph/mod.rs`, all on the same class:
//     `if current_depth >= max_depth { continue; }` (and the
//     `if next_depth >= min_depth` variant in `traverse`).
//     A mutation `>=` → `>` walks one extra hop. The cases
//     below pin a chain of length 4 and assert each BFS
//     returns exactly the nodes the depth window allows —
//     one more node would be a regression. The four BFS
//     variants covered: `traverse` (line 1258 and line 1281),
//     `subgraph_around` (line 1343), and `bfs_from` (line
//     1574). The harness filters by `--test sensor_coexistence`,
//     so the assertions live here rather than in the
//     `cfg(test)` block in `graph/mod.rs`.
//
//     Each builder is kept self-contained (no shared
//     `tempfile::tempdir` so the `#[test]` instances don't
//     race on disk).

fn build_four_hop_chain(tag: &str) -> (lain::graph::GraphDatabase, [String; 4]) {
    let dir = std::env::temp_dir().join(format!("lain_coex_bfs4_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let g = lain::graph::GraphDatabase::new(&dir).expect("graph");
    let ns = lain::schema::RepoNamespace::for_test();
    let mk = |name: &str, path: &str| -> lain::schema::GraphNode {
        let mut n =
            lain::schema::GraphNode::new(lain::schema::NodeType::Module, name.into(), path.into());
        n.id = lain::schema::GraphNode::generate_id(
            &lain::schema::NodeType::Module,
            path,
            name,
            Some(1),
            &ns,
        );
        n
    };
    let start = mk("start", "src/start.rs");
    let a = mk("a", "src/a.rs");
    let b = mk("b", "src/b.rs");
    let c = mk("c", "src/c.rs");
    let ids = [start.id.clone(), a.id.clone(), b.id.clone(), c.id.clone()];
    g.insert_nodes_batch(&[start, a, b, c]).unwrap();
    g.insert_edges_batch(&[
        lain::schema::GraphEdge::new(
            lain::schema::EdgeType::Calls,
            ids[0].clone(),
            ids[1].clone(),
        ),
        lain::schema::GraphEdge::new(
            lain::schema::EdgeType::Calls,
            ids[1].clone(),
            ids[2].clone(),
        ),
        lain::schema::GraphEdge::new(
            lain::schema::EdgeType::Calls,
            ids[2].clone(),
            ids[3].clone(),
        ),
    ])
    .expect("insert edges");
    (g, ids)
}

fn build_three_hop_chain(tag: &str) -> (lain::graph::GraphDatabase, [String; 3]) {
    let dir = std::env::temp_dir().join(format!("lain_coex_bfs3_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let g = lain::graph::GraphDatabase::new(&dir).expect("graph");
    let ns = lain::schema::RepoNamespace::for_test();
    let mk = |name: &str, path: &str| -> lain::schema::GraphNode {
        let mut n =
            lain::schema::GraphNode::new(lain::schema::NodeType::Module, name.into(), path.into());
        n.id = lain::schema::GraphNode::generate_id(
            &lain::schema::NodeType::Module,
            path,
            name,
            Some(1),
            &ns,
        );
        n
    };
    let start = mk("start", "src/start.rs");
    let a = mk("a", "src/a.rs");
    let b = mk("b", "src/b.rs");
    let ids = [start.id.clone(), a.id.clone(), b.id.clone()];
    g.insert_nodes_batch(&[start, a, b]).unwrap();
    g.insert_edges_batch(&[
        lain::schema::GraphEdge::new(
            lain::schema::EdgeType::Calls,
            ids[0].clone(),
            ids[1].clone(),
        ),
        lain::schema::GraphEdge::new(
            lain::schema::EdgeType::Calls,
            ids[1].clone(),
            ids[2].clone(),
        ),
    ])
    .expect("insert edges");
    (g, ids)
}

#[test]
fn t9_bfs_from_max_depth_one_returns_one_neighbor() {
    // Mutation harness target: `bfs_from` line 1574
    // `if depth >= max_depth { continue; }` → `>` walks one
    // extra hop. The 3-hop chain has `b` at depth 2; with
    // `max_depth=1` the BFS must not see `b`.
    let (g, ids) = build_three_hop_chain("bfs_from");
    let results = g.bfs_from(&ids[0], 1);
    let names: std::collections::HashSet<String> =
        results.into_iter().map(|(n, _, _)| n.name).collect();
    assert_eq!(
        names,
        std::collections::HashSet::from(["a".to_string()]),
        "max_depth=1 must return exactly one neighbor, got: {:?}",
        names
    );
}

#[test]
fn t9_traverse_with_range_one_two_does_not_walk_depth_three() {
    // Mutation harness target: `traverse` line 1258
    // `if current_depth >= max_depth { continue; }` → `>` walks
    // one extra hop. With a 4-hop chain and `1..2`, the
    // depth-3 node (`c`) must NOT appear; mutated, it does.
    let (g, ids) = build_four_hop_chain("traverse");
    let nodes = g
        .traverse(
            &ids[0],
            lain::schema::EdgeType::Calls,
            1..2,
            petgraph::Direction::Outgoing,
        )
        .expect("traverse");
    let names: std::collections::HashSet<String> = nodes.into_iter().map(|n| n.name).collect();
    assert_eq!(
        names,
        std::collections::HashSet::from(["a".to_string(), "b".to_string()]),
        "traverse 1..2 must yield depth-1 and depth-2 only, got: {:?}",
        names
    );
}

#[test]
fn t9_traverse_inner_next_depth_at_least_min_depth() {
    // Mutation harness target: `traverse` line 1281
    // `if next_depth >= min_depth { result.push(node); }` → `>`
    // drops the boundary node. With a 4-hop chain and `2..3`,
    // the depth-2 node (`b`) MUST appear (mutated: dropped,
    // so the result is just `{c}` instead of `{b, c}`).
    let (g, ids) = build_four_hop_chain("traverse_inner");
    let nodes = g
        .traverse(
            &ids[0],
            lain::schema::EdgeType::Calls,
            2..3,
            petgraph::Direction::Outgoing,
        )
        .expect("traverse");
    let names: std::collections::HashSet<String> = nodes.into_iter().map(|n| n.name).collect();
    assert_eq!(
        names,
        std::collections::HashSet::from(["b".to_string(), "c".to_string()]),
        "traverse 2..3 must yield depth-2 and depth-3, got: {:?}",
        names
    );
}

#[test]
fn t9_subgraph_around_radius_one_includes_only_one_hop() {
    // Mutation harness target: `subgraph_around` line 1343
    // `if current_depth >= radius { continue; }` → `>` walks
    // one extra hop. The 3-hop chain has `b` at depth 2; with
    // `radius=1` the BFS must not see `b`.
    let (g, ids) = build_three_hop_chain("subgraph");
    let sub = g.subgraph_around(&ids[0], 1).expect("subgraph");
    let names: std::collections::HashSet<String> = sub.into_iter().map(|(n, _)| n.name).collect();
    assert_eq!(
        names,
        std::collections::HashSet::from(["start".to_string(), "a".to_string()]),
        "radius=1 must include center + 1-hop only, got: {:?}",
        names
    );
}

// ─── Review follow-ups: pin the two invariants the sql-read fix leans on ─
//
// `line_end: None` on the synthetic `sql-read:` node and the name-guarded
// `sensor_owner_of` arm are both load-bearing and were pinned only by
// comments. Each of these fails if a plausible refactor breaks one.

/// `line_end: None` keeps `util::enclosing_symbol` from resolving the
/// synthetic node (it requires both bounds). If a "cleanup" copies the
/// gRPC shape (`line_end = Some(line)`), the node spans exactly its
/// line with range 0, *beats* the real function in `enclosing_symbol`'s
/// `min_by`, and `ReadsTable` edges silently stop riding the enclosing
/// function — and a same-line topic subscribe would re-clobber the fact.
#[test]
fn a_rescan_keeps_reads_table_on_the_enclosing_function() {
    let ws = workspace("same_line_topic_and_sql");
    // Both idioms on ONE line: this is the case that re-introduces the
    // clobber if `line_end` is ever set to `Some(line)`.
    write(
        &ws,
        "src/jobs.py",
        "def job():\n    consumer = KafkaConsumer(\"orders.created\"); cursor.execute(\"SELECT id FROM shipments\")\n",
    );

    // `run_all` runs sensors only — no tree-sitter indexer — so mint the
    // enclosing symbol ourselves, exactly as `build_graph` in the sql
    // unit test does. Without it `enclosing_or_file` falls back to the
    // file node and the assertion below would be vacuous.
    let graph = GraphDatabase::new(&ws.join("db.bin")).expect("open graph");
    let ns = RepoNamespace::for_test();
    let mut fn_node = lain::schema::GraphNode::new_in(
        NodeType::Function,
        "job".into(),
        "src/jobs.py".into(),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(2);
    fn_node.id = lain::schema::GraphNode::generate_id(
        &NodeType::Function,
        "src/jobs.py",
        "job",
        Some(1),
        &ns,
    );
    let fn_node_id = fn_node.id.clone();
    graph.upsert_node(fn_node).expect("insert enclosing symbol");

    for round in 0..1 {
        run_all(&graph, &ws, &ns, "svc");
        let edges = graph.all_edges();
        let reads: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == lain::schema::EdgeType::ReadsTable)
            .collect();
        assert!(!reads.is_empty(), "round {round}: no ReadsTable edge");
        for e in &reads {
            let src = graph.get_node(&e.source_id).unwrap().expect("source node");
            assert!(
                !src.name.starts_with("sql-read:"),
                "round {round}: ReadsTable must not ride the synthetic \
                 sql-read node — got {}",
                src.name
            );
            assert_eq!(
                src.id, fn_node_id,
                "round {round}: ReadsTable must ride the enclosing function, \
                 got {} ({})",
                src.name, src.id
            );
        }
    }

    // Both facts survive too.
    assert!(!facts(&graph, |c| matches!(c, ContractFact::TableConsumer(_))).is_empty());
    assert!(!facts(&graph, |c| matches!(c, ContractFact::TopicConsumer(_))).is_empty());
}

/// The other half of the same bug: a symbol node carrying `TopicConsumer`
/// is owned by `EventSensor`, so `event_sensor`'s rescan is entitled to
/// delete it — symbol record and every incident edge with it.
#[test]
fn a_topic_consumer_on_a_symbol_node_makes_it_sensor_retractable() {
    use lain::graph::sensor_owner_of;
    use lain::server::federation::contracts::model::{TopicConsumerFact, TopicConsumerKind};
    let mut n =
        lain::schema::GraphNode::new(NodeType::Function, "job".into(), "src/jobs.py".into());
    n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders.created".into(),
        kind: TopicConsumerKind::Subscription,
    }));
    assert_eq!(
        sensor_owner_of(&n),
        None,
        "a symbol node carrying TopicConsumer must not be sensor-retractable: \
         retraction would delete a real function node and every edge attached \
         to it. Only the synthetic `topic-read:` node is owned by EventSensor."
    );
}

/// The SECOND-scan half of the bug class, kept green as a regression
/// pin. The pre-fix mechanism: `replace_sensor_output` step 2 removes
/// an owner's nodes **and their incident edges** (`graph/mod.rs:896`,
/// `remove_node`), and `event_sensor` upserted `TopicConsumer` onto the
/// enclosing symbol node — making that symbol `SensorOwner::EventSensor`.
/// On the next `run_all`, event's rescan retracted the symbol and with
/// it every edge another sensor attached to it — `sql_sensor`'s
/// `ReadsTable` included. The fact came back (event re-inserts it); the
/// edges did not, because they are not in event's `edges` slice.
///
/// Not a `line_end` problem: `util::enclosing_symbol` returned the
/// existing symbol's id, and `upsert_node`'s hydration guard
/// (`graph/mod.rs:416`) skips the replace for an unhydrated node onto a
/// hydrated one. The destroy path is the delete-then-insert of
/// `replace_sensor_output`, which `upsert_node` alone never exercises.
///
/// If this fails with `round 1: no ReadsTable edge`, the shared-symbol
/// shape has returned: `TopicConsumer` is riding a node another sensor
/// owns again.
#[test]
fn a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan() {
    let ws = workspace("same_line_two_scans");
    write(
        &ws,
        "src/jobs.py",
        "def job():\n    consumer = KafkaConsumer(\"orders.created\"); cursor.execute(\"SELECT id FROM shipments\")\n",
    );
    let graph = GraphDatabase::new(&ws.join("db.bin")).expect("open graph");
    let ns = RepoNamespace::for_test();
    let mut fn_node = lain::schema::GraphNode::new_in(
        NodeType::Function,
        "job".into(),
        "src/jobs.py".into(),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(2);
    fn_node.id = lain::schema::GraphNode::generate_id(
        &NodeType::Function,
        "src/jobs.py",
        "job",
        Some(1),
        &ns,
    );
    graph.upsert_node(fn_node).expect("insert enclosing symbol");

    for round in 0..2 {
        run_all(&graph, &ws, &ns, "svc");
        let reads: Vec<_> = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == lain::schema::EdgeType::ReadsTable)
            .collect();
        assert!(!reads.is_empty(), "round {round}: no ReadsTable edge");
    }
}

/// The name guard on `sensor_owner_of`'s `TableConsumer` arm is what
/// stops `replace_sensor_output(SqlSensor, …)` from deleting a real
/// symbol node that (for a pre-fix graph) carries the fact. Both
/// directions must hold.
#[test]
fn sensor_owner_of_owns_only_synthetic_sql_read_nodes() {
    use lain::graph::sensor_owner_of;
    use lain::server::federation::contracts::model::{ContractFact, TableConsumerFact};

    let synthetic = {
        let mut n = lain::schema::GraphNode::new(
            NodeType::Function,
            format!("sql-read:src/jobs.py:{}", 3),
            "src/jobs.py".into(),
        );
        n.contract = Some(ContractFact::TableConsumer(TableConsumerFact {
            tables: vec!["shipments".to_string()],
        }));
        n
    };
    assert_eq!(
        sensor_owner_of(&synthetic),
        Some(SensorOwner::SqlSensor),
        "a synthetic sql-read node must be owned by the sql sensor so a \
         rescan retracts a reader whose SQL site was deleted"
    );

    // The migration guard: a pre-fix graph stores the fact on the real
    // symbol node. That node must NOT be owned by SqlSensor — retracting
    // it would delete a real function node from the graph.
    let symbol = {
        let mut n =
            lain::schema::GraphNode::new(NodeType::Function, "job".into(), "src/jobs.py".into());
        n.contract = Some(ContractFact::TableConsumer(TableConsumerFact {
            tables: vec!["shipments".to_string()],
        }));
        n
    };
    assert_eq!(
        sensor_owner_of(&symbol),
        None,
        "a symbol node carrying TableConsumer must not be sensor-retractable"
    );
}

/// The rescan-hygiene story: delete the SQL site and the reader must go
/// away on the next pass. This is the behaviour the `sensor_owner_of`
/// arm exists to provide.
#[test]
fn deleting_the_sql_site_retracts_the_reader_on_rescan() {
    let ws = workspace("sql_site_deleted");
    write(
        &ws,
        "src/jobs.py",
        "def job():\n    cursor.execute(\"SELECT id FROM shipments\")\n",
    );

    let graph = scan(&ws);
    let before = facts(&graph, |c| matches!(c, ContractFact::TableConsumer(_))).len();
    assert_eq!(before, 1, "one reader expected, got {before}");

    // Remove the SQL entirely; the reader must be retracted.
    write(&ws, "src/jobs.py", "def job():\n    return 1\n");
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let after = facts(&graph, |c| matches!(c, ContractFact::TableConsumer(_))).len();
    assert_eq!(
        after, 0,
        "a reader whose SQL site was deleted must be retracted on rescan"
    );
}

/// The same hygiene story for `topic-read:` — the behaviour the
/// `sensor_owner_of` `TopicConsumer` arm exists to provide. Without
/// that arm, deleting a subscribe site would leave a phantom consumer
/// in `ContractIndex`: the false-positive direction, where a reader
/// that no longer exists still keeps `NoKnownImpact` off the table.
#[test]
fn deleting_the_topic_site_retracts_the_consumer_on_rescan() {
    let ws = workspace("topic_site_deleted");
    write(
        &ws,
        "src/jobs.py",
        "def job():\n    consumer = KafkaConsumer(\"orders.created\")\n",
    );

    let graph = scan(&ws);
    let before = facts(&graph, |c| matches!(c, ContractFact::TopicConsumer(_))).len();
    assert_eq!(before, 1, "one consumer expected, got {before}");

    // Remove the subscribe site entirely; the consumer must be retracted.
    write(&ws, "src/jobs.py", "def job():\n    return 1\n");
    run_all(&graph, &ws, &RepoNamespace::for_test(), "svc");

    let after = facts(&graph, |c| matches!(c, ContractFact::TopicConsumer(_))).len();
    assert_eq!(
        after, 0,
        "a consumer whose topic site was deleted must be retracted on rescan"
    );
}

/// `graphql-call:` / `rpc-call:` nodes are `Function`-typed with BOTH
/// bounds set (`line_end = Some(site_line)`), so `util::enclosing_symbol`
/// — which picks the smallest `line_start..=line_end` covering the line
/// — returns them at range 0 and they **beat the real enclosing
/// function**. On a same-line site (bundled/minified code is exactly
/// this layout) the peer's edge then anchors on the synthetic call node
/// instead of the function, and function-rooted traversal misses the
/// read. `sql-read:`/`topic-read:` avoid this by keeping `line_end: None`.
///
/// Pinned so the two pre-existing emitters cannot regress the fix's
/// anchor rule: ReadsTable must ride the enclosing function.
#[test]
fn a_peer_edge_anchors_on_the_function_not_the_call_node() {
    let ws = workspace("call_node_anchor");
    // One line: a graphql call site AND a SQL site. The call site is
    // detected by graphql_consumer (which emits `graphql-call:`), the
    // SQL by sql_sensor.
    write(
        &ws,
        "src/jobs.py",
        "def job():\n    gql(\"query { x }\"); cursor.execute(\"SELECT id FROM shipments\")\n",
    );

    let graph = GraphDatabase::new(&ws.join("db.bin")).expect("open graph");
    let ns = RepoNamespace::for_test();
    let mut fn_node = lain::schema::GraphNode::new_in(
        NodeType::Function,
        "job".into(),
        "src/jobs.py".into(),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(2);
    fn_node.id = lain::schema::GraphNode::generate_id(
        &NodeType::Function,
        "src/jobs.py",
        "job",
        Some(1),
        &ns,
    );
    let fn_node_id = fn_node.id.clone();
    graph.upsert_node(fn_node).expect("insert enclosing symbol");

    for round in 0..2 {
        run_all(&graph, &ws, &ns, "svc");
        let reads: Vec<_> = graph
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == lain::schema::EdgeType::ReadsTable)
            .collect();
        assert!(!reads.is_empty(), "round {round}: no ReadsTable edge");
        for e in &reads {
            let src = graph.get_node(&e.source_id).unwrap().expect("source node");
            assert_eq!(
                src.id, fn_node_id,
                "round {round}: ReadsTable must anchor on the enclosing \
                 function, not on a synthetic call node — got {} ({:?})",
                src.name, src.node_type
            );
        }
    }
}

// ─── Task 3 audit: no sensor shares a symbol node with another ───────
//
// The bug class is "a sensor sets a `ContractFact` on a node whose id
// comes from `util::enclosing_symbol`", which makes that symbol the
// sensor's own and therefore deletable — along with every edge a peer
// attached to it. Audited every `.contract = Some(ContractFact::…)`
// site in `src/server/sensors/`:
//
//   sensor                      id shape                          verdict
//   grpc_handler_link_sensor    `rpc-handler:{name}`              synthetic, safe
//   grpc_consumer_sensor        `rpc-call:{svc}:{method}`         synthetic, safe
//   graphql_consumer_sensor     `graphql-call:{op}:{field}`       synthetic, safe
//   graphql_resolver_link       `graphql-handler:{op}:{field}`    synthetic, safe
//   util::emit_graphql_field_refs `graphql-read:{op}:{f}:{sel}`   synthetic, safe
//   sql_sensor                  `sql-read:{path}:{line}`          synthetic, safe (Task 2)
//   event_sensor                `topic-read:{path}:{line}`        synthetic, safe (Task 2)
//   grpc_provider_sensor        `Schema`/`Field` + `{svc}/{m}`    own node types, safe
//   graphql_provider_sensor     `Schema`/`Field` + `{op}:{f}`     own node types, safe
//   openapi_sensor / openapi_schema  route/schema/field           own node types, safe
//   sql_sensor                  `Table`                           own node type, safe
//   websocket_sensor            `ws:client:{host}:{route}` /      prefixed names on
//                               `ws:server:{route}`               HttpClientCall/HttpRoute
//   field_access_sensor         `FieldRef` id from the field chain, NOT `enclosing_symbol`
//                               (the symbol rides only the `ReadsField` *edge* source)
//
// No writer sets a fact on an `enclosing_symbol` id. The closest pair is
// `websocket_sensor` and `http_client_sensor`, which share the
// `HttpClientCall` **node type** — that is safe only because their names
// differ (`ws:client:…` vs the URL template) so the ids differ. That is
// a load-bearing coincidence, so it is pinned below as a regression
// guard: it is not proving a bug exists, it is proving the pair keeps
// not colliding.
//
// The table above answers the FACT-collision question only. A second
// direction exists: a synthetic node can be visible to
// `util::enclosing_symbol` and therefore steal a peer's *edge anchor*
// even though its fact is safe. `graphql-call:` / `rpc-call:` did
// exactly that — `Function`-typed with `line_end = Some(line)`, so at
// range 0 they beat the real function in `enclosing_symbol`'s
// `min_by`. Both now keep `line_end: None` like `sql-read:` /
// `topic-read:`, pinned by `a_peer_edge_anchors_on_the_function_not_the_call_node`.

#[test]
fn websocket_and_http_client_keep_separate_nodes_and_edges() {
    let ws = workspace("ws_and_http_client");
    // The two idioms in one workspace. Note the calls sit inside
    // functions, but that does NOT make `SendsHttp` sourceable here:
    // `scan()` runs `run_all` only, with no tree-sitter indexer, so no
    // symbol nodes exist to anchor an edge on. See the edge-count note
    // further down.
    write(
        &ws,
        "src/api.ts",
        "export function load() {\n  return fetch(\"https://x.test/a\").then(r => r.json());\n}\n",
    );
    write(
        &ws,
        "src/feed.ts",
        "export function watch() {\n  const s = new WebSocket(\"ws://x.test/feed\");\n  return s;\n}\n",
    );

    let graph = scan(&ws);

    let ws_facts = facts(&graph, |c| matches!(c, ContractFact::WebSocketConsumer(_))).len();
    assert!(ws_facts > 0, "the WebSocketConsumer fact was never emitted");

    let http_calls = nodes_of(&graph, NodeType::HttpClientCall);
    assert!(
        http_calls.len() >= 2,
        "the plain HTTP call must survive alongside the WS dial — got {} \
         HttpClientCall nodes, which means the two sensors collided on a node id",
        http_calls.len()
    );

    // The lossy half of the bug class is the *edges* attached to a shared
    // symbol — that property is pinned by
    // `a_rescan_keeps_reads_table_on_the_enclosing_function_second_scan`,
    // which mints the enclosing symbol explicitly. This fixture has no
    // symbol nodes (`scan()` runs `run_all` only, no tree-sitter
    // indexer), so `SendsHttp`/`ReadsTable` edges cannot be sourced here
    // and counting them would be vacuous. What this fixture *can* pin is
    // node survival and the invariant below.
    for n in graph.get_all_nodes() {
        if n.contract.is_none() {
            continue;
        }
        let synthetic = [
            "sql-read:",
            "topic-read:",
            "rpc-call:",
            "rpc-handler:",
            "graphql-call:",
            "graphql-handler:",
            "graphql-read:",
            "ws:client:",
            "ws:server:",
        ]
        .iter()
        .any(|p| n.name.starts_with(p));
        // The collision surface is exactly "a node `util::enclosing_symbol`
        // can return" — it filters `Function` and `Method` (`util.rs:458`).
        // A fact on any *other* node type cannot collide with a symbol id,
        // so the rule is "not a symbol type, or synthetic". Expressed this
        // way rather than as an allowlist of contract-only types: an
        // allowlist silently omits `HttpClientCall`/`Module` and reports
        // safe nodes as violations.
        let is_symbol_type = matches!(n.node_type, NodeType::Function | NodeType::Method);
        assert!(
            synthetic || !is_symbol_type,
            "fact-bearing node {} ({:?}, name {:?}) is a symbol-type node whose \
             name is not a known synthetic prefix — `enclosing_symbol` can \
             return it, so a peer sensor may attach edges to it and one of the \
             two rescan owners will delete them",
            n.id,
            n.node_type,
            n.name
        );
    }
}

// ─── Ownership contract ─────────────────────────────────────────────
//
// `sensor_owner_of` and the sensor that calls `replace_sensor_output`
// must agree. When they diverge the node is simply never retracted —
// which is exactly how the proto and graphql schema/module staleness
// bugs shipped. This table is the contract; a new sensor must add a row.

#[test]
fn every_owned_node_shape_is_retracted_by_the_sensor_that_makes_it() {
    use lain::federation::contracts::model::{
        ContractKey, Direction, GraphqlHandlerFact, GraphqlHandlerOrigin, GraphqlOp,
        GraphqlProviderFact, HostPart, HttpMethod, ProviderFact, ProviderOrigin, RpcConsumerFact,
        RpcHandlerFact, RpcHandlerOrigin, RpcSystem, SymbolKey, Table, TableConsumerFact,
        TopicConsumerFact, TopicConsumerKind, WebSocketHandlerFact,
    };
    use lain::server::sensors::util::{SQL_READ_PREFIX, TOPIC_READ_PREFIX};

    let mk = |ty: NodeType, name: &str, path: &str| {
        lain::schema::GraphNode::new(ty, name.to_string(), path.to_string())
    };
    let sym = |path: &str, name: &str| SymbolKey {
        repo: lain::federation::repo_id::RepoId::new("orders").unwrap(),
        path: path.to_string(),
        container: None,
        name: name.to_string(),
    };

    let mut rows: Vec<(&str, lain::schema::GraphNode, Option<SensorOwner>)> = Vec::new();

    // HTTP routes: the provider's origin decides the owner.
    let mut n = mk(NodeType::HttpRoute, "GET /a", "src/a.rs");
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/a".into(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    rows.push(("http route (code)", n, Some(SensorOwner::HttpSensor)));

    let mut n = mk(NodeType::HttpRoute, "GET /b", "openapi.yaml");
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/b".into(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::OpenApi,
    }));
    rows.push(("http route (openapi)", n, Some(SensorOwner::OpenApiSensor)));

    rows.push((
        "http client call",
        mk(NodeType::HttpClientCall, "GET /a", "src/a.py"),
        Some(SensorOwner::HttpClientSensor),
    ));
    rows.push((
        "field ref",
        mk(NodeType::FieldRef, "customer.id", "src/a.py"),
        Some(SensorOwner::FieldAccessSensor),
    ));
    rows.push((
        "topic node",
        mk(NodeType::Topic, "kafka/orders.created", "src/a.py"),
        Some(SensorOwner::EventSensor),
    ));

    // Schema / Field ownership is by file extension: the *emitter* owns
    // them, not the family the extension suggests.
    let mut n = mk(NodeType::Schema, "Order", "proto/a.proto");
    n.contract = Some(ContractFact::Schema {
        direction: Direction::Response,
    });
    rows.push(("proto schema", n, Some(SensorOwner::GrpcProviderSensor)));

    let mut n = mk(NodeType::Schema, "Order", "schema.graphql");
    n.contract = Some(ContractFact::Schema {
        direction: Direction::Response,
    });
    rows.push((
        "graphql schema",
        n,
        Some(SensorOwner::GraphqlProviderSensor),
    ));

    let mut n = mk(NodeType::Schema, "Order", "openapi.json");
    n.contract = Some(ContractFact::Schema {
        direction: Direction::Response,
    });
    rows.push(("openapi schema", n, Some(SensorOwner::OpenApiSensor)));

    let mut n = mk(NodeType::Schema, "Order", "payload.avsc");
    n.contract = Some(ContractFact::Schema {
        direction: Direction::Response,
    });
    rows.push(("avro schema", n, Some(SensorOwner::EventSensor)));

    // Legacy fact-less scanners.
    rows.push((
        "proto service module",
        mk(NodeType::Module, "orders.Orders.GetOrder", "proto/a.proto"),
        Some(SensorOwner::ProtoSensor),
    ));
    rows.push((
        "graphql operation interface",
        mk(NodeType::Interface, "Query: orders", "schema.graphql"),
        Some(SensorOwner::GraphqlSensor),
    ));

    // Contract-sensor nodes carry their own fact and are claimed by
    // their own arm even on shared node types / extensions.
    let mut n = mk(NodeType::Module, "Orders.GetOrder", "proto/a.proto");
    n.contract = Some(ContractFact::RpcProvider(
        lain::federation::contracts::model::RpcProviderFact {
            system: RpcSystem::Grpc,
            service: "orders.Orders".into(),
            method: "GetOrder".into(),
            request_type: "Order".into(),
            response_type: "Order".into(),
            handler: None,
        },
    ));
    rows.push((
        "grpc provider module",
        n,
        Some(SensorOwner::GrpcProviderSensor),
    ));

    let mut n = mk(NodeType::Module, "rpc-handler:get_order", "src/a.rs");
    n.contract = Some(ContractFact::RpcHandler(RpcHandlerFact {
        rpc_service: ContractKey::Rpc {
            system: RpcSystem::Grpc,
            service: "orders.Orders".into(),
            method: "GetOrder".into(),
        },
        handler_function: sym("src/a.rs", "get_order"),
        origin: RpcHandlerOrigin::GoRegister,
    }));
    rows.push((
        "grpc handler link",
        n,
        Some(SensorOwner::GrpcHandlerLinkSensor),
    ));

    let mut n = mk(NodeType::Function, "rpc-call:Orders.GetOrder", "src/a.py");
    n.contract = Some(ContractFact::RpcConsumer(RpcConsumerFact {
        system: RpcSystem::Grpc,
        service: "orders.Orders".into(),
        method: "GetOrder".into(),
        channel_target: None,
        channel_host_part: HostPart::None,
    }));
    rows.push((
        "grpc consumer call",
        n,
        Some(SensorOwner::GrpcConsumerSensor),
    ));

    let mut n = mk(NodeType::Module, "Query:orders", "schema.graphql");
    n.contract = Some(ContractFact::GraphqlProvider(GraphqlProviderFact {
        op: GraphqlOp::Query,
        field: "orders".into(),
        return_type: "Order".into(),
    }));
    rows.push((
        "graphql provider",
        n,
        Some(SensorOwner::GraphqlProviderSensor),
    ));

    let mut n = mk(NodeType::Module, "graphql-handler:Query:orders", "src/a.ts");
    n.contract = Some(ContractFact::GraphqlHandler(GraphqlHandlerFact {
        graphql_field: ContractKey::Graphql {
            op: GraphqlOp::Query,
            field: "orders".into(),
        },
        handler_function: sym("src/a.ts", "orders"),
        origin: GraphqlHandlerOrigin::Apollo,
    }));
    rows.push((
        "graphql resolver link",
        n,
        Some(SensorOwner::GraphqlResolverLinkSensor),
    ));

    let mut n = mk(NodeType::Function, "graphql-call:Query:orders", "src/a.ts");
    n.contract = Some(ContractFact::GraphqlConsumer(
        lain::federation::contracts::model::GraphqlConsumerFact {
            op: GraphqlOp::Query,
            field: "orders".into(),
        },
    ));
    rows.push((
        "graphql consumer call",
        n,
        Some(SensorOwner::GraphqlConsumerSensor),
    ));

    let mut n = mk(
        NodeType::HttpClientCall,
        "ws:client:x.test/feed",
        "src/a.ts",
    );
    n.contract = Some(ContractFact::WebSocketHandler(WebSocketHandlerFact {
        route: "/feed".into(),
        handler: sym("src/a.ts", "feed"),
    }));
    rows.push(("websocket handler", n, Some(SensorOwner::WebSocketSensor)));

    // Synthetic per-site nodes are owned by name prefix.
    let mut n = mk(
        NodeType::Function,
        &format!("{SQL_READ_PREFIX}src/a.py:3"),
        "src/a.py",
    );
    n.contract = Some(ContractFact::TableConsumer(TableConsumerFact {
        tables: vec!["t".into()],
    }));
    rows.push(("sql-read site", n, Some(SensorOwner::SqlSensor)));

    let mut n = mk(NodeType::Table, "shipments", "src/a.py");
    n.contract = Some(ContractFact::Table(Table {
        service: String::new(),
        name: "shipments".into(),
    }));
    rows.push(("sql table", n, Some(SensorOwner::SqlSensor)));

    let mut n = mk(
        NodeType::Function,
        &format!("{TOPIC_READ_PREFIX}src/a.py:3"),
        "src/a.py",
    );
    n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "t".into(),
        kind: TopicConsumerKind::Subscription,
    }));
    rows.push(("topic-read site", n, Some(SensorOwner::EventSensor)));

    // A plain symbol is nobody's — that is the migration arm's job.
    rows.push((
        "plain symbol",
        mk(NodeType::Function, "job", "src/a.py"),
        None,
    ));

    for (what, node, want) in &rows {
        assert_eq!(
            sensor_owner_of(node),
            *want,
            "ownership drift for {what}: sensor_owner_of disagrees with the \
             sensor that calls replace_sensor_output"
        );
    }

    // Every owner variant must appear, or a new sensor can be added
    // without a row and its retraction stays unpinned.
    let covered: std::collections::BTreeSet<String> = rows
        .iter()
        .filter_map(|(_, _, o)| o.map(|s| format!("{s:?}")))
        .collect();
    let no_node_owner = [
        // `entry_point_sensor` writes `GraphNode.entry`, a field rather
        // than a node, and `replace_sensor_output(EntryPointSensor, …)`
        // is a deliberate wipe-and-reapply of that field across every
        // node. There is no node shape to pin.
        "EntryPointSensor",
    ];
    for variant in [
        "HttpSensor",
        "OpenApiSensor",
        "HttpClientSensor",
        "FieldAccessSensor",
        "EntryPointSensor",
        "EventSensor",
        "SqlSensor",
        "ProtoSensor",
        "GraphqlSensor",
        "GrpcProviderSensor",
        "GrpcConsumerSensor",
        "GrpcHandlerLinkSensor",
        "GraphqlProviderSensor",
        "GraphqlConsumerSensor",
        "GraphqlResolverLinkSensor",
        "WebSocketSensor",
    ] {
        if no_node_owner.contains(&variant) {
            continue;
        }
        assert!(
            covered.contains(variant),
            "SensorOwner::{variant} has no row in the ownership contract \
             table — add one so its retraction is pinned"
        );
    }
}
