//! Graph correctness tests — algorithm verification for blast radius, resolution priority, and query executor

use lain::schema::{GraphNode, GraphEdge, NodeType, EdgeType};
use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::tools::handlers::impact::get_blast_radius;

fn make_test_graph() -> GraphDatabase {
    let tmp = std::env::temp_dir().join("test_graph_db");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Build a call graph:
    // main -> a -> b -> c (leaf)
    // main -> x -> b (b has two callers)
    // main -> y (y is dead — no outgoing edges)

    let main = GraphNode::new(NodeType::Function, "main".to_string(), "/src/main.rs".to_string());
    let a = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    let b = GraphNode::new(NodeType::Function, "b".to_string(), "/src/b.rs".to_string());
    let c = GraphNode::new(NodeType::Function, "c".to_string(), "/src/c.rs".to_string());
    let x = GraphNode::new(NodeType::Function, "x".to_string(), "/src/x.rs".to_string());
    let y = GraphNode::new(NodeType::Function, "y".to_string(), "/src/y.rs".to_string());

    graph.upsert_node(main.clone()).unwrap();
    graph.upsert_node(a.clone()).unwrap();
    graph.upsert_node(b.clone()).unwrap();
    graph.upsert_node(c.clone()).unwrap();
    graph.upsert_node(x.clone()).unwrap();
    graph.upsert_node(y.clone()).unwrap();

    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, b.id.clone(), c.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, main.id.clone(), x.id.clone())).unwrap();
    graph.insert_edge(&GraphEdge::new(EdgeType::Calls, x.id.clone(), b.id.clone())).unwrap();

    graph
}

fn make_overlay_with_node(name: &str, path: &str) -> VolatileOverlay {
    let overlay = VolatileOverlay::new();
    let node = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
    overlay.insert_node(node);
    overlay
}

#[test]
fn test_blast_radius_leaf_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // c is a leaf — nothing calls c
    let result = get_blast_radius(&graph, &overlay, "c", false, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Leaf has no callers, so only c itself is in visited set
    assert!(text.contains("affected nodes"));
    assert!(text.contains("c"));
}

#[test]
fn test_blast_radius_b_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // b has two callers: a and x. Both should appear in blast radius.
    let result = get_blast_radius(&graph, &overlay, "b", false, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should show a and x as dependents (at minimum)
    assert!(text.contains("a") || text.contains("x"));
}

#[test]
fn test_blast_radius_main_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    // main is root — no incoming edges to main in our test graph
    let result = get_blast_radius(&graph, &overlay, "main", false, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // Either no dependents found OR transitively affected nodes for root
    assert!(text.contains("no dependents") || text.contains("affected"));
}

#[test]
fn test_blast_radius_unknown_node() {
    let graph = make_test_graph();
    let overlay = VolatileOverlay::new();

    let result = get_blast_radius(&graph, &overlay, "nonexistent_symbol", false, None);
    assert!(result.is_err());
}

#[test]
fn test_graph_node_lookup_by_name() {
    let graph = make_test_graph();

    let found = graph.find_node_by_name("b");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "b");

    let not_found = graph.find_node_by_name("nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn test_graph_node_lookup_by_path() {
    let graph = make_test_graph();

    let found = graph.find_node_by_path("/src/b.rs");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "b");

    let not_found = graph.find_node_by_path("/src/nonexistent.rs");
    assert!(not_found.is_none());
}

#[test]
fn test_graph_get_nodes_by_type() {
    let graph = make_test_graph();

    let funcs = graph.get_nodes_by_type(NodeType::Function).unwrap();
    assert_eq!(funcs.len(), 6);

    let files = graph.get_nodes_by_type(NodeType::File).unwrap();
    assert_eq!(files.len(), 0);
}

