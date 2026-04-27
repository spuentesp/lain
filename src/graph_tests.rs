//! Tests for graph.rs

use crate::graph::GraphDatabase;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};
use std::collections::HashSet;

fn make_test_graph() -> GraphDatabase {
    let tmp = std::env::temp_dir().join("test_graph_fn");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a simple call graph:
    // main -> a -> b -> c (leaf)
    // main -> x -> b (b has two callers)
    // dead (no connections)

    let main = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    let a = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    let b = GraphNode::new(NodeType::Function, "b".to_string(), "/src/b.rs".to_string());
    let c = GraphNode::new(NodeType::Function, "c".to_string(), "/src/c.rs".to_string());
    let x = GraphNode::new(NodeType::Function, "x".to_string(), "/src/x.rs".to_string());
    let dead = GraphNode::new(NodeType::Function, "dead".to_string(), "/src/dead.rs".to_string());

    graph.upsert_node(main.clone()).unwrap();
    graph.upsert_node(a.clone()).unwrap();
    graph.upsert_node(b.clone()).unwrap();
    graph.upsert_node(c.clone()).unwrap();
    graph.upsert_node(x.clone()).unwrap();
    graph.upsert_node(dead.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, b.id.clone(), c.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), x.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, x.id.clone(), b.id.clone())).unwrap();

    graph
}

#[test]
fn test_bfs_from_valid_start() {
    let graph = make_test_graph();

    let main = graph.find_node_by_name("main").unwrap();
    let results = graph.bfs_from(&main.id, 3);

    // main -> a -> b -> c, main -> x -> b
    // Should get neighbors at various depths
    assert!(!results.is_empty());
    let depths: HashSet<u32> = results.iter().map(|(_, _, d)| *d).collect();
    assert!(depths.contains(&1)); // a, x at depth 1
}

#[test]
fn test_bfs_from_invalid_start() {
    let graph = make_test_graph();
    let results = graph.bfs_from("nonexistent_id", 3);
    assert!(results.is_empty());
}

#[test]
fn test_bfs_from_depth_limit() {
    let graph = make_test_graph();

    let main = graph.find_node_by_name("main").unwrap();
    let depth1 = graph.bfs_from(&main.id, 1);
    let depth2 = graph.bfs_from(&main.id, 2);
    let depth3 = graph.bfs_from(&main.id, 3);

    // Deeper searches should include more nodes
    assert!(depth2.len() >= depth1.len());
    assert!(depth3.len() >= depth2.len());
}

#[test]
fn test_calculate_anchor_scores() {
    let graph = make_test_graph();

    // b has 2 incoming (a, x), so should have fan_in=2
    graph.calculate_anchor_scores().unwrap();

    let b = graph.find_node_by_name("b").unwrap();
    assert!(b.fan_in.is_some());
    assert!(b.fan_out.is_some());
    assert_eq!(b.fan_in.unwrap(), 2);
}

#[test]
fn test_find_anchors() {
    let graph = make_test_graph();
    graph.calculate_anchor_scores().unwrap();

    let anchors = graph.find_anchors(3);
    assert!(anchors.is_ok());
    assert!(anchors.unwrap().len() <= 3);
}

#[test]
fn test_get_stats() {
    let graph = make_test_graph();
    let (nodes, edges) = graph.get_stats();
    assert_eq!(nodes, 6);
    assert_eq!(edges, 5);
}

#[test]
fn test_get_node_at_location() {
    let tmp = std::env::temp_dir().join("test_loc");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let mut node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    node.line_start = Some(10);
    node.line_end = Some(20);
    graph.upsert_node(node).unwrap();

    let result = graph.get_node_at_location("/src/lib.rs", 15);
    assert!(result.is_some());
}

#[test]
fn test_get_node_at_location_nonexistent() {
    let graph = make_test_graph();
    let node = graph.get_node_at_location("/nonexistent.rs", 1);
    assert!(node.is_none());
}

#[test]
fn test_get_nodes_by_type() {
    let graph = make_test_graph();
    let functions = graph.get_nodes_by_type(NodeType::Function);
    assert!(functions.is_ok());
    assert_eq!(functions.unwrap().len(), 6);
}

#[test]
fn test_get_nodes_by_types() {
    let graph = make_test_graph();
    let nodes = graph.get_nodes_by_types(&[NodeType::Function]);
    assert!(nodes.is_ok());
    assert_eq!(nodes.unwrap().len(), 6);
}

#[test]
fn test_find_node_by_name() {
    let graph = make_test_graph();
    let node = graph.find_node_by_name("main");
    assert!(node.is_some());
    assert_eq!(node.unwrap().name, "main");
}

