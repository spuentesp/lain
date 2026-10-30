use crate::error::LainError;
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// On-disk envelope for `federated_graph.bin`: the `LNF2` magic, then a
/// little-endian `u32` schema version, then the payload. There is no
/// headerless legacy path — a file without the magic is rejected as a
/// schema mismatch (`LainError::FederationSchemaMismatch`) rather than
/// loaded as v1, a version mismatch is the same error, and a corrupt
/// body under a valid header is `LainError::FederationPayloadCorrupt`.
///
/// Schema v3 adds contract-federation node/edge variants and
/// `GraphNode.contract` / `GraphNode.entry` / `GraphEdge.site` /
/// `GraphEdge.detail` fields. Per the repo's federation-schema policy
/// (`AGENTS.md`), the version bump ships in the same commit as the
/// layout change. Old v2 files are refused at load — recovery is
/// `lain reindex`.
pub const FEDERATION_GRAPH_MAGIC: &[u8] = b"LNF2";
pub const FEDERATION_GRAPH_VERSION: u32 = 4;
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
    /// Open a bulk write section. Until the matching [`Self::end_bulk`],
    /// writes do not each rewrite the backing file; `end_bulk` writes it
    /// once. Only a full reindex should use this: a crash inside the section
    /// leaves the previous file in place, and `lain reindex` rebuilds it.
    /// No-op for backends without a backing file.
    fn begin_bulk(&self) {}

    /// Close a bulk section and write once if anything changed inside it.
    fn end_bulk(&self) -> Result<(), LainError> {
        Ok(())
    }

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
    /// Remove edges matching `(source_id, target_id, edge_type)`. Endpoints
    /// stay. Companion to [`Self::remove_nodes`] for cases where the caller
    /// and callee are still indexed but the edge between them should be
    /// retracted (the federation's reconciliation pass — see
    /// `FederatedIndex::project_edges`). Dedups the input; returns the
    /// number of edges actually removed from the backend.
    fn remove_edges(&self, edges: &[GraphEdge]) -> Result<usize, LainError>;
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>;
    /// Default: `get_node(gid)?.is_some()`. Backends may override
    /// when they have a cheaper existence check (e.g. a `DashMap`
    /// probe that avoids deserializing the full node). Test-only
    /// backends like `HashMapBackend` rely on the default.
    fn has_node(&self, global_id: &str) -> Result<bool, LainError> {
        Ok(self.get_node(global_id)?.is_some())
    }
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
    /// Edges of `edge_type` that end at `target_id`, in no particular order.
    ///
    /// The per-node lookup `all_edges` is the wrong tool for: callers that
    /// ask this once per consumer or per visited node (`get_service`'s
    /// `used_by` walk) must not clone and scan the whole edge set each
    /// time. The default is that scan, so a simple backend stays correct;
    /// [`PetgraphBackend`] overrides it with an index lookup.
    fn edges_to(&self, target_id: &str, edge_type: EdgeType) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .all_edges()?
            .into_iter()
            .filter(|e| e.edge_type == edge_type && e.target_id == target_id)
            .collect())
    }
    /// Edges of `edge_type` that start at `source_id`, in no particular order.
    /// The by-source twin of [`Self::edges_to`], with the same default scan
    /// and the same indexed petgraph override.
    fn edges_from(
        &self,
        source_id: &str,
        edge_type: EdgeType,
    ) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .all_edges()?
            .into_iter()
            .filter(|e| e.edge_type == edge_type && e.source_id == source_id)
            .collect())
    }
    /// Call `f` on every edge by reference, in the order `all_edges` returns
    /// them. For callers that filter the edge set and keep a few edges:
    /// `all_edges` clones all of them (about 44 MB plus strings on Saleor)
    /// just to throw most away. The default clones (a simple backend stays
    /// correct); [`PetgraphBackend`] overrides it to walk under its read
    /// guard with no clone. `f` must not call back into the backend: the
    /// guard is held while it runs.
    fn visit_edges(&self, f: &mut dyn FnMut(&GraphEdge)) -> Result<(), LainError> {
        for e in self.all_edges()? {
            f(&e);
        }
        Ok(())
    }
    /// [`Self::visit_edges`] for nodes, in the order `list_nodes` returns
    /// them. Same contract: `f` must not call back into the backend.
    fn visit_nodes(&self, f: &mut dyn FnMut(&GraphNode)) -> Result<(), LainError> {
        for n in self.list_nodes()? {
            f(&n);
        }
        Ok(())
    }
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
    /// F2 typed impact traversal (§5.2). BFS from every node in
    /// `starts` at distance 0, following edges per the propagation
    /// table (see [`impact_propagation`]). Each node is visited once
    /// at its shortest distance; tie-breaks go to the predecessor
    /// with higher `min_confidence` then lexicographically smaller
    /// `GlobalId` so output is deterministic. Edges below
    /// `min_confidence` are not followed. A path is emitted for every
    /// visited node that has no unvisited successor or sits at
    /// `depth`; `cap` limits emitted paths and `truncated` is set
    /// when `cap` or `depth` cut the search. Output paths are sorted
    /// by `min_confidence` desc, then length asc, then leaf
    /// `GlobalId`. PR 4 ships the table with only `Calls` returning
    /// `Incoming`; later PRs switch their own edge types on.
    fn traverse_impact(
        &self,
        starts: &[&str],
        depth: u32,
        cap: usize,
        min_confidence: f32,
    ) -> Result<ImpactResult, LainError>;
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
    /// When `true`, [`Self::save`] is a no-op and no file under
    /// `data_dir` is ever written. Snapshot federations (`§8.5`) live
    /// here: they need a real `GraphBackend` to walk, but every
    /// snapshot is built from on-disk cache entries and never persists
    /// itself.
    ephemeral: bool,
    /// Open [`GraphBackend::begin_bulk`] sections. While non-zero, `save`
    /// only marks the file dirty instead of rewriting the whole graph.
    bulk_depth: AtomicUsize,
    /// A write happened inside a bulk section; `end_bulk` saves once.
    bulk_dirty: AtomicBool,
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
                        "Rejecting corrupt federation graph payload at {}: {error}. Run `lain reindex` to rebuild.",
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
            ephemeral: false,
            bulk_depth: AtomicUsize::new(0),
            bulk_dirty: AtomicBool::new(false),
        })
    }

    /// Build a `PetgraphBackend` that never writes to disk (`§8.5`).
    /// The wrapped `GraphDatabase` is a fresh in-memory graph; the
    /// `save()` override skips every disk write.
    ///
    /// `data_dir` is used only as a diagnostic identifier — no file is
    /// ever created or read at that path. The caller typically passes
    /// `data_dir/<snapshot_id>` so logs identify the snapshot.
    pub fn ephemeral(data_dir: &Path) -> Self {
        let db = crate::graph::GraphDatabase::empty_writable();
        Self {
            db,
            index: DashMap::new(),
            bin_path: data_dir.to_path_buf(),
            ephemeral: true,
            bulk_depth: AtomicUsize::new(0),
            bulk_dirty: AtomicBool::new(false),
        }
    }

    /// True iff [`Self::save`] is a no-op. Snapshot federations use
    /// this so callers and tests can assert "no disk write happened".
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// Save the federated graph to disk, prepending the schema envelope
    /// (magic + version) before the bincode payload so the canonical
    /// file is always self-describing on the next load. No-op for
    /// ephemeral backends (`§8.5` snapshot federations).
    fn save(&self) -> Result<(), LainError> {
        if self.ephemeral {
            return Ok(());
        }
        if self.bulk_depth.load(Ordering::Acquire) > 0 {
            self.bulk_dirty.store(true, Ordering::Release);
            return Ok(());
        }
        self.write_file()
    }

    /// Unconditional checkpoint of the whole federated graph to `bin_path`.
    fn write_file(&self) -> Result<(), LainError> {
        // The database streams its encode to the `.payload` file and, in the
        // same pass, to `federated_graph.bin` behind the schema envelope: the
        // two files are byte-identical apart from that header, and neither
        // the payload nor a headered copy of it is ever held in memory.
        let mut header = Vec::with_capacity(FEDERATION_GRAPH_HEADER_LEN);
        header.extend_from_slice(FEDERATION_GRAPH_MAGIC);
        header.extend_from_slice(&FEDERATION_GRAPH_VERSION.to_le_bytes());
        self.db
            .save_to_disk_sync_mirrored(Some(crate::graph::PayloadMirror {
                path: &self.bin_path,
                prefix: &header,
            }))
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
        self.save()
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
    fn begin_bulk(&self) {
        self.bulk_depth.fetch_add(1, Ordering::AcqRel);
    }

    fn end_bulk(&self) -> Result<(), LainError> {
        let previous = self.bulk_depth.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "end_bulk without begin_bulk");
        if previous == 1 && self.bulk_dirty.swap(false, Ordering::AcqRel) {
            return self.write_file();
        }
        Ok(())
    }

    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        let global_id = GlobalId::parse(&node.id)?;
        self.db.upsert_node(node.clone())?;
        self.index.insert(node.id, global_id);
        self.save()
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
        self.save()
    }

    fn upsert_edges_batch(&self, edges: &[GraphEdge]) -> Result<(), LainError> {
        if edges.is_empty() {
            return Ok(());
        }
        self.db.upsert_edges_batch_strict(edges)?;
        self.save()
    }

    fn upsert_nodes_batch(&self, nodes: &[GraphNode]) -> Result<(), LainError> {
        if nodes.is_empty() {
            return Ok(());
        }
        let mut parsed_ids: Vec<(String, GlobalId)> = Vec::with_capacity(nodes.len());
        for node in nodes {
            let global_id = GlobalId::parse(&node.id)?;
            parsed_ids.push((node.id.clone(), global_id));
        }
        self.db.insert_nodes_batch(nodes)?;
        for (id, global_id) in parsed_ids {
            self.index.insert(id, global_id);
        }
        self.save()
    }

    fn remove_nodes(&self, global_ids: &[String]) -> Result<usize, LainError> {
        let removed = self.db.remove_nodes_by_ids(global_ids)?;
        for id in global_ids {
            self.index.remove(id);
        }
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    fn remove_edges(&self, edges: &[GraphEdge]) -> Result<usize, LainError> {
        let removed = self.db.remove_edges(edges)?;
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        self.db.get_node_by_id(global_id)
    }

    fn has_node(&self, global_id: &str) -> Result<bool, LainError> {
        Ok(self.db.has_node(global_id))
    }

    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(self.db.find_all_nodes_by_name(name))
    }

    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError> {
        Ok(self.db.get_all_nodes())
    }

    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self.db.all_edges())
    }

    fn edges_to(&self, target_id: &str, edge_type: EdgeType) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .db
            .get_edges_to(target_id)?
            .into_iter()
            .filter(|e| e.edge_type == edge_type)
            .collect())
    }

    fn edges_from(
        &self,
        source_id: &str,
        edge_type: EdgeType,
    ) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .db
            .get_edges_from(source_id)?
            .into_iter()
            .filter(|e| e.edge_type == edge_type)
            .collect())
    }

    fn visit_edges(&self, f: &mut dyn FnMut(&GraphEdge)) -> Result<(), LainError> {
        let graph = self.db.graph_ref_for_read();
        for e in graph.edge_weights() {
            f(e);
        }
        Ok(())
    }

    fn visit_nodes(&self, f: &mut dyn FnMut(&GraphNode)) -> Result<(), LainError> {
        let graph = self.db.graph_ref_for_read();
        for n in graph.node_weights() {
            f(n);
        }
        Ok(())
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

    fn traverse_impact(
        &self,
        starts: &[&str],
        depth: u32,
        cap: usize,
        min_confidence: f32,
    ) -> Result<ImpactResult, LainError> {
        use petgraph::stable_graph::{EdgeIndex, NodeIndex};

        // Walk the live graph under one read guard, on petgraph indices.
        // The old version cloned every node into a `HashMap` and every
        // qualifying edge into per-target lists on EVERY call (hundreds of
        // MB on Saleor) to walk a small part of it. Here the adjacency is
        // `(confidence, neighbour, edge)` index triples, ids stay borrowed
        // from the graph, and only the nodes and edges on an emitted path
        // are cloned. The guard is held for the index build plus the BFS
        // (milliseconds), not across anything that re-enters the backend.
        let graph = self.db.graph_ref_for_read();
        let by_id: std::collections::HashMap<&str, NodeIndex> = graph
            .node_indices()
            .map(|i| (graph[i].id.as_str(), i))
            .collect();

        // Edge indices ascend in the order `all_edges` returned them, so
        // ties below keep their old, deterministic order.
        let mut by_target: std::collections::HashMap<NodeIndex, Vec<(f32, NodeIndex, EdgeIndex)>> =
            std::collections::HashMap::new();
        for ei in graph.edge_indices() {
            let Some((source, target)) = graph.edge_endpoints(ei) else {
                continue;
            };
            let e = &graph[ei];
            // Confidence must be evidence-backed. Explicit weights win;
            // otherwise derive it from provenance. A legacy edge with
            // neither is unknown (0.0), not implicitly verified static data.
            let conf = e.weight.unwrap_or(match e.provenance.as_ref() {
                Some(crate::schema::EdgeProvenance::Static { .. })
                | Some(crate::schema::EdgeProvenance::Confirmed { .. }) => 1.0,
                Some(crate::schema::EdgeProvenance::Heuristic { confidence, .. }) => *confidence,
                Some(crate::schema::EdgeProvenance::Runtime { .. }) => 0.9,
                None => 0.0,
            });
            if conf < min_confidence {
                continue;
            }
            match impact_propagation(&e.edge_type) {
                Propagation::Incoming => by_target
                    .entry(target)
                    .or_default()
                    .push((conf, source, ei)),
                Propagation::Outgoing => by_target
                    .entry(source)
                    .or_default()
                    .push((conf, target, ei)),
                Propagation::Stop => {}
            }
        }
        // Stable predecessor selection: each entry holds the
        // candidate's confidence and neighbour; the BFS picks the
        // highest-confidence, then lexicographically smallest id.
        for entries in by_target.values_mut() {
            entries.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| graph[a.1].id.cmp(&graph[b.1].id))
            });
        }

        // BFS from every start at distance 0. The brief: "an endpoint
        // has one start per provider node" — every entry of `starts`
        // is seeded independently.
        let mut visited: std::collections::HashSet<NodeIndex> = std::collections::HashSet::new();
        let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
        for s in starts {
            if let Some(&ix) = by_id.get(*s) {
                if visited.insert(ix) {
                    queue.push_back((ix, 0));
                }
            }
        }

        let mut paths: Vec<ImpactPath> = Vec::new();
        let mut truncated = false;

        // For each visited node, remember (predecessor, incoming edge,
        // min_confidence so far). Used to reconstruct paths when a leaf
        // is reached. The first predecessor to claim a node wins
        // (already ordered by the sort above), so output is
        // deterministic.
        let mut pred: std::collections::HashMap<NodeIndex, (NodeIndex, EdgeIndex, f32)> =
            std::collections::HashMap::new();

        while let Some((current, current_depth)) = queue.pop_front() {
            let successors = by_target.get(&current);
            let has_unvisited_successor = successors
                .map(|v| v.iter().any(|(_, id, _)| !visited.contains(id)))
                .unwrap_or(false);

            // §5.2 emission rule: emit a path when (a) at `depth` or
            // (b) no unvisited successor. The seed itself, when it
            // has no edges, also satisfies (b).
            let at_depth = current_depth >= depth;
            if at_depth || !has_unvisited_successor {
                if paths.len() >= cap {
                    truncated = true;
                } else {
                    paths.push(reconstruct_path(&graph, current, &pred));
                }
            }

            if at_depth {
                truncated = true;
                continue;
            }

            if let Some(successors) = successors {
                for (conf, next, edge) in successors {
                    if !visited.insert(*next) {
                        continue;
                    }
                    // Edge confidence already passed the floor at
                    // indexing time. The `min_confidence` carried on
                    // the path is the minimum of every edge on it,
                    // updated here as we descend.
                    let new_min_conf = if let Some((_, _, prev_conf)) = pred.get(&current) {
                        conf.min(*prev_conf)
                    } else {
                        *conf
                    };
                    pred.insert(*next, (current, *edge, new_min_conf));
                    queue.push_back((*next, current_depth + 1));
                }
            }
        }

        // Sort by min_confidence desc, then length asc, then leaf id.
        paths.sort_by(|a, b| {
            b.min_confidence
                .partial_cmp(&a.min_confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.hops.len().cmp(&b.hops.len()))
                .then_with(|| {
                    let a_leaf = a.hops.last().map(|h| h.node.id.as_str()).unwrap_or("");
                    let b_leaf = b.hops.last().map(|h| h.node.id.as_str()).unwrap_or("");
                    a_leaf.cmp(b_leaf)
                })
        });

        Ok(ImpactResult { paths, truncated })
    }
}

