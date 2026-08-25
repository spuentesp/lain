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
    /// Bulk-upsert nodes with a single disk save at the end. The
    /// federation's `project_repo` path inserts one node per repo
    /// symbol (~3k+ for the lain repo); calling `upsert_node` per
    /// node would do ~3k disk syncs. This batches the inserts and
    /// saves once. Idempotency is the same as `upsert_node` — the
    /// backend deduplicates on global id.
    fn upsert_nodes_batch(&self, nodes: &[GraphNode]) -> Result<(), LainError>;
    /// Bulk-upsert edges with a single disk save at the end. The
    /// federation's `project_repo` path inserts one edge per intra-repo
    /// edge (~10k+ for the lain repo); calling `upsert_edge` per-edge
    /// would do ~10k disk syncs. This batches the inserts and saves
    /// once. Idempotency is the same as `upsert_edge` — backend
    /// deduplicates on source+target+type.
    fn upsert_edges_batch(&self, edges: &[GraphEdge]) -> Result<(), LainError>;
    /// Remove nodes by global id, with their incident edges.
    ///
    /// `project_repo` upserts a repo's nodes but had no way to retract one, so
    /// the federated view kept every symbol a repo ever had. Deleting a
    /// function left it answering `search_org` forever even after the per-repo
    /// graph had correctly dropped it.
    fn remove_nodes(&self, global_ids: &[String]) -> Result<usize, LainError>;
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>;
    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError>;
    /// Return every node currently in the backend. Used by
    /// `mcp::federation_tools::search_org` as a fallback for nodes inserted
    /// into the backend directly (bypassing `add_repo` / `project_repo`).
    /// Same pattern as `resolve_symbol` falling back to `find_nodes_by_name`.
    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError>;
    /// Return every edge currently in the backend. Used by
    /// `mcp::federation_tools::get_workspace_graph` to build a
    /// cross-repo graph view. Not a hot path (called once per dashboard
    /// render) so the per-call overhead is acceptable.
    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError>;
    /// BFS along edges of `edge` starting at `start`. `direction`
    /// controls whether we follow outgoing edges (the default — "what
    /// does X depend on") or incoming edges ("what depends on X" —
    /// the *blast radius* semantic).
    fn traverse(
        &self,
        start: &str,
        edge: EdgeType,
        depth: Range<u32>,
        direction: petgraph::Direction,
    ) -> Result<Vec<GraphNode>, LainError>;
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
    /// Used by the planned-but-not-yet-implemented
    /// `federation_index_for_test` test fixture to seed a synthetic
    /// federation with `insert_nodes_batch` / `insert_edges_batch`
    /// without paying the per-write `save_to_disk_sync` cost of
    /// `upsert_node_global` / `upsert_edge` — the latter would serialize
    /// 50K writes to disk for a small-fixture perf test.
    ///
    /// The `&GraphDatabase` view is enough for batch inserts: callers cannot
    /// mutate petgraph state outside of the documented batch methods, and
    /// any internal `save_to_disk_sync` they trigger is an explicit choice.
    #[cfg(test)]
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

    fn upsert_edges_batch(&self, edges: &[GraphEdge]) -> Result<(), LainError> {
        if edges.is_empty() {
            return Ok(());
        }
        for edge in edges {
            self.db.upsert_edge(edge.clone())?;
        }
        self.db.save_to_disk_sync()
    }

    fn upsert_nodes_batch(&self, nodes: &[GraphNode]) -> Result<(), LainError> {
        if nodes.is_empty() {
            return Ok(());
        }
        for node in nodes {
            let global_id = GlobalId::parse(&node.id)?;
            self.db.upsert_node(node.clone())?;
            self.index.insert(node.id.clone(), global_id);
        }
        self.db.save_to_disk_sync()
    }

    fn remove_nodes(&self, global_ids: &[String]) -> Result<usize, LainError> {
        let removed = self.db.remove_nodes_by_ids(global_ids)?;
        for id in global_ids {
            self.index.remove(id);
        }
        if removed > 0 {
            self.db.save_to_disk_sync()?;
        }
        Ok(removed)
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

    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self.db.all_edges())
    }

    fn traverse(
        &self,
        start: &str,
        edge: EdgeType,
        depth: Range<u32>,
        direction: petgraph::Direction,
    ) -> Result<Vec<GraphNode>, LainError> {
        self.db.traverse(start, edge, depth, direction)
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