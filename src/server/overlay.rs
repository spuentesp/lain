//! Volatile overlay using petgraph
//!
//! In-memory graph that mirrors uncommitted Git diffs for real-time synchronization.
//!
//! Transport-side subscription helpers (HTTP snapshot / NDJSON stream
//! clients) live in [`sidecar`]; broadcast and apply-loop types live
//! in [`stream`]. This module is just the data type and its CRUD.

pub mod sidecar;
pub mod stream;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::revision_log::{LookupResult, RevisionLog};
use parking_lot::{Mutex, RwLock};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
pub use sidecar::subscribe;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
pub use stream::{
    broadcast_overlay_diff, subscribe_apply, subscribe_channel, OverlayDiff, RevisionId,
};
use tracing::{debug, info};

/// Volatile overlay graph using petgraph
#[derive(Clone)]
pub struct VolatileOverlay {
    graph: Arc<RwLock<DiGraph<GraphNode, EdgeType>>>,
    node_index_map: Arc<RwLock<HashMap<String, NodeIndex>>>,
    bloom_filter: Arc<RwLock<Vec<u8>>>, // Simple Bloom Filter for fast existence checks
    /// Last time the overlay was modified
    last_updated: Arc<RwLock<Instant>>,
    /// Bounded history of overlay mutations, fed by `insert_node` /
    /// `insert_edge`. Wrapped in its own `Mutex` so enqueue does not widen
    /// the surface of the existing RwLocks; the lock is held only for the
    /// `VecDeque` push. See Task 1.2 of the coordination staleness/audit
    /// design for the lock-decision rationale.
    log: Arc<Mutex<RevisionLog>>,
}

/// Cheap clone (already provided by the `#[derive(Clone)]` on the
/// struct — every field is an `Arc`, so duplicating the overlay just
/// bumps the reference counts). Used for sharing one overlay between
/// `FederatedIndex` (so index() can touch it) and `LainServer`
/// (which the tool executor dispatches against).
impl VolatileOverlay {
    /// Create a new volatile overlay
    pub fn new() -> Self {
        Self {
            graph: Arc::new(RwLock::new(DiGraph::new())),
            node_index_map: Arc::new(RwLock::new(HashMap::new())),
            bloom_filter: Arc::new(RwLock::new(vec![0u8; 1024])), // 8192 bits
            last_updated: Arc::new(RwLock::new(Instant::now())),
            log: Arc::new(Mutex::new(RevisionLog::new())),
        }
    }

    /// Returns how long ago the overlay was last updated
    pub fn last_update_age_secs(&self) -> f64 {
        let last = *self.last_updated.read();
        last.elapsed().as_secs_f64()
    }

    /// Bump the overlay's last-updated timestamp without changing
    /// nodes. Used after a successful indexing pass so the freshness
    /// indicator reflects "we just indexed" rather than "no edits
    /// ever". The index path doesn't insert nodes through the
    /// overlay (it writes the static graph), so without this the
    /// freshness banner stays "stale" forever on a freshly-indexed
    /// server.
    pub fn touch(&self) {
        *self.last_updated.write() = Instant::now();
    }

    /// Highest revision id assigned by this overlay's internal `RevisionLog`.
    /// Returns 0 when nothing has been inserted yet. See Task 1.2 of the
    /// coordination staleness/audit design.
    pub fn current_revision(&self) -> RevisionId {
        self.log.lock().current_revision()
    }

    /// All retained diffs strictly newer than `rev`. See
    /// `RevisionLog::diffs_since` for the meaning of each `LookupResult`
    /// arm.
    pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult> {
        self.log.lock().diffs_since(rev)
    }

    fn update_bloom(&self, id: &str) {
        let mut filter = self.bloom_filter.write();
        let h1 = self.hash_str(id, 0) % 8192;
        let h2 = self.hash_str(id, 1) % 8192;
        filter[(h1 / 8) as usize] |= 1 << (h1 % 8);
        filter[(h2 / 8) as usize] |= 1 << (h2 % 8);
    }

