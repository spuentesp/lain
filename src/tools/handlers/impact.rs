//! Impact analysis domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::tools::utils::resolve_node;
use crate::tools::{UiSession, UiSessionData, BlastRadiusNode, DIAGNOSTICS_PORT};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// Store a UI session and append its interactive link to the output string.
fn store_ui_session_and_append_link(
    sessions: &Arc<AsyncMutex<HashMap<String, UiSession>>>,
    session_type: &str,
    data: UiSessionData,
    url_path: &str,
    output: &mut String,
) {
    let session_id = Uuid::new_v4().to_string();
    let session = UiSession {
        id: session_id.clone(),
        session_type: session_type.to_string(),
        created_at: std::time::SystemTime::now(),
        expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(600),
        data,
    };

    let sessions_clone = Arc::clone(sessions);
    let session_id_clone = session_id.clone();
    let session_clone = session;
    tokio::spawn(async move {
        let mut guard = sessions_clone.lock().await;
        let now = std::time::SystemTime::now();
        guard.retain(|_, s| s.expires_at > now);
        guard.insert(session_id_clone, session_clone);
    });

    output.push_str(&format!(
        "\n\n[Interactive {}: http://localhost:{}/ui/{}/{}]",
        url_path, DIAGNOSTICS_PORT, url_path, session_id
    ));
}

pub fn get_blast_radius(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    include_coupling: bool,
    ui_sessions: Option<&Arc<AsyncMutex<HashMap<String, UiSession>>>>,
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;

    // Overlay freshness indicator
    let overlay_age = overlay.last_update_age_secs();
    let freshness = if overlay_age < 5.0 {
        format!("live ({}s ago)", format!("{:.1}", overlay_age))
    } else if overlay_age < 60.0 {
        format!("recent ({}s ago)", format!("{:.0}", overlay_age))
    } else {
        "stale".to_string()
    };

    let mut output = format!(
        "Blast radius for '{}':\n- {} ({:?})\n- Overlay freshness: {}",
        symbol, node.name, node.node_type, freshness
    );

    // Blast radius = BFS over INCOMING edges (who depends on this symbol)
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((node.id.clone(), 0));

    let mut affected_names: Vec<String> = Vec::new();
    let mut session_nodes: Vec<BlastRadiusNode> = Vec::new();

    // Confidence tracking: nodes resolved via LSP vs tree-sitter fallback
    // Each unique node is counted once (first time it's visited)
    let mut lsp_resolved = 0u32;
    let mut tree_sitter_fallback = 0u32;

    while let Some((id, depth)) = queue.pop_front() {
        if visited.contains(&id) { continue; }
        visited.insert(id.clone());

        if let Ok(incoming) = graph.get_edges_to(&id) {
            for e in incoming {
                let source_id = e.source_id.clone();
                if !visited.contains(&source_id) {
                    if let Ok(Some(caller)) = graph.get_node(&source_id) {
                        let is_direct = depth == 0;
                        affected_names.push(format!(
                            "  - {} ({:?}) in {}",
                            caller.name, caller.node_type, caller.path
                        ));
                        session_nodes.push(BlastRadiusNode {
                            id: caller.id.clone(),
                            name: caller.name.clone(),
                            node_type: format!("{:?}", caller.node_type),
                            path: caller.path.clone(),
                            depth,
                            is_direct,
                        });

                        // Confidence: LSP sync = high confidence, tree-sitter only = fallback
                        // Count each unique caller node once (first visit)
                        let node_sync_time = caller.last_lsp_sync.unwrap_or(0);
                        if node_sync_time > 0 {
                            lsp_resolved += 1;
                        } else {
                            tree_sitter_fallback += 1;
                        }
                    }
                    queue.push_back((source_id, depth + 1));
                }
            }
        }
    }

    // Confidence summary
    let total_visited = lsp_resolved + tree_sitter_fallback;
    let confidence_pct = if total_visited > 0 {
        (lsp_resolved as f32 / total_visited as f32 * 100.0) as u32
    } else {
        100
    };

    // Add confidence field as prominent header when tree-sitter fallback used
    if tree_sitter_fallback > 0 {
        output.push_str(&format!(
            "\n\n⚠ Confidence: {}% ({} nodes via LSP, {} nodes via tree-sitter name-match)",
            confidence_pct, lsp_resolved, tree_sitter_fallback
        ));
    }

    let total_affected = visited.len().saturating_sub(1); // exclude start node
    if affected_names.is_empty() {
        output.push_str("\n  (no dependents found — symbol may be a leaf or not yet indexed)");
        // Don't show total count when there are no names to show
    } else {
        let show = affected_names.len().min(20);
        for name in &affected_names[..show] {
            output.push_str(&format!("\n{}", name));
        }
        if affected_names.len() > 20 {
            output.push_str(&format!("\n  ... and {} more", affected_names.len() - 20));
        }
        output.push_str(&format!("\n- Total transitively affected nodes: {}", total_affected));
    }

    if include_coupling {
        let partners = graph.get_co_change_partners(&node.path)?;
        if !partners.is_empty() {
            output.push_str("\n\nCoupled Files (Git Co-Changes):\n");
            for (p, c) in partners.iter().take(5) {
                output.push_str(&format!("- {} (changed together {} times)\n", p, c));
            }
        }
    }

    // Store UI session if rich format requested
    if let Some(sessions) = ui_sessions {
        let data = UiSessionData::BlastRadius {
            symbol: symbol.to_string(),
            nodes: session_nodes,
        };
        store_ui_session_and_append_link(sessions, "blast-radius", data, "blast-radius", &mut output);
        output.push_str("\nClick nodes to mark approved, then describe your selection to the agent.");
    }

    Ok(output)
}

pub fn get_coupling_radar(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    ui_sessions: Option<&Arc<AsyncMutex<HashMap<String, UiSession>>>>,
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;

    let partners = graph.get_co_change_partners(&node.path)?;

    if partners.is_empty() {
        return Ok(format!(
            "No co-change coupling found for '{}' ({})",
            symbol, node.path
        ));
    }

    let mut output = format!(
        "Files that co-change with '{}' ({}) — top {} partners:\n{}",
        symbol,
        node.path,
        partners.len(),
        partners.iter().take(10).enumerate().map(|(i, (p, c))| {
            format!("{}. {} (changed together {} times)", i + 1, p, c)
        }).collect::<Vec<_>>().join("\n")
    );

    // Store UI session if rich format requested
    if let Some(sessions) = ui_sessions {
        let data = UiSessionData::Coupling {
            symbol: symbol.to_string(),
            matrix: vec![],
            files: partners.iter().map(|(p, _)| p.clone()).take(20).collect(),
        };
        store_ui_session_and_append_link(sessions, "coupling", data, "coupling", &mut output);
        output.push_str("\nClick cells to see co-change details, then describe your selection to the agent.");
    }

    Ok(output)
}