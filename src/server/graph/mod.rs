//! Stable In-Memory Graph Database using petgraph
//!
//! Uses petgraph's StableGraph for robust graph operations and
//! bincode for high-performance binary persistence. The on-disk
//! representation, version checks, and read-only inspection live
//! in [`persist`].

pub(crate) mod persist;

pub use persist::{inspect_persisted_graph, GraphInspectionError, PATH_FORMAT_VERSION};

use crate::error::LainError;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use dashmap::DashMap;
use parking_lot::RwLock;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeRef, IntoNodeReferences};
use petgraph::Direction;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

/// The canonical graph key for a file: workspace-relative, forward-slashed.
///
/// Every site that mints or looks up a path key goes through this. That is
/// the property the orphan sweep depends on — producer keys (from the
/// scanner) and consumer keys (from `git.get_all_tracked_files`, which
/// returns absolute paths) are only comparable because both sides are
/// reduced here first. Checking for a mismatch defensively would not work:
/// an absolute-vs-relative set difference looks like "every node is an
/// orphan", not like an error.
///
/// Paths outside `workspace` (out-of-tree dependencies surfaced by LSP) are
/// kept in their own string form rather than being forced into a bogus
/// relative path; they are stable, just not workspace-relative.
///
/// The workspace and the path may name the same directory through different
/// spellings — a symlink, like macOS's `/var` → `/private/var` under every
/// temp dir, or a checkout reached through a linked directory. A literal
/// prefix match then failed for every file, so every node id carried an
/// absolute path, and cross-repo edges (keyed by relative path) never
/// matched. The canonical forms are compared only when the literal match
/// fails, so the common case costs nothing extra.
pub fn graph_path(workspace: &Path, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(workspace) {
        return crate::server::path_util::posix_string(rel);
    }
    let canonical_rel = || -> Option<PathBuf> {
        let ws = dunce::canonicalize(workspace).ok()?;
        if let Ok(rel) = path.strip_prefix(&ws) {
            return Some(rel.to_path_buf());
        }
        let p = dunce::canonicalize(path).ok()?;
        p.strip_prefix(&ws).ok().map(Path::to_path_buf)
    };
    match canonical_rel() {
        Some(rel) => crate::server::path_util::posix_string(&rel),
        None => crate::server::path_util::posix_string(path),
    }
}

#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,
    /// Shared with every clone, like `graph`: a clone taken before indexing
    /// (the tool context is one) must see the ids and paths written later,
    /// or lookups by id/path miss nodes the shared `graph` already holds.
    index_map: Arc<DashMap<String, NodeIndex>>,
    path_index: Arc<DashMap<String, Vec<NodeIndex>>>,
    last_commit: Arc<RwLock<Option<String>>>,
    persistence_path: PathBuf,
    /// When true, every public `insert_*` / `set_*` / `save_to_disk` returns
    /// `LainError::Other("graph is read-only")`. Set by `open_read_only`,
    /// used by sidecar processes that subscribe to an owner's overlay
    /// stream and never mutate the static graph on disk.
    read_only: bool,
    /// Namespace used by `insert_co_change_edges` and
    /// `get_co_change_partners` when minting/looking up the File-node
    /// ids the co-change edges reference. Must match the namespace
    /// the File nodes were inserted under, otherwise the edge endpoints
    /// don't resolve in `index_map` and the edges are silently dropped
    /// by `insert_edges_batch`. Defaults to `RepoNamespace::for_test()`
    /// (stable, test-only); production call sites set it to the owning
    /// repo's namespace via [`Self::set_namespace`] before the first
    /// co-change write.
    namespace: RepoNamespace,
    /// Edges the resolve phase wrote whose target id is not local
    /// (wishlist #13 — cross-repo `Calls`). The petgraph cannot store
    /// an edge to a node that isn't in `index_map`, so instead of
    /// silently dropping them — which hid the federation's headline
    /// bug for months — they live here until the federation's
    /// `project_repo` drains them with [`Self::take_pending_external_edges`]
    /// and emits them to the federated backend. The `Arc<Mutex<…>>`
    /// matches the other shared fields so `GraphDatabase::clone` is
    /// still cheap and points at the same accumulator.
    pending_external_edges: Arc<parking_lot::Mutex<Vec<GraphEdge>>>,
}

/// How current the graph is for one file.
///
/// The index is driven by git commits, so a file edited but not yet committed
/// is invisible to it. That is the common case while an agent is working, and
/// the graph cannot detect it from commit history alone — only by comparing the
/// file on disk against when it was last scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The file has not changed since it was indexed.
    Fresh,
    /// The file was modified after it was last scanned. Answers about it may
    /// omit new symbols or still show ones that were removed.
    Dirty { modified_ago: std::time::Duration },
    /// No nodes for this path — never indexed, or not tracked by git.
    Absent,
}

impl Freshness {
    /// One line to prepend to a tool response, or `None` when the file is
    /// current and there is nothing worth saying.
    ///
    /// Scoped to the file backing the answer rather than the whole graph: a
    /// global "N commits behind" banner on every response is noise that trains
    /// the reader to ignore it, while "this file changed 4m ago" is a fact they
    /// can act on.
    pub fn note(&self, path: &str) -> Option<String> {
        match self {
            Freshness::Fresh => None,
            Freshness::Dirty { modified_ago } => {
                let secs = modified_ago.as_secs();
                let ago = if secs < 90 {
                    format!("{secs}s")
                } else if secs < 5400 {
                    format!("{}m", secs / 60)
                } else {
                    format!("{}h", secs / 3600)
                };
                Some(format!(
                    "⚠ {path} was modified {ago} ago, after it was last indexed — \
                     this answer may be missing recent changes."
                ))
            }
            Freshness::Absent => Some(format!(
                "⚠ {path} is not in the graph — it may be untracked by git, or not yet indexed."
            )),
        }
    }
}

impl GraphDatabase {
    pub fn new(memory_path: &Path) -> Result<Self, LainError> {
        let db = Self::empty(memory_path);
        if memory_path.exists() {
            db.load_from_disk()?;
        }
        Ok(db)
    }

    /// An in-memory view for protocol probes; persistence writes are disabled.
    pub fn empty_read_only() -> Self {
        let mut db = Self::empty(Path::new(""));
        db.read_only = true;
        db
    }