fn reconstruct_path(
    graph: &petgraph::stable_graph::StableGraph<GraphNode, GraphEdge>,
    leaf: petgraph::stable_graph::NodeIndex,
    pred: &std::collections::HashMap<
        petgraph::stable_graph::NodeIndex,
        (
            petgraph::stable_graph::NodeIndex,
            petgraph::stable_graph::EdgeIndex,
            f32,
        ),
    >,
) -> ImpactPath {
    // Walk back through `pred` from `leaf` to the seed. Hops are
    // emitted in seed → leaf order so callers can read them as a
    // story: "the seed, reached via edge E1, then node N1, via edge
    // E2, then node N2, …". Only the nodes and edges on this path are
    // cloned.
    let mut rev: Vec<ImpactHop> = Vec::new();
    let mut current = leaf;
    let mut min_conf = 1.0_f32;
    while let Some((prev, edge, conf)) = pred.get(&current) {
        // The hop carries the node we arrived AT (i.e. `current`),
        // not the predecessor.
        rev.push(ImpactHop {
            edge: graph[*edge].clone(),
            node: graph[current].clone(),
        });
        min_conf = min_conf.min(*conf);
        current = *prev;
    }
    if rev.is_empty() {
        // Degenerate path: the leaf IS a seed (or has no predecessors).
        // Emit a single self-loop hop so callers can read the path's
        // node without unwrapping an empty `hops` vec. The edge
        // carries the same id as source and target so it doesn't
        // masquerade as a real edge in any consumer.
        let node = graph[leaf].clone();
        let mut placeholder_edge =
            GraphEdge::new(EdgeType::Calls, node.id.clone(), node.id.clone());
        placeholder_edge.weight = Some(1.0);
        rev.push(ImpactHop {
            edge: placeholder_edge,
            node,
        });
    } else {
        rev.reverse();
    }
    ImpactPath {
        hops: rev,
        min_confidence: if pred.is_empty() { 1.0 } else { min_conf },
    }
}

