use crate::error::LainError;
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use std::ops::Range;
use std::path::Path;

/// On-disk envelope for `federated_graph.bin`: the `LNF2` magic followed
/// by a little-endian `u32` schema version. Anything else — headerless
/// bytes, an unknown magic, a version mismatch, or a corrupt body under a
/// valid header — is rejected with `LainError::FederationSchemaMismatch`
/// rather than loaded.
pub const FEDERATION_GRAPH_MAGIC: &[u8] = b"LNF2";
pub const FEDERATION_GRAPH_VERSION: u32 = 2;
pub const FEDERATION_GRAPH_HEADER_LEN: usize = FEDERATION_GRAPH_MAGIC.len() + 4;

/// Sibling file holding the validated payload (everything after the
/// envelope header); this is what `GraphDatabase` is opened against, so
/// reloads see only bytes that passed `validate_persisted_payload`.
fn payload_path_for(bin_path: &Path) -> std::path::PathBuf {
    let mut p = bin_path.as_os_str().to_owned();
    p.push(".payload");
    std::path::PathBuf::from(p)
}

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
    fn subgraph_around(
        &self,
        center: &str,
        radius: u32,
    ) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
}

pub struct PetgraphBackend {
    db: GraphDatabase,
    index: DashMap<String, GlobalId>,
    bin_path: PathBuf,
    payload_path: PathBuf,
}

impl PetgraphBackend {
    pub fn new(data_dir: &Path) -> Result<Self, LainError> {
        let bin_path = data_dir.join("federated_graph.bin");
        let payload_path = payload_path_for(&bin_path);

        if bin_path.exists() {
            let bytes = std::fs::read(&bin_path)?;
            // A zero-byte file is *truncated*, not "no graph yet" — the
            // envelope (magic + version, 8 bytes) is mandatory, and any
            // shorter file means a torn write or a hand-crafted sentinel.
            // Treating it as a valid no-op (the previous behaviour) lets a
            // `GraphDatabase::new` soft-fall-through mask the corruption.
            if bytes.is_empty()
                || bytes.len() < FEDERATION_GRAPH_HEADER_LEN
                || &bytes[..FEDERATION_GRAPH_MAGIC.len()] != FEDERATION_GRAPH_MAGIC
            {
                return Err(LainError::FederationSchemaMismatch {
                    found: 0,
                    required: FEDERATION_GRAPH_VERSION,
                });
            } else {
                let found = u32::from_le_bytes([
                    bytes[FEDERATION_GRAPH_MAGIC.len()],
                    bytes[FEDERATION_GRAPH_MAGIC.len() + 1],
                    bytes[FEDERATION_GRAPH_MAGIC.len() + 2],
                    bytes[FEDERATION_GRAPH_MAGIC.len() + 3],
                ]);
                if found != FEDERATION_GRAPH_VERSION {
                    return Err(LainError::FederationSchemaMismatch {
                        found,
                        required: FEDERATION_GRAPH_VERSION,
                    });
                }
                let payload = &bytes[FEDERATION_GRAPH_HEADER_LEN..];
                GraphDatabase::validate_persisted_payload(payload).map_err(|error| {
                    tracing::warn!(
                        "Rejecting corrupt federation graph payload at {}: {error}. Remove it and re-run to rebuild.",
                        bin_path.display()
                    );
                    LainError::FederationPayloadCorrupt {
                        reason: error.to_string(),
                    }
                })?;
                std::fs::write(&payload_path, payload)?;
            }
        }

        let db = GraphDatabase::new(&payload_path)?;
        let index = DashMap::new();
        for node in db.get_all_nodes() {
            if let Ok(global_id) = GlobalId::parse(&node.id) {
                index.insert(node.id, global_id);
            }
        }
        Ok(Self {
            db,
            index,
            bin_path,
            payload_path,
        })
    }

    /// Save the federated graph to disk, prepending the schema envelope
    /// (magic + version) before the bincode payload so the canonical
    /// file is always self-describing on the next load.
    fn save(&self) -> Result<(), LainError> {
        self.db.save_to_disk_sync()?;
        let payload = std::fs::read(&self.payload_path)?;
        let mut with_header = Vec::with_capacity(FEDERATION_GRAPH_HEADER_LEN + payload.len());
        with_header.extend_from_slice(FEDERATION_GRAPH_MAGIC);
        with_header.extend_from_slice(&FEDERATION_GRAPH_VERSION.to_le_bytes());
        with_header.extend_from_slice(&payload);
        std::fs::write(&self.bin_path, &with_header)?;
        Ok(())
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

    fn subgraph_around(
        &self,
        center: &str,
        radius: u32,
    ) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        self.db.subgraph_around(center, radius)
    }

    fn node_count(&self) -> usize {
        self.db.node_count()
    }

    fn edge_count(&self) -> usize {
        self.db.edge_count()
    }
}
