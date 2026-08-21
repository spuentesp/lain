//! Stable In-Memory Graph Database using petgraph
//!
//! Uses petgraph's StableGraph for robust graph operations and 
//! bincode for high-performance binary persistence.

use crate::error::LainError;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use petgraph::stable_graph::{StableGraph, NodeIndex};
use petgraph::visit::{EdgeRef, IntoNodeReferences};
use petgraph::Direction;
use tracing::warn;

/// Bumped whenever the meaning of `GraphNode.path` changes. Version 2 is
/// the switch from mixed absolute/relative paths to a single
/// workspace-relative form. A graph written by an older lain deserializes
/// fine (the bincode layout is unchanged) but its keys are absolute, so
/// merging it into a v2 graph would double every node instead of updating
/// it. `load_from_disk` therefore discards anything that isn't v2 and lets
/// the caller rebuild from source.
pub const PATH_FORMAT_VERSION: u32 = 2;

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
    let s = rel.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

#[derive(Serialize, Deserialize)]
struct GraphState {
    graph: StableGraph<GraphNode, GraphEdge>,
    index_map: HashMap<String, NodeIndex>,
    last_commit: Option<String>,
    /// Absent in graphs written before the canonical-path change; serde
    /// defaults it to 0, which fails the version check and forces a rebuild.
    #[serde(default)]
    path_format_version: u32,
}

