//! Tests for tools/handlers/query.rs

use crate::tools::handlers::query::{query_graph, describe_schema};
use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};
use serde_json::Map;

fn make_test_graph() -> GraphDatabase {
    let tmp = std::env::temp_dir().join("test_query_handler");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let fn1 = GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string());
    let fn2 = GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string());

    graph.upsert_node(fn1.clone()).unwrap();
    graph.upsert_node(fn2.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, fn1.id.clone(), fn2.id.clone())).unwrap();

    graph
}

#[test]
fn test_query_graph_default() {
    let graph = make_test_graph();

    let result = query_graph(&graph, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should be valid JSON
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn test_query_graph_with_query_arg() {
    let graph = make_test_graph();

    let mut args = Map::new();
    args.insert("query".to_string(), serde_json::json!({
        "ops": [
            {"op": "find", "type": "Function"}
        ]
    }));

    let result = query_graph(&graph, Some(&args));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

#[test]
fn test_query_graph_with_empty_ops() {
    let graph = make_test_graph();

    let mut args = Map::new();
    args.insert("query".to_string(), serde_json::json!({
        "ops": []
    }));

    let result = query_graph(&graph, Some(&args));
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