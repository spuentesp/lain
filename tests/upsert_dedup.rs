use lain::graph::GraphDatabase;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

#[test]
fn upsert_edge_is_idempotent() {
    let db = GraphDatabase::new(&std::env::temp_dir().join("test_upsert_dedup.bin")).unwrap();
    let a = GraphNode::new(NodeType::Function, "a".into(), "/src/a.rs".into());
    let b = GraphNode::new(NodeType::Function, "b".into(), "/src/b.rs".into());
    db.upsert_node(a.clone()).unwrap();
    db.upsert_node(b.clone()).unwrap();
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();
    db.upsert_edge(GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone())).unwrap();
    assert_eq!(db.all_edges().len(), 1, "same edge inserted twice must collapse to one");
}
