//! Tests for overlay.rs

use crate::overlay::VolatileOverlay;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};

#[test]
fn test_overlay_new() {
    let overlay = VolatileOverlay::new();
    let stats = overlay.stats();
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
}

#[test]
fn test_overlay_insert_node() {
    let overlay = VolatileOverlay::new();
    let node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let id = node.id.clone();
    let idx = overlay.insert_node(node);
    assert!(idx.index() < usize::MAX);

    let retrieved = overlay.get_node(&id);
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().name, "test_fn");
}

#[test]
fn test_overlay_insert_node_upsert() {
    let overlay = VolatileOverlay::new();
    let node = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    let id = node.id.clone();
    overlay.insert_node(node);

    // Insert with same ID — should replace
    let mut node2 = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    node2.signature = Some("new_sig".to_string());
    overlay.insert_node(node2);

    // Node should be updated
    let retrieved = overlay.get_node(&id);
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().signature, Some("new_sig".to_string()));
}

#[test]
fn test_overlay_get_node_not_found() {
    let overlay = VolatileOverlay::new();
    let result = overlay.get_node("nonexistent_id");
    assert!(result.is_none());
}

#[test]
fn test_overlay_get_all_nodes() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string());
    let n3 = GraphNode::new(NodeType::Struct, "struct1".to_string(), "/src/lib.rs".to_string());

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    let all = overlay.get_all_nodes();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_overlay_find_nodes_by_name() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "test_function".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "Test_Function".to_string(), "/src/lib.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "other".to_string(), "/src/lib.rs".to_string());

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    let found = overlay.find_nodes_by_name("test_function");
    assert!(!found.is_empty());
    assert!(found.iter().any(|n| n.name == "test_function" || n.name == "Test_Function"));
}

#[test]
fn test_overlay_find_nodes_by_name_case_insensitive() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "MyFunction".to_string(), "/src/lib.rs".to_string());
    overlay.insert_node(n1);

    let found = overlay.find_nodes_by_name("myfunction");
    assert!(!found.is_empty());
    assert_eq!(found[0].name, "MyFunction");
}

#[test]
fn test_overlay_find_nodes_by_name_not_found() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "test_fn".to_string(), "/src/lib.rs".to_string());
    overlay.insert_node(n1);

    let found = overlay.find_nodes_by_name("nonexistent");
    assert!(found.is_empty());
}

#[test]
fn test_overlay_find_nodes_by_type() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Struct, "struct1".to_string(), "/src/lib.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string());

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    let funcs = overlay.find_nodes_by_type(&NodeType::Function);
    assert_eq!(funcs.len(), 2);

    let structs = overlay.find_nodes_by_type(&NodeType::Struct);
    assert_eq!(structs.len(), 1);
}

#[test]
fn test_overlay_find_nodes_by_path() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/main.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/main.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "fn3".to_string(), "/src/lib.rs".to_string());

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    let found = overlay.find_nodes_by_path("/src/main.rs");
    assert_eq!(found.len(), 2);
}

#[test]
fn test_overlay_insert_edge() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);

    let edge = GraphEdge::new(EdgeType::Calls, n1_id.clone(), n2_id.clone());
    let result = overlay.insert_edge(&edge);
    assert!(result.is_ok());
}

#[test]
fn test_overlay_insert_edge_source_not_found() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    overlay.insert_node(n1);

    let edge = GraphEdge::new(EdgeType::Calls, "nonexistent".to_string(), n1_id);
    let result = overlay.insert_edge(&edge);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Source node not found"));
}

#[test]
fn test_overlay_insert_edge_target_not_found() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    overlay.insert_node(n1);

    let edge = GraphEdge::new(EdgeType::Calls, n1_id, "nonexistent".to_string());
    let result = overlay.insert_edge(&edge);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Target node not found"));
}

