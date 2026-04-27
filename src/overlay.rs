//! Volatile overlay using petgraph
//!
//! In-memory graph that mirrors uncommitted Git diffs for real-time synchronization.

use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;
use tracing::{debug, info};

/// Volatile overlay graph using petgraph
#[derive(Clone)]
pub struct VolatileOverlay {
    graph: Arc<RwLock<DiGraph<GraphNode, EdgeType>>>,
    node_index_map: Arc<RwLock<HashMap<String, NodeIndex>>>,
    bloom_filter: Arc<RwLock<Vec<u8>>>, // Simple Bloom Filter for fast existence checks
    /// Last time the overlay was modified
    last_updated: Arc<RwLock<Instant>>,
}

impl VolatileOverlay {
    /// Create a new volatile overlay
    pub fn new() -> Self {
        Self {
            graph: Arc::new(RwLock::new(DiGraph::new())),
            node_index_map: Arc::new(RwLock::new(HashMap::new())),
            bloom_filter: Arc::new(RwLock::new(vec![0u8; 1024])), // 8192 bits
            last_updated: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Returns how long ago the overlay was last updated
    pub fn last_update_age_secs(&self) -> f64 {
        let last = *self.last_updated.read();
        last.elapsed().as_secs_f64()
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

        // Upsert: if node already exists, remove the old one first to avoid orphans
        if let Some(&old_idx) = index_map.get(&node.id) {
            graph.remove_node(old_idx);
        }

        self.update_bloom(&node.id);
        let index = graph.add_node(node.clone());
        index_map.insert(node.id.clone(), index);

        // Update freshness timestamp
        *self.last_updated.write() = Instant::now();

        debug!("Upserted node into volatile overlay: {}", node.name);
        index
    }

    /// Insert an edge into the overlay
    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), String> {
        let index_map = self.node_index_map.read();

        // Copy indices out since they borrow from index_map
        let source_idx = *index_map.get(&edge.source_id)
            .ok_or_else(|| format!("Source node not found: {}", edge.source_id))?;
        let target_idx = *index_map.get(&edge.target_id)
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

        debug!("Inserted edge into volatile overlay: {} -> {}", edge.source_id, edge.target_id);
        Ok(())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<GraphNode> {
        if !self.check_bloom(id) { return None; }
        
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();
        
        index_map.get(id).and_then(|idx| graph.node_weight(*idx).cloned())
    }

    /// Get all nodes
    pub fn get_all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph.node_indices()
            .filter_map(|idx| graph.node_weight(idx).cloned())
            .collect()
    }

    /// Get all edges
    pub fn get_all_edges(&self) -> Vec<(GraphNode, GraphNode, EdgeType)> {
        let graph = self.graph.read();

        graph.edge_indices()
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
        
        graph.node_indices()
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
        
        graph.node_indices()
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
        
        graph.node_indices()
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
        
        graph.edges(idx)
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
        graph.edge_indices()
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
