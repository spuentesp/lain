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
    // `run_all` will clobber again.
    for n in nodes_of(&graph, NodeType::Schema) {
        assert_eq!(
            sensor_owner_of(&n),
            Some(SensorOwner::GraphqlSensor),
            "Schema node {} should be owned by the GraphQL sensor family",
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
            Some(SensorOwner::ProtoSensor),
            "proto Schema node {} must be owned by the gRPC sensor family",
            n.id
        );
    }
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
