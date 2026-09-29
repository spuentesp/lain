use crate::error::LainError;
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// On-disk envelope for `federated_graph.bin`: the `LNF2` magic followed
/// by a little-endian `u32` schema version. Anything else — headerless
/// bytes, an unknown magic, a version mismatch, or a corrupt body under a
/// valid header — is rejected with `LainError::FederationSchemaMismatch`
/// rather than loaded.
///
/// Schema v3 (PR 3) adds contract-federation node/edge variants and
/// `GraphNode.contract` / `GraphNode.entry` / `GraphEdge.site` /
/// `GraphEdge.detail` fields. Per the repo's federation-schema policy
/// (`AGENTS.md`), the version bump ships in the same commit as the
/// layout change. Old v2 files are refused at load — recovery is
/// `lain reindex`.
pub const FEDERATION_GRAPH_MAGIC: &[u8] = b"LNF2";
pub const FEDERATION_GRAPH_VERSION: u32 = 3;
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
        for edge in edges {
            self.db.upsert_edge(edge.clone())?;
        }
        self.save()
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

    fn traverse_impact(
        &self,
        starts: &[&str],
        depth: u32,
        cap: usize,
        min_confidence: f32,
    ) -> Result<ImpactResult, LainError> {
        // Snapshot the relevant data up front so the BFS walks a
        // borrowed view rather than holding the graph lock across the
        // whole traversal. `all_edges` is `O(E)` per call, but the
        // federation's BFS hot path runs against a snapshot the
        // server already keeps in memory; the per-call overhead is
        // acceptable here, and `MemgraphBackend` (the deferred escape
        // hatch called out in `federation/AGENTS.md`) can override this
        // with index-only walks without changing the signature.
        let mut nodes: std::collections::HashMap<String, GraphNode> = self
            .list_nodes()?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let edges = self.all_edges()?;
        let mut by_target: std::collections::HashMap<String, Vec<(f32, String, GraphEdge)>> =
            std::collections::HashMap::new();
        for e in edges {
            // Treat a missing `weight` as `Some(1.0)` (static / tree-sitter
            // edges) — the brief says edges below `min_confidence` are
            // not followed; absence shouldn't make an edge silently
            // disappear when the caller set a positive floor.
            let conf = e.weight.unwrap_or(1.0);
            if conf < min_confidence {
                continue;
            }
            match impact_propagation(&e.edge_type) {
                Propagation::Incoming => by_target.entry(e.target_id.clone()).or_default().push((
                    conf,
                    e.source_id.clone(),
                    e,
                )),
                Propagation::Outgoing => by_target.entry(e.source_id.clone()).or_default().push((
                    conf,
                    e.target_id.clone(),
                    e,
                )),
                Propagation::Stop => {}
            }
        }
        // Stable predecessor selection: each entry holds the
        // candidate's confidence and source id; the BFS picks the
        // highest-confidence, then lexicographically smallest id.
        for entries in by_target.values_mut() {
            entries.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            });
        }

        // BFS from every start at distance 0. The brief: "an endpoint
        // has one start per provider node" — every entry of `starts`
        // is seeded independently.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        for s in starts {
            if nodes.contains_key(*s) && visited.insert((*s).to_string()) {
                queue.push_back(((*s).to_string(), 0));
            }
        }

        let mut paths: Vec<ImpactPath> = Vec::new();
        let mut truncated = false;

        // For each visited node, remember (predecessor id, incoming
        // edge, min_confidence so far). Used to reconstruct paths
        // when a leaf is reached. The first predecessor to claim a
        // node wins (already ordered by the sort above), so output
        // is deterministic.
        let mut pred: std::collections::HashMap<String, (String, GraphEdge, f32)> =
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
                    paths.push(reconstruct_path(&current, &pred, &mut nodes));
                }
            }

            if at_depth {
                truncated = true;
                continue;
            }

            if let Some(successors) = successors {
                for (conf, next_id, edge) in successors {
                    if !visited.insert(next_id.clone()) {
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
                    pred.insert(
                        next_id.clone(),
                        (current.clone(), edge.clone(), new_min_conf),
                    );
                    queue.push_back((next_id.clone(), current_depth + 1));
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
    leaf: &str,
    pred: &std::collections::HashMap<String, (String, GraphEdge, f32)>,
    nodes: &mut std::collections::HashMap<String, GraphNode>,
) -> ImpactPath {
    // Walk back through `pred` from `leaf` to the seed. Hops are
    // emitted in seed → leaf order so callers can read them as a
    // story: "the seed, reached via edge E1, then node N1, via edge
    // E2, then node N2, …".
    let mut rev: Vec<ImpactHop> = Vec::new();
    let mut current = leaf.to_string();
    let mut min_conf = 1.0_f32;
    while let Some((prev_id, edge, conf)) = pred.get(&current) {
        // The hop carries the node we arrived AT (i.e. `current`),
        // not the predecessor.
        let node = nodes.get(&current).cloned().unwrap_or_else(|| {
            // Defensive: an entry in `pred` whose node vanished
            // between edge indexing and path emission would be a
            // torn-write we cannot recover from. Surface an empty
            // placeholder so the test still parses; the emit path is
            // deterministic and the trace will point at this hop.
            let mut n = GraphNode::new(NodeType::Function, String::new(), String::new());
            n.id = current.clone();
            n
        });
        rev.push(ImpactHop {
            edge: edge.clone(),
            node,
        });
        min_conf = min_conf.min(*conf);
        current = prev_id.clone();
    }
    if rev.is_empty() {
        // Degenerate path: the leaf IS a seed (or has no predecessors).
        // Emit a single self-loop hop so callers can read the path's
        // node without unwrapping an empty `hops` vec. The edge
        // carries the same id as source and target so it doesn't
        // masquerade as a real edge in any consumer.
        let node = nodes.get(leaf).cloned().unwrap_or_else(|| {
            let mut n = GraphNode::new(NodeType::Function, String::new(), String::new());
            n.id = leaf.to_string();
            n
        });
        let mut placeholder_edge =
            GraphEdge::new(EdgeType::Calls, leaf.to_string(), leaf.to_string());
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
// PR 4 ships the table with **only `Calls` returning `Incoming`**
// (the §5.2 code block is the *final* table; the tracker checklist
// "only `Calls` on" governs PR 4's actual state). Every other
// variant — including `Produces`, `CallsHttp`, `SendsHttp`, `Binds`,
// `Consumes`, `ReadsField`, `HasField`, `RequestSchema`,
// `ResponseSchema`, `PayloadSchema` — returns `Stop` for now. Later
// PRs (7 / 8 / 9 / 15 per the tracker) flip their own types on. The
// match is exhaustive: a new `EdgeType` variant will fail to compile
// until a propagation has been decided for it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propagation {
    Incoming,
    Outgoing,
    Stop,
}

pub fn impact_propagation(e: &EdgeType) -> Propagation {
    match e {
        EdgeType::Calls => Propagation::Incoming,
        EdgeType::CallsHttp
        | EdgeType::SendsHttp
        | EdgeType::Binds
        | EdgeType::Consumes
        | EdgeType::ReadsField
        | EdgeType::HasField
        | EdgeType::RequestSchema
        | EdgeType::ResponseSchema
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
        | EdgeType::BusTopic
        | EdgeType::RouteMatches
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