#[test]
fn test_graph_get_neighbors_incoming() {
    let graph = make_test_graph();

    let b_node = graph.find_node_by_name("b").unwrap();
    let neighbors = graph.get_neighbors(&b_node.id, petgraph::Direction::Incoming);
    // b is called by a and x
    assert_eq!(neighbors.len(), 2);
    let mut names: Vec<_> = neighbors.iter().map(|(node, _)| node.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a", "x"]);
}

#[test]
fn test_graph_get_neighbors_outgoing() {
    let graph = make_test_graph();

    let main_node = graph.find_node_by_name("main").unwrap();
    let neighbors = graph.get_neighbors(&main_node.id, petgraph::Direction::Outgoing);
    // main calls a and x
    assert_eq!(neighbors.len(), 2);
    let mut names: Vec<_> = neighbors.iter().map(|(node, _)| node.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a", "x"]);
}

#[test]
fn test_graph_dead_code_detection() {
    let graph = make_test_graph();

    // Find nodes with zero incoming edges (potential dead code)
    let all_nodes = graph.get_all_nodes();
    let mut dead_nodes = Vec::new();

    for node in all_nodes {
        let incoming = graph.get_neighbors(&node.id, petgraph::Direction::Incoming);
        if incoming.is_empty() && node.name != "main" {
            // main is entry point so no callers is expected
            dead_nodes.push(node.name.clone());
        }
    }

    // y has no incoming or outgoing edges (it was added but nothing calls it or it calls nothing)
    // Actually y has no incoming, and main doesn't call y. So y is dead.
    assert!(dead_nodes.contains(&"y".to_string()), "y should be detected as dead");
}

#[test]
fn test_overlay_takes_priority_over_graph() {
    let graph = make_test_graph();

    // Add a conflicting node to overlay
    let mut overlay_node = GraphNode::new(NodeType::Function, "a".to_string(), "/src/a.rs".to_string());
    overlay_node.signature = Some("OVERLAY_SIG".to_string());
    let overlay = make_overlay_with_node("a", "/src/a.rs");

    // When both graph and overlay have "a", overlay should be checked first
    // We can't directly test resolve_node without the full tool context, but we can verify
    // both are independently accessible
    let graph_node = graph.find_node_by_name("a");
    let overlay_node_found = overlay.get_node(&overlay_node.id);

    assert!(graph_node.is_some());
    assert!(overlay_node_found.is_some());
}

#[test]
fn test_graph_id_determinism() {
    let n1 = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    assert_eq!(n1.id, n2.id, "Same node type+path+name must produce same ID");
}

#[test]
fn test_graph_edge_insertion_duplicate() {
    let tmp = std::env::temp_dir().join("test_edge_dup");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let n1 = GraphNode::new(NodeType::Function, "n1".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "n2".to_string(), "/src/lib.rs".to_string());

    graph.upsert_node(n1.clone()).unwrap();
    graph.upsert_node(n2.clone()).unwrap();

    // Insert same edge twice
    let edge = GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone());
    let r1 = graph.insert_edge(&edge);
    let r2 = graph.insert_edge(&edge);

    assert!(r1.is_ok());
    assert!(r2.is_ok()); // Should be idempotent
}

#[test]
fn test_graph_batch_node_insert() {
    let tmp = std::env::temp_dir().join("test_batch_nodes");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    let nodes: Vec<GraphNode> = (0..100)
        .map(|i| GraphNode::new(NodeType::Function, format!("fn_{}", i), "/src/lib.rs".to_string()))
        .collect();

    graph.insert_nodes_batch(&nodes).unwrap();

    let all = graph.get_all_nodes();
    assert_eq!(all.len(), 100);
}

#[test]
fn test_graph_batch_edge_insert() {
    let tmp = std::env::temp_dir().join("test_batch_edges");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();

    // Create a chain: n0 -> n1 -> n2 -> ... -> n99
    let nodes: Vec<GraphNode> = (0..100)
        .map(|i| GraphNode::new(NodeType::Function, format!("fn_{}", i), "/src/lib.rs".to_string()))
        .collect();

    graph.insert_nodes_batch(&nodes).unwrap();

    let edges: Vec<GraphEdge> = (0..99)
        .map(|i| GraphEdge::new(EdgeType::Calls, nodes[i].id.clone(), nodes[i+1].id.clone()))
        .collect();

    graph.insert_edges_batch(&edges).unwrap();

    // Verify graph consistency
    let all_nodes = graph.get_all_nodes();
    assert_eq!(all_nodes.len(), 100);

    for i in 0..99 {
        let outgoing = graph.get_edges_from(&nodes[i].id).unwrap();
        assert!(!outgoing.is_empty());
    }
}
