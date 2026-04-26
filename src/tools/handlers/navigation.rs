//! Navigation domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::{GraphNode, NodeType};

pub fn trace_dependency(graph: &GraphDatabase, symbol: &str) -> Result<String, LainError> {
    let start_node = graph.get_node(symbol)?
        .ok_or_else(|| LainError::NotFound(format!("Symbol not found: {}", symbol)))?;

    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let mut results = Vec::new();
    queue.push_back(start_node);

    while let Some(node) = queue.pop_front() {
        if visited.contains(&node.id) {
            continue;
        }
        visited.insert(node.id.clone());
        results.push(node);
    }

    Ok(format!("Found {} dependency nodes:\n{}",
        results.len(),
        results.iter().map(|n| format!("- {} ({:?})", n.name, n.node_type)).collect::<Vec<_>>().join("\n")
    ))
}

pub fn get_call_chain(graph: &GraphDatabase, from: &str, to: &str) -> Result<String, LainError> {
    let start = graph.get_node(from)?
        .ok_or_else(|| LainError::NotFound(format!("Start symbol not found: {}", from)))?;
    let end = graph.get_node(to)?
        .ok_or_else(|| LainError::NotFound(format!("End symbol not found: {}", to)))?;

    // Simple BFS to find shortest path
    use std::collections::{VecDeque, HashMap};
    let mut queue = VecDeque::new();
    let mut parents = HashMap::new();

    queue.push_back(start.id.clone());
    parents.insert(start.id.clone(), None);

    let mut found = false;
    while let Some(current_id) = queue.pop_front() {
        if current_id == end.id {
            found = true;
            break;
        }

        let edges = graph.get_edges_from(&current_id)?;
        for edge in edges {
            if !parents.contains_key(&edge.target_id) {
                parents.insert(edge.target_id.clone(), Some(current_id.clone()));
                queue.push_back(edge.target_id.clone());
            }
        }
    }

    if !found {
        return Ok(format!("No call path found from '{}' to '{}'.", from, to));
    }

    // Reconstruct path
    let mut path = Vec::new();
    let mut current = Some(end.id.clone());
    while let Some(id) = current {
        let node = graph.get_node_by_id(&id)?.unwrap();
        path.push(node.name);
        current = parents.get(&id).unwrap().clone();
    }
    path.reverse();

    Ok(format!("## Call Chain: {} -> {}\n\n{}", from, to, path.join(" → ")))
}

pub fn navigate_to_anchor(graph: &GraphDatabase, symbol: &str) -> Result<String, LainError> {
    let start = graph.get_node(symbol)?
        .ok_or_else(|| LainError::NotFound(format!("Symbol not found: {}", symbol)))?;

    use std::collections::VecDeque;
    let mut queue = VecDeque::new();
    let mut visited = std::collections::HashSet::new();
    let mut best_anchor: Option<GraphNode> = None;

    queue.push_back(start);

    while let Some(current) = queue.pop_front() {
        if visited.contains(&current.id) { continue; }
        visited.insert(current.id.clone());

        let score = current.anchor_score.unwrap_or(0.0);
        if best_anchor.is_none() || score > best_anchor.as_ref().unwrap().anchor_score.unwrap_or(0.0) {
            best_anchor = Some(current.clone());
        }

        let edges = graph.get_edges_from(&current.id)?;
        for edge in edges {
            if let Some(target) = graph.get_node_by_id(&edge.target_id)? {
                queue.push_back(target);
            }
        }
    }

    match best_anchor {
        Some(anchor) if anchor.name != symbol => {
            Ok(format!("The foundational anchor for '{}' is **{}** (score: {:.3}, path: {}).\n\nThis node is more foundational because it has a higher fan-in/fan-out ratio.", 
                symbol, anchor.name, anchor.anchor_score.unwrap_or(0.0), anchor.path))
        },
        _ => Ok(format!("'{}' appears to be foundational already, or no other anchors were reachable from it.", symbol))
    }
}

pub fn get_layered_map(graph: &GraphDatabase, layer: usize, granularity: &str) -> Result<String, LainError> {
    let mut all_nodes = Vec::new();
    for node_type in [NodeType::File, NodeType::Namespace, NodeType::Class, NodeType::Function] {
        all_nodes.extend(graph.get_nodes_by_type(node_type)?);
    }

    // Filter nodes by depth
    let filtered: Vec<_> = all_nodes.into_iter()
        .filter(|n| n.depth_from_main.unwrap_or(u32::MAX) as usize == layer)
        .collect();

    if filtered.is_empty() {
        return Ok(format!("No nodes found at Layer {}. Ensure you have run 'run_enrichment'.", layer));
    }

    let mut output = format!("## Architectural Map: Layer {}\n\n", layer);
    
    match granularity {
        "module" => {
            let mut modules = std::collections::HashSet::new();
            for n in filtered {
                if n.node_type == NodeType::File {
                    if let Some(parent_path) = std::path::Path::new(&n.path).parent() {
                        modules.insert(parent_path.to_string_lossy().to_string());
                    }
                } else if n.node_type == NodeType::Namespace {
                    modules.insert(n.path.clone());
                }
            }
            output.push_str("### Modules involved in this layer:\n");
            for m in modules {
                output.push_str(&format!("- **{}**\n", m));
            }
        },
        "file" => {
            output.push_str("### Files involved in this layer:\n");
            let files: std::collections::HashSet<_> = filtered.into_iter()
                .map(|n| n.path.clone())
                .collect();
            for f in files {
                output.push_str(&format!("- {}\n", f));
            }
        },
        _ => {
            output.push_str("### Symbols at this layer:\n");
            for n in filtered {
                output.push_str(&format!("- {} ({:?}) in {}\n", n.name, n.node_type, n.path));
            }
        }
    }

    output.push_str(&format!("\n*Use `get_layered_map(layer: {})` to see what these components depend on.*", layer + 1));

    Ok(output)
}