#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,
    index_map: DashMap<String, NodeIndex>,
    path_index: DashMap<String, Vec<NodeIndex>>,
    last_commit: Arc<RwLock<Option<String>>>,
    persistence_path: PathBuf,
    /// When true, every public `insert_*` / `set_*` / `save_to_disk` returns
    /// `LainError::Other("graph is read-only")`. Set by `open_read_only`,
    /// used by sidecar processes that subscribe to an owner's overlay
    /// stream and never mutate the static graph on disk.
    read_only: bool,
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
        let db = Self {
            graph: Arc::new(RwLock::new(StableGraph::new())),
            index_map: DashMap::new(),
            path_index: DashMap::new(),
            last_commit: Arc::new(RwLock::new(None)),
            persistence_path: memory_path.to_path_buf(),
            read_only: false,
        };

        if memory_path.exists() {
            db.load_from_disk()?;
        }
        Ok(db)
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
        use rayon::prelude::*;

        self.check_writable()?;
        // Phase 1: Collect indices and path entries under graph lock
        let mut graph = self.graph.write();

        // Collect work for parallel DashMap updates: (node_id, path, idx)
        let dash_work: Vec<(String, String, NodeIndex)> = new_nodes.iter().filter_map(|node| {
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
                Some((node.id.clone(), path, idx))
            }
        }).collect();

        // Release graph lock before parallel DashMap updates
        drop(graph);

        // Phase 2: Parallel DashMap updates (sharded internally, no contention)
        dash_work.into_par_iter().for_each(|(id, path, idx)| {
            self.index_map.insert(id, idx);
            self.path_index.entry(path).or_default().push(idx);
        });

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
            for (path, indices) in new_entries {
                if indices.is_empty() {
                    self.path_index.remove(&path);
                } else {
                    self.path_index.insert(path, indices);
                }
            }
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
    /// Callers must skip the sweep when `tracked` is empty (git failed) and
    /// when the index pass was partial (files simply not visited yet).
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

        let source_idx = self.index_map.get(&edge.source_id)
            .map(|r| *r.value())
            .ok_or_else(|| LainError::NotFound(format!("Source node {} not found", edge.source_id)))?;
        let target_idx = self.index_map.get(&edge.target_id)
            .map(|r| *r.value())
            .ok_or_else(|| LainError::NotFound(format!("Target node {} not found", edge.target_id)))?;

        graph.add_edge(source_idx, target_idx, edge.clone());
        Ok(())
    }

    pub fn insert_edges_batch(&self, new_edges: &[GraphEdge]) -> Result<(), LainError> {
        self.check_writable()?;
        let mut graph = self.graph.write();

        for edge in new_edges {
            if let (Some(s), Some(t)) = (
                self.index_map.get(&edge.source_id).map(|r| *r.value()),
                self.index_map.get(&edge.target_id).map(|r| *r.value())
            ) {
                graph.add_edge(s, t, edge.clone());
            }
        }
        Ok(())
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
            if graph.edges_connecting(s, t).any(|e| e.weight().edge_type == edge.edge_type) {
                return Ok(());
            }
        }
        drop(graph);
        self.insert_edge(&edge)
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        let graph = self.graph.read();

        Ok(self.index_map.get(id).and_then(|r| graph.node_weight(*r.value()).cloned()))
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
                if !parents.contains_key(&next) {
                    parents.insert(next, Some(current));
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
        Ok(indices.into_iter().filter_map(|idx| graph.node_weight(idx).cloned()).collect())
    }

    pub fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
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
        Ok(indices.into_iter().filter_map(|idx| {
            let node = graph.node_weight(idx).cloned()?;
            let edges = graph.edges_directed(idx, Direction::Outgoing)
                .filter(|e| selected.contains(&e.target()))
                .map(|e| e.weight().clone())
                .collect();
            Some((node, edges))
        }).collect())
    }

    pub fn node_count(&self) -> usize {
        self.graph.read().node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.read().edge_count()
    }

    pub fn get_nodes_by_type(&self, node_type: NodeType) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        Ok(graph.node_weights()
            .filter(|n| n.node_type == node_type)
            .cloned()
            .collect())
    }

    /// Get nodes matching any of the given node types in a single graph traversal
    pub fn get_nodes_by_types(&self, node_types: &[NodeType]) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        Ok(graph.node_weights()
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
        graph.node_references().map(|(_, node)| node.clone()).collect()
    }

    pub fn all_edges(&self) -> Vec<GraphEdge> {
        let graph = self.graph.read();
        graph.edge_weights().cloned().collect()
    }

    pub fn find_node_by_name(&self, name: &str) -> Option<GraphNode> {
        self.graph.read().node_weights().find(|n| n.name == name).cloned()
    }

    pub fn find_node_by_path(&self, path: &str) -> Option<GraphNode> {
        self.graph.read().node_weights().find(|n| n.path == path).cloned()
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
        graph.node_weights()
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
                    let label = if n.is_deprecated { Some("deprecated") } else { None };
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
    pub fn get_neighbors(&self, node_id: &str, direction: Direction) -> Vec<(GraphNode, GraphEdge)> {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(node_id).map(|r| *r.value()) else { return Vec::new(); };

        graph.edges_directed(idx, direction)
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
    pub fn bfs_from(
        &self,
        start_id: &str,
        max_depth: u32,
    ) -> Vec<(GraphNode, GraphEdge, u32)> {
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

        let Some(idx) = self.index_map.get(source_id).map(|r| *r.value()) else { return Ok(Vec::new()); };

        Ok(graph.edges_directed(idx, Direction::Outgoing)
            .map(|e| e.weight().clone())
            .collect())
    }

    pub fn get_edges_to(&self, target_id: &str) -> Result<Vec<GraphEdge>, LainError> {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(target_id).map(|r| *r.value()) else { return Ok(Vec::new()); };

        Ok(graph.edges_directed(idx, Direction::Incoming)
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
        // Hub semantics (spec 2026-08-21-anchor-hub-scoring-design):
        // an anchor is an ORCHESTRATION hub — called by many
        // (calls_in), coordinating many (calls_out), with a real
        // body (size_factor). Only Calls edges count: the old
        // fan_in/(fan_out+1) counted every edge type (including
        // Contains from the parent file) and actively punished
        // fan_out, which is backwards for hubs — it put 1-line
        // helpers like `as_str` at the top of find_anchors.
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
                    // log2(1 + calls_out): a leaf that calls nothing is
                    // not an orchestration hub and scores 0 — no matter
                    // how many callers it has (the `as_str` problem).
                    calls_in * (1.0 + calls_out).log2() * size_factor
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
        let mut current_layer: Vec<NodeIndex> = graph.node_indices()
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
                if visited.contains_key(&idx) { continue; }
                visited.insert(idx, depth);
                
                if let Some(node) = graph.node_weight_mut(idx) {
                    node.depth_from_main = Some(depth);
                }
                
                // Find children via Contains edges
                let children: Vec<_> = graph.edges_directed(idx, Direction::Outgoing)
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
        Ok(graph.node_weights()
            .filter(|n| n.name == "main" || n.name == "App")
            .cloned()
            .collect())
    }

    pub fn insert_co_change_edges(&self, pairs: &[(String, String, usize)]) -> Result<(), LainError> {
        let mut edges = Vec::new();
        for (p1, p2, count) in pairs {
            let filename1 = Path::new(p1).file_name().unwrap_or_default().to_string_lossy().to_string();
            let filename2 = Path::new(p2).file_name().unwrap_or_default().to_string_lossy().to_string();
            
            let id1 = GraphNode::generate_id(&NodeType::File, p1, &filename1, None);
            let id2 = GraphNode::generate_id(&NodeType::File, p2, &filename2, None);
            
            let mut edge = GraphEdge::new(EdgeType::CoChangedWith, id1, id2);
            edge.weight = Some(*count as f32);
            edges.push(edge);
        }
        // Use batch insertion which is inherently resilient to missing nodes
        self.insert_edges_batch(&edges)
    }

    pub fn get_co_change_partners(&self, file_path: &str) -> Result<Vec<(String, usize)>, LainError> {
        let graph = self.graph.read();

        let filename = Path::new(file_path).file_name().unwrap_or_default().to_string_lossy().to_string();
        let id = GraphNode::generate_id(&NodeType::File, file_path, &filename, None);
        let Some(idx) = self.index_map.get(&id).map(|r| *r.value()) else { return Ok(Vec::new()); };

        Ok(graph.edges_directed(idx, Direction::Outgoing)
            .filter(|e| e.weight().edge_type == EdgeType::CoChangedWith)
            .map(|e| {
                let target_node = &graph[e.target()];
                (target_node.path.clone(), e.weight().weight.unwrap_or(0.0) as usize)
            })
            .collect())
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
        use petgraph::visit::{EdgeRef, IntoEdgeReferences};
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
            indices.iter()
                .filter_map(|&idx| graph.node_weight(idx))
                .filter(|n| n.node_type != NodeType::File)
                .filter(|n| n.line_start.unwrap_or(0) <= line && n.line_end.unwrap_or(0) >= line)
                .min_by_key(|n| n.line_end.unwrap_or(0).saturating_sub(n.line_start.unwrap_or(0)))
                .cloned()
        } else {
            None
        }
    }

    pub fn has_references_from(&self, id: &str) -> bool {
        let graph = self.graph.read();

        let Some(idx) = self.index_map.get(id).map(|r| *r.value()) else { return false; };

        graph.edges_directed(idx, Direction::Outgoing)
            .any(|e| e.weight().edge_type == EdgeType::Calls || e.weight().edge_type == EdgeType::Uses)
    }

    /// Save graph to disk asynchronously (non-blocking)
    pub async fn save_to_disk(&self) -> Result<(), LainError> {
        self.check_writable()?;
        // Clone state under lock (fast)
        let (data, tmp_path, persistence_path) = {
            let state = GraphState {
                path_format_version: PATH_FORMAT_VERSION,
                graph: self.graph.read().clone(),
                index_map: self.index_map.iter().map(|r| (r.key().clone(), *r.value())).collect(),
                last_commit: self.last_commit.read().clone(),
            };
            let data = bincode::serialize(&state).map_err(|e| LainError::Database(e.to_string()))?;
            let tmp_path = self.persistence_path.with_extension("tmp");
            let persistence_path = self.persistence_path.clone();
            (data, tmp_path, persistence_path)
        };

        // Create parent dir and write file (I/O - async)
        if let Some(parent) = persistence_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| LainError::Database(e.to_string()))?;
        }

        // Atomic save: write to .tmp and rename
        tokio::fs::write(&tmp_path, data).await.map_err(|e| LainError::Database(e.to_string()))?;
        tokio::fs::rename(&tmp_path, &persistence_path).await.map_err(|e| LainError::Database(e.to_string()))?;

        Ok(())
    }

    pub fn save_to_disk_sync(&self) -> Result<(), LainError> {
        let state = GraphState {
                path_format_version: PATH_FORMAT_VERSION,
            graph: self.graph.read().clone(),
            index_map: self.index_map.iter().map(|r| (r.key().clone(), *r.value())).collect(),
            last_commit: self.last_commit.read().clone(),
        };
        let data = bincode::serialize(&state).map_err(|e| LainError::Database(e.to_string()))?;
        if let Some(parent) = self.persistence_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LainError::Database(e.to_string()))?;
        }
        let tmp_path = self.persistence_path.with_extension("tmp");
        std::fs::write(&tmp_path, data).map_err(|e| LainError::Database(e.to_string()))?;
        std::fs::rename(&tmp_path, &self.persistence_path).map_err(|e| LainError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn load_from_disk(&self) -> Result<(), LainError> {
        // load_from_disk is allowed on read-only graphs — it's how we hydrate
        // the static sidecar view from the owner's on-disk snapshot. Only
        // *mutations* are gated by `check_writable`.
        let data = std::fs::read(&self.persistence_path).map_err(|e| LainError::Database(e.to_string()))?;

        // Fail soft. A graph we cannot read is not a fatal condition: the
        // source tree is the source of truth and the caller re-indexes. This
        // path used to `?` the deserialize error straight out of
        // `GraphDatabase::new`, which turned any format change into a startup
        // crash instead of a rebuild.
        let state: GraphState = match bincode::deserialize(&data) {
            Ok(state) => state,
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
            path_index.entry(node.path.clone()).or_insert_with(Vec::new).push(idx);
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
        let state = GraphState {
                path_format_version: PATH_FORMAT_VERSION,
            graph: self.graph.read().clone(),
            index_map: self.index_map.iter().map(|r| (r.key().clone(), *r.value())).collect(),
            last_commit: self.last_commit.read().clone(),
        };
        serde_json::to_string_pretty(&state).map_err(|e| LainError::Database(e.to_string()))
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

        g.replace_nodes_for_paths(&["src/gone.rs".to_string()], &[]).unwrap();

        assert!(g.find_node_by_name("gone").is_none());
        assert!(g.find_node_by_name("keep").is_some(), "other files untouched");
    }

    /// Namespace nodes are directory-scoped and shared by every file beneath
    /// them, so a per-file replace must leave them alone.
    #[test]
    fn replace_preserves_namespace_nodes() {
        let g = db("lain_test_replace_ns");
        let ns = GraphNode::new(NodeType::Namespace, "src".into(), "src".into());
        g.insert_nodes_batch(&[ns]).unwrap();

        g.replace_nodes_for_paths(&["src".to_string()], &[]).unwrap();

        assert!(g.find_node_by_name("src").is_some(), "namespace must survive");
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
        assert!(g.find_node_by_name("b").is_some(), "sibling in same file survives");
        assert!(g.get_node(&b_id).unwrap().is_some(), "survivor still resolves by id");
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

        assert!(g.find_node_by_path("src/solo.rs").is_none(), "path entry cleared");
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
        assert_eq!(g.freshness(Path::new("/nowhere"), "src/nope.rs"), Freshness::Absent);
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
        g.insert_nodes_batch(&[node_at("src/b.rs", far_future)]).unwrap();

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
            edges.push(GraphEdge::new(EdgeType::Calls, caller.id.clone(), helper.id.clone()));
            nodes.push(caller);
        }
        for i in 0..5 {
            let caller = func(&format!("hubcaller{i}"), "src/b.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, caller.id.clone(), hub.id.clone()));
            nodes.push(caller);
        }
        for i in 0..10 {
            let callee = func(&format!("callee{i}"), "src/c.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, hub.id.clone(), callee.id.clone()));
            nodes.push(callee);
        }
        g.insert_nodes_batch(&nodes).unwrap();
        for e in edges {
            g.upsert_edge(e).unwrap();
        }

        g.calculate_anchor_scores().unwrap();

        let helper_score = g.get_node(&helper.id).unwrap().unwrap().anchor_score.unwrap();
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
            edges.push(GraphEdge::new(EdgeType::Calls, caller.id.clone(), leaf.id.clone()));
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
        g.insert_nodes_batch(&[test_hub, cfg_test_hub, caller, callee]).unwrap();
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
        let mut edges = vec![GraphEdge::new(EdgeType::Calls, prod.id.clone(), callee.id.clone())];
        for i in 0..30 {
            let tcaller = func(&format!("test_caller{i}"), "tests/it.rs", (1, 10));
            edges.push(GraphEdge::new(EdgeType::Calls, tcaller.id.clone(), prod.id.clone()));
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
}
