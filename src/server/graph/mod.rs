//! Stable In-Memory Graph Database using petgraph
//!
//! Uses petgraph's StableGraph for robust graph operations and
//! bincode for high-performance binary persistence. The on-disk
//! representation, version checks, and read-only inspection live
//! in [`persist`].

mod persist;

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
pub fn graph_path(workspace: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(workspace).unwrap_or(path);
    crate::server::path_util::posix_string(rel)
}

#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,
    // The three secondary indexes are wrapped in `Arc` so that
    // `GraphDatabase::clone()` (the derive) shares the underlying
    // index state across handles — the `graph` field is already
    // `Arc<RwLock<…>>` for the same reason. `DashMap::clone()` is a
    // deep copy that builds new shards, so a plain `DashMap` field
    // would diverge between two clones of the same database: the
    // federation tests already depend on `db().clone()` and
    // `db().upsert_node(...)` reflecting through subsequent lookups
    // via the original handle. `Arc<DashMap<…>>` makes that work.
    index_map: Arc<DashMap<String, NodeIndex>>,
    path_index: Arc<DashMap<String, Vec<NodeIndex>>>,
    /// A3 — secondary index keyed on `node.name`. Maintained under the same
    /// write lock that updates `index_map` and `path_index`, so a reader that
    /// consults it under the graph read lock sees the old pair or the new
    /// pair, never a mix. The hot call site is `resolve_static_edges` in
    /// the indexing pipeline, which used to scan every node on every pass
    /// (`db.get_all_nodes()`) to build a `HashMap<name, …>` for name-keyed
    /// ref resolution; with this index that scan becomes an O(1) lookup
    /// per ref.
    name_index: Arc<DashMap<String, Vec<NodeIndex>>>,
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
            name_index: Arc::new(DashMap::new()),
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
            let name = node.name.clone();
            let idx = graph.add_node(node.clone());
            self.index_map.insert(node.id.clone(), idx);
            self.path_index.entry(path).or_default().push(idx);
            self.name_index.entry(name).or_default().push(idx);
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
                    let name = node.name.clone();
                    let idx = graph.add_node(node.clone());
                    // Pre-fix this was deferred to Phase 2 (after
                    // `drop(graph)`), creating the race window. Move
                    // it into Phase 1 — still under the same write
                    // lock as the petgraph insert.
                    self.index_map.insert(node.id.clone(), idx);
                    self.path_index.entry(path.clone()).or_default().push(idx);
                    self.name_index.entry(name).or_default().push(idx);
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
                            let name = n.name.clone();
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
                            // A3 — same rationale for `name_index`: a
                            // node that genuinely goes away must drop
                            // its name entry too. The replacement
                            // re-inserts under the same id (deterministic)
                            // and the same name, so the entry is rebuilt
                            // by the loop below — but a *different* node
                            // that subsequently takes this name (e.g. a
                            // re-introduced symbol with the same name in
                            // a different file) would otherwise leak an
                            // entry pointing at a vacated slot.
                            let name_now_empty =
                                if let Some(mut entry) = self.name_index.get_mut(&name) {
                                    entry.retain(|i| *i != idx);
                                    entry.is_empty()
                                } else {
                                    false
                                };
                            if name_now_empty {
                                self.name_index.remove(&name);
                            }
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
                    let idx = graph.add_node((*node).clone());
                    self.index_map.insert(node.id.clone(), idx);
                    // A3 — mirror the id/path insertion in the
                    // `name_index`. The remove loop above already
                    // dropped the stale name entry for any node that
                    // went away under the same name, so this re-adds
                    // it cleanly; for genuinely new names it is the
                    // first insertion.
                    self.name_index
                        .entry(node.name.clone())
                        .or_default()
                        .push(idx);
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
        let mut cleared_names: Vec<(String, NodeIndex)> = Vec::new();
        {
            let mut graph = self.graph.write();
            for id in ids {
                let Some(idx) = self.index_map.get(id).map(|r| *r.value()) else {
                    continue;
                };
                if let Some(node) = graph.node_weight(idx) {
                    cleared_paths.push((node.path.clone(), idx));
                    // A3 — mirror the path bookkeeping for the name
                    // index so a removed id doesn't leave a name
                    // entry pointing at a vacated slot.
                    cleared_names.push((node.name.clone(), idx));
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
        // A3 — same cleanup for name_index.
        for (name, idx) in cleared_names {
            let now_empty = if let Some(mut entry) = self.name_index.get_mut(&name) {
                entry.retain(|i| *i != idx);
                entry.is_empty()
            } else {
                false
            };
            if now_empty {
                self.name_index.remove(&name);
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
                    graph.add_edge(s, t, edge.clone());
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

    /// A5 — hand out a read guard on the underlying `StableGraph` so
    /// callers that already hold a `NodeIndex` (e.g. from
    /// `find_indices_by_name`) can resolve it to a node weight
    /// without going back through the id-keyed `index_map`. The guard
    /// is released when the caller's scope exits; concurrent writers
    /// block until then.
    ///
    /// Read-only callers in the indexing pipeline prefer this over
    /// `get_node`/`get_node_by_id` because it avoids the extra
    /// `DashMap` lookup — the resolver already has the index.
    pub fn graph_ref_for_read(
        &self,
    ) -> parking_lot::RwLockReadGuard<'_, StableGraph<GraphNode, GraphEdge>> {
        self.graph.read()
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
        // A3 — route through `name_index` instead of a full
        // `node_weights()` scan. The index returns the `NodeIndex`
        // values for the name; we still clone the node weights and
        // sort them so the output order matches the previous contract.
        let graph = self.graph.read();
        let mut hits: Vec<GraphNode> = self
            .name_index
            .get(name)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|idx| graph.node_weight(*idx).cloned())
                    .collect()
            })
            .unwrap_or_default();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
        hits
    }

    /// A3 — snapshot of the `NodeIndex` values for `name`. Used by the
    /// indexing resolve phase to look up name-keyed refs without cloning
    /// every node in the graph. Returns an empty vec if the name has
    /// no entries — same contract as the previous `get_all_nodes()`
    /// filter.
    pub fn find_indices_by_name(&self, name: &str) -> Vec<NodeIndex> {
        self.name_index
            .get(name)
            .map(|indices| indices.value().clone())
            .unwrap_or_default()
    }

    pub fn find_node_by_path(&self, path: &str) -> Option<GraphNode> {
        // A4: route through `path_index` instead of a full `node_weights()`
        // scan. `has_node_at_path` already does this; the only difference
        // here is what we return. Falls back to None when the path has no
        // entries in the index — same contract as the linear scan.
        let graph = self.graph.read();
        self.path_index.get(path).and_then(|indices| {
            indices
                .iter()
                .find_map(|idx| graph.node_weight(*idx).cloned())
        })
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
        // A6 — split into a read-only compute phase (no graph write
        // lock; runs `par_iter` over nodes for free parallelism on the
        // hot Pass-1 walk) and a write phase (one lock, applies
        // scores). The graph is read-only during the compute phase;
        // the indexing pipeline is the single writer, so the invariant
        // holds.
        //
        // The function's contract is unchanged: scores end up on the
        // node weights, the top symbol normalizes to 100.0, anchor
        // semantics (Calls-only, test-path filtered, log2(1+calls_out),
        // dead-function baseline) are preserved verbatim.
        let (raws, max_raw) = self.compute_anchor_raws()?;
        self.write_anchor_scores(&raws, max_raw)?;
        Ok(())
    }

    /// A6 — compute the raw hub score for every node and the
    /// corpus-wide max. Read-only: takes no write lock, parallel over
    /// node indices via rayon. The two-pass design (compute max,
    /// normalize) requires the full raws Vec plus the max; both come
    /// out of this single pass because the max is just a fold over
    /// the per-node raws.
    ///
    /// Hub semantics preserved from the previous inline version:
    ///
    ///   raw = calls_in * log2(1 + calls_out) * size_factor
    ///   size_factor = min(1, body_lines / 8)
    ///
    /// Test-path symbols and edges with test-path endpoints are
    /// excluded. Functions with zero callers get a half-strength
    /// baseline (`size_factor * 0.5`) so dead code stays visible
    /// without outranking anything that has at least one caller.
    /// Pinned by `anchor_hub_tests::*`.
    fn compute_anchor_raws(&self) -> Result<(Vec<(NodeIndex, f32)>, f32), LainError> {
        use rayon::prelude::*;

        let graph = self.graph.read();
        let indices: Vec<NodeIndex> = graph.node_indices().collect();

        let raws: Vec<(NodeIndex, f32)> = indices
            .par_iter()
            .map(|&idx| {
                let node = &graph[idx];
                if is_test_path(&node.path) {
                    return (idx, 0.0);
                }
                let raw = match node.node_type {
                    NodeType::Function | NodeType::Method => {
                        let calls_in_unfiltered = graph
                            .edges_directed(idx, Direction::Incoming)
                            .filter(|e| e.weight().edge_type == EdgeType::Calls)
                            .count() as f32;
                        let calls_in = graph
                            .edges_directed(idx, Direction::Incoming)
                            .filter(|e| e.weight().edge_type == EdgeType::Calls)
                            .filter(|e| !is_test_path(&graph[e.source()].path))
                            .count() as f32;
                        let calls_out = graph
                            .edges_directed(idx, Direction::Outgoing)
                            .filter(|e| e.weight().edge_type == EdgeType::Calls)
                            .filter(|e| !is_test_path(&graph[e.target()].path))
                            .count() as f32;
                        let body_lines = match (node.line_start, node.line_end) {
                            (Some(s), Some(e)) => e.saturating_sub(s) as f32 + 1.0,
                            _ => 1.0,
                        };
                        let size_factor = (body_lines / 8.0).min(1.0);
                        if calls_in_unfiltered == 0.0 {
                            size_factor * 0.5
                        } else {
                            calls_in * (1.0 + calls_out).log2() * size_factor
                        }
                    }
                    _ => 0.0,
                };
                (idx, raw)
            })
            .collect();

        let max_raw = raws.iter().map(|(_, r)| *r).fold(0.0f32, f32::max);
        Ok((raws, max_raw))
    }

    /// A6 — apply the normalized anchor score plus fan_in / fan_out /
    /// calls_in / calls_out to every node. Takes one write lock for
    /// the whole pass; the per-node writes are O(1) and the lock is
    /// uncontended outside the indexing pipeline.
    fn write_anchor_scores(
        &self,
        raws: &[(NodeIndex, f32)],
        max_raw: f32,
    ) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        for (idx, raw) in raws {
            let fan_in = graph.neighbors_directed(*idx, Direction::Incoming).count() as u32;
            let fan_out = graph.neighbors_directed(*idx, Direction::Outgoing).count() as u32;
            let calls_in = graph
                .edges_directed(*idx, Direction::Incoming)
                .filter(|e| e.weight().edge_type == EdgeType::Calls)
                .count() as u32;
            let calls_out = graph
                .edges_directed(*idx, Direction::Outgoing)
                .filter(|e| e.weight().edge_type == EdgeType::Calls)
                .count() as u32;
            let normalized = if max_raw > 0.0 {
                raw / max_raw * 100.0
            } else {
                0.0
            };
            if let Some(node) = graph.node_weight_mut(*idx) {
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
        let mut by_path: HashMap<String, usize> = HashMap::new();
        for e in graph
            .edges_directed(idx, Direction::Outgoing)
            .filter(|e| e.weight().edge_type == EdgeType::CoChangedWith)
        {
            let target_node = &graph[e.target()];
            let count = e.weight().weight.unwrap_or(0.0) as usize;
            let slot = by_path.entry(target_node.path.clone()).or_insert(0);
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

        let mut path_index = HashMap::new();
        for (idx, node) in state.graph.node_references() {
            path_index
                .entry(node.path.clone())
                .or_insert_with(Vec::new)
                .push(idx);
        }

        *self.graph.write() = state.graph;
        self.index_map.clear();
        for (k, v) in state.index_map {
            self.index_map.insert(k, v);
        }
        self.path_index.clear();
        for (k, v) in path_index {
            self.path_index.insert(k, v);
        }
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
mod name_index_tests {
    //! A3 — invariants for the secondary `name_index` on `GraphDatabase`.
    //!
    //! The index is a thin lookup accelerator for the resolve phase; it
    //! must stay in sync with the graph through every mutation path
    //! (`insert_nodes_batch`, `replace_nodes_for_paths`,
    //! `remove_nodes_by_ids`, `upsert_node`). These tests pin that
    //! invariant: a stale entry that points at a vacated slot, or a
    //! missing entry for a live node, would either miss real refs or
    //! manufacture edges to ghosts. Either failure mode is silent at
    //! the call site, so we pin both halves explicitly.

    use super::*;
    use crate::schema::{GraphNode, NodeType};

    fn db(name: &str) -> GraphDatabase {
        let tmp = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&tmp);
        GraphDatabase::new(&tmp).unwrap()
    }

    fn func(name: &str, path: &str) -> GraphNode {
        GraphNode::new(NodeType::Function, name.into(), path.into())
    }

    /// `find_indices_by_name` returns the indices that were inserted,
    /// and `find_all_nodes_by_name` returns the matching node set in
    /// the same (path, id) order the previous linear-scan implementation
    /// produced.
    #[test]
    fn name_index_reflects_insert_nodes_batch() {
        let g = db("lain_test_name_index_insert");
        let alpha = func("alpha", "src/a.rs");
        let beta = func("beta", "src/b.rs");
        g.insert_nodes_batch(&[alpha.clone(), beta.clone()])
            .unwrap();

        let alpha_idx = g.find_indices_by_name("alpha");
        assert_eq!(
            alpha_idx.len(),
            1,
            "alpha must have exactly one entry in the name index"
        );
        let alpha_hits = g.find_all_nodes_by_name("alpha");
        assert_eq!(
            alpha_hits.len(),
            1,
            "alpha must surface from the name-keyed lookup"
        );
        assert_eq!(
            alpha_hits[0].id, alpha.id,
            "the lookup must return alpha's actual node"
        );

        let beta_idx = g.find_indices_by_name("beta");
        assert_eq!(beta_idx.len(), 1);
        assert_ne!(
            alpha_idx[0], beta_idx[0],
            "distinct nodes -> distinct indices"
        );

        assert!(
            g.find_indices_by_name("nonexistent").is_empty(),
            "unknown names must return an empty vec"
        );
    }

    /// Two nodes sharing the same name (common in the wild — eleven
    /// `fn parse` in this repo) must coexist in the name index with
    /// both indices present.
    #[test]
    fn name_index_handles_shared_names_across_paths() {
        let g = db("lain_test_name_index_shared");
        let p1 = func("parse", "src/a.rs");
        let p2 = func("parse", "src/b.rs");
        let p3 = func("parse", "src/c.rs");
        g.insert_nodes_batch(&[p1.clone(), p2.clone(), p3.clone()])
            .unwrap();

        let parse_indices = g.find_indices_by_name("parse");
        assert_eq!(
            parse_indices.len(),
            3,
            "all three `parse` nodes must be in the name index; got {}",
            parse_indices.len()
        );

        let nodes = g.find_all_nodes_by_name("parse");
        assert_eq!(nodes.len(), 3);
        // Sorted by (path, id) — same order the previous linear scan produced.
        assert_eq!(nodes[0].path, "src/a.rs");
        assert_eq!(nodes[1].path, "src/b.rs");
        assert_eq!(nodes[2].path, "src/c.rs");
    }

    /// Re-scanning a file replaces its nodes under the same id (deterministic)
    /// and the same name. The remove loop above must drop the stale name
    /// entry, and the insert loop must re-add it.
    #[test]
    fn name_index_stays_in_sync_after_replace_nodes_for_paths() {
        let g = db("lain_test_name_index_replace");
        let alpha = func("alpha", "src/a.rs");
        let beta = func("beta", "src/a.rs");
        g.insert_nodes_batch(&[alpha.clone(), beta.clone()])
            .unwrap();

        g.replace_nodes_for_paths(&["src/a.rs".to_string()], std::slice::from_ref(&alpha))
            .unwrap();

        let alpha_idx = g.find_indices_by_name("alpha");
        let beta_idx = g.find_indices_by_name("beta");
        assert_eq!(
            alpha_idx.len(),
            1,
            "alpha survives the replace and the name index reflects it"
        );
        assert!(
            beta_idx.is_empty(),
            "beta was removed; its name entry must be gone — got {} entries",
            beta_idx.len()
        );
    }

    /// A symbol that goes away under a name that another surviving node
    /// also uses must leave the name index with the right survivors, not
    /// leak the removed index.
    #[test]
    fn name_index_does_not_leak_removed_indices_on_replace() {
        let g = db("lain_test_name_index_replace_shared");
        let p1 = func("parse", "src/a.rs");
        let p2 = func("parse", "src/b.rs");
        g.insert_nodes_batch(&[p1.clone(), p2.clone()]).unwrap();
        let p2_idx_before = g.find_indices_by_name("parse")[1];
        assert_eq!(g.find_indices_by_name("parse").len(), 2);

        g.replace_nodes_for_paths(&["src/a.rs".to_string()], &[])
            .unwrap();

        let parse_idx = g.find_indices_by_name("parse");
        assert_eq!(
            parse_idx.len(),
            1,
            "the survivor must remain; got {} entries",
            parse_idx.len()
        );
        assert_eq!(
            parse_idx[0], p2_idx_before,
            "the survivor index must be the original src/b.rs parse"
        );
    }

    /// `remove_nodes_by_ids` mirrors the path_index cleanup for the name
    /// index. A removed id must drop its name entry.
    #[test]
    fn name_index_stays_in_sync_after_remove_nodes_by_ids() {
        let g = db("lain_test_name_index_remove_by_id");
        let a = func("alpha", "src/a.rs");
        let b = func("alpha", "src/b.rs");
        g.insert_nodes_batch(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(g.find_indices_by_name("alpha").len(), 2);

        g.remove_nodes_by_ids(std::slice::from_ref(&a.id)).unwrap();

        let alpha_idx = g.find_indices_by_name("alpha");
        assert_eq!(
            alpha_idx.len(),
            1,
            "one alpha remains; name index must reflect exactly that"
        );
        let survivors = g.find_all_nodes_by_name("alpha");
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].id, b.id);
    }

    /// `upsert_node` (the single-node sibling of `insert_nodes_batch`)
    /// must keep the index in sync too. The federated backend uses this
    /// path, so a regression here would corrupt cross-repo lookups.
    /// The federation resolver regression test
    /// (`runtime_trace::server::tests::federation_resolver_narrows_via_code_repo_attribute`)
    /// exercises the cross-clone propagation end-to-end; this unit test
    /// pins the single-handle invariant.
    #[test]
    fn name_index_stays_in_sync_after_upsert_node() {
        let g = db("lain_test_name_index_upsert");
        g.upsert_node(func("alpha", "src/a.rs")).unwrap();
        assert_eq!(g.find_indices_by_name("alpha").len(), 1);

        // Upsert with the same id (deterministic) but a different path
        // — this is the rename case. Same name, same id, different
        // path; the name entry stays (id is the key, name is the
        // secondary index key), path entry updates.
        let mut renamed = func("alpha", "src/c.rs");
        renamed.id = func("alpha", "src/a.rs").id;
        g.upsert_node(renamed).unwrap();
        assert_eq!(
            g.find_indices_by_name("alpha").len(),
            1,
            "rename under the same id/name keeps the entry count at 1"
        );
    }

    /// Cloning the database must share the name index (Arc<DashMap<…>>).
    /// The federation tests rely on this — the per-repo db is cloned
    /// out of `FederatedIndex` and indexed via `upsert_node`, and
    /// subsequent reads via the original handle must see those writes.
    /// DashMap's `Clone` is a deep copy of the shards; without the
    /// `Arc` wrap, mutations would diverge across handles and the
    /// resolver would silently miss every node.
    #[test]
    fn name_index_propagates_across_clones() {
        let g = db("lain_test_name_index_clone");
        let handle = g.clone();
        g.upsert_node(func("alpha", "src/a.rs")).unwrap();

        let via_handle = handle.find_indices_by_name("alpha");
        assert_eq!(
            via_handle.len(),
            1,
            "a clone must see the upsert; got {} entries",
            via_handle.len()
        );
    }
}