#[test]
fn test_overlay_insert_edge_duplicate_idempotent() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);

    let edge = GraphEdge::new(EdgeType::Calls, n1_id, n2_id);

    // Insert twice should be idempotent
    let r1 = overlay.insert_edge(&edge);
    let r2 = overlay.insert_edge(&edge);
    assert!(r1.is_ok());
    assert!(r2.is_ok());
}

#[test]
fn test_overlay_get_outgoing_edges() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee1".to_string(), "/src/lib.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "callee2".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();
    let n3_id = n3.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id.clone(), n2_id)).unwrap();
    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id.clone(), n3_id)).unwrap();

    let outgoing = overlay.get_outgoing_edges(&n1_id);
    assert_eq!(outgoing.len(), 2);
}

#[test]
fn test_overlay_get_outgoing_edges_none() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "lonely".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    overlay.insert_node(n1);

    let outgoing = overlay.get_outgoing_edges(&n1_id);
    assert!(outgoing.is_empty());
}

#[test]
fn test_overlay_get_outgoing_edges_unknown_node() {
    let overlay = VolatileOverlay::new();
    let outgoing = overlay.get_outgoing_edges("nonexistent_id");
    assert!(outgoing.is_empty());
}

#[test]
fn test_overlay_stats() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);

    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id, n2_id)).unwrap();

    let stats = overlay.stats();
    assert_eq!(stats.node_count, 2);
    assert_eq!(stats.edge_count, 1);
}

#[test]
fn test_overlay_clear() {
    let overlay = VolatileOverlay::new();
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));

    overlay.clear();

    let stats = overlay.stats();
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
    assert!(overlay.get_all_nodes().is_empty());
}

#[test]
fn test_overlay_clear_then_insert() {
    let overlay = VolatileOverlay::new();
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));
    overlay.clear();
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string()));

    let stats = overlay.stats();
    assert_eq!(stats.node_count, 1);
}

#[test]
fn test_overlay_merge() {
    let overlay1 = VolatileOverlay::new();
    let overlay2 = VolatileOverlay::new();

    overlay1.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));
    overlay1.insert_node(GraphNode::new(NodeType::Function, "fn2".to_string(), "/src/lib.rs".to_string()));
    overlay2.insert_node(GraphNode::new(NodeType::Function, "fn3".to_string(), "/src/lib.rs".to_string()));

    overlay1.merge(&overlay2);

    // overlay1 should now have all 3 nodes
    let all = overlay1.get_all_nodes();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_overlay_merge_preserves_edges() {
    let overlay1 = VolatileOverlay::new();
    let overlay2 = VolatileOverlay::new();

    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay1.insert_node(n1);
    overlay1.insert_node(n2);
    overlay1.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id.clone(), n2_id.clone())).unwrap();

    overlay2.insert_node(GraphNode::new(NodeType::Function, "other".to_string(), "/src/lib.rs".to_string()));

    overlay1.merge(&overlay2);

    // Edge from caller to callee should be preserved
    let outgoing = overlay1.get_outgoing_edges(&n1_id);
    assert_eq!(outgoing.len(), 1);
}

#[test]
fn test_overlay_get_all_edges() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id, n2_id)).unwrap();

    let edges = overlay.get_all_edges();
    assert_eq!(edges.len(), 1);
    let (src, tgt, et) = &edges[0];
    assert_eq!(src.name, "caller");
    assert_eq!(tgt.name, "callee");
    assert_eq!(*et, EdgeType::Calls);
}

#[test]
fn test_overlay_stats_empty() {
    let overlay = VolatileOverlay::new();
    let stats = overlay.stats();
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
}

#[test]
fn test_overlay_default() {
    let overlay = VolatileOverlay::default();
    let stats = overlay.stats();
    assert_eq!(stats.node_count, 0);
}

#[test]
fn test_overlay_insert_node_multiple() {
    let overlay = VolatileOverlay::new();

    for i in 0..100 {
        overlay.insert_node(GraphNode::new(NodeType::Function, format!("fn_{}", i), "/src/lib.rs".to_string()));
    }

    let stats = overlay.stats();
    assert_eq!(stats.node_count, 100);
}

