//! Navigation domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType};
use crate::tools::utils::resolve_node;
use crate::tools::{UiSession, UiSessionData, DIAGNOSTICS_PORT};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub fn trace_dependency(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    // 1. Resolve handle
    let start_node = resolve_node(graph, overlay, symbol)?;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();
    queue.push_back(start_node);

    while let Some(node) = queue.pop_front() {
        if visited.contains(&node.id) {
            continue;
        }
        visited.insert(node.id.clone());
        results.push(node.clone());

        // Get edges from both static and overlay
        let mut targets = HashSet::new();

        // Static edges
        if let Ok(edges) = graph.get_edges_from(&node.id) {
            for e in edges { targets.insert(e.target_id); }
        }

        // Overlay edges
        let overlay_edges = overlay.get_outgoing_edges(&node.id);
        for (target, _) in overlay_edges {
            targets.insert(target.id);
        }

        for tid in targets {
            if let Some(target_node) = overlay.get_node(&tid) {
                queue.push_back(target_node);
            } else if let Ok(Some(target_node)) = graph.get_node(&tid) {
                queue.push_back(target_node);
            }
        }
    }

    Ok(format!("Found {} dependency nodes in Merged Brain:\n{}",
        results.len(),
        results.iter().map(|n| format!("- {} ({:?})", n.name, n.node_type)).collect::<Vec<_>>().join("\n")
    ))
}

pub fn get_call_chain(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    from: &str,
    to: &str,
    ui_sessions: Option<&Arc<AsyncMutex<HashMap<String, UiSession>>>>,
) -> Result<String, LainError> {
    let start = resolve_node(graph, overlay, from)?;
    let end = resolve_node(graph, overlay, to)?;

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

        let mut targets = HashSet::new();
        if let Ok(edges) = graph.get_edges_from(&current_id) {
            for e in edges { targets.insert(e.target_id); }
        }
        let overlay_edges = overlay.get_outgoing_edges(&current_id);
        for (target, _) in overlay_edges {
            targets.insert(target.id);
        }

        for tid in targets {
            if !parents.contains_key(&tid) {
                parents.insert(tid.clone(), Some(current_id.clone()));
                queue.push_back(tid);
            }
        }
    }

    if !found {
        return Ok(format!("No call path found from '{}' to '{}' in Merged Brain.", from, to));
    }

    let mut path = Vec::new();
    let mut current = Some(end.id.clone());
    while let Some(id) = current {
        let node = if let Some(n) = overlay.get_node(&id) { Some(n) } else { graph.get_node(&id)? };
        if let Some(n) = node {
            path.push(n.name);
        }
        current = parents.get(&id).cloned().flatten();
    }
    path.reverse();

    let mut output = format!("## Call Chain: {} -> {}\n\n{}", from, to, path.join(" → "));

    // Store UI session if rich format requested
    if let Some(sessions) = ui_sessions {
        let session_id = Uuid::new_v4().to_string();
        let session = UiSession {
            id: session_id.clone(),
            session_type: "call-chain".to_string(),
            created_at: std::time::SystemTime::now(),
            expires_at: std::time::SystemTime::now()
                + std::time::Duration::from_secs(600),
            data: UiSessionData::CallChain {
                from: from.to_string(),
                to: to.to_string(),
                path: path.clone(),
            },
        };

        let sessions_clone = Arc::clone(sessions);
        let session_id_clone = session_id.clone();
        let session_clone = session;
        let handle = tokio::spawn(async move {
            let mut guard = sessions_clone.lock().await;
            // Clean up expired sessions before insert (bounded memory)
            let now = std::time::SystemTime::now();
            guard.retain(|_, s| s.expires_at > now);
            guard.insert(session_id_clone, session_clone);
        });
        if let Err(e) = Handle::current().block_on(handle) {
            tracing::warn!("session store spawn error: {}", e);
        }

        output.push_str(&format!(
            "\n\n[Interactive call chain: http://localhost:{}/ui/call-chain/{}]",
            DIAGNOSTICS_PORT, session_id
        ));
        output.push_str("\nClick nodes to explore, then describe your selection to the agent.");
    }

    Ok(output)
}

pub fn navigate_to_anchor(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let start = resolve_node(graph, overlay, symbol)?;

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut best_anchor: Option<GraphNode> = None;

    queue.push_back(start);

    while let Some(current) = queue.pop_front() {
        if visited.contains(&current.id) { continue; }
        visited.insert(current.id.clone());

        let score = current.anchor_score.unwrap_or(0.0);
        if best_anchor.is_none() || score > best_anchor.as_ref().unwrap().anchor_score.unwrap_or(0.0) {
            best_anchor = Some(current.clone());
        }

        // Neighbors from both
        let mut targets = HashSet::new();
        if let Ok(edges) = graph.get_edges_from(&current.id) {
            for edge in edges { targets.insert(edge.target_id); }
        }
        for (target, _) in overlay.get_outgoing_edges(&current.id) {
            targets.insert(target.id);
        }

        for tid in targets {
            if let Some(target) = overlay.get_node(&tid) {
                queue.push_back(target);
            } else if let Ok(Some(target)) = graph.get_node(&tid) {
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

pub fn get_layered_map(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    layer: usize,
    granularity: &str
) -> Result<String, LainError> {
    let mut all_nodes = Vec::new();
    for node_type in [NodeType::File, NodeType::Namespace, NodeType::Class, NodeType::Function] {
        all_nodes.extend(graph.get_nodes_by_type(node_type)?);
    }

    // Merge overlay using HashSet for O(N)
    let mut seen_ids: HashSet<String> = all_nodes.iter().map(|n| n.id.clone()).collect();
    for on in overlay.get_all_nodes() {
        if seen_ids.insert(on.id.clone()) { all_nodes.push(on); }
    }

    let filtered: Vec<_> = all_nodes.into_iter()
        .filter(|n| n.depth_from_main.unwrap_or(u32::MAX) as usize == layer)
        .collect();

    if filtered.is_empty() {
        return Ok(format!("No nodes found at Layer {}. Ensure core memory is built.", layer));
    }

    let mut output = format!("## Architectural Map: Layer {}\n\n", layer);

    match granularity {
        "module" => {
            let mut modules = HashSet::new();
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
            let files: HashSet<_> = filtered.into_iter()
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
