//! Tests for tools/handlers/testing.rs

use crate::server::tools::handlers::testing::{find_untested_functions, get_test_template, find_test_file, get_coverage_summary};
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

    // Structural shape: explicit named metrics, no verdict, no ambiguous
    // single number that an LLM could quote as "line-level coverage".
    assert!(text.contains("**Structural reach:**"), "missing structural reach label: {text}");
    assert!(text.contains("**Entrypoint coverage:**"), "missing entrypoint coverage label: {text}");
    assert!(text.contains("`structural_reach:"), "missing machine-readable structural_reach key: {text}");
    assert!(text.contains("`entrypoint_coverage:"), "missing machine-readable entrypoint_coverage key: {text}");

    // Old headline and verdict are gone.
    assert!(!text.contains("Excellent coverage"), "stale 'Excellent coverage!' verdict still present");
    assert!(!text.contains("Estimated coverage"), "stale 'Estimated coverage' headline still present");
    assert!(!text.contains("\u{26a0}\u{fe0f} Coverage is below"), "stale warning branch still present");
    assert!(!text.contains("\u{2139}\u{fe0f} Consider adding tests"), "stale info branch still present");

    // The footer caveat must survive.
    assert!(text.contains("structural estimate, not actual line-level coverage"));

    // Existing structural data still present.
    assert!(text.contains("Total functions"));
    assert!(text.contains("Potentially untested"));
}

#[test]
fn test_get_coverage_summary_specific_module() {
    let (graph, overlay) = make_test_graph();

    let result = get_coverage_summary(&graph, &overlay, Some("/src/lib.rs"));
    assert!(result.is_ok());
    let text = result.unwrap();

    assert!(text.contains("**Structural reach:**"), "{text}");
    assert!(text.contains("**Entrypoint coverage:**"), "{text}");
    assert!(!text.contains("Excellent coverage"), "{text}");
    assert!(!text.contains("Estimated coverage"), "{text}");
    assert!(text.contains("structural estimate, not actual line-level coverage"));
}

#[test]
fn test_get_coverage_summary_with_entry_point() {
    // Build a graph with a real `main` entry point so entrypoint_coverage
    // and structural_reach exercise the non-vacuous path.
    let tmp = std::env::temp_dir().join("test_coverage_entrypoint");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let main_node = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    let handler_node = GraphNode::new(NodeType::Function, "handler".to_string(), "/src/lib.rs".to_string());
    let orphan_node = GraphNode::new(NodeType::Function, "orphan".to_string(), "/src/lib.rs".to_string());

    graph.upsert_node(main_node.clone()).unwrap();
    graph.upsert_node(handler_node.clone()).unwrap();
    graph.upsert_node(orphan_node.clone()).unwrap();
    graph.insert_edge(
        &GraphEdge::new(EdgeType::Calls, main_node.id.clone(), handler_node.id.clone()),
    ).unwrap();

    let overlay = VolatileOverlay::new();
    let result = get_coverage_summary(&graph, &overlay, None).unwrap();

    // Headline shape: pipe-separated single line.
    assert!(result.contains("**Structural reach:**") && result.contains("**Entrypoint coverage:**"),
        "{result}");

    // Backticked keys present, old headline absent.
    assert!(result.contains("`structural_reach:"));
    assert!(result.contains("`entrypoint_coverage:"));
    assert!(!result.contains("Excellent coverage"));

    // With 1 entry point (`main`), entrypoint_coverage is either 0.00 or 1.00
    // depending on fan_in — the formatter must produce a literal percentage
    // in [0%, 100%], not the vacuous "100%" fallback unconditionally.
    let header = result
        .lines()
        .find(|l| l.contains("**Entrypoint coverage:**"))
        .expect("missing entrypoint coverage line");
    let pct_str = header
        .split("**Entrypoint coverage:**")
        .nth(1)
        .and_then(|s| s.split('%').next())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .expect("non-numeric entrypoint coverage");
    assert!((0.0..=100.0).contains(&pct_str), "out-of-range entrypoint coverage: {pct_str}");

    // Footer still present.
    assert!(result.contains("structural estimate, not actual line-level coverage"));
}