#[test]
fn test_overlay_get_incoming_edges() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id, n2_id.clone())).unwrap();

    let incoming = overlay.get_incoming_edges(&n2_id);
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].0.name, "caller");
}

#[test]
fn test_overlay_get_incoming_edges_multiple() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller1".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "caller2".to_string(), "/src/lib.rs".to_string());
    let n3 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();
    let n3_id = n3.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);
    overlay.insert_node(n3);

    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n1_id, n3_id.clone())).unwrap();
    overlay.insert_edge(&GraphEdge::new(EdgeType::Calls, n2_id.clone(), n3_id.clone())).unwrap();

    let incoming = overlay.get_incoming_edges(&n3_id);
    assert_eq!(incoming.len(), 2);
}

#[test]
fn test_overlay_get_incoming_edges_none() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "lonely".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    overlay.insert_node(n1);

    let incoming = overlay.get_incoming_edges(&n1_id);
    assert!(incoming.is_empty());
}

#[test]
fn test_overlay_get_incoming_edges_unknown_node() {
    let overlay = VolatileOverlay::new();
    let incoming = overlay.get_incoming_edges("nonexistent_id");
    assert!(incoming.is_empty());
}

#[test]
fn test_overlay_insert_edge_all_edge_types() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "a".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "b".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);

    for et in [
        EdgeType::Calls,
        EdgeType::Uses,
        EdgeType::Contains,
        EdgeType::Imports,
        EdgeType::Implements,
    ] {
        let edge = GraphEdge::new(et.clone(), n1_id.clone(), n2_id.clone());
        let r = overlay.insert_edge(&edge);
        assert!(r.is_ok());
    }
}

#[test]
fn test_overlay_find_nodes_by_path_not_found() {
    let overlay = VolatileOverlay::new();
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));

    let found = overlay.find_nodes_by_path("/nonexistent/path.rs");
    assert!(found.is_empty());
}

#[test]
fn test_overlay_last_update_age_secs_initial() {
    let overlay = VolatileOverlay::new();
    // Freshly created overlay should have age close to 0
    let age = overlay.last_update_age_secs();
    assert!(age < 1.0, "Fresh overlay should have age < 1s, got {}", age);
}

#[test]
fn test_overlay_last_update_age_updates_on_insert() {
    let overlay = VolatileOverlay::new();
    // Insert a node
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));
    let age = overlay.last_update_age_secs();
    assert!(age < 1.0, "After insert, overlay should have fresh timestamp, got {}", age);
}

#[test]
fn test_overlay_last_update_age_updates_on_clear() {
    let overlay = VolatileOverlay::new();
    overlay.insert_node(GraphNode::new(NodeType::Function, "fn1".to_string(), "/src/lib.rs".to_string()));
    overlay.clear();
    let age = overlay.last_update_age_secs();
    assert!(age < 1.0, "After clear, overlay should have fresh timestamp, got {}", age);
}

#[test]
fn test_overlay_last_update_age_updates_on_edge() {
    let overlay = VolatileOverlay::new();
    let n1 = GraphNode::new(NodeType::Function, "caller".to_string(), "/src/lib.rs".to_string());
    let n2 = GraphNode::new(NodeType::Function, "callee".to_string(), "/src/lib.rs".to_string());
    let n1_id = n1.id.clone();
    let n2_id = n2.id.clone();

    overlay.insert_node(n1);
    overlay.insert_node(n2);

    // Insert edge - should refresh timestamp
    let edge = GraphEdge::new(EdgeType::Calls, n1_id, n2_id);
    overlay.insert_edge(&edge).unwrap();

    let age = overlay.last_update_age_secs();
    assert!(age < 1.0, "After edge insert, overlay should have fresh timestamp, got {}", age);
}