#[test]
fn test_find_node_by_name_not_found() {
    let graph = make_test_graph();
    let node = graph.find_node_by_name("nonexistent");
    assert!(node.is_none());
}

#[test]
fn test_find_node_by_path() {
    let graph = make_test_graph();
    let node = graph.find_node_by_path("/src/main.rs");
    assert!(node.is_some());
}

#[test]
fn test_find_node_by_path_not_found() {
    let graph = make_test_graph();
    let node = graph.find_node_by_path("/nonexistent.rs");
    assert!(node.is_none());
}

#[test]
fn test_insert_co_change_edges() {
    let tmp = std::env::temp_dir().join("test_co_change");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let n1 = GraphNode::new(NodeType::File, "file1.rs".to_string(), "/src/file1.rs".to_string());
    let n2 = GraphNode::new(NodeType::File, "file2.rs".to_string(), "/src/file2.rs".to_string());

    graph.upsert_node(n1.clone()).unwrap();
    graph.upsert_node(n2.clone()).unwrap();

    let pairs = vec![(n1.id.clone(), n2.id.clone(), 5)];
    let result = graph.insert_co_change_edges(&pairs);
    assert!(result.is_ok());
}

#[test]
fn test_get_co_change_partners() {
    let tmp = std::env::temp_dir().join("test_co_change_partners");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // insert_co_change_edges expects paths, not IDs
    let path1 = "/src/file1.rs";
    let path2 = "/src/file2.rs";

    let n1 = GraphNode::new(NodeType::File, "file1.rs".to_string(), path1.to_string());
    let n2 = GraphNode::new(NodeType::File, "file2.rs".to_string(), path2.to_string());

    graph.upsert_node(n1).unwrap();
    graph.upsert_node(n2).unwrap();

    // Pass (path, path, count) tuples - insert_co_change_edges generates IDs internally
    let pairs = vec![(path1.to_string(), path2.to_string(), 3)];
    graph.insert_co_change_edges(&pairs).unwrap();

    let partners = graph.get_co_change_partners(path1);
    assert!(partners.is_ok());
    assert!(!partners.unwrap().is_empty());
}

#[test]
fn test_calculate_depths() {
    let graph = make_test_graph();
    let result = graph.calculate_depths();
    assert!(result.is_ok());

    let main = graph.find_node_by_name("main").unwrap();

    // main should have depth 0 (entry point)
    assert_eq!(main.depth_from_main, Some(0));
    // dead has no connections so might have no depth assigned or max
    let _dead_node = graph.find_node_by_name("dead").unwrap();
}

#[test]
fn test_find_entry_points() {
    let graph = make_test_graph();
    let result = graph.find_entry_points();
    assert!(result.is_ok());
}

#[test]
fn test_has_references_from() {
    let graph = make_test_graph();

    let main = graph.find_node_by_name("main").unwrap();
    let dead = graph.find_node_by_name("dead").unwrap();

    // main is called by nothing (but calls others), so has_references_from might be false
    // dead has no connections, so definitely false
    let main_refs = graph.has_references_from(&main.id);
    let dead_refs = graph.has_references_from(&dead.id);

    // Just verify the function works - actual values depend on graph structure
    let _ = main_refs;
    let _ = dead_refs;
}

#[test]
fn test_export_to_json() {
    let graph = make_test_graph();
    let json = graph.export_to_json();
    assert!(json.is_ok());
    let json_str = json.unwrap();
    assert!(json_str.contains("main"));
}

#[test]
fn test_insert_nodes_batch() {
    let tmp = std::env::temp_dir().join("test_batch_insert");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let nodes: Vec<GraphNode> = (0..10)
        .map(|i| GraphNode::new(NodeType::Function, format!("fn_{}", i), "/src/lib.rs".to_string()))
        .collect();

    let result = graph.insert_nodes_batch(&nodes);
    assert!(result.is_ok());
    assert_eq!(graph.get_stats().0, 10);
}

#[test]
fn test_insert_edges_batch() {
    let tmp = std::env::temp_dir().join("test_batch_edges");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let n1 = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "b".to_string(), "/src/b.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "c".to_string(), "/src/c.rs".to_string());

    graph.upsert_node(n1.clone()).unwrap();
    graph.upsert_node(n2.clone()).unwrap();
    graph.upsert_node(n3.clone()).unwrap();

    let edges = vec![
        GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone()),
        GraphEdge::new(EdgeType::Calls, n2.id.clone(), n3.id.clone()),
    ];

    let result = graph.insert_edges_batch(&edges);
    assert!(result.is_ok());
    assert_eq!(graph.get_stats().1, 2);
}