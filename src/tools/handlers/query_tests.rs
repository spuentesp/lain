//! Tests for tools/handlers/query.rs

use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::tools::handlers::query::{describe_schema, query_graph};
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

#[test]
fn test_query_graph_default() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();

    let result = query_graph(&graph, &embedder, &cache, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should be valid JSON
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn test_query_graph_with_query_arg() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();

    let mut args = Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": [
                {"op": "find", "type": "Function"}
            ]
        }),
    );

    let result = query_graph(&graph, &embedder, &cache, Some(&args));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn test_query_graph_with_empty_ops() {
    let graph = make_test_graph();
    let (embedder, cache) = test_embedder_and_cache();

    let mut args = Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!({
            "ops": []
        }),
    );

    let result = query_graph(&graph, &embedder, &cache, Some(&args));
    assert!(result.is_ok());
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