    fn empty(memory_path: &Path) -> Self {
        Self {
            graph: Arc::new(RwLock::new(StableGraph::new())),
            index_map: Arc::new(DashMap::new()),
            path_index: Arc::new(DashMap::new()),
            last_commit: Arc::new(RwLock::new(None)),
            persistence_path: memory_path.to_path_buf(),
            read_only: false,
            // Default to the test namespace so existing callers
            // (which all happen to write File nodes under that
            // namespace too) keep working. Production call sites that
            // write File nodes under a per-repo namespace must call
            // `set_namespace` after `new` and before the first
            // `insert_co_change_edges`.
            namespace: RepoNamespace::for_test(),
            pending_external_edges: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }

    /// Set the namespace used by `insert_co_change_edges` and
    /// `get_co_change_partners` when minting/looking up the File-node
    /// ids the co-change edges reference. Must match the namespace
    /// the File nodes were inserted under; see the field's doc
    /// comment for the failure mode when it doesn't.
    pub fn set_namespace(&mut self, namespace: RepoNamespace) {
        self.namespace = namespace;
    }

    /// Current namespace used by co-change id minting. Tests use this
    /// to assert against the same id space the graph will use.
    pub fn namespace(&self) -> &RepoNamespace {
        &self.namespace
    }

    /// Open an existing on-disk graph as immutable.
    ///
    /// Sidecar processes use this to share an owner's static graph without
    /// ever acquiring the workspace write lock. Every mutating method on
    /// the returned handle returns `LainError::Other("graph is read-only")`.
    pub fn open_read_only(memory_path: &Path) -> Result<Self, LainError> {
        let mut g = GraphDatabase::new(memory_path)?;
        g.read_only = true;
        Ok(g)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn check_writable(&self) -> Result<(), LainError> {
        if self.read_only {
            Err(LainError::Other("graph is read-only".into()))
        } else {
            Ok(())
        }
    }

    pub fn insert_node(&self, node: &GraphNode) -> Result<(), LainError> {
        self.upsert_node(node.clone())
    }

    pub fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        if let Some(idx) = self.index_map.get(&node.id).map(|r| *r.value()) {
            let existing_hydrated = graph[idx].is_hydrated;
            if node.is_hydrated || !existing_hydrated {
                graph[idx] = node;
            }
        } else {
            let path = node.path.clone();
            let idx = graph.add_node(node.clone());
            self.index_map.insert(node.id.clone(), idx);
            self.path_index.entry(path).or_default().push(idx);
        }
        Ok(())
    }

    pub fn insert_nodes_batch(&self, new_nodes: &[GraphNode]) -> Result<(), LainError> {
        self.check_writable()?;
        // Phase 1: Collect indices and path entries under graph lock
        let mut graph = self.graph.write();

        // Collect work: also update the indexes inline (still holding
        // the graph write lock). The previous code released the graph
        // lock before inserting into `self.index_map`, which left a
        // window where a concurrent `insert_edges_batch` (looking up
        // the endpoint via `index_map.get(...)`) would find no entry
        // for nodes that were just added to petgraph, silently dropping
        // edges. Doing the DashMap updates under the same write lock
        // is the same approach the comment in the lone-correct version
        // at graph.rs:401-409 describes (kept serialized with the
        // graph write). The DashMap is sharded internally; a single
        // writer thread doesn't contend on its shards.
        let dash_work: Vec<(String, String, NodeIndex)> = new_nodes
            .iter()
            .filter_map(|node| {
                if let Some(idx) = self.index_map.get(&node.id).map(|r| *r.value()) {
                    // Update existing
                    let existing_hydrated = graph[idx].is_hydrated;
                    if node.is_hydrated || !existing_hydrated {
                        graph[idx] = node.clone();
                    }
                    None
                } else {
                    let path = node.path.clone();
                    let idx = graph.add_node(node.clone());
                    // Pre-fix this was deferred to Phase 2 (after
                    // `drop(graph)`), creating the race window. Move
                    // it into Phase 1 — still under the same write
                    // lock as the petgraph insert.
                    self.index_map.insert(node.id.clone(), idx);
                    self.path_index.entry(path.clone()).or_default().push(idx);
                    Some((node.id.clone(), path, idx))
                }
            })
            .collect();

        // Drop the graph lock now that the indexes are in sync with
        // the petgraph inserts.
        drop(graph);

        // Phase 2 (no-op): the previous parallel DashMap update moved
        // into Phase 1 to keep index updates atomic with the graph
        // write. `dash_work` is still returned for any future caller
        // that wants to do post-insert bookkeeping on the assigned
        // indices, but the inserts themselves are already done.
        let _ = dash_work;

        Ok(())
    }

    /// Replace every node recorded under `paths` with `nodes`, so a re-scan
    /// of a file is idempotent instead of additive.
    ///
    /// Without this the graph only ever grows: a symbol deleted from a file,
    /// a file deleted from the repo, or a file moved to a new path all leave
    /// their nodes behind forever. Those orphans are not merely inert — name
    /// resolution can pick one, and since it carries no live edges the caller
    /// gets a confident answer with a stale path and no callers.
    ///
    /// Atomicity: removals and insertions happen inside one graph write lock,
    /// and each path's `path_index` entry is swapped in a single operation
    /// rather than cleared and refilled. A concurrent reader therefore never
    /// observes a file with zero symbols — which matters because lain's whole
    /// premise is several agents querying while one indexes.
    ///
    /// `Namespace` nodes are never removed: their key is a directory shared by
    /// every file beneath it, so scoping them to a file would delete a module
    /// each time any one of its files was scanned.
    ///
    /// Passing a path with no corresponding entries in `nodes` deletes that
    /// path's nodes outright — the deleted-file case.
    ///
    /// Returns the number of nodes removed.
    pub fn replace_nodes_for_paths(
        &self,
        paths: &[String],
        nodes: &[GraphNode],
    ) -> Result<usize, LainError> {
        use std::collections::HashMap as StdHashMap;

        self.check_writable()?;

        // Group incoming nodes by their own path key so each path's index
        // entry can be swapped wholesale below.
        let mut by_path: StdHashMap<&str, Vec<&GraphNode>> = StdHashMap::new();
        for node in nodes {
            by_path.entry(node.path.as_str()).or_default().push(node);
        }

        let mut removed_ids: Vec<String> = Vec::new();
        let mut new_entries: Vec<(String, Vec<NodeIndex>)> = Vec::new();
        // Incoming edges that must survive the replacement.
        //
        // `remove_node` takes every incident edge with it. For the file
        // being re-scanned that is correct — its own outgoing edges are
        // rebuilt from the fresh scan. But *incoming* edges from files
        // that are NOT in this pass are collateral: an incremental
        // re-index only re-resolves refs from the files it scanned, so
        // a caller in an untouched file is never restored and its edge
        // is gone for good. Left unhandled this erodes the graph on
        // every incremental pass — the observed end state was 37 of 335
        // files whose symbols had no edges at all, and functions that
        // were demonstrably called reporting zero callers.
        //
        // Node ids are deterministic (same path + name + kind), so a
        // symbol that still exists after the re-scan comes back under
        // the same id and the edge is still meaningful. A symbol that
        // was genuinely deleted does not come back, and the edge stays
        // dropped — which is also correct.
        let replaced_paths: HashSet<&str> = paths.iter().map(|p| p.as_str()).collect();
        let mut preserved_incoming: Vec<GraphEdge> = Vec::new();
        let mut collateral_edges = 0usize;
        let mut restored_edges = 0usize;

        {
            let mut graph = self.graph.write();

            for path in paths {
                // Drop the old nodes for this path, keeping Namespace nodes.
                let old = self
                    .path_index
                    .get(path)
                    .map(|r| r.value().clone())
                    .unwrap_or_default();
                let mut kept: Vec<NodeIndex> = Vec::new();
                for idx in old {
                    match graph.node_weight(idx) {
                        Some(n) if n.node_type == NodeType::Namespace => {
                            kept.push(idx);
                        }
                        Some(n) => {
                            let id = n.id.clone();
                            collateral_edges += graph.edges(idx).count();
                            // Capture inbound edges from files this pass
                            // is not rebuilding, before they go with the
                            // node. Sources inside `replaced_paths` are
                            // skipped: those files are being re-scanned
                            // and will re-resolve their own edges, so
                            // restoring them here would duplicate.
                            for e in graph.edges_directed(idx, Direction::Incoming) {
                                let src_is_replaced = graph
                                    .node_weight(e.source())
                                    .map(|s| replaced_paths.contains(s.path.as_str()))
                                    .unwrap_or(true);
                                if !src_is_replaced {
                                    preserved_incoming.push(e.weight().clone());
                                }
                            }
                            graph.remove_node(idx); // incident edges go with it
                                                    // Remove the stale id → index entry NOW, not
                                                    // after the replacements are inserted: node ids
                                                    // are deterministic (same path+name → same id),
                                                    // so a deferred removal wipes the *fresh* entry
                                                    // inserted below and every id-keyed lookup
                                                    // (get_edges_to, blast radius) silently returns
                                                    // empty while name-keyed lookups still work.
                            self.index_map.remove(&id);
                            removed_ids.push(id);
                        }
                        // index pointed at a vacated slot; nothing to remove
                        None => {}
                    }
                }

                // Add this path's replacements in the same locked section.
                let mut fresh = kept;
                let mut seen_ids: HashSet<String> = HashSet::new();
                for node in by_path.get(path.as_str()).into_iter().flatten() {
                    // Two source files in the same directory emit the
                    // same `Namespace` node (deterministic id) — without
                    // a guard the second `add_node` creates an orphan
                    // petgraph entry that holds incident edges but is
                    // invisible to the id-keyed index. Keep the first.
                    if !seen_ids.insert(node.id.clone()) {
                        continue;
                    }
                    // A Namespace kept above comes back in the fresh scan
                    // under the same id. Adding it again left two nodes for
                    // one id — edges split between them, and `lain doctor`
                    // rejecting the saved graph ("graph index does not match
                    // its nodes") after every re-index. Refresh it in place.
                    if let Some(existing) = self.index_map.get(&node.id).map(|r| *r.value()) {
                        if let Some(w) = graph.node_weight_mut(existing).filter(|w| w.id == node.id)
                        {
                            *w = (*node).clone();
                            if !fresh.contains(&existing) {
                                fresh.push(existing);
                            }
                            continue;
                        }
                    }
                    let idx = graph.add_node((*node).clone());
                    self.index_map.insert(node.id.clone(), idx);
                    fresh.push(idx);
                }
                new_entries.push((path.clone(), fresh));
            }

            // Swap the indexes while still holding the graph write lock.
            // Doing it after releasing the lock left a window in which
            // `path_index` still pointed at NodeIndex values already removed
            // from the graph, so a concurrent reader resolved every one of
            // them to `None` and concluded the file had no nodes at all. That
            // was observable: two tools in the same batch disagreed about
            // whether a file was in the graph. Readers that consult
            // `path_index` under the graph read lock now see the old pair or
            // the new pair, never a mix.
            // (`index_map` entries for removed ids were already dropped
            // inline above — see the removal loop for why deferring that
            // wipes freshly re-inserted entries for deterministic ids.)
            // Restore the inbound edges whose endpoints both still
            // exist. Done inside the same write lock so no reader ever
            // observes the node back without its callers.
            for edge in &preserved_incoming {
                if let (Some(s), Some(t)) = (
                    self.index_map.get(&edge.source_id).map(|r| *r.value()),
                    self.index_map.get(&edge.target_id).map(|r| *r.value()),
                ) {
                    let already = graph
                        .edges_connecting(s, t)
                        .any(|e| e.weight().edge_type == edge.edge_type);
                    if !already {
                        graph.add_edge(s, t, edge.clone());
                        restored_edges += 1;
                    }
                }
            }

            for (path, indices) in new_entries {
                if indices.is_empty() {
                    self.path_index.remove(&path);
                } else {
                    self.path_index.insert(path, indices);
                }
            }
        }

        if collateral_edges > 0 {
            tracing::debug!(
                "replace_nodes_for_paths: {} path(s), {} node(s) removed, \
                 {collateral_edges} incident edge(s) removed with them, \
                 {restored_edges} inbound edge(s) restored from unchanged files",
                paths.len(),
                removed_ids.len()
            );
        }

        Ok(removed_ids.len())
    }

    /// Remove nodes by id, with their incident edges. Companion to
    /// [`Self::replace_nodes_for_paths`] for callers that key by id rather than
    /// by file — the federated backend rewrites every node to a global id, and
    /// two repos can share a path, so path is not a usable key there.
    pub fn remove_nodes_by_ids(&self, ids: &[String]) -> Result<usize, LainError> {
        self.check_writable()?;

        let mut removed = 0usize;
        let mut cleared_paths: Vec<(String, NodeIndex)> = Vec::new();
        {
            let mut graph = self.graph.write();
            for id in ids {
                let Some(idx) = self.index_map.get(id).map(|r| *r.value()) else {
                    continue;
                };
                if let Some(node) = graph.node_weight(idx) {
                    cleared_paths.push((node.path.clone(), idx));
                }
                if graph.remove_node(idx).is_some() {
                    removed += 1;
                }
            }
        }
        for id in ids {
            self.index_map.remove(id);
        }
        // Keep path_index consistent with the graph, or later lookups resolve
        // through a vacated slot.
        for (path, idx) in cleared_paths {
            let now_empty = if let Some(mut entry) = self.path_index.get_mut(&path) {
                entry.retain(|i| *i != idx);
                entry.is_empty()
            } else {
                false
            };
            if now_empty {
                self.path_index.remove(&path);
            }
        }
        Ok(removed)
    }

    /// How current the graph is for `path` (a graph key, i.e. workspace-relative).
    ///
    /// Uses `last_lsp_sync`, the wall-clock second at which the scan read the
    /// file, which every node already carries — so this needs no extra state.
    /// A file whose mtime is newer than that was edited after being scanned.
    ///
    /// Both sides are whole seconds, so an edit landing in the same second as
    /// the scan reads as `Fresh`. That is an acceptable miss for a hint.
    pub fn freshness(&self, workspace: &Path, path: &str) -> Freshness {
        // Hold the graph lock across the `path_index` read: the writer updates
        // both under this same lock, so this observes a consistent pair rather
        // than an index that has outlived the nodes it points at.
        let last_scan = {
            let graph = self.graph.read();
            let Some(indices) = self.path_index.get(path).map(|r| r.value().clone()) else {
                return Freshness::Absent;
            };
            indices
                .iter()
                .filter_map(|idx| graph.node_weight(*idx))
                .filter_map(|n| n.last_lsp_sync)
                .max()
        };
        let Some(last_scan) = last_scan else {
            return Freshness::Absent;
        };

        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            workspace.join(path)
        };
        let Ok(mtime) = std::fs::metadata(&resolved).and_then(|m| m.modified()) else {
            // Gone from disk. The orphan sweep reclaims it on the next complete
            // pass; until then say nothing rather than guess.
            return Freshness::Fresh;
        };
        let mtime_secs = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        if mtime_secs > last_scan {
            Freshness::Dirty {
                modified_ago: std::time::SystemTime::now()
                    .duration_since(mtime)
                    .unwrap_or_default(),
            }
        } else {
            Freshness::Fresh
        }
    }

