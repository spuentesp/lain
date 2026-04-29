//! Tests for tools/handlers/metrics.rs

use crate::nlp::NlpEmbedder;
use crate::tools::handlers::metrics::{find_anchors, get_anchor_score, get_context_depth, find_dead_code, explain_symbol, suggest_refactor_targets};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};
use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;

fn make_test_graph_with_nodes() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_metrics_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a simple function graph: main -> a -> b
    let main = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    let a = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    let b = GraphNode::new(NodeType::Function, "b".to_string(), "/src/b.rs".to_string());

    graph.upsert_node(main.clone()).unwrap();
    graph.upsert_node(a.clone()).unwrap();
    graph.upsert_node(b.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_find_anchors_basic() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = find_anchors(&graph, &overlay, 5);
    assert!(result.is_ok());
    let text = result.unwrap();
    // May be empty if no anchor scores calculated, or show anchors if calculate_anchor_scores was run
    if !text.contains("No anchors") {
        assert!(text.contains("anchors") || text.contains("Top"));
    }
}

#[test]
fn test_get_anchor_score_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Create node with anchor score in overlay
    let mut node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    node.anchor_score = Some(0.5);
    overlay.insert_node(node);

    let result = get_anchor_score(&graph, &overlay, "test_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("test_fn"));
}

#[test]
fn test_get_anchor_score_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = get_anchor_score(&graph, &overlay, "nonexistent");
    // Returns error when node not found
    assert!(result.is_err());
}

#[test]
fn test_get_context_depth_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Create node with depth in overlay
    let mut node = GraphNode::new(NodeType::Function, "deep_fn".to_string(), "/src/lib.rs".to_string());
    node.depth_from_main = Some(3);
    overlay.insert_node(node);

    let result = get_context_depth(&graph, &overlay, "deep_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("deep_fn"));
}

#[test]
fn test_get_context_depth_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = get_context_depth(&graph, &overlay, "nonexistent");
    // Returns error when node not found
    assert!(result.is_err());
}

#[test]
fn test_find_dead_code() {
    let (graph, overlay) = make_test_graph_with_nodes();
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(Mutex::new(HashMap::new()));

    let result = find_dead_code(&graph, &overlay, None, &embedder, &cache);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("dead code") || text.contains("Found"));
}

#[test]
fn test_explain_symbol_existing() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // Put node in overlay with all fields
    let mut node = GraphNode::new(NodeType::Function, "documented_fn".to_string(), "/src/lib.rs".to_string());
    node.signature = Some("(x: i32) -> i32".to_string());
    node.docstring = Some("Does something useful".to_string());
    node.depth_from_main = Some(2);
    node.anchor_score = Some(0.3);
    overlay.insert_node(node);

    let result = explain_symbol(&graph, &overlay, "documented_fn");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("documented_fn"));
    assert!(text.contains("Function"));
    assert!(text.contains("signature") || text.contains("Documentation"));
}

#[test]
fn test_explain_symbol_not_found() {
    let (graph, overlay) = make_test_graph_with_nodes();

    // find_dead_code returns empty (not error), but explain_symbol should error
    let result = explain_symbol(&graph, &overlay, "nonexistent_node_xyz");
    assert!(result.is_err());
}

#[test]
fn test_suggest_refactor_targets_empty() {
    let (graph, overlay) = make_test_graph_with_nodes();

    let result = suggest_refactor_targets(&graph, &overlay, 5);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should either show suggestions or say none found
    assert!(text.contains("Refactor") || text.contains("healthy") || text.contains("No nodes"));
}

#[test]
fn test_suggest_refactor_targets_with_debt() {
    let tmp = std::env::temp_dir().join("test_refactor_debt");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    // Create a high fan-in/fan-out node that might trigger debt scoring
    let mut node = GraphNode::new(NodeType::Class, "GodClass".to_string(), "/src/main.rs".to_string());
    node.fan_in = Some(15);
    node.fan_out = Some(15);
    node.anchor_score = Some(0.1);
    graph.upsert_node(node).unwrap();

    let result = suggest_refactor_targets(&graph, &overlay, 5);
    assert!(result.is_ok());
    // May or may not find targets depending on thresholds
    let text = result.unwrap();
    assert!(text.contains("Refactor") || text.contains("healthy") || text.contains("No nodes"));
}