//! Contract tests for GraphBackend. The same tests will run against PetgraphBackend
//! in Task 7. Here we use a simple in-memory HashMap impl to define the contract.
use crate::error::LainError;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::RwLock;

pub struct HashMapBackend {
    nodes: RwLock<HashMap<String, GraphNode>>,
    edges: RwLock<Vec<GraphEdge>>,
}

impl HashMapBackend {
    pub fn new() -> Self {
        Self { nodes: RwLock::new(HashMap::new()), edges: RwLock::new(Vec::new()) }
    }
}

impl GraphBackend for HashMapBackend {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        self.nodes.write().unwrap().insert(node.id.clone(), node);
        Ok(())
    }
    fn upsert_node_global(
        &self,
        global_id: &str,
        kind: NodeType,
        path: &str,
        name: &str,
    ) -> Result<(), LainError> {
        let mut node = GraphNode::new(kind, name.to_string(), path.to_string());
        node.id = global_id.to_string();
        self.upsert_node(node)
    }
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        self.edges.write().unwrap().push(edge);
        Ok(())
    }
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        Ok(self.nodes.read().unwrap().get(global_id).cloned())
    }
    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(self
            .nodes
            .read()
            .unwrap()
            .values()
            .filter(|n| n.name == name)
            .cloned()
            .collect())
    }
    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError> {
        Ok(self.nodes.read().unwrap().values().cloned().collect())
    }
    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self.edges.read().unwrap().clone())
    }
    fn traverse(&self, _start: &str, _edge: EdgeType, _depth: Range<u32>) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn find_path(&self, _from: &str, _to: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn subgraph_around(&self, _center: &str, _radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        Ok(Vec::new())
    }
    fn node_count(&self) -> usize { self.nodes.read().unwrap().len() }
    fn edge_count(&self) -> usize { self.edges.read().unwrap().len() }
}

#[test]
fn contract_upsert_node_roundtrips() {
    let b = HashMapBackend::new();
    let n = GraphNode::new(NodeType::Function, "f".into(), "src/lib.rs".into());
    b.upsert_node(n.clone()).unwrap();
    assert_eq!(b.node_count(), 1);
    assert_eq!(b.get_node(&n.id).unwrap().unwrap().id, n.id);
}

#[test]
fn contract_upsert_edge_increments_count() {
    let b = HashMapBackend::new();
    let n1 = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let n2 = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    b.upsert_node(n1.clone()).unwrap();
    b.upsert_node(n2.clone()).unwrap();
    b.upsert_edge(GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone())).unwrap();
    assert_eq!(b.node_count(), 2);
    assert_eq!(b.edge_count(), 1);
}

#[test]
fn contract_get_missing_returns_none() {
    let b = HashMapBackend::new();
    assert!(b.get_node("nope").unwrap().is_none());
}

#[test]
fn petgraph_backend_persists_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.upsert_node_global(
        "repo1:Function:src/lib.rs:f",
        NodeType::Function,
        "src/lib.rs",
        "f",
    )
    .unwrap();
    assert_eq!(b.node_count(), 1);
    drop(b);

    let b2 = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!(b2.node_count(), 1);
    assert!(b2
        .get_node("repo1:Function:src/lib.rs:f")
        .unwrap()
        .is_some());
}