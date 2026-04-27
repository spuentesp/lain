//! Tests for tools/handlers/architecture.rs

use crate::tools::handlers::architecture::{explore_architecture, list_entry_points, compare_modules, get_master_map, architectural_observations};
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType, EdgeType, GraphEdge};

fn make_test_graph() -> (GraphDatabase, VolatileOverlay) {
    let tmp = std::env::temp_dir().join("test_arch_handlers");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let file1 = GraphNode::new(NodeType::File, "main.rs".to_string(), "/src/main.rs".to_string());
    let file2 = GraphNode::new(NodeType::File, "lib.rs".to_string(), "/src/lib.rs".to_string());
    let ns = GraphNode::new(NodeType::Namespace, "src".to_string(), "/src".to_string());

    graph.upsert_node(file1.clone()).unwrap();
    graph.upsert_node(file2.clone()).unwrap();
    graph.upsert_node(ns.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Contains, ns.id.clone(), file1.id.clone())).unwrap();

    let overlay = VolatileOverlay::new();
    (graph, overlay)
}

#[test]
fn test_explore_architecture_basic() {
    let (graph, overlay) = make_test_graph();

    let result = explore_architecture(&graph, &overlay, 10);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Architecture") || text.contains("files"));
}

#[test]
fn test_explore_architecture_depth_filter() {
    let (graph, overlay) = make_test_graph();

    let result = explore_architecture(&graph, &overlay, 1);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Architecture") || text.contains("Depth"));
}

#[test]
fn test_list_entry_points_basic() {
    let (graph, overlay) = make_test_graph();

    let result = list_entry_points(&graph, &overlay);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Entry Points") || text.contains("main") || text.contains("App"));
}

#[test]
fn test_list_entry_points_with_main() {
    let tmp = std::env::temp_dir().join("test_entry_main");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let main_node = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    graph.upsert_node(main_node).unwrap();

    let result = list_entry_points(&graph, &overlay);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("main"));
}

#[test]
fn test_compare_modules_existing() {
    let (graph, overlay) = make_test_graph();

    // Add nodes to overlay so compare_modules can resolve them
    let node_a = GraphNode::new(NodeType::File, "a.rs".to_string(), "/src/a.rs".to_string());
    let node_b = GraphNode::new(NodeType::File, "b.rs".to_string(), "/src/b.rs".to_string());
    overlay.insert_node(node_a);
    overlay.insert_node(node_b);

    let result = compare_modules(&graph, &overlay, "a.rs", "b.rs");
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Comparison") || text.contains("a.rs") || text.contains("b.rs"));
}

#[test]
fn test_compare_modules_not_found() {
    let (graph, overlay) = make_test_graph();

    let result = compare_modules(&graph, &overlay, "nonexistent_a", "nonexistent_b");
    assert!(result.is_err());
}

#[test]
fn test_get_master_map_basic() {
    let (graph, overlay) = make_test_graph();

    let result = get_master_map(&graph, &overlay);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Master Map") || text.contains("Staleness"));
}

#[test]
fn test_architectural_observations_basic() {
    let (graph, _overlay) = make_test_graph();

    let result = architectural_observations(&graph, 0, 0);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Observations") || text.contains("Fan-Out") || text.contains("analyzed"));
}

#[test]
fn test_architectural_observations_high_threshold() {
    let (graph, _overlay) = make_test_graph();

    // Very high threshold - unlikely to find anything
    let result = architectural_observations(&graph, 1000, 100);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("Observations") || text.contains("analyzed"));
}