use crate::error::LainError;
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use std::ops::Range;
use std::path::Path;

pub trait GraphBackend: Send + Sync {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError>;
    fn upsert_node_global(
        &self,
        global_id: &str,
        kind: NodeType,
        path: &str,
        name: &str,
    ) -> Result<(), LainError>;
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError>;
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>;
    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError>;
    /// Return every node currently in the backend. Used by
    /// `mcp::federation_tools::search_org` as a fallback for nodes inserted
    /// into the backend directly (bypassing `add_repo` / `project_repo`).
    /// Same pattern as `resolve_symbol` falling back to `find_nodes_by_name`.
    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError>;
    fn traverse(&self, start: &str, edge: EdgeType, depth: Range<u32>) -> Result<Vec<GraphNode>, LainError>;
    fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError>;
    fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
}

pub struct PetgraphBackend {
    db: GraphDatabase,
    index: DashMap<String, GlobalId>,
}

impl PetgraphBackend {
    pub fn new(data_dir: &Path) -> Result<Self, LainError> {
        let db = GraphDatabase::new(&data_dir.join("federated_graph.bin"))?;
        let index = DashMap::new();
        for node in db.get_all_nodes() {
            if let Ok(global_id) = GlobalId::parse(&node.id) {
                index.insert(node.id, global_id);
            }
        }
        Ok(Self { db, index })
    }

    pub fn upsert_node_global(
        &self,
        global_id: &str,
        kind: NodeType,
        path: &str,
        name: &str,
    ) -> Result<(), LainError> {
        let parsed = GlobalId::parse(global_id)?;
        let mut node = GraphNode::new(kind, name.to_string(), path.to_string());
        node.id = global_id.to_string();
        self.db.upsert_node(node)?;
        self.index.insert(global_id.to_string(), parsed);
        self.db.save_to_disk_sync()
    }

    /// Direct access to the underlying `GraphDatabase` for bulk operations.
    /// Used by the `test-utils` federation fixture helper
    /// (`federation_index_for_test`) to seed a synthetic federation with
    /// `insert_nodes_batch` / `insert_edges_batch` without paying the
    /// per-write `save_to_disk_sync` cost of `upsert_node_global` /
    /// `upsert_edge` — the latter would serialize 50K writes to disk for a
    /// small-fixture perf test.
    ///
    /// The `&GraphDatabase` view is enough for batch inserts: callers cannot
    /// mutate petgraph state outside of the documented batch methods, and
    /// any internal `save_to_disk_sync` they trigger is an explicit choice.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn db(&self) -> &crate::graph::GraphDatabase {
        &self.db
    }
}

impl GraphBackend for PetgraphBackend {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        let global_id = GlobalId::parse(&node.id)?;
        self.db.upsert_node(node.clone())?;
        self.index.insert(node.id, global_id);
        self.db.save_to_disk_sync()
    }

    fn upsert_node_global(
        &self,
        global_id: &str,
        kind: NodeType,
        path: &str,
        name: &str,
    ) -> Result<(), LainError> {
        Self::upsert_node_global(self, global_id, kind, path, name)
    }

    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        self.db.upsert_edge(edge)?;
        self.db.save_to_disk_sync()
    }

    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        self.db.get_node_by_id(global_id)
    }

    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(self
            .db
            .get_all_nodes()
            .into_iter()
            .filter(|n| n.name == name)
            .collect())
    }

    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError> {
        Ok(self.db.get_all_nodes())
    }

    fn traverse(&self, start: &str, edge: EdgeType, depth: Range<u32>) -> Result<Vec<GraphNode>, LainError> {
        self.db.traverse(start, edge, depth)
    }

    fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError> {
        self.db.find_path(from, to)
    }

    fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        self.db.subgraph_around(center, radius)
    }

    fn node_count(&self) -> usize {
        self.db.node_count()
    }

    fn edge_count(&self) -> usize {
        self.db.edge_count()
    }
}