// ─── F2 typed impact traversal (§5.2) ─────────────────────────────────
//
// `Propagation` decides, for each `EdgeType`, which direction the BFS
// walks from a visited node. `Incoming` follows edges whose target is
// the node (so the BFS ascends to its callers / dependents).
// `Outgoing` follows edges whose source is the node. `Stop` ignores
// the edge type entirely.
//
// The per-PR carve-out is recorded in `.
// The §5.2 code block describes the *final* propagation table; the
// tracker checklist governs what actually ships. PR 4 turned on
// `Calls`. PR 7 turned on `CallsHttp`, `SendsHttp`, `Binds`. PR 8
// (this) turns on `RequestSchema`, `ResponseSchema`, `HasField`.
// Future PRs: 9 adds `ReadsField`, 15 adds `PayloadSchema` /
// `Produces` / `Consumes`. The match is exhaustive: a new
// `EdgeType` variant will fail to compile until a propagation has
// been decided for it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propagation {
    Incoming,
    Outgoing,
    Stop,
}

pub fn impact_propagation(e: &EdgeType) -> Propagation {
    match e {
        EdgeType::Calls
        | EdgeType::CallsHttp
        // SDL provider -> resolver: a resolver change reaches the
        // contract the same way a handler change reaches its route.
        | EdgeType::CallsGraphql
        | EdgeType::SendsHttp
        | EdgeType::Binds
        | EdgeType::RequestSchema
        | EdgeType::ResponseSchema
        | EdgeType::HasField
        | EdgeType::ReadsField
        // Phase D (spec §7): from the table, both reads and writes
        // walk back to the function that touches it — the same
        // direction as `SendsHttp` / `Calls`. An operator asking
        // "what depends on the orders table?" gets the answering
        // function; an operator asking "what does this function
        // touch?" gets the table when they start at the function
        // and follow forward (a separate traversal pass).
        | EdgeType::ReadsTable
        | EdgeType::WritesTable
        // Anchor-miner stage: heuristic `BusTopic` / `RouteMatches`
        // edges (minted by `federation::anchor_miner`, and by the
        // dynamic-dispatch sensor on per-repo graphs) are couplings
        // the impact traversal must surface — they carry their
        // confidence so `min_confidence` filters them and the claims
        // pipeline grades them `needs-investigation`. Stopping here
        // was part of the silence the edge-minting layer exists to
        // fix.
        | EdgeType::BusTopic
        | EdgeType::RouteMatches => Propagation::Incoming,
        EdgeType::Consumes
        | EdgeType::PayloadSchema
        | EdgeType::Produces
        | EdgeType::Contains
        | EdgeType::Imports
        | EdgeType::CoChangedWith
        | EdgeType::Pattern
        | EdgeType::Uses
        | EdgeType::Implements
        | EdgeType::DeployedTo
        | EdgeType::CrossRepoSameSymbol
        | EdgeType::DynamicDispatch
        | EdgeType::RuntimeCall
        | EdgeType::ReadsFrom => Propagation::Stop,
    }
}

/// One edge plus the node that edge reaches. `edge` points at the
/// predecessor of `node`, so a path of `[H1, H2]` reads as "arrive at
/// H1.node via H1.edge, then arrive at H2.node via H2.edge". The
/// last hop's node is the path's leaf.
#[derive(Debug, Clone)]
pub struct ImpactHop {
    pub edge: GraphEdge,
    pub node: GraphNode,
}

/// One leaf-to-seed chain. `hops` is non-empty (the seed itself
/// emits a path with one hop when it has no incoming edges).
/// `min_confidence` is the minimum confidence across every edge on
/// the path; ties between paths are broken by this value first.
#[derive(Debug, Clone)]
pub struct ImpactPath {
    pub hops: Vec<ImpactHop>,
    pub min_confidence: f32,
}

/// Result of a typed impact traversal. `paths` is sorted per §5.2
/// (min_confidence desc, length asc, leaf GlobalId); `truncated` is
/// `true` when `cap` or `depth` cut the search.
#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub paths: Vec<ImpactPath>,
    pub truncated: bool,
}
