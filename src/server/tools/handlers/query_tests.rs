//! Tests for tools/handlers/query.rs

use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::presence::{OccupancyMap, PresenceRegistry};
use crate::server::tools::handlers::query::{describe_schema, query_graph};
use parking_lot::Mutex;
use serde_json::Map;
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_graph() -> GraphDatabase {
    let tmp = std::env::temp_dir().join("test_query_handler");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let fn1 = GraphNode::new(
        NodeType::Function,
        "fn1".to_string(),
        "/src/lib.rs".to_string(),
    );
    let fn2 = GraphNode::new(
        NodeType::Function,
        "fn2".to_string(),
        "/src/lib.rs".to_string(),
    );

    graph.upsert_node(fn1.clone()).unwrap();
    graph.upsert_node(fn2.clone()).unwrap();

    graph
        .insert_edge(&GraphEdge::new(
            EdgeType::Calls,
            fn1.id.clone(),
            fn2.id.clone(),
        ))
        .unwrap();

    graph
}

fn test_embedder_and_cache() -> (NlpEmbedder, Arc<Mutex<HashMap<String, Vec<f32>>>>) {
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));
    (embedder, cache)
}

/// Empty multiplayer registries for the existing tests — none of them
/// assert against the `occupancy` payload, they only care that the
/// `query_graph` JSON result is well-formed.
fn empty_presence() -> (PresenceRegistry, OccupancyMap) {
    (PresenceRegistry::new(), OccupancyMap::new())
}

#[test]
fn test_query_graph_default() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();
    let (presence, occupancy) = empty_presence();

    let result = query_graph(&graph, &embedder, &cache, &presence, &occupancy, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should be valid JSON
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
    // `occupancy.active_agents` is always present (empty list when no agents).
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(v.get("occupancy").is_some(), "occupancy key missing");
    assert_eq!(v["occupancy"]["active_agents"].as_array().unwrap().len(), 0);
}

#[test]
fn test_query_graph_with_query_arg() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();
    let (presence, occupancy) = empty_presence();

    let mut args = Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": [
                {"op": "find", "type": "Function"}
            ]
        }),
    );

    let result = query_graph(&graph, &embedder, &cache, &presence, &occupancy, Some(&args));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn test_query_graph_with_empty_ops() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();
    let (presence, occupancy) = empty_presence();

    let mut args = Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": []
        }),
    );

    let result = query_graph(&graph, &embedder, &cache, &presence, &occupancy, Some(&args));
    assert!(result.is_ok());
}

/// Pin the schema documented in `docs/query-language.md`. Each op
/// in the `ops` array must have a discriminator key `op` (one of
/// `find`, `connect`, `filter`, `semantic_filter`, `group`, `sort`,
/// `limit`) — the per-op type is selected by serde via
/// `#[serde(tag = "op", rename_all = "lowercase")]`. A misread
/// variant shape (e.g. `{"find": "Function"}` with the type name
/// as a key instead of `{"op": "find", "type": "Function"}`) must
/// fail with a serde `missing field 'op'` error, which is the
/// right diagnostic for an agent that got the schema wrong.
///
/// This test also documents that the wishlist's example
/// `{"ops":[{"find":"Function"},{"limit":3}]}` is a misread of the
/// docs — the real format is `{"op":"find","type":"Function"}`.
#[test]
fn test_query_graph_schema_matches_docs() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();
    let (presence, occupancy) = empty_presence();

    // Documented format: discriminator `op` + per-op fields. Must
    // succeed.
    let mut good_args = Map::new();
    good_args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": [
                {"op": "find", "type": "Function"},
                {"op": "limit", "count": 3}
            ]
        }),
    );
    let result = query_graph(
        &graph,
        &embedder,
        &cache,
        &presence,
        &occupancy,
        Some(&good_args),
    );
    assert!(
        result.is_ok(),
        "documented query_graph schema must work; got {:?}",
        result.err()
    );

    // Misread format: type name as key, no `op` discriminator.
    // The serde error must clearly say `missing field 'op'` so an
    // agent that got the schema wrong can self-correct.
    let mut bad_args = Map::new();
    bad_args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": [
                {"find": "Function"},
                {"limit": {"count": 3}}
            ]
        }),
    );
    let result = query_graph(
        &graph,
        &embedder,
        &cache,
        &presence,
        &occupancy,
        Some(&bad_args),
    );
    let err = result.err().expect("misread format must error");
    let err_text = err.to_string();
    assert!(
        err_text.contains("missing field") && err_text.contains("op"),
        "serde error must mention 'op' so agents can self-correct; got: {err_text}"
    );
}

#[test]
fn test_describe_schema() {
    let result = describe_schema();
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should be valid JSON describing schema
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
    // Should contain schema information
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(value.is_object());
}