    /// Drop every node whose file is no longer tracked, and report how many.
    ///
    /// This is a net, not the mechanism: per-file replacement during a scan and
    /// explicit handling of git-reported deletions are what keep the graph
    /// honest. The sweep catches what those miss (a file removed outside git's
    /// view, an interrupted earlier run) and clears any backlog inherited from
    /// builds that never deleted anything.
    ///
    /// `tracked` must be built with [`graph_path`], the same helper the scanner
    /// mints node paths with. That is the actual safety property here, and it
    /// cannot be replaced by a check: `git.get_all_tracked_files` returns
    /// absolute paths, and comparing those against relative node keys does not
    /// look like an error — it looks like every node being an orphan, and the
    /// sweep would delete the entire graph. Routing both sides through one
    /// helper makes the mismatch unrepresentable.
    ///
    /// A successfully enumerated empty tracked set removes all obsolete nodes.
    /// Skip the sweep if Git enumeration failed or the tracked set is partial.
    /// Deliberately has no "refuses to delete more than N%" tripwire: the first
    /// sweep against a graph built by an older lain legitimately drops about
    /// half of it, so a ratio guard would block exactly the cleanup wanted.
    pub fn prune_orphans(&self, tracked: &HashSet<String>) -> Result<usize, LainError> {
        self.check_writable()?;

        // Iterate the per-path index rather than every node: distinct paths are
        // a fraction of node count, and each stale one drops as a whole bucket.
        let stale: Vec<String> = self
            .path_index
            .iter()
            .map(|r| r.key().clone())
            .filter(|key| !tracked.contains(key))
            .collect();

        let mut removed = 0usize;
        for key in stale {
            removed += self.replace_nodes_for_paths(&[key], &[])?;
        }
        Ok(removed)
    }

    pub fn upsert_nodes_batch(&self, new_nodes: Vec<GraphNode>) -> Result<(), LainError> {
        for node in new_nodes {
            self.upsert_node(node)?;
        }
        Ok(())
    }

    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        let source_idx = self
            .index_map
            .get(&edge.source_id)
            .map(|r| *r.value())
            .ok_or_else(|| {
                LainError::NotFound(format!("Source node {} not found", edge.source_id))
            })?;
        let target_idx = self
            .index_map
            .get(&edge.target_id)
            .map(|r| *r.value())
            .ok_or_else(|| {
                LainError::NotFound(format!("Target node {} not found", edge.target_id))
            })?;

