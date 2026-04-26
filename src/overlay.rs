//! Volatile overlay using petgraph
//!
//! In-memory graph that mirrors uncommitted Git diffs for real-time synchronization.

use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

/// Volatile overlay graph using petgraph
#[derive(Clone)]
pub struct VolatileOverlay {
    graph: Arc<RwLock<DiGraph<GraphNode, EdgeType>>>,
    node_index_map: Arc<RwLock<HashMap<String, NodeIndex>>>,
}

impl VolatileOverlay {
    /// Create a new volatile overlay
    pub fn new() -> Self {
        Self {
            graph: Arc::new(RwLock::new(DiGraph::new())),
            node_index_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert a node into the overlay
    pub fn insert_node(&self, node: GraphNode) -> NodeIndex {
        let mut graph = self.graph.write().unwrap();
        let mut index_map = self.node_index_map.write().unwrap();
        
        let index = graph.add_node(node.clone());
        index_map.insert(node.id.clone(), index);
        
        debug!("Inserted node into volatile overlay: {}", node.name);
        index
    }

    /// Insert an edge into the overlay
    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), String> {
        let graph = self.graph.read().unwrap();
        let index_map = self.node_index_map.read().unwrap();
        
        let source_idx = index_map.get(&edge.source_id)
            .ok_or_else(|| format!("Source node not found: {}", edge.source_id))?;
        let target_idx = index_map.get(&edge.target_id)
            .ok_or_else(|| format!("Target node not found: {}", edge.target_id))?;
        
        // Check if edge already exists
        let edges = graph.edges(*source_idx);
        for e in edges {
            if e.target() == *target_idx && *e.weight() == edge.edge_type {
                return Ok(()); // Edge already exists
            }
        }
        
        drop(graph); // Release read lock
        
        let mut graph = self.graph.write().unwrap();
        graph.add_edge(*source_idx, *target_idx, edge.edge_type.clone());
        
        debug!("Inserted edge into volatile overlay: {} -> {}", edge.source_id, edge.target_id);
        Ok(())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<GraphNode> {
        let graph = self.graph.read().unwrap();
        let index_map = self.node_index_map.read().unwrap();
        
        index_map.get(id).and_then(|idx| graph.node_weight(*idx).cloned())
    }

    /// Get all nodes
    pub fn get_all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read().unwrap();
        graph.node_indices()
            .filter_map(|idx| graph.node_weight(idx).cloned())
            .collect()
    }

    /// Get all edges
    pub fn get_all_edges(&self) -> Vec<(GraphNode, GraphNode, EdgeType)> {
        let graph = self.graph.read().unwrap();

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
        let graph = self.graph.read().unwrap();
        
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
        let graph = self.graph.read().unwrap();
        
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

    /// Get outgoing edges from a node
    pub fn get_outgoing_edges(&self, node_id: &str) -> Vec<(GraphNode, EdgeType)> {
        let graph = self.graph.read().unwrap();
        let index_map = self.node_index_map.read().unwrap();
        
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
        let graph = self.graph.read().unwrap();
        let index_map = self.node_index_map.read().unwrap();
        
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
        let mut graph = self.graph.write().unwrap();
        let mut index_map = self.node_index_map.write().unwrap();
        
        *graph = DiGraph::new();
        index_map.clear();
        
        info!("Volatile overlay cleared");
    }

    /// Get statistics
    pub fn stats(&self) -> OverlayStats {
        let graph = self.graph.read().unwrap();
        
        OverlayStats {
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
        }
    }

    /// Merge another overlay into this one
    pub fn merge(&self, other: &VolatileOverlay) {
        let other_graph = other.graph.read().unwrap();
        let mut graph = self.graph.write().unwrap();
        let mut index_map = self.node_index_map.write().unwrap();
        
        // Copy nodes
        for idx in other_graph.node_indices() {
            if let Some(node) = other_graph.node_weight(idx) {
                let new_idx = graph.add_node(node.clone());
                index_map.insert(node.id.clone(), new_idx);
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
