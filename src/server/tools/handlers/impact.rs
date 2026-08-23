//! Impact analysis domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::server::tools::utils::resolve_node;
use crate::server::tools::{UiSession, UiSessionData, BlastRadiusNode};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// Store a UI session and append its interactive link to the output string.
/// Must be `async` so we can `.lock().await` synchronously — spawning the
/// insert races the agent's immediate fetch of the URL we return.
/// `port` is the actual HTTP listener port (stdio mode passes none and
/// the link is never emitted).
async fn store_ui_session_and_append_link(
    sessions: &Arc<AsyncMutex<HashMap<String, UiSession>>>,
    port: u16,
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

    {
        let mut guard = sessions.lock().await;
        let now = std::time::SystemTime::now();
        guard.retain(|_, s| s.expires_at > now);
        guard.insert(session_id.clone(), session);
    }

    output.push_str(&format!(
        "\n\n[Interactive {}: http://localhost:{}/ui/{}/{}]",
        url_path, port, url_path, session_id
    ));
}

/// How many dependents to name per section before summarizing.
const LIST_CAP: usize = 20;

pub async fn get_blast_radius(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    include_coupling: bool,
    ui_sessions: Option<(&Arc<AsyncMutex<HashMap<String, UiSession>>>, u16)>,
) -> Result<String, LainError> {
    let (node, other_defs) =
        crate::server::tools::utils::resolve_node_ambiguous(graph, overlay, symbol)?;

    // Overlay freshness indicator
    let overlay_age = overlay.last_update_age_secs();
    let freshness = if overlay_age < 5.0 {
        format!("live ({}s ago)", format!("{:.1}", overlay_age))
    } else if overlay_age < 60.0 {
        format!("recent ({}s ago)", format!("{:.0}", overlay_age))
    } else {
        "stale".to_string()
    };

    let mut output = crate::server::tools::utils::ambiguity_note(&node, &other_defs);
    output.push_str(&format!(
        "Blast radius for '{}':\n- {} ({:?})\n- Overlay freshness: {}",
        symbol, node.name, node.node_type, freshness
    ));

    // Blast radius = BFS over INCOMING edges (who depends on this symbol)
    let mut visited: HashSet<String> = HashSet::new();
    // Nodes already pushed into the queue. Without this, a caller with
    // two edges to already-visited nodes gets enqueued twice: the
    // display list shows it duplicated while `visited` counts it once
    // (observed live: "Total: 5" under a 6-line list with a repeated
    // `src (Namespace)` row).
    let mut queued: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((node.id.clone(), 0));
    queued.insert(node.id.clone());

    // (depth-of-caller, formatted row). Depth is kept so the report can
    // separate real callers from nodes that merely reach this one.
    let mut affected_names: Vec<(u32, String)> = Vec::new();
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
                // Only dependency edges. "What breaks if I change this?"
                // is about callers and users, not containment: following
                // the `Contains` edge from a symbol's own file hopped up
                // to the File node and then out through everything that
                // file touches. Observed live — a private helper with
                // exactly three callers reported 564 affected nodes,
                // 16% of the graph, including symbols in files with no
                // reference to it at all.
                if !matches!(e.edge_type, crate::schema::EdgeType::Calls | crate::schema::EdgeType::Uses) {
                    continue;
                }
                let source_id = e.source_id.clone();
                if visited.contains(&source_id) || !queued.insert(source_id.clone()) {
                    continue;
                }
                if let Ok(Some(caller)) = graph.get_node(&source_id) {
                    let is_direct = depth == 0;
                    affected_names.push((depth + 1, format!(
                        "  - {} ({:?}) in {}",
                        caller.name, caller.node_type, caller.path
                    )));
                    session_nodes.push(BlastRadiusNode {
                        id: caller.id.clone(),
                        name: caller.name.clone(),
                        node_type: format!("{:?}", caller.node_type),
                        path: caller.path.clone(),
                        // The caller sits one hop past the node we
                        // popped, so its depth is `depth + 1`. Emitting
                        // the parent's depth put every direct caller at
                        // 0, the seed's own level, and disagreed with
                        // the `[depth N]` tags in the text report.
                        depth: depth + 1,
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

    // The headline count must equal the number of listed names
    // (affected_names/session_nodes grow in lockstep, one per unique
    // resolvable caller); deriving it from `visited` instead drifted
    // whenever an edge pointed at an unresolvable node.
    let total_affected = affected_names.len();
    if affected_names.is_empty() {
        output.push_str("\n  (no dependents found — symbol may be a leaf or not yet indexed)");
        // Don't show total count when there are no names to show
    } else {
        // Direct callers and transitive reach answer different questions and
        // must not collapse into one number. Reverse closure through a
        // central dispatcher is huge and still correct: this helper has
        // three callers, and 434 nodes can reach it. Emitting all 434 in
        // discovery order buried the three that actually call it.
        let direct: Vec<&String> = affected_names
            .iter()
            .filter(|(d, _)| *d == 1)
            .map(|(_, n)| n)
            .collect();
        let mut by_depth: BTreeMap<u32, usize> = BTreeMap::new();
        for (d, _) in &affected_names {
            *by_depth.entry(*d).or_insert(0) += 1;
        }

        output.push_str(&format!("\n- Direct dependents ({}):", direct.len()));
        for name in direct.iter().take(LIST_CAP) {
            output.push_str(&format!("\n{}", name));
        }
        if direct.len() > LIST_CAP {
            output.push_str(&format!(
                "\n  ... and {} more direct",
                direct.len() - LIST_CAP
            ));
        }

        let indirect: Vec<&(u32, String)> =
            affected_names.iter().filter(|(d, _)| *d > 1).collect();
        if !indirect.is_empty() {
            let deepest = by_depth.keys().next_back().copied().unwrap_or(1);
            output.push_str(&format!(
                "\n- Indirect dependents ({}), reaching it only through the callers above; deepest chain {} levels:",
                indirect.len(),
                deepest
            ));
            // Still listed by name — the point of the split is that the
            // three real callers stop being buried, not that the rest
            // becomes invisible. Depth is tagged so a reader can tell a
            // direct break from a transitive one.
            for (d, name) in indirect.iter().take(LIST_CAP) {
                output.push_str(&format!("\n{} [depth {}]", name, d));
            }
            if indirect.len() > LIST_CAP {
                output.push_str(&format!(
                    "\n  ... and {} more indirect, by depth:",
                    indirect.len() - LIST_CAP
                ));
                for (d, count) in by_depth.iter().filter(|(d, _)| **d > 1) {
                    output.push_str(&format!("\n  - depth {}: {}", d, count));
                }
            }
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
    if let Some((sessions, port)) = ui_sessions {
        let data = UiSessionData::BlastRadius {
            symbol: symbol.to_string(),
            nodes: session_nodes,
        };
        store_ui_session_and_append_link(sessions, port, "blast-radius", data, "blast-radius", &mut output).await;
        output.push_str("\nClick nodes to mark approved, then describe your selection to the agent.");
    }

    Ok(output)
}

pub async fn get_coupling_radar(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    ui_sessions: Option<(&Arc<AsyncMutex<HashMap<String, UiSession>>>, u16)>,
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
    if let Some((sessions, port)) = ui_sessions {
        let data = UiSessionData::Coupling {
            symbol: symbol.to_string(),
            matrix: vec![],
            files: partners.iter().map(|(p, _)| p.clone()).take(20).collect(),
        };
        store_ui_session_and_append_link(sessions, port, "coupling", data, "coupling", &mut output).await;
        output.push_str("\nClick cells to see co-change details, then describe your selection to the agent.");
    }

    Ok(output)
}