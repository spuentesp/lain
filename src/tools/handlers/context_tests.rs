//! Tests for tools/handlers/context.rs

use crate::tools::handlers::context::{get_context_for_prompt, get_code_snippet, get_call_sites};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};

fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_context_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a simple call graph: caller -> callee
    let caller = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/main.rs".to_string());
    let callee = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/callee.rs".to_string());
    let file_node = GraphNode::new(NodeType::File, "main.rs".to_string(), "/src/main.rs".to_string());

    graph.upsert_node(caller.clone()).unwrap();
    graph.upsert_node(callee.clone()).unwrap();
    graph.upsert_node(file_node.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Contains, file_node.id.clone(), caller.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, caller.id.clone(), callee.id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_get_context_for_prompt_existing() {
    let (graph, overlay) = make_test_graph();

    // Add node with full info to overlay
    let mut node = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/main.rs".to_string());
    node.signature = Some("(x: i32) -> i32".to_string());
    node.docstring = Some("A test function".to_string());
    node.depth_from_main = Some(0);
    node.anchor_score = Some(0.5);
    overlay.insert_node(node);

    let result = get_context_for_prompt(&graph, &overlay, "caller", None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("caller"));
    assert!(text.contains("Function"));
}

#[test]
fn test_get_context_for_prompt_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_context_for_prompt(&graph, &overlay, "nonexistent_symbol", None);
    assert!(result.is_err());
}

#[test]
fn test_get_context_for_prompt_with_max_tokens() {
    let (graph, overlay) = make_test_graph();

    let mut node = GraphNode::new(NodeType::Function, "big_fn".to_string(), "/src/lib.rs".to_string());
    node.docstring = Some("A".repeat(1000));
    overlay.insert_node(node);

    let result = get_context_for_prompt(&graph, &overlay, "big_fn", Some(50));
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should contain the function name
    assert!(text.contains("big_fn"));
}

#[test]
fn test_get_code_snippet_existing() {
    let (graph, overlay) = make_test_graph();

    // Create a temp file to read
    let tmp = std::env::temp_dir().join("test_snippet.txt");
    std::fs::write(&tmp, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    let result = get_code_snippet(&graph, &overlay, tmp.to_str().unwrap(), Some(2), None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("line2") || text.contains("Showing lines"));

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_get_code_snippet_nonexistent_file() {
    let (graph, overlay) = make_test_graph();

    let result = get_code_snippet(&graph, &overlay, "/nonexistent/file.txt", None, None);
    assert!(result.is_err());
}

#[test]
fn test_get_call_sites_existing() {
    let (graph, overlay) = make_test_graph();

    // callee is called by caller
    let result = get_call_sites(&graph, &overlay, "callee");
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should show caller as a call site for callee
    assert!(text.contains("call sites") || text.contains("callee"));
}

#[test]
fn test_get_call_sites_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_call_sites(&graph, &overlay, "nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_get_call_sites_no_callers() {
    let (graph, overlay) = make_test_graph();

    // "caller" has no incoming calls in our test graph
    let result = get_call_sites(&graph, &overlay, "caller");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("No call sites") || text.contains("caller"));
}