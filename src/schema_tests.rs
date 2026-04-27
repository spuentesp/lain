//! Tests for schema types

use crate::schema::{GraphNode, GraphEdge, NodeType, EdgeType};

#[test]
fn test_node_type_display() {
    assert_eq!(format!("{}", NodeType::Function), "Function");
    assert_eq!(format!("{}", NodeType::Struct), "Struct");
    assert_eq!(format!("{}", NodeType::File), "File");
}

#[test]
fn test_edge_type_display() {
    assert_eq!(format!("{}", EdgeType::Calls), "Calls");
    assert_eq!(format!("{}", EdgeType::Contains), "Contains");
    assert_eq!(format!("{}", EdgeType::Imports), "Imports");
}

#[test]
fn test_graph_node_new() {
    let node = GraphNode::new(NodeType::Function, "test_func".to_string(), "/src/lib.rs".to_string());
    assert_eq!(node.name, "test_func");
    assert_eq!(node.path, "/src/lib.rs");
    assert_eq!(node.node_type, NodeType::Function);
    assert!(!node.id.is_empty());
}

#[test]
fn test_graph_node_id_deterministic() {
    let node1 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/lib.rs".to_string());
    let node2 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/lib.rs".to_string());
    assert_eq!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_name() {
    let node1 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/lib.rs".to_string());
    let node2 = GraphNode::new(NodeType::Function, "bar".to_string(), "/src/lib.rs".to_string());
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_path() {
    let node1 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/a.rs".to_string());
    let node2 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/b.rs".to_string());
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_type() {
    let node1 = GraphNode::new(NodeType::Function, "foo".to_string(), "/src/lib.rs".to_string());
    let node2 = GraphNode::new(NodeType::Struct, "foo".to_string(), "/src/lib.rs".to_string());
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_all_node_types() {
    let types = [
        NodeType::File,
        NodeType::Namespace,
        NodeType::Module,
        NodeType::Package,
        NodeType::Class,
        NodeType::Interface,
        NodeType::Struct,
        NodeType::Enum,
        NodeType::Trait,
        NodeType::Function,
        NodeType::Method,
        NodeType::Property,
        NodeType::Variable,
        NodeType::Constant,
        NodeType::HttpRoute,
        NodeType::Topic,
        NodeType::Resource,
        NodeType::Schema,
    ];

    for ntype in types {
        let node = GraphNode::new(ntype.clone(), "test".to_string(), "/test.rs".to_string());
        assert_eq!(node.node_type, ntype);
    }
}

#[test]
fn test_graph_node_with_line_range() {
    let mut node = GraphNode::new(NodeType::Function, "range_fn".to_string(), "/src/lib.rs".to_string());
    node.line_start = Some(10);
    node.line_end = Some(25);
    assert_eq!(node.line_start, Some(10));
    assert_eq!(node.line_end, Some(25));
}

#[test]
fn test_graph_node_with_signature() {
    let mut node = GraphNode::new(NodeType::Function, "sig_fn".to_string(), "/src/lib.rs".to_string());
    node.signature = Some("(a: i32, b: String) -> bool".to_string());
    assert!(node.signature.is_some());
}

#[test]
fn test_graph_node_with_docstring() {
    let mut node = GraphNode::new(NodeType::Function, "doc_fn".to_string(), "/src/lib.rs".to_string());
    node.docstring = Some("This is a documentation string".to_string());
    assert!(node.docstring.is_some());
}

#[test]
fn test_graph_node_with_metadata() {
    let mut node = GraphNode::new(NodeType::Function, "meta_fn".to_string(), "/src/lib.rs".to_string());
    node.fan_in = Some(5);
    node.fan_out = Some(10);
    node.anchor_score = Some(0.75);
    node.depth_from_main = Some(3);
    node.co_change_count = Some(7);
    node.is_deprecated = true;
    assert_eq!(node.fan_in, Some(5));
    assert_eq!(node.fan_out, Some(10));
    assert_eq!(node.anchor_score, Some(0.75));
    assert_eq!(node.depth_from_main, Some(3));
    assert_eq!(node.co_change_count, Some(7));
    assert!(node.is_deprecated);
}

#[test]
fn test_graph_node_clone() {
    let node = GraphNode::new(NodeType::Function, "clone_fn".to_string(), "/src/lib.rs".to_string());
    let cloned = node.clone();
    assert_eq!(cloned.id, node.id);
    assert_eq!(cloned.name, node.name);
    assert_eq!(cloned.node_type, node.node_type);
}

#[test]
fn test_graph_edge_new() {
    let edge = GraphEdge::new(EdgeType::Calls, "node_a".to_string(), "node_b".to_string());
    assert_eq!(edge.source_id, "node_a");
    assert_eq!(edge.target_id, "node_b");
    assert_eq!(edge.edge_type, EdgeType::Calls);
}

#[test]
fn test_graph_edge_all_types() {
    let types = [
        EdgeType::Contains,
        EdgeType::Calls,
        EdgeType::Uses,
        EdgeType::Implements,
        EdgeType::Imports,
        EdgeType::CoChangedWith,
        EdgeType::Pattern,
        EdgeType::CallsHttp,
        EdgeType::Produces,
        EdgeType::Consumes,
        EdgeType::DeployedTo,
    ];

    for edge_type in types {
        let edge = GraphEdge::new(edge_type.clone(), "src".to_string(), "tgt".to_string());
        assert_eq!(edge.edge_type, edge_type);
    }
}

#[test]
fn test_graph_edge_clone() {
    let edge = GraphEdge::new(EdgeType::Calls, "src".to_string(), "tgt".to_string());
    let cloned = edge.clone();
    assert_eq!(cloned.source_id, edge.source_id);
    assert_eq!(cloned.target_id, edge.target_id);
    assert_eq!(cloned.edge_type, edge.edge_type);
}

#[test]
fn test_graph_node_serialize() {
    let node = GraphNode::new(NodeType::Function, "ser_fn".to_string(), "/src/lib.rs".to_string());
    let json = serde_json::to_string(&node).unwrap();
    assert!(json.contains("ser_fn"));
    assert!(json.contains("Function"));
}

#[test]
fn test_graph_node_deserialize() {
    let json = r#"{"id":"test-id","node_type":"Function","name":"deser_fn","path":"/src/lib.rs","line_start":null,"line_end":null,"signature":null,"docstring":null,"embedding":null,"fan_in":null,"fan_out":null,"anchor_score":null,"depth_from_main":null,"co_change_count":null,"is_deprecated":false,"last_lsp_sync":null,"last_git_sync":null,"commit_hash":null,"is_hydrated":true}"#;
    let node: GraphNode = serde_json::from_str(json).unwrap();
    assert_eq!(node.name, "deser_fn");
    assert_eq!(node.node_type, NodeType::Function);
}

#[test]
fn test_graph_edge_serialize() {
    let edge = GraphEdge::new(EdgeType::Calls, "src_id".to_string(), "tgt_id".to_string());
    let json = serde_json::to_string(&edge).unwrap();
    assert!(json.contains("src_id"));
    assert!(json.contains("tgt_id"));
    assert!(json.contains("Calls"));
}

#[test]
fn test_graph_edge_deserialize() {
    let json = r#"{"source_id":"a","target_id":"b","edge_type":"Imports","metadata":{}}"#;
    let edge: GraphEdge = serde_json::from_str(json).unwrap();
    assert_eq!(edge.source_id, "a");
    assert_eq!(edge.target_id, "b");
    assert_eq!(edge.edge_type, EdgeType::Imports);
}

#[test]
fn test_node_type_equality() {
    assert_eq!(NodeType::Function, NodeType::Function);
    assert_ne!(NodeType::Function, NodeType::Struct);
}

#[test]
fn test_edge_type_equality() {
    assert_eq!(EdgeType::Calls, EdgeType::Calls);
    assert_ne!(EdgeType::Calls, EdgeType::Contains);
}