    fn check_bloom(&self, id: &str) -> bool {
        let filter = self.bloom_filter.read();
        let h1 = self.hash_str(id, 0) % 8192;
        let h2 = self.hash_str(id, 1) % 8192;
        let b1 = filter[(h1 / 8) as usize] & (1 << (h1 % 8)) != 0;
        let b2 = filter[(h2 / 8) as usize] & (1 << (h2 % 8)) != 0;
        b1 && b2
    }

    fn hash_str(&self, s: &str, seed: u32) -> u32 {
        let mut hash = seed;
        for b in s.as_bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(*b as u32);
        }
        hash
    }

    /// Insert a node into the overlay.
    /// If a node with the same ID already exists, it is replaced (upsert).
    pub fn insert_node(&self, node: GraphNode) -> NodeIndex {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();

        // Upsert: if node already exists, remove the old one first to avoid orphans.
        // `DiGraph::remove_node` swap-removes (see `remove_node`'s comment for the
        // full explanation) — repoint whichever id mapped to the pre-removal last
        // index, or that node becomes unreachable/misresolved on every later
        // lookup once anything else gets removed or re-upserted.
        if let Some(&old_idx) = index_map.get(&node.id) {
            let last_index = NodeIndex::new(graph.node_count() - 1);
            // Guarded the same way `remove_node` guards its own repoint:
            // only touch `index_map` for the swapped-in node if a node
            // actually came out. `old_idx` is read from `index_map`
            // itself here, so in today's code `remove_node` returning
            // `None` would mean `index_map` and `graph` already
            // disagreed before this call — but repointing unconditionally
            // on that premise would corrupt an unrelated id's mapping
            // instead of just doing nothing, which is what should happen
            // when there was nothing to swap.
            if graph.remove_node(old_idx).is_some() && old_idx != last_index {
                if let Some(moved_idx) = index_map.values_mut().find(|v| **v == last_index) {
                    *moved_idx = old_idx;
                }
            }
        }

        self.update_bloom(&node.id);
        let index = graph.add_node(node.clone());
        index_map.insert(node.id.clone(), index);

        // Update freshness timestamp
        *self.last_updated.write() = Instant::now();

        // Record this mutation in the revision log. The lock is held only
        // for the `VecDeque::push_back` inside `enqueue`; nothing else is
        // touched here. The `added` vector carries the node that was just
        // upserted so subscribers replaying the diffs reconstruct the same
        // state we have in the graph above.
        {
            let mut log = self.log.lock();
            log.enqueue(OverlayDiff {
                revision: 0, // overwritten by `enqueue`; the log owns numbering
                added: vec![node.clone()],
                removed: vec![],
                updated: vec![],
            });
        }

        debug!("Upserted node into volatile overlay: {}", node.name);
        index
    }

    /// Upsert a node. Identical to `insert_node`; exists so the
    /// `subscribe_apply` apply loop can mirror the `OverlayDiff`
    /// vocabulary (`added` / `updated` both call insert, `removed`
    /// calls `remove_node`) without inventing a second upsert path.
    pub fn upsert_node(&self, node: GraphNode) {
        self.insert_node(node);
    }

    /// Remove a node by id. Returns `true` if the node existed and was
    /// removed, `false` if no node with that id was present. Used by
    /// `subscribe_apply` to honour the `removed` field of
    /// `OverlayDiff`. Edges incident to the removed node are dropped
    /// implicitly by petgraph's `remove_node`.
    pub fn remove_node(&self, id: &str) -> bool {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();

        match index_map.remove(id) {
            Some(idx) => {
                // `DiGraph` (petgraph::Graph) is not a `StableGraph`:
                // removing a node swap-removes it — unless `idx` is
                // already the last index, whatever node was at
                // `node_count() - 1` gets moved into `idx`'s now-vacant
                // slot, and that other node's index changes. Capture the
                // last index *before* removing, so `index_map` can be
                // repointed for whichever id was mapped there — without
                // this, that id's entry keeps pointing at an index that
                // no longer holds its node (or now holds a different
                // one), and every subsequent lookup or removal for it
                // silently fails or targets the wrong node. This was
                // invisible as long as callers only ever removed one
                // node per overlay before reading it again; removing
                // more than one in the same pass (e.g.
                // `remove_nodes_for_path` dropping several stale paths
                // in one `sync_overlay` cycle) is what exposed it.
                let last_index = NodeIndex::new(graph.node_count() - 1);
                if graph.remove_node(idx).is_some() {
                    *self.last_updated.write() = Instant::now();
                    if idx != last_index {
                        if let Some(moved_idx) = index_map.values_mut().find(|v| **v == last_index)
                        {
                            *moved_idx = idx;
                        }
                    }
                    self.log.lock().enqueue(OverlayDiff {
                        revision: 0,
                        added: vec![],
                        removed: vec![id.to_string()],
                        updated: vec![],
                    });
                    debug!("Removed node from volatile overlay: {}", id);
                    true
                } else {
                    // index_map claimed the node was present but the
                    // graph had already dropped it (e.g. via `clear`).
                    false
                }
            }
            None => false,
        }
    }

    /// Insert an edge into the overlay
    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), String> {
        let index_map = self.node_index_map.read();

        // Copy indices out since they borrow from index_map
        let source_idx = *index_map
            .get(&edge.source_id)
            .ok_or_else(|| format!("Source node not found: {}", edge.source_id))?;
        let target_idx = *index_map
            .get(&edge.target_id)
            .ok_or_else(|| format!("Target node not found: {}", edge.target_id))?;

        // Release index_map lock before acquiring graph lock
        drop(index_map);

        let mut graph = self.graph.write();

        // Check if edge already exists under write lock
        for e in graph.edges(source_idx) {
            if e.target() == target_idx && *e.weight() == edge.edge_type {
                return Ok(()); // Edge already exists
            }
        }

        graph.add_edge(source_idx, target_idx, edge.edge_type.clone());
        *self.last_updated.write() = Instant::now();

        // Record this mutation in the revision log. `OverlayDiff` has no
        // dedicated edge field today (the broadcast bus carries node
        // diffs only), so we enqueue an empty diff purely so the revision
        // counter advances in lockstep with the petgraph mutation. This
        // keeps `current_revision` monotonic across both insert paths;
        // once `OverlayDiff` grows an edges field this site should carry
        // it. Same lock-decision story as `insert_node`: only the log is
        // touched, briefly.
        {
            let mut log = self.log.lock();
            log.enqueue(OverlayDiff {
                revision: 0,
                added: vec![],
                removed: vec![],
                updated: vec![],
            });
        }

        debug!(
            "Inserted edge into volatile overlay: {} -> {}",
            edge.source_id, edge.target_id
        );
        Ok(())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<GraphNode> {
        if !self.check_bloom(id) {
            return None;
        }

        let graph = self.graph.read();
        let index_map = self.node_index_map.read();

        index_map
            .get(id)
            .and_then(|idx| graph.node_weight(*idx).cloned())
    }

    /// Remove every overlay node whose `path` equals the given path.
    /// Returns the number of nodes removed (zero if the path was not
    /// represented in the overlay).
    /// Used by `RepoIndex::sync_overlay` and
    /// `LainServer::process_change` to drop stale entries for a file
    /// before re-scanning it via LSP. Without a path-keyed remove the
    /// caller would have to either `overlay.clear()` (which wipes
    /// every repo's entries in the shared federation overlay) or
    /// enumerate every node id it knows about (which it doesn't).
    /// Concurrency: this holds the index_map lock briefly to copy the
    /// candidate ids, then calls `remove_node` for each (which takes
    /// per-node locks). Other writers can insert nodes between
    /// iterations; a concurrent insert for the same path is fine —
    /// `insert_node` is upsert, so the next sync will reconcile.
    pub fn remove_nodes_for_path(&self, path: &str) -> usize {
        let ids: Vec<String> = {
            let graph = self.graph.read();
            let index_map = self.node_index_map.read();
            graph
                .node_indices()
                .filter_map(|idx| {
                    let node = graph.node_weight(idx)?;
                    if node.path == path {
                        index_map
                            .iter()
                            .find(|(_, v)| **v == idx)
                            .map(|(k, _)| k.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut removed = 0usize;
        for id in ids {
            if self.remove_node(&id) {
                removed += 1;
            }
        }
        removed
    }

    /// Get all nodes
    pub fn get_all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph
            .node_indices()
            .filter_map(|idx| graph.node_weight(idx).cloned())
            .collect()
    }

    /// Get all edges
    pub fn get_all_edges(&self) -> Vec<(GraphNode, GraphNode, EdgeType)> {
        let graph = self.graph.read();

        graph
            .edge_indices()
            .filter_map(|idx| {
                let (source, target) = graph.edge_endpoints(idx)?;
                let source_node = graph.node_weight(source)?.clone();
                let target_node = graph.node_weight(target)?.clone();
                let edge_type = graph.edge_weight(idx)?.clone();
                Some((source_node, target_node, edge_type))
            })
            .collect()
    }

    /// Find nodes by name (fuzzy match)
    pub fn find_nodes_by_name(&self, name: &str) -> Vec<GraphNode> {
        let graph = self.graph.read();

        graph
            .node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if node.name.to_lowercase().contains(&name.to_lowercase()) {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find nodes by type
    pub fn find_nodes_by_type(&self, node_type: &NodeType) -> Vec<GraphNode> {
        let graph = self.graph.read();

        graph
            .node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if &node.node_type == node_type {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn find_nodes_by_path(&self, path: &str) -> Vec<GraphNode> {
        let graph = self.graph.read();

        graph
            .node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if node.path == path {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get outgoing edges from a node
    pub fn get_outgoing_edges(&self, node_id: &str) -> Vec<(GraphNode, EdgeType)> {
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();

        let idx = match index_map.get(node_id) {
            Some(idx) => *idx,
            None => return vec![],
        };

        graph
            .edges(idx)
            .filter_map(|e| {
                let target_node = graph.node_weight(e.target())?.clone();
                Some((target_node, e.weight().clone()))
            })
            .collect()
    }

    /// Get incoming edges to a node
    pub fn get_incoming_edges(&self, node_id: &str) -> Vec<(GraphNode, EdgeType)> {
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();

        let idx = match index_map.get(node_id) {
            Some(idx) => *idx,
            None => return vec![],
        };

        // Need to iterate all edges to find incoming
        graph
            .edge_indices()
            .filter_map(|eid| {
                let (source, target) = graph.edge_endpoints(eid)?;
                if target == idx {
                    let source_node = graph.node_weight(source)?.clone();
                    let edge_type = graph.edge_weight(eid)?.clone();
                    Some((source_node, edge_type))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Clear the overlay
    pub fn clear(&self) {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();
        let mut bloom = self.bloom_filter.write();

        *graph = DiGraph::new();
        index_map.clear();
        *bloom = vec![0u8; 1024];
        *self.last_updated.write() = Instant::now();

        info!("Volatile overlay cleared");
    }

    /// Get statistics
    pub fn stats(&self) -> OverlayStats {
        let graph = self.graph.read();

        OverlayStats {
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
        }
    }

    /// Merge another overlay into this one
    pub fn merge(&self, other: &VolatileOverlay) {
        let other_graph = other.graph.read();
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();

        // Copy nodes
        for idx in other_graph.node_indices() {
            if let Some(node) = other_graph.node_weight(idx) {
                let new_idx = graph.add_node(node.clone());
                index_map.insert(node.id.clone(), new_idx);
                self.update_bloom(&node.id);
            }
        }

        // Copy edges
        for idx in other_graph.edge_indices() {
            if let Some((source, target)) = other_graph.edge_endpoints(idx) {
                if let Some(edge_type) = other_graph.edge_weight(idx) {
                    let source_node = other_graph.node_weight(source).unwrap();
                    let target_node = other_graph.node_weight(target).unwrap();

                    if let (Some(&new_source), Some(&new_target)) = (
                        index_map.get(&source_node.id),
                        index_map.get(&target_node.id),
                    ) {
                        graph.add_edge(new_source, new_target, edge_type.clone());
                    }
                }
            }
        }
    }
}

impl Default for VolatileOverlay {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the overlay
#[derive(Debug, Clone)]
pub struct OverlayStats {
    pub node_count: usize,
    pub edge_count: usize,
}
