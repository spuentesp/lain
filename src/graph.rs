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

#[derive(Serialize, Deserialize)]
struct GraphState {
    graph: StableGraph<GraphNode, GraphEdge>,
    index_map: HashMap<String, NodeIndex>,
    last_commit: Option<String>,
}

#[derive(Clone)]
pub struct GraphDatabase {
    graph: Arc<RwLock<StableGraph<GraphNode, GraphEdge>>>,
    index_map: DashMap<String, NodeIndex>,
    path_index: DashMap<String, Vec<NodeIndex>>,
    last_commit: Arc<RwLock<Option<String>>>,
    persistence_path: PathBuf,
}

impl GraphDatabase {
    pub fn new(memory_path: &Path) -> Result<Self, LainError> {
        let db = Self {
            graph: Arc::new(RwLock::new(StableGraph::new())),
            index_map: DashMap::new(),
            path_index: DashMap::new(),
            last_commit: Arc::new(RwLock::new(None)),
            persistence_path: memory_path.to_path_buf(),
        };

        if memory_path.exists() {
            db.load_from_disk()?;
        }
        Ok(db)
    }

    pub fn insert_node(&self, node: &GraphNode) -> Result<(), LainError> {
        self.upsert_node(node.clone())
    }

    pub fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
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

    pub fn upsert_nodes_batch(&self, new_nodes: Vec<GraphNode>) -> Result<(), LainError> {
        for node in new_nodes {
            self.upsert_node(node)?;
        }
        Ok(())
    }

    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), LainError> {
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

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, LainError> {
        let graph = self.graph.read();

        Ok(self.index_map.get(id).and_then(|r| graph.node_weight(*r.value()).cloned()))
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
            .map(|e| {
                let neighbor_idx = e.target();
                let neighbor = graph.node_weight(neighbor_idx).cloned().unwrap();
                (neighbor, e.weight().clone())
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
        let mut graph = self.graph.write();
        
        let indices: Vec<_> = graph.node_indices().collect();
        for idx in indices {
            let fan_in = graph.neighbors_directed(idx, Direction::Incoming).count() as u32;
            let fan_out = graph.neighbors_directed(idx, Direction::Outgoing).count() as u32;
            
            if let Some(node) = graph.node_weight_mut(idx) {
                node.fan_in = Some(fan_in);
                node.fan_out = Some(fan_out);
                node.anchor_score = Some(fan_in as f32 / (fan_out as f32 + 1.0));
            }
        }
        Ok(())
    }

    pub fn find_anchors(&self, limit: usize) -> Result<Vec<GraphNode>, LainError> {
        let graph = self.graph.read();
        let mut sorted: Vec<_> = graph.node_weights().cloned().collect();
        sorted.sort_by(|a, b| b.anchor_score.unwrap_or(0.0).partial_cmp(&a.anchor_score.unwrap_or(0.0)).unwrap());
        Ok(sorted.into_iter().take(limit).collect())
    }

    pub fn calculate_depths(&self) -> Result<(), LainError> {
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
            
            let id1 = GraphNode::generate_id(&NodeType::File, p1, &filename1);
            let id2 = GraphNode::generate_id(&NodeType::File, p2, &filename2);
            
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
        let id = GraphNode::generate_id(&NodeType::File, file_path, &filename);
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
        *self.last_commit.write() = Some(hash);
        Ok(())
    }

    pub fn get_stats(&self) -> (usize, usize) {
        let graph = self.graph.read();
        (graph.node_count(), graph.edge_count())
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
        // Clone state under lock (fast)
        let (data, tmp_path, persistence_path) = {
            let state = GraphState {
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

    pub fn load_from_disk(&self) -> Result<(), LainError> {
        let data = std::fs::read(&self.persistence_path).map_err(|e| LainError::Database(e.to_string()))?;
        let state: GraphState = bincode::deserialize(&data).map_err(|e| LainError::Database(e.to_string()))?;

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
            graph: self.graph.read().clone(),
            index_map: self.index_map.iter().map(|r| (r.key().clone(), *r.value())).collect(),
            last_commit: self.last_commit.read().clone(),
        };
        serde_json::to_string_pretty(&state).map_err(|e| LainError::Database(e.to_string()))
    }
}
