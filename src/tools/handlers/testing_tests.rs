//! Tests for tools/handlers/testing.rs

use crate::tools::handlers::testing::{find_untested_functions, get_test_template, find_test_file, get_coverage_summary};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};

fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_testing_handlers");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a function with no incoming calls (potential dead/untested)
    let untested = GraphNode::new(NodeType::Function, "unused_fn".to_string(), "/src/lib.rs".to_string());
    // Create a function with incoming calls (tested)
    let tested = GraphNode::new(NodeType::Function, "tested_fn".to_string(), "/src/lib.rs".to_string());
    let caller = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/main.rs".to_string());

    graph.upsert_node(untested.clone()).unwrap();
    graph.upsert_node(tested.clone()).unwrap();
    graph.upsert_node(caller.clone()).unwrap();

    // caller -> tested_fn (tested has a caller)
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, caller.id.clone(), tested.id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_find_untested_functions_basic() {
    let (graph, _) = make_test_graph();

    let result = find_untested_functions(&graph, &VolatileOverlay::new(), None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Untested") || text.contains("caller") || text.contains("found"));
}

#[test]
fn test_find_untested_functions_with_limit() {
    let (graph, overlay) = make_test_graph();

    let result = find_untested_functions(&graph, &overlay, Some(5));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Untested") || text.contains("found"));
}

#[test]
fn test_get_test_template_function() {
    let (graph, overlay) = make_test_graph();

    let mut node = GraphNode::new(NodeType::Function, "my_function".to_string(), "/src/lib.rs".to_string());
    node.signature = Some("(x: i32, y: String) -> Result<i32, Error>".to_string());
    overlay.insert_node(node);

    let result = get_test_template(&graph, &overlay, "my_function");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("my_function"));
    assert!(text.contains("#[cfg(test)]"));
}

#[test]
fn test_get_test_template_struct() {
    let (graph, overlay) = make_test_graph();

    let node = GraphNode::new(NodeType::Struct, "MyStruct".to_string(), "/src/model.rs".to_string());
    overlay.insert_node(node);

    let result = get_test_template(&graph, &overlay, "MyStruct");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("MyStruct"));
    assert!(text.contains("Default") || text.contains("new"));
}

#[test]
fn test_get_test_template_enum() {
    let (graph, overlay) = make_test_graph();

    let node = GraphNode::new(NodeType::Enum, "Status".to_string(), "/src/types.rs".to_string());
    overlay.insert_node(node);

    let result = get_test_template(&graph, &overlay, "Status");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Status"));
    assert!(text.contains("variants"));
}

#[test]
fn test_get_test_template_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = get_test_template(&graph, &overlay, "nonexistent_function");
    assert!(result.is_err());
}

#[test]
fn test_find_test_file_with_src_path() {
    let (graph, _overlay) = make_test_graph();

    let result = find_test_file(&graph, "/src/main.rs");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("main.rs") || text.contains("test"));
}

#[test]
fn test_find_test_file_nonexistent() {
    let (graph, _overlay) = make_test_graph();

    let result = find_test_file(&graph, "/nonexistent/path.rs");
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should either find nothing or give search advice
    assert!(text.contains("not found") || text.contains("Search") || text.contains("test"));
}

#[test]
fn test_get_coverage_summary_all() {
    let (graph, overlay) = make_test_graph();

    let result = get_coverage_summary(&graph, &overlay, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Coverage") || text.contains("functions"));
    assert!(text.contains("Total") || text.contains("untested"));
}

#[test]
fn test_get_coverage_summary_specific_module() {
    let (graph, overlay) = make_test_graph();

    let result = get_coverage_summary(&graph, &overlay, Some("/src/lib.rs"));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Coverage") || text.contains("lib.rs"));
}