        graph.add_edge(source_idx, target_idx, edge.clone());
        Ok(())
    }

    /// Insert a batch of edges, skipping any whose endpoints aren't in
    /// the graph. Returns the number that were **dropped**.
    ///
    /// The skip itself is necessary — an edge to a node that no longer
    /// exists can't be added — but it used to be silent, and that
    /// silence hid a real defect: in one production graph, 37 of 335
    /// files had symbols with no `Contains` edge from their own file
    /// node, so their symbols were orphaned and every structural query
    /// about them came back empty. Nothing logged it, no counter
    /// recorded it, and the graph reported itself healthy.
    ///
    /// Callers should log a non-zero return. An indexing pass that
    /// drops edges produced a graph that does not describe the code.
    ///
    /// Cross-repo edges (wishlist #13): an edge whose source IS in the
    /// local index but whose target is not (the resolve phase consulted
    /// the federation's `CrossRepoResolver` and got back a global id)
    /// cannot be added to the local petgraph. Stash it in
    /// [`Self::pending_external_edges`] so the federation's `project_repo`
    /// can drain it via [`Self::take_pending_external_edges`] and emit
    /// it to the federated backend, where the target's global id is
    /// already valid. An edge whose source is also missing is a true
    /// orphan (no caller to attach to) and is counted in the dropped
    /// return — same behavior as before this change, kept so the
    /// `label` warning still reports the genuinely broken case.
    /// Attach an embedding to the node `id` if it still exists. Returns
    /// whether it did.
    ///
    /// The background embedding pass computes for milliseconds per node
    /// while re-indexes run; writing back the copy it read (`upsert_node`)
    /// resurrected nodes a re-index had deleted in the meantime, and
    /// overwrote fields a re-index had refreshed.
    pub fn set_embedding(&self, id: &str, embedding: String) -> Result<bool, LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();
        let Some(idx) = self.index_map.get(id).map(|r| *r.value()) else {
            return Ok(false);
        };
        match graph.node_weight_mut(idx).filter(|n| n.id == id) {
            Some(node) => {
                node.embedding = Some(embedding);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn insert_edges_batch(&self, new_edges: &[GraphEdge]) -> Result<usize, LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();
        let mut external = self.pending_external_edges.lock();

        let mut dropped = 0usize;
        for edge in new_edges {
            let source_local = self.index_map.get(&edge.source_id).is_some();
            let target_local = self.index_map.get(&edge.target_id).is_some();
            match (source_local, target_local) {
                (true, true) => {
                    let s = self
                        .index_map
                        .get(&edge.source_id)
                        .map(|r| *r.value())
                        .unwrap();
                    let t = self
                        .index_map
                        .get(&edge.target_id)
                        .map(|r| *r.value())
                        .unwrap();
                    // One edge per (source, target, type). Re-inserting an
                    // existing edge — every incremental pass re-emits the
                    // folder `Contains` edges it keeps, and re-resolving
                    // unchanged callers re-emits their calls — piled up
                    // duplicates that inflated every count.
                    let exists = graph
                        .edges_connecting(s, t)
                        .any(|e| e.weight().edge_type == edge.edge_type);
                    if !exists {
                        graph.add_edge(s, t, edge.clone());
                    }
                }
                (true, false) => {
                    // Caller exists locally, callee lives in another
                    // repo. Hold the edge for the federation's
                    // `project_repo` to drain.
                    external.push(edge.clone());
                }
                (false, _) => {
                    // Truly orphan — neither endpoint is local.
                    // `insert_edges_reporting` warns about these.
                    dropped += 1;
                }
            }
        }
        Ok(dropped)
    }

    /// Drain every edge the resolve phase stashed because its target
    /// lives outside the local repo (wishlist #13). The federation's
    /// `project_repo` calls this after the regular intra-repo edge
    /// pass: each drained edge has `source_id` rewritten through the
    /// per-repo `local_to_global` map and `target_id` passed through
    /// unchanged because it is already in global form
    /// (`repo_id:Kind:path:name`). Returns the drained vec; calling it
    /// twice in a row returns an empty vec the second time.
    pub fn take_pending_external_edges(&self) -> Vec<GraphEdge> {
        std::mem::take(&mut *self.pending_external_edges.lock())
    }

    /// Insert an edge idempotently: same `(source, target, edge_type)`
    /// triple is added at most once. `project_repo` runs on every
    /// add_repo / reload / watcher-triggered index, so without dedup
    /// the federation backend accumulates N copies of each edge over
    /// the lifetime of the server (observed: `total_edges` = 127k
    /// instead of the per-repo 15k on the lain repo itself).
    pub fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        let graph = self.graph.read();
        let source_idx = self.index_map.get(&edge.source_id).map(|r| *r.value());
        let target_idx = self.index_map.get(&edge.target_id).map(|r| *r.value());
        if let (Some(s), Some(t)) = (source_idx, target_idx) {
            if graph
                .edges_connecting(s, t)
                .any(|e| e.weight().edge_type == edge.edge_type)
            {
                return Ok(());
            }
        }
        drop(graph);
        self.insert_edge(&edge)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        let graph = self.graph.read();

        Ok(self
            .index_map
            .get(id)
            .and_then(|r| graph.node_weight(*r.value()).cloned()))
    }

    pub fn get_node_by_id(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        self.get_node(id)
    }

    pub fn traverse(
        &self,
        start: &str,
        edge_type: EdgeType,
        depth: std::ops::Range<u32>,
        direction: Direction,
    ) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        let Some(start_idx) = self.index_map.get(start).map(|r| *r.value()) else {
            return Ok(Vec::new());
        };
        let min_depth = depth.start;
        let max_depth = depth.end;
        if min_depth > max_depth {
            return Ok(Vec::new());
        }

        let mut visited = HashSet::from([start_idx]);
        let mut queue = VecDeque::from([(start_idx, 0)]);
        let mut result = Vec::new();
        while let Some((current, current_depth)) = queue.pop_front() {
            if current_depth >= max_depth {
                continue;
            }
            for graph_edge in graph.edges_directed(current, direction) {
                if graph_edge.weight().edge_type != edge_type {
                    continue;
                }
                // The "other end" of an edge relative to `current`:
                //   - Outgoing: edges are `current → other`; other end
                //     is `.target()`.
                //   - Incoming: edges are `other → current`; other end
                //     is `.source()`. (`.target()` is `current` itself
                //     in that case, which would loop us back to where
                //     we started.)
                let next = match direction {
                    petgraph::Direction::Outgoing => graph_edge.target(),
                    petgraph::Direction::Incoming => graph_edge.source(),
                };
                if !visited.insert(next) {
                    continue;
                }
                let next_depth = current_depth + 1;
                if let Some(node) = graph.node_weight(next).cloned() {
                    if next_depth >= min_depth {
                        result.push(node);
                    }
                    queue.push_back((next, next_depth));
                }
            }
        }
        Ok(result)
    }

    pub fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        let (Some(from_idx), Some(to_idx)) = (
            self.index_map.get(from).map(|r| *r.value()),
            self.index_map.get(to).map(|r| *r.value()),
        ) else {
            return Ok(Vec::new());
        };
        let mut parents = HashMap::from([(from_idx, None)]);
        let mut queue = VecDeque::from([from_idx]);
        while let Some(current) = queue.pop_front() {
            if current == to_idx {
                break;
            }
            for graph_edge in graph.edges_directed(current, Direction::Outgoing) {
                let next = graph_edge.target();
                if let std::collections::hash_map::Entry::Vacant(e) = parents.entry(next) {
                    e.insert(Some(current));
                    queue.push_back(next);
                }
            }
        }
        if !parents.contains_key(&to_idx) {
            return Ok(Vec::new());
        }
        let mut indices = Vec::new();
        let mut current = Some(to_idx);
        while let Some(idx) = current {
            indices.push(idx);
            current = parents[&idx];
        }
        indices.reverse();
        Ok(indices
            .into_iter()
            .filter_map(|idx| graph.node_weight(idx).cloned())
            .collect())
    }

    pub fn subgraph_around(
        &self,
        center: &str,
        radius: u32,
    ) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        let graph = self.graph.read();
        let Some(center_idx) = self.index_map.get(center).map(|r| *r.value()) else {
            return Ok(Vec::new());
        };
        let mut visited = HashSet::from([center_idx]);
        let mut queue = VecDeque::from([(center_idx, 0)]);
        let mut indices = Vec::new();
        while let Some((current, current_depth)) = queue.pop_front() {
            indices.push(current);
            if current_depth >= radius {
                continue;
            }
            for graph_edge in graph.edges_directed(current, Direction::Outgoing) {
                let next = graph_edge.target();
                if visited.insert(next) {
                    queue.push_back((next, current_depth + 1));
                }
            }
        }
        let selected: HashSet<_> = indices.iter().copied().collect();
        Ok(indices
            .into_iter()
            .filter_map(|idx| {
                let node = graph.node_weight(idx).cloned()?;
                let edges = graph
                    .edges_directed(idx, Direction::Outgoing)
                    .filter(|e| selected.contains(&e.target()))
                    .map(|e| e.weight().clone())
                    .collect();
                Some((node, edges))
            })
            .collect())
    }

    pub fn node_count(&self) -> usize {
        self.graph.read().node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.read().edge_count()
    }

    pub fn get_nodes_by_type(&self, node_type: NodeType) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        Ok(graph
            .node_weights()
            .filter(|n| n.node_type == node_type)
            .cloned()
            .collect())
    }

    /// Get nodes matching any of the given node types in a single graph traversal
    pub fn get_nodes_by_types(&self, node_types: &[NodeType]) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        Ok(graph
            .node_weights()
            .filter(|n| node_types.contains(&n.node_type))
            .cloned()
            .collect())
    }

    pub fn get_all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph.node_weights().cloned().collect()
    }

    pub fn all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph
            .node_references()
            .map(|(_, node)| node.clone())
            .collect()
    }

    pub fn all_edges(&self) -> Vec<GraphEdge> {
        let graph = self.graph.read();
        graph.edge_weights().cloned().collect()
    }

    /// One node with this name, chosen deterministically.
    ///
    /// This used to be `node_weights().find(...)` — petgraph iteration
    /// order, which is neither meaningful nor stable across reindexes.
    /// With eleven `fn parse` definitions in this repo, two calls could
    /// legitimately answer about two different functions, which is how
    /// `find_anchors` and `get_anchor_score` ended up reporting
    /// different scores "for `parse`". Sorting by (path, id) at least
    /// makes the choice repeatable; [`Self::find_all_nodes_by_name`]
    /// is what callers should use when they need to know a name was
    /// ambiguous at all.
    pub fn find_node_by_name(&self, name: &str) -> Option<GraphNode> {
        self.find_all_nodes_by_name(name).into_iter().next()
    }

    /// Every node with this name, sorted by (path, id) so the order is
    /// stable across reindexes.
    pub fn find_all_nodes_by_name(&self, name: &str) -> Vec<GraphNode> {
        let mut hits: Vec<GraphNode> = self
            .graph
            .read()
            .node_weights()
            .filter(|n| n.name == name)
            .cloned()
            .collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
        hits
    }

    pub fn find_node_by_path(&self, path: &str) -> Option<GraphNode> {
        self.graph
            .read()
            .node_weights()
            .find(|n| n.path == path)
            .cloned()
    }

    /// O(1) existence check via `path_index`, for callers that only need
    /// to know "is there anything here" (e.g. `RepoIndex::sync_overlay`'s
    /// staleness sweep, called once per stale path every cycle) rather
    /// than the node itself — `find_node_by_path` does a full
    /// `node_weights()` scan for that.
    pub fn has_node_at_path(&self, path: &str) -> bool {
        self.path_index.get(path).is_some_and(|v| !v.is_empty())
    }

    /// Query nodes with optional filters (used by query executor)
    pub fn query_nodes(
        &self,
        type_selector: Option<&crate::query::spec::TypeSelector>,
        name_selector: Option<&crate::query::spec::NameSelector>,
        label_selector: Option<&crate::query::spec::LabelSelector>,
        path_filter: Option<&str>,
    ) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph
            .node_weights()
            .filter(|n| {
                // Type filter
                if let Some(sel) = type_selector {
                    let node_type_str = n.node_type.to_string();
                    if !sel.matches(&node_type_str) {
                        return false;
                    }
                }
                // Name filter
                if let Some(sel) = name_selector {
                    if !sel.matches(&n.name) {
                        return false;
                    }
                }
                // Label filter (is_deprecated is the only label for now)
                if let Some(sel) = label_selector {
                    let label = if n.is_deprecated {
                        Some("deprecated")
                    } else {
                        None
                    };
                    if !sel.matches(label) {
                        return false;
                    }
                }
                // Path filter
                if let Some(path) = path_filter {
                    if !n.path.contains(path) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Get neighbors of a node by ID
    pub fn get_neighbors(
        &self,
        node_id: &str,
        direction: Direction,
    ) -> Vec<(GraphNode, GraphEdge)> {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(node_id).map(|r| *r.value()) else {
            return Vec::new();
        };

        graph
            .edges_directed(idx, direction)
            .filter_map(|e| {
                let neighbor_idx = match direction {
                    Direction::Incoming => e.source(),
                    Direction::Outgoing => e.target(),
                };
                graph
                    .node_weight(neighbor_idx)
                    .cloned()
                    .map(|neighbor| (neighbor, e.weight().clone()))
            })
            .collect()
    }

    /// BFS traverse from a node ID following outgoing edges with depth tracking.
    /// Returns (neighbor_node, edge, depth) tuples.
    pub fn bfs_from(&self, start_id: &str, max_depth: u32) -> Vec<(GraphNode, GraphEdge, u32)> {
        let graph = self.graph.read();

        let Some(start_idx) = self.index_map.get(start_id).map(|r| *r.value()) else {
            return Vec::new();
        };

        let mut results = Vec::new();
        let mut visited = HashSet::new();
        let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
        queue.push_back((start_idx, 0));

        while let Some((current_idx, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            for edge in graph.edges_directed(current_idx, Direction::Outgoing) {
                let neighbor_idx = edge.target();
                if visited.contains(&neighbor_idx) {
                    continue;
                }
                visited.insert(neighbor_idx);

                if let Some(neighbor) = graph.node_weight(neighbor_idx).cloned() {
                    results.push((neighbor, edge.weight().clone(), depth + 1));
                    queue.push_back((neighbor_idx, depth + 1));
                }
            }
        }

        results
    }

    pub fn get_edges_from(&self, source_id: &str) -> Result<Vec<GraphEdge>, LainError> {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(source_id).map(|r| *r.value()) else {
            return Ok(Vec::new());
        };

        Ok(graph
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| e.weight().clone())
            .collect())
    }

    pub fn get_edges_to(&self, target_id: &str) -> Result<Vec<GraphEdge>, LainError> {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(target_id).map(|r| *r.value()) else {
            return Ok(Vec::new());
        };

        Ok(graph
            .edges_directed(idx, Direction::Incoming)
            .map(|e| e.weight().clone())
            .collect())
    }

    pub fn calculate_anchor_scores(&self) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        // Two-pass: compute raw hub scores, find the corpus-wide max,
        // then normalize every node so the top symbol scores 100 and
        // everything else scales accordingly.
        //
        // Without this normalization the raw score grows unbounded as
        // the corpus grows (we've observed values up to 1063 in
        // production). That makes the search ranking
        // `sim + anchor_weight * anchor` anchor-dominated, hiding
        // semantically better matches and producing different rankings
        // across reindexes of the same code.
        //
        // With percentile normalization:
        //   - The top symbol in any corpus always scores 100
        //   - Ranks reflect *relative* importance, not raw fan_in
        //   - Rankings are stable across reindexes unless the identity
        //     of the top changes
        //   - Composes cleanly with the per-candidate-set min-max
        //     normalization in search.rs
        let indices: Vec<_> = graph.node_indices().collect();

        // Pass 1: compute raw hub scores, find max.
        //
        // Hub semantics. An anchor is an ORCHESTRATION hub — called
        // by many (calls_in), coordinating many (calls_out), with a
        // real body (size_factor):
        //
        //     raw = calls_in * log2(1 + calls_out) * size_factor
        //     size_factor = min(1, body_lines / 8)
        //
        // Only Calls edges count. The superseded fan_in/(fan_out+1)
        // counted every edge type (including Contains from the parent
        // file) and actively punished fan_out, which is backwards for
        // hubs — it put 1-line helpers like `as_str` at the top of
        // find_anchors.
        //
        // The approved design wrote `log2(2 + calls_out)`, so that
        // calls_out = 0 yielded a factor of 1. This uses `1 +`
        // deliberately: a function that calls nothing coordinates
        // nothing, so it scores 0 however many callers it has — the
        // stronger form of the same intent. Pinned by
        // `anchor_hub_tests::{leaf_utility_scores_zero,
        // hub_outranks_trivial_helper}`.
        //
        // Known limit: Calls edges are name-resolved when LSP type
        // info is unavailable, so every `.as_str()` in the repo
        // collapses onto one node and inflates its calls_in. A
        // ubiquitous method name can still surface at the top;
        // `find_anchors` reports the path so you can see which
        // definition was scored.
        let mut max_raw: f32 = 0.0;
        let mut raws: Vec<(petgraph::graph::NodeIndex, f32)> = Vec::with_capacity(indices.len());
        for idx in &indices {
            let node = &graph[*idx];
            // Test code is hub-shaped (fixtures call everything and are
            // called by every test) but anchors are entry points into
            // the PRODUCT. Test-path symbols score 0, and Calls edges
            // with a test-path endpoint don't count toward fan-in/out
            // either (fifty `test_*` callers don't make `default` an
            // orchestration hub). Inline `#[cfg(test)]` modules inside
            // regular src files are only detectable via the
            // `*_tests.rs` / `tests.rs` file-stem conventions.
            if is_test_path(&node.path) {
                raws.push((*idx, 0.0));
                continue;
            }
            let raw = match node.node_type {
                NodeType::Function | NodeType::Method => {
                    // Unfiltered Calls count — used only to decide
                    // whether the baseline applies. The baseline
                    // is for functions with *no* callers at all
                    // (a wishlist-#14 small-fixture artifact, where
                    // every function scores 0 and the sort is
                    // unstable). A function whose only callers are
                    // tests still scores 0 — the test-caller filter
                    // is a stronger "test code doesn't count as
                    // production signal" statement, not a dead-code
                    // signal, and we must not relax it by handing
                    // the function a baseline weight.
                    let calls_in_unfiltered = graph
                        .edges_directed(*idx, Direction::Incoming)
                        .filter(|e| e.weight().edge_type == EdgeType::Calls)
                        .count() as f32;
                    let calls_in = graph
                        .edges_directed(*idx, Direction::Incoming)
                        .filter(|e| e.weight().edge_type == EdgeType::Calls)
                        .filter(|e| !is_test_path(&graph[e.source()].path))
                        .count() as f32;
                    let calls_out = graph
                        .edges_directed(*idx, Direction::Outgoing)
                        .filter(|e| e.weight().edge_type == EdgeType::Calls)
                        .filter(|e| !is_test_path(&graph[e.target()].path))
                        .count() as f32;
                    let body_lines = match (node.line_start, node.line_end) {
                        (Some(s), Some(e)) => e.saturating_sub(s) as f32 + 1.0,
                        _ => 1.0,
                    };
                    let size_factor = (body_lines / 8.0).min(1.0);
                    if calls_in_unfiltered == 0.0 {
                        // Baseline weight for functions with no
                        // callers. Without this, every raw in a small
                        // fixture (or a fixture where the LSP path
                        // didn't pick up calls) is 0, the max_raw is
                        // 0, and every normalized score is 0 — the
                        // sort is unstable at zero and top anchors
                        // come back in arbitrary order. With 0.5x,
                        // dead functions stay visible (so the user
                        // can see what was indexed) but any function
                        // with at least one caller outranks them:
                        // calls_in >= 1 and calls_out >= 1 gives
                        // raw >= 1 * 1 * size_factor = size_factor,
                        // which exceeds 0.5 * size_factor. Pinned by
                        // `anchor_hub_tests::dead_function_baseline_weight`.
                        size_factor * 0.5
                    } else {
                        // log2(1 + calls_out): a leaf that calls
                        // nothing is not an orchestration hub and
                        // scores 0 — no matter how many callers it
                        // has (the `as_str` problem).
                        calls_in * (1.0 + calls_out).log2() * size_factor
                    }
                }
                _ => 0.0,
            };
            if raw > max_raw {
                max_raw = raw;
            }
            raws.push((*idx, raw));
        }

        // Pass 2: write fan_in/fan_out + normalized anchor back
        for (idx, raw) in raws {
            let fan_in = graph.neighbors_directed(idx, Direction::Incoming).count() as u32;
            let fan_out = graph.neighbors_directed(idx, Direction::Outgoing).count() as u32;
            // Calls-only counts, stored alongside the all-edge ones.
            // "How many callers?" is a different question from "how
            // coupled is this?", and answering the first with the
            // second is why a dead-code check could never fire: the
            // `Contains` edge from a symbol's own file guarantees
            // `fan_in >= 1`. No test-path filter here — that is an
            // anchor-scoring policy, not a fact about the graph.
            let calls_in = graph
                .edges_directed(idx, Direction::Incoming)
                .filter(|e| e.weight().edge_type == EdgeType::Calls)
                .count() as u32;
            let calls_out = graph
                .edges_directed(idx, Direction::Outgoing)
                .filter(|e| e.weight().edge_type == EdgeType::Calls)
                .count() as u32;
            // 100.0 scale so display "anchor 12.34" is human-readable;
            // top-of-corpus symbol always scores 100 regardless of how
            // big the codebase grows.
            let normalized = if max_raw > 0.0 {
                raw / max_raw * 100.0
            } else {
                0.0
            };
            if let Some(node) = graph.node_weight_mut(idx) {
                node.fan_in = Some(fan_in);
                node.fan_out = Some(fan_out);
                node.calls_in = Some(calls_in);
                node.calls_out = Some(calls_out);
                node.anchor_score = Some(normalized);
            }
        }
        Ok(())
    }

    /// Top symbols by `anchor_score`. Many real codebases contain
    /// dozens of identically-named trivial helpers (e.g. `as_str()` calls
    /// everywhere); without dedup `find_anchors` would return the same
    /// name 20 times in a row. We dedup by NAME and keep the
    /// best-scoring instance of each, so the top-N output reads as a
    /// meaningful list of distinct anchors. The key is the name alone,
    /// not (name, kind): a `parse` function and a `parse` method are
    /// the same anchor for a reader skimming the list.
    pub fn find_anchors(&self, limit: usize) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        let mut sorted: Vec<_> = graph.node_weights().cloned().collect();
        sorted.sort_by(|a, b| {
            b.anchor_score
                .unwrap_or(0.0)
                .total_cmp(&a.anchor_score.unwrap_or(0.0))
        });
        let mut by_name: std::collections::HashMap<String, GraphNode> =
            std::collections::HashMap::new();
        for n in sorted {
            // Insert only the first (best-scoring) instance per name.
            // `sorted` is already descending by score.
            by_name.entry(n.name.clone()).or_insert(n);
        }
        // Re-sort the deduped set by score (the HashMap insert order
        // is not guaranteed to be sorted).
        let mut out: Vec<GraphNode> = by_name.into_values().collect();
        out.sort_by(|a, b| {
            b.anchor_score
                .unwrap_or(0.0)
                .total_cmp(&a.anchor_score.unwrap_or(0.0))
        });
        Ok(out.into_iter().take(limit).collect())
    }

    pub fn calculate_depths(&self) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        // 1. Reset
        for node in graph.node_weights_mut() {
            node.depth_from_main = None;
        }

        // 2. BFS from entry points
        let mut current_layer: Vec<NodeIndex> = graph
            .node_indices()
            .filter(|&idx| {
                let n = &graph[idx];
                n.name == "main" || n.name == "App"
            })
            .collect();

        let mut depth = 0;
        let mut visited = HashMap::new();

        while !current_layer.is_empty() && depth < 50 {
            let mut next_layer = Vec::new();
            for idx in current_layer {
                if visited.contains_key(&idx) {
                    continue;
                }
                visited.insert(idx, depth);

                if let Some(node) = graph.node_weight_mut(idx) {
                    node.depth_from_main = Some(depth);
                }

                // Find children via Contains edges
                let children: Vec<_> = graph
                    .edges_directed(idx, Direction::Outgoing)
                    .filter(|e| e.weight().edge_type == EdgeType::Contains)
                    .map(|e| e.target())
                    .collect();

                next_layer.extend(children);
            }
            current_layer = next_layer;
            depth += 1;
        }
        Ok(())
    }

    pub fn find_entry_points(&self) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        Ok(graph
            .node_weights()
            .filter(|n| n.name == "main" || n.name == "App")
            .cloned()
            .collect())
    }

    pub fn insert_co_change_edges(
        &self,
        pairs: &[(String, String, usize)],
    ) -> Result<(), LainError> {
        let mut edges = Vec::new();
        for (p1, p2, count) in pairs {
            let filename1 = Path::new(p1)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let filename2 = Path::new(p2)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let id1 =
                GraphNode::generate_id(&NodeType::File, p1, &filename1, None, &self.namespace);
            let id2 =
                GraphNode::generate_id(&NodeType::File, p2, &filename2, None, &self.namespace);

            let mut edge = GraphEdge::new(EdgeType::CoChangedWith, id1, id2);
            edge.weight = Some(*count as f32);
            edges.push(edge);
        }
        // Use batch insertion which is inherently resilient to missing nodes
        self.insert_edges_batch(&edges).map(|_| ())
    }

    pub fn get_co_change_partners(
        &self,
        file_path: &str,
    ) -> Result<Vec<(String, usize)>, LainError> {
        let graph = self.graph.read();

        let filename = Path::new(file_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let id =
            GraphNode::generate_id(&NodeType::File, file_path, &filename, None, &self.namespace);
        let Some(idx) = self.index_map.get(&id).map(|r| *r.value()) else {
            return Ok(Vec::new());
        };

        // Fold by target path before returning. The graph can hold more
        // than one node for a path (and more than one edge into them),
        // and the raw edge list was emitted verbatim — which is why
        // co-change output showed `src/server/presence_lock.rs (2 times)`
        // three separate times in a four-row list.
        //
        // Max, not sum: the weights are counts of the same underlying
        // co-change relationship observed through different nodes, so
        // adding them would inflate the number rather than merge it.
        //
        // Both directions: a pair is stored once, from the path that sorts
        // first. Reading only outgoing edges hid every partner whose path
        // sorts before this file's — `sessions.py` never saw `models.py`.
        let mut by_path: HashMap<String, usize> = HashMap::new();
        for (e, other) in graph
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| (e.weight(), e.target()))
            .chain(
                graph
                    .edges_directed(idx, Direction::Incoming)
                    .map(|e| (e.weight(), e.source())),
            )
            .filter(|(w, _)| w.edge_type == EdgeType::CoChangedWith)
        {
            let partner = &graph[other];
            let count = e.weight.unwrap_or(0.0) as usize;
            let slot = by_path.entry(partner.path.clone()).or_insert(0);
            *slot = (*slot).max(count);
        }
        let mut out: Vec<(String, usize)> = by_path.into_iter().collect();
        // Strongest partner first, then by path so equal counts are
        // stable across runs instead of following HashMap order.
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    pub fn get_last_commit(&self) -> Result<Option<String>, LainError> {
        Ok(self.last_commit.read().clone())
    }

    /// Whether the persisted File nodes were minted under a different id
    /// namespace than this graph's — a graph written by a Lain that used a
    /// per-process random namespace. Such a graph must be rebuilt: its ids
    /// match nothing this process derives (co-change edges, lookups by id).
    pub fn minted_in_other_namespace(&self) -> bool {
        let graph = self.graph.read();
        let stale = graph
            .node_weights()
            .filter(|n| n.node_type == NodeType::File)
            .take(20)
            // With the node's own line: scanner File nodes have none, but a
            // language server's FILE-kind symbols carry one in their id.
            .any(|n| {
                n.id != GraphNode::generate_id(
                    &NodeType::File,
                    &n.path,
                    &n.name,
                    n.line_start,
                    &self.namespace,
                )
            });
        stale
    }

    /// Drop every node and edge and forget the indexed commit, so the next
    /// build is a full scan.
    pub fn reset(&self) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();
        *graph = StableGraph::new();
        self.index_map.clear();
        self.path_index.clear();
        self.pending_external_edges.lock().clear();
        *self.last_commit.write() = None;
        Ok(())
    }

    pub fn set_last_commit(&self, hash: String) -> Result<(), LainError> {
        self.check_writable()?;
        *self.last_commit.write() = Some(hash);
        Ok(())
    }

    pub fn get_stats(&self) -> (usize, usize) {
        let graph = self.graph.read();
        (graph.node_count(), graph.edge_count())
    }

    /// Edge counts grouped by `EdgeType`. Used by `get_health` to
    /// surface the edge-type histogram so operators can tell at a
    /// glance whether the indexer produced the `Calls` and `Uses`
    /// edges (vs. just the cheaper-to-extract `Contains` /
    /// `CoChangedWith` from the static tree-sitter + git phases).
    /// Without this, the only signal that the call graph is empty
    /// is "every impact query returns nothing," which is the exact
    /// failure the user reported as Bug 2.
    pub fn edge_counts_by_type(&self) -> std::collections::BTreeMap<String, usize> {
        use petgraph::visit::IntoEdgeReferences;
        use std::collections::BTreeMap;
        let graph = self.graph.read();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for edge in graph.edge_references() {
            let key = format!("{:?}", edge.weight().edge_type);
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }

    pub fn get_node_at_location(&self, path: &str, line: u32) -> Option<GraphNode> {
        let graph = self.graph.read();

        if let Some(indices) = self.path_index.get(path) {
            indices
                .iter()
                .filter_map(|&idx| graph.node_weight(idx))
                .filter(|n| n.node_type != NodeType::File)
                .filter(|n| n.line_start.unwrap_or(0) <= line && n.line_end.unwrap_or(0) >= line)
                .min_by_key(|n| {
                    n.line_end
                        .unwrap_or(0)
                        .saturating_sub(n.line_start.unwrap_or(0))
                })
                .cloned()
        } else {
            None
        }
    }

    /// The `File` node for `path`, via the path index.
    pub fn get_file_node(&self, path: &str) -> Option<GraphNode> {
        let graph = self.graph.read();
        let indices = self.path_index.get(path)?;
        indices
            .iter()
            .filter_map(|&idx| graph.node_weight(idx))
            .find(|n| n.node_type == NodeType::File)
            .cloned()
    }

    pub fn has_references_from(&self, id: &str) -> bool {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(id).map(|r| *r.value()) else {
            return false;
        };

        graph.edges_directed(idx, Direction::Outgoing).any(|e| {
            e.weight().edge_type == EdgeType::Calls || e.weight().edge_type == EdgeType::Uses
        })
    }

    /// Build a snapshot of this graph in its on-disk representation.
    /// Used by the save/load/export paths to keep the snapshot
    /// construction in one place.
    fn build_state(&self) -> persist::GraphState {
        let index_map: HashMap<String, NodeIndex> = self
            .index_map
            .iter()
            .map(|r| (r.key().clone(), *r.value()))
            .collect();
        let last_commit = self.last_commit.read().clone();
        persist::GraphState::new(self.graph.read().clone(), index_map, last_commit)
    }

    /// Save graph to disk asynchronously (non-blocking)
    pub async fn save_to_disk(&self) -> Result<(), LainError> {
        self.check_writable()?;
        // Clone state under lock (fast)
        let (data, persistence_path) = {
            let data = persist::encode_state(&self.build_state())
                .map_err(|e| LainError::Database(e.to_string()))?;
            let persistence_path = self.persistence_path.clone();
            (data, persistence_path)
        };

        // Atomic save: write to .tmp and rename via shared helper
        crate::cli::io::tokio_write_file_atomic(&persistence_path, &data)
            .await
            .map_err(|e| LainError::Database(e.to_string()))?;

        Ok(())
    }

    pub fn save_to_disk_sync(&self) -> Result<(), LainError> {
        let data = persist::encode_state(&self.build_state())
            .map_err(|e| LainError::Database(e.to_string()))?;
        crate::cli::io::write_file_atomic(&self.persistence_path, &data)
            .map_err(|e| LainError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn load_from_disk(&self) -> Result<(), LainError> {
        // load_from_disk is allowed on read-only graphs — it's how we hydrate
        // the static sidecar view from the owner's on-disk snapshot. Only
        // *mutations* are gated by `check_writable`.
        let data = std::fs::read(&self.persistence_path)
            .map_err(|e| LainError::Database(e.to_string()))?;

        // Fail soft. A graph we cannot read is not a fatal condition: the
        // source tree is the source of truth and the caller re-indexes. This
        // path used to `?` the deserialize error straight out of
        // `GraphDatabase::new`, which turned any format change into a startup
        // crash instead of a rebuild.
        let state = match persist::decode_state(&data) {
            Ok((state, _)) => state,
            Err(e) => {
                warn!(
                    "Ignoring unreadable graph at {}: {e}. Starting empty; \
                     the next index pass will rebuild it.",
                    self.persistence_path.display()
                );
                return Ok(());
            }
        };

        if state.path_format_version != PATH_FORMAT_VERSION {
            warn!(
                "Ignoring graph at {} written with path format v{} (this build expects v{}). \
                 Starting empty; the next index pass will rebuild it. Its node ids encode a \
                 different path convention, so merging would duplicate every node.",
                self.persistence_path.display(),
                state.path_format_version,
                PATH_FORMAT_VERSION
            );
            return Ok(());
        }

        let mut state = state;
        persist::heal_duplicate_ids(&mut state);

        let mut path_index = HashMap::new();
        for (idx, node) in state.graph.node_references() {
            path_index
                .entry(node.path.clone())
                .or_insert_with(Vec::new)
                .push(idx);
        }

        // Swap the graph and refill the indices under one write lock. The
        // indices are shared by every clone, so refilling them after the
        // lock dropped let a concurrent reader see the new graph with an
        // empty index (or stale NodeIndex values into it).
        let mut graph = self.graph.write();
        *graph = state.graph;
        self.index_map.clear();
        for (k, v) in state.index_map {
            self.index_map.insert(k, v);
        }
        self.path_index.clear();
        for (k, v) in path_index {
            self.path_index.insert(k, v);
        }
        drop(graph);
        *self.last_commit.write() = state.last_commit;
        Ok(())
    }

    pub fn export_to_json(&self) -> Result<String, LainError> {
        serde_json::to_string_pretty(&self.build_state())
            .map_err(|e| LainError::Database(e.to_string()))
    }
}

/// Test code is hub-shaped (fixtures call everything, every test calls
/// fixtures) but anchors are entry points into the PRODUCT. Detect by
/// path conventions: a `tests/` directory component, or the Rust
/// `*_tests.rs` / `*_test.rs` / `tests.rs` file-stem conventions used
/// for `#[cfg(test)]` modules under `src/`. Inline cfg(test) modules
/// in regular src files are not detectable by path.
fn is_test_path(path: &str) -> bool {
    if path.split('/').any(|c| c == "tests") {
        return true;
    }
    let stem = path.rsplit('/').next().unwrap_or(path);
    let stem = stem.strip_suffix(".rs").unwrap_or(stem);
    stem == "tests" || stem.ends_with("_tests") || stem.ends_with("_test")
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    fn db(name: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    /// Re-scanning a file must drop the symbols it no longer defines. Before
    /// `replace_nodes_for_paths` the graph only ever grew, so a deleted
    /// function kept answering queries — with a stale path and no live edges.
    #[test]
    fn replace_drops_symbols_the_file_no_longer_defines() {
        let g = db("lain_test_replace_drop");
        let alpha = GraphNode::new(NodeType::Function, "alpha".into(), "src/probe.rs".into());
        let beta = GraphNode::new(NodeType::Function, "beta".into(), "src/probe.rs".into());
        g.insert_nodes_batch(&[alpha.clone(), beta]).unwrap();
        assert!(g.find_node_by_name("beta").is_some(), "precondition");

        let removed = g
            .replace_nodes_for_paths(&["src/probe.rs".to_string()], &[alpha])
            .unwrap();

        assert_eq!(removed, 2, "both old nodes removed before reinsert");
        assert!(g.find_node_by_name("alpha").is_some(), "alpha survives");
        assert!(g.find_node_by_name("beta").is_none(), "beta is gone");
    }

    /// A path with no replacement nodes is the deleted-file case.
    #[test]
    fn replace_with_no_nodes_deletes_the_file() {
        let g = db("lain_test_replace_del");
        let gone = GraphNode::new(NodeType::Function, "gone".into(), "src/gone.rs".into());
        let keep = GraphNode::new(NodeType::Function, "keep".into(), "src/keep.rs".into());
        g.insert_nodes_batch(&[gone, keep]).unwrap();

        g.replace_nodes_for_paths(&["src/gone.rs".to_string()], &[])
            .unwrap();

        assert!(g.find_node_by_name("gone").is_none());
        assert!(
            g.find_node_by_name("keep").is_some(),
            "other files untouched"
        );
    }

    /// Namespace nodes are directory-scoped and shared by every file beneath
    /// them, so a per-file replace must leave them alone.
    #[test]
    fn replace_preserves_namespace_nodes() {
        let g = db("lain_test_replace_ns");
        let ns = GraphNode::new(NodeType::Namespace, "src".into(), "src".into());
        g.insert_nodes_batch(&[ns]).unwrap();

        g.replace_nodes_for_paths(&["src".to_string()], &[])
            .unwrap();

        assert!(
            g.find_node_by_name("src").is_some(),
            "namespace must survive"
        );
    }

    /// Re-scanning a directory re-emits its Namespace node under the same
    /// id; that must refresh the kept node, not add a second one.
    #[test]
    fn replace_does_not_duplicate_a_kept_namespace() {
        let g = db("lain_test_replace_ns_dup");
        let ns = GraphNode::new(NodeType::Namespace, "src".into(), "src".into());
        g.insert_nodes_batch(std::slice::from_ref(&ns)).unwrap();

        for _ in 0..2 {
            g.replace_nodes_for_paths(&["src".to_string()], std::slice::from_ref(&ns))
                .unwrap();
        }

        assert_eq!(g.find_all_nodes_by_name("src").len(), 1);
        let state = g.build_state();
        assert_eq!(state.index_map.len(), state.graph.node_count());
    }

    /// A graph saved with a duplicated Namespace loads with one copy, and
    /// the stray copy's edges move onto it.
    #[test]
    fn load_heals_duplicate_ids() {
        let tmp = std::env::temp_dir().join("lain_test_heal_dup");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();
        let ns = GraphNode::new(NodeType::Namespace, "src".into(), "src".into());
        let f = GraphNode::new(NodeType::File, "a.rs".into(), "src/a.rs".into());
        g.insert_nodes_batch(&[ns.clone(), f.clone()]).unwrap();

        let mut state = g.build_state();
        let stray = state.graph.add_node(ns.clone());
        let file = state.index_map[&f.id];
        state.graph.add_edge(
            stray,
            file,
            GraphEdge::new(EdgeType::Contains, ns.id.clone(), f.id.clone()),
        );
        std::fs::write(&g.persistence_path, persist::encode_state(&state).unwrap()).unwrap();

        g.load_from_disk().unwrap();
        assert_eq!(g.find_all_nodes_by_name("src").len(), 1);
        let edges = g.get_edges_from(&ns.id).unwrap();
        assert!(edges.iter().any(|e| e.target_id == f.id), "{edges:?}");
        let saved = g.build_state();
        assert_eq!(saved.index_map.len(), saved.graph.node_count());
    }

    /// A workspace reached through a symlink still yields relative keys.
    #[cfg(unix)]
    #[test]
    fn graph_path_sees_through_a_symlinked_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src/lib.rs"), "").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real = dunce::canonicalize(&real).unwrap();

        // Workspace spelled through the link, file path canonical…
        assert_eq!(graph_path(&link, &real.join("src/lib.rs")), "src/lib.rs");
        // …and the other way round.
        assert_eq!(graph_path(&real, &link.join("src/lib.rs")), "src/lib.rs");
        // Outside the workspace stays absolute.
        let outside = tmp.path().join("elsewhere.rs");
        assert_eq!(
            graph_path(&real, &outside),
            crate::server::path_util::posix_string(&outside)
        );
    }

    /// An embedding computed for a node a re-index removed meanwhile does
    /// not bring it back.
    #[test]
    fn set_embedding_never_resurrects_a_node() {
        let g = db("lain_test_set_embedding");
        let n = GraphNode::new(NodeType::Function, "gone".into(), "src/gone.rs".into());
        let id = n.id.clone();
        g.insert_nodes_batch(std::slice::from_ref(&n)).unwrap();
        assert!(g.set_embedding(&id, "[1.0]".into()).unwrap());
        assert_eq!(
            g.get_node(&id).unwrap().unwrap().embedding.as_deref(),
            Some("[1.0]")
        );
        g.replace_nodes_for_paths(&["src/gone.rs".to_string()], &[])
            .unwrap();
        assert!(!g.set_embedding(&id, "[2.0]".into()).unwrap());
        assert!(g.get_node(&id).unwrap().is_none());
    }

    /// The sweep drops files git no longer tracks and leaves the rest alone.
    #[test]
    fn prune_orphans_removes_untracked_only() {
        let g = db("lain_test_prune");
        let live = GraphNode::new(NodeType::Function, "live".into(), "src/live.rs".into());
        let dead = GraphNode::new(NodeType::Function, "dead".into(), "src/dead.rs".into());
        g.insert_nodes_batch(&[live, dead]).unwrap();

        let tracked: HashSet<String> = ["src/live.rs".to_string()].into_iter().collect();
        let removed = g.prune_orphans(&tracked).unwrap();

        assert_eq!(removed, 1);
        assert!(g.find_node_by_name("live").is_some());
        assert!(g.find_node_by_name("dead").is_none());
    }
}

#[cfg(test)]
mod remove_by_id_tests {
    use super::*;

    /// The federated backend keys by global id, and two repos can share a file
    /// path, so removal there cannot go through the path index.
    #[test]
    fn remove_nodes_by_ids_drops_only_the_named_nodes() {
        let tmp = std::env::temp_dir().join("lain_test_rm_by_id");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();

        let a = GraphNode::new(NodeType::Function, "a".into(), "src/x.rs".into());
        let b = GraphNode::new(NodeType::Function, "b".into(), "src/x.rs".into());
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        g.insert_nodes_batch(&[a, b]).unwrap();

        let removed = g.remove_nodes_by_ids(&[a_id]).unwrap();

        assert_eq!(removed, 1);
        assert!(g.find_node_by_name("a").is_none());
        assert!(
            g.find_node_by_name("b").is_some(),
            "sibling in same file survives"
        );
        assert!(
            g.get_node(&b_id).unwrap().is_some(),
            "survivor still resolves by id"
        );
    }

    #[test]
    fn batch_removal_preserves_stable_indices_and_survivor_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let g = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
        let nodes: Vec<_> = (0..6)
            .map(|i| GraphNode::new(NodeType::Function, format!("f{i}"), format!("src/{i}.rs")))
            .collect();
        g.insert_nodes_batch(&nodes).unwrap();
        let ids = vec![
            nodes[0].id.clone(),
            nodes[4].id.clone(),
            nodes[2].id.clone(),
        ];
        assert_eq!(g.remove_nodes_by_ids(&ids).unwrap(), 3);
        for (i, node) in nodes.iter().enumerate() {
            assert_eq!(g.get_node(&node.id).unwrap().is_some(), i % 2 == 1);
            assert_eq!(g.find_node_by_path(&node.path).is_some(), i % 2 == 1);
        }
    }

    /// Removing the last node for a path must clear the path entry too, or a
    /// later lookup resolves through a vacated slot.
    #[test]
    fn remove_nodes_by_ids_clears_emptied_path_entry() {
        let tmp = std::env::temp_dir().join("lain_test_rm_path_clear");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();

        let only = GraphNode::new(NodeType::Function, "only".into(), "src/solo.rs".into());
        let id = only.id.clone();
        g.insert_nodes_batch(&[only]).unwrap();

        g.remove_nodes_by_ids(&[id]).unwrap();

        assert!(
            g.find_node_by_path("src/solo.rs").is_none(),
            "path entry cleared"
        );
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    fn node_at(path: &str, scanned_at: i64) -> GraphNode {
        let mut n = GraphNode::new(NodeType::Function, "f".into(), path.into());
        n.last_lsp_sync = Some(scanned_at);
        n
    }

    #[test]
    fn absent_when_path_has_no_nodes() {
        let tmp = std::env::temp_dir().join("lain_test_fresh_absent");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();
        assert_eq!(
            g.freshness(Path::new("/nowhere"), "src/nope.rs"),
            Freshness::Absent
        );
    }

    /// The signal that matters: a file edited but not committed is invisible to
    /// a commit-driven index, so mtime is the only thing that reveals it.
    #[test]
    fn dirty_when_file_is_newer_than_the_scan() {
        let ws = std::env::temp_dir().join("lain_test_fresh_dirty_ws");
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/a.rs"), "fn f() {}").unwrap();

        let tmp = std::env::temp_dir().join("lain_test_fresh_dirty");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();
        // Scanned long ago; the file on disk is from just now.
        g.insert_nodes_batch(&[node_at("src/a.rs", 1)]).unwrap();

        match g.freshness(&ws, "src/a.rs") {
            Freshness::Dirty { .. } => {}
            other => panic!("expected Dirty, got {other:?}"),
        }
        assert!(g.freshness(&ws, "src/a.rs").note("src/a.rs").is_some());
    }

    #[test]
    fn fresh_when_scan_is_newer_than_the_file() {
        let ws = std::env::temp_dir().join("lain_test_fresh_ok_ws");
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/b.rs"), "fn f() {}").unwrap();

        let tmp = std::env::temp_dir().join("lain_test_fresh_ok");
        let _ = std::fs::remove_dir_all(&tmp);
        let g = GraphDatabase::new(&tmp).unwrap();
        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        g.insert_nodes_batch(&[node_at("src/b.rs", far_future)])
            .unwrap();

        assert_eq!(g.freshness(&ws, "src/b.rs"), Freshness::Fresh);
        // A current file must produce no banner — a note on every answer is
        // noise, and noise gets ignored.
        assert!(g.freshness(&ws, "src/b.rs").note("src/b.rs").is_none());
    }
}

#[cfg(test)]
mod anchor_hub_tests {
    use super::*;

    fn db(name: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    fn func(name: &str, path: &str, lines: (u32, u32)) -> GraphNode {
        let mut n = GraphNode::new(NodeType::Function, name.into(), path.into());
        n.line_start = Some(lines.0);
        n.line_end = Some(lines.1);
        n
    }

    /// A trivial 1-line helper with 20 callers must rank BELOW a
    /// 30-line hub with 5 callers and 10 callees. This is the
    /// `as_str` problem: the old fan_in/(fan_out+1) formula put
    /// the helper on top; hub scoring must not.
    #[test]
    fn hub_outranks_trivial_helper() {
        let g = db("lain_test_anchor_hub");
        let helper = func("as_str", "src/util.rs", (10, 10));
        let hub = func("orchestrate", "src/core.rs", (1, 30));
        let mut nodes = vec![helper.clone(), hub.clone()];
        let mut edges = Vec::new();
        for i in 0..20 {
            let caller = func(&format!("caller{i}"), "src/a.rs", (1, 10));
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                helper.id.clone(),
            ));
            nodes.push(caller);
        }
        for i in 0..5 {
            let caller = func(&format!("hubcaller{i}"), "src/b.rs", (1, 10));
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                hub.id.clone(),
            ));
            nodes.push(caller);
        }
        for i in 0..10 {
            let callee = func(&format!("callee{i}"), "src/c.rs", (1, 10));
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                hub.id.clone(),
                callee.id.clone(),
            ));
            nodes.push(callee);
        }
        g.insert_nodes_batch(&nodes).unwrap();
        for e in edges {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let helper_score = g
            .get_node(&helper.id)
            .unwrap()
            .unwrap()
            .anchor_score
            .unwrap();
        let hub_score = g.get_node(&hub.id).unwrap().unwrap().anchor_score.unwrap();
        assert!(
            hub_score > helper_score,
            "hub ({hub_score}) should outrank trivial helper ({helper_score})"
        );
        assert_eq!(hub_score, 100.0, "hub is the corpus max, normalizes to 100");
    }

    /// Types/structs/namespaces never rank as anchors — the handler
    /// filters them out anyway, so the scorer aligns with display.
    #[test]
    fn non_functions_score_zero() {
        let g = db("lain_test_anchor_nonfn");
        let s = GraphNode::new(NodeType::Struct, "Config".into(), "src/cfg.rs".into());
        let caller = func("use_cfg", "src/a.rs", (1, 10));
        let edge = GraphEdge::new(EdgeType::Calls, caller.id.clone(), s.id.clone());
        let sid = s.id.clone();
        g.insert_nodes_batch(&[s, caller]).unwrap();
        g.upsert_edge(edge).unwrap();

        g.calculate_anchor_scores().unwrap();

        let score = g.get_node(&sid).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "struct must score 0");
    }

    /// A leaf utility called by everyone but calling nothing is NOT an
    /// orchestration hub: calls_out=0 must zero the score. Live check
    /// on the lain repo showed `as_str` (91 callers, 0 callees) still
    /// ranking top-3 when the log used `2 +` (factor 1 for leaves).
    #[test]
    fn leaf_utility_scores_zero() {
        let g = db("lain_test_anchor_leaf");
        let leaf = func("as_str", "src/util.rs", (1, 10));
        let mut nodes = vec![leaf.clone()];
        let mut edges = Vec::new();
        for i in 0..50 {
            let caller = func(&format!("caller{i}"), "src/a.rs", (1, 10));
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                leaf.id.clone(),
            ));
            nodes.push(caller);
        }
        g.insert_nodes_batch(&nodes).unwrap();
        for e in edges {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let score = g.get_node(&leaf.id).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "leaf with calls_out=0 must score 0");
    }

    /// Test helpers are hubs of the test suite, not of the product.
    /// Live check: `make_test_graph` (tests/common) ranked #1 on the
    /// lain repo. Symbols under a `tests/` path never rank as anchors,
    /// and neither do the `*_tests.rs` / `tests.rs` file-stem
    /// conventions used for `#[cfg(test)]` modules under src/.
    #[test]
    fn test_code_scores_zero() {
        let g = db("lain_test_anchor_testcode");
        let test_hub = func("make_test_graph", "tests/common/mod.rs", (1, 60));
        let cfg_test_hub = func("make_test_graph", "src/server/graph_tests.rs", (1, 60));
        let caller = func("a_test", "tests/foo.rs", (1, 20));
        let callee = func("helper", "src/util.rs", (1, 8));
        let e1 = GraphEdge::new(EdgeType::Calls, caller.id.clone(), test_hub.id.clone());
        let e2 = GraphEdge::new(EdgeType::Calls, test_hub.id.clone(), callee.id.clone());
        let e3 = GraphEdge::new(EdgeType::Calls, caller.id.clone(), cfg_test_hub.id.clone());
        let e4 = GraphEdge::new(EdgeType::Calls, cfg_test_hub.id.clone(), callee.id.clone());
        let tid = test_hub.id.clone();
        let cid = cfg_test_hub.id.clone();
        g.insert_nodes_batch(&[test_hub, cfg_test_hub, caller, callee])
            .unwrap();
        for e in [e1, e2, e3, e4] {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let score = g.get_node(&tid).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "tests/ dir symbol must score 0");
        let score = g.get_node(&cid).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "*_tests.rs (cfg(test) module) must score 0");
    }

    /// Calls FROM test code don't make a production function an
    /// orchestration hub. Live check: `Default::default` impls ranked
    /// top-3 because fifty `test_*` functions call them.
    #[test]
    fn calls_from_test_code_do_not_count() {
        let g = db("lain_test_anchor_testcallers");
        let prod = func("default", "src/config.rs", (1, 15));
        let callee = func("helper", "src/util.rs", (1, 8));
        let mut nodes = vec![prod.clone(), callee.clone()];
        // calls_out = 1 so the leaf rule alone can't zero the score;
        // only the test-caller filter can.
        let mut edges = vec![GraphEdge::new(
            EdgeType::Calls,
            prod.id.clone(),
            callee.id.clone(),
        )];
        for i in 0..30 {
            let tcaller = func(&format!("test_caller{i}"), "tests/it.rs", (1, 10));
            edges.push(GraphEdge::new(
                EdgeType::Calls,
                tcaller.id.clone(),
                prod.id.clone(),
            ));
            nodes.push(tcaller);
        }
        g.insert_nodes_batch(&nodes).unwrap();
        for e in edges {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let score = g.get_node(&prod.id).unwrap().unwrap().anchor_score.unwrap();
        assert_eq!(score, 0.0, "called only from tests must score 0");
    }

    /// A Function and a Method sharing a name are the same anchor for
    /// a reader — `parse` the fn and `parse` the method showed up as
    /// two entries on the lain repo. Dedup is by name, not (name, kind).
    #[test]
    fn same_name_function_and_method_dedup_to_one() {
        let g = db("lain_test_anchor_namededup");
        let f = func("parse", "src/a.rs", (1, 30));
        let mut m = GraphNode::new(NodeType::Method, "parse".into(), "src/b.rs".into());
        m.line_start = Some(1);
        m.line_end = Some(30);
        let hub_caller = func("caller", "src/c.rs", (1, 20));
        let callee = func("helper", "src/util.rs", (1, 8));
        // Give both `parse` nodes the same score-relevant shape.
        let e1 = GraphEdge::new(EdgeType::Calls, hub_caller.id.clone(), f.id.clone());
        let e2 = GraphEdge::new(EdgeType::Calls, hub_caller.id.clone(), m.id.clone());
        let e3 = GraphEdge::new(EdgeType::Calls, f.id.clone(), callee.id.clone());
        let e4 = GraphEdge::new(EdgeType::Calls, m.id.clone(), callee.id.clone());
        g.insert_nodes_batch(&[f, m, hub_caller, callee]).unwrap();
        for e in [e1, e2, e3, e4] {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let anchors = g.find_anchors(10).unwrap();
        let parses = anchors.iter().filter(|n| n.name == "parse").count();
        assert_eq!(parses, 1, "function+method `parse` must dedup to one entry");
    }

    /// A function with `calls_in = 0` gets a baseline weight of
    /// `size_factor * 0.5`. Without this, every raw in a fixture
    /// where the LSP path didn't pick up calls is 0; `max_raw` is
    /// 0; every normalized score is 0; the sort is unstable at
    /// zero. Wishlist #14: `find_anchors` returned 0.000 for every
    /// function in small fixtures, the order came back arbitrary.
    ///
    /// The half-strength baseline keeps dead functions visible (so
    /// the user can see what was indexed) while ensuring any
    /// function with at least one caller outranks them:
    /// `calls_in >= 1 ∧ calls_out >= 1 ⇒ raw >= size_factor`,
    /// which strictly exceeds `0.5 * size_factor`.
    #[test]
    fn dead_function_baseline_weight() {
        let g = db("lain_test_anchor_baseline");
        // Big dead function — 16 lines, so size_factor = min(16/8, 1) = 1.0.
        let big_dead = func("big_dead", "src/util.rs", (1, 16));
        // Live hub — same size_factor (1.0), 1 caller, 1 callee.
        // raw = 1 * log2(2) * 1.0 ≈ 1.0 > 0.5 * 1.0 = 0.5 (big_dead).
        let hub = func("hub", "src/core.rs", (1, 16));
        let hub_caller = func("hub_caller", "src/a.rs", (1, 10));
        let hub_callee = func("hub_callee", "src/b.rs", (1, 10));
        let bid = big_dead.id.clone();
        let hid = hub.id.clone();
        let hid2 = hub.id.clone();
        let hid3 = hub.id.clone();
        let cid = hub_caller.id.clone();
        let lid = hub_callee.id.clone();
        g.insert_nodes_batch(&[big_dead, hub, hub_caller, hub_callee])
            .unwrap();
        g.upsert_edge(GraphEdge::new(EdgeType::Calls, cid, hid2))
            .unwrap();
        g.upsert_edge(GraphEdge::new(EdgeType::Calls, hid3, lid))
            .unwrap();

        g.calculate_anchor_scores().unwrap();

        let dead_score = g.get_node(&bid).unwrap().unwrap().anchor_score.unwrap();
        let hub_score = g.get_node(&hid).unwrap().unwrap().anchor_score.unwrap();
        assert!(
            dead_score > 0.0,
            "dead function must have a non-zero baseline ({dead_score}); \
             without it, every raw is 0 in a small fixture and the sort \
             is unstable at zero"
        );
        assert!(
            hub_score > dead_score,
            "live hub ({hub_score}) must outrank dead function \
             ({dead_score}) of identical size_factor"
        );
    }
}

#[cfg(test)]
mod clone_tests {
    use super::*;

    /// The tool context holds a clone taken at startup, before a cold index
    /// runs. When the id/path indices were copied rather than shared, that
    /// clone saw the nodes (shared `graph`) but none of their ids or paths:
    /// callers, blast radius and freshness all answered "nothing" for the
    /// whole first session on a new repo.
    /// A co-change pair is visible from both files, whichever path sorts
    /// first.
    #[test]
    fn co_change_partners_are_symmetric() {
        let tmp = std::env::temp_dir().join("lain_test_cochange_symmetric");
        let _ = std::fs::remove_dir_all(&tmp);
        let db = GraphDatabase::new(&tmp).unwrap();
        let ns = crate::schema::RepoNamespace::for_test();
        let files: Vec<GraphNode> = ["src/models.py", "src/sessions.py"]
            .iter()
            .map(|p| {
                let name = Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                GraphNode::new_in(NodeType::File, name, p.to_string(), &ns)
            })
            .collect();
        db.insert_nodes_batch(&files).unwrap();
        db.insert_co_change_edges(&[("src/models.py".into(), "src/sessions.py".into(), 3)])
            .unwrap();
        assert_eq!(
            db.get_co_change_partners("src/sessions.py").unwrap(),
            vec![("src/models.py".to_string(), 3)]
        );
        assert_eq!(
            db.get_co_change_partners("src/models.py").unwrap(),
            vec![("src/sessions.py".to_string(), 3)]
        );
    }

    /// A graph written under another namespace (older Lain: random per
    /// process) is detected, and `reset` empties it for a full rebuild.
    #[test]
    fn a_graph_from_another_namespace_is_detected_and_reset() {
        let tmp = std::env::temp_dir().join("lain_test_namespace_mismatch");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut db = GraphDatabase::new(&tmp).unwrap();
        let old_ns = crate::schema::RepoNamespace::fresh();
        let file = GraphNode::new_in(NodeType::File, "a.rs".into(), "src/a.rs".into(), &old_ns);
        db.insert_nodes_batch(&[file]).unwrap();
        db.set_last_commit("abc".into()).unwrap();

        db.set_namespace(old_ns);
        assert!(!db.minted_in_other_namespace(), "same namespace");
        db.set_namespace(crate::schema::RepoNamespace::from_workspace(&tmp));
        assert!(db.minted_in_other_namespace(), "different namespace");

        // A language server's FILE-kind symbol carries its line in its id;
        // it is not a sign of another namespace.
        let ns = crate::schema::RepoNamespace::from_workspace(&tmp);
        let lsp_file = GraphNode::new_in(NodeType::File, "b.rs".into(), "src/b.rs".into(), &ns)
            .with_location_in(0, 40, &ns);
        let fresh = GraphDatabase::new(&tmp.join("fresh")).unwrap();
        let mut fresh = fresh;
        fresh.set_namespace(ns);
        fresh.insert_nodes_batch(&[lsp_file]).unwrap();
        assert!(!fresh.minted_in_other_namespace(), "LSP file symbol");

        db.reset().unwrap();
        assert_eq!(db.get_stats(), (0, 0));
        assert!(!db.has_node_at_path("src/a.rs"));
        assert_eq!(db.get_last_commit().unwrap(), None);
    }

    #[test]
    fn clone_taken_before_indexing_sees_later_writes() {
        let tmp = std::env::temp_dir().join("lain_test_clone_shares_indices");
        let _ = std::fs::remove_dir_all(&tmp);
        let writer = GraphDatabase::new(&tmp).unwrap();
        let reader = writer.clone();

        let caller = GraphNode::new(NodeType::Function, "caller".into(), "src/a.rs".into());
        let callee = GraphNode::new(NodeType::Function, "callee".into(), "src/a.rs".into());
        let (caller_id, callee_id) = (caller.id.clone(), callee.id.clone());
        writer.insert_nodes_batch(&[caller, callee]).unwrap();
        writer
            .insert_edges_batch(&[GraphEdge::new(
                EdgeType::Calls,
                caller_id.clone(),
                callee_id.clone(),
            )])
            .unwrap();

        assert!(reader.get_node(&callee_id).unwrap().is_some(), "id lookup");
        let incoming = reader.get_edges_to(&callee_id).unwrap();
        assert_eq!(incoming.len(), 1, "edge visible through the clone");
        assert_eq!(incoming[0].source_id, caller_id);
        assert!(
            reader.has_node_at_path("src/a.rs"),
            "path index visible through the clone"
        );
    }
}
