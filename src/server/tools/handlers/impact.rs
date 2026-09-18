//! Impact analysis domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::server::tools::utils::resolve_node;
use crate::server::tools::{BlastRadiusNode, UiSession, UiSessionData};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// Store a UI session and append its interactive link to the output string.
/// Must be `async` so we can `.lock().await` synchronously — spawning the
/// insert races the agent's immediate fetch of the URL we return.
/// `port` is the actual HTTP listener port (stdio mode passes none and
/// the link is never emitted).
/// `ttl` comes from `IngestionConfig::ui_session_ttl_secs`. It was a
/// literal `600` here and in `navigation.rs` while the documented knob
/// carried the same default and no reader, so editing it did nothing.
async fn store_ui_session_and_append_link(
    sessions: &Arc<AsyncMutex<HashMap<String, UiSession>>>,
    port: u16,
    session_type: &str,
    data: UiSessionData,
    url_path: &str,
    output: &mut String,
    ttl: std::time::Duration,
) {
    let session_id = Uuid::new_v4().to_string();
    let session = UiSession {
        id: session_id.clone(),
        session_type: session_type.to_string(),
        created_at: std::time::SystemTime::now(),
        expires_at: std::time::SystemTime::now() + ttl,
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

/// Default minimum confidence a heuristic edge needs to pass through
/// `get_blast_radius` without an explicit `include_weak_edges=true`
/// flag. Overridable via the `LAIN_HEURISTIC_MIN_CONFIDENCE` env var
/// (parsed at call time so operators can tune it without a restart).
fn heuristic_min_confidence() -> f32 {
    match std::env::var("LAIN_HEURISTIC_MIN_CONFIDENCE") {
        Ok(s) => s.parse::<f32>().unwrap_or(0.5).clamp(0.0, 1.0),
        Err(_) => 0.5,
    }
}

/// True when an edge type counts as a "heuristic" caller that the
/// static graph alone would miss. These are kept out of the main
/// blast-radius list unless `include_weak_edges=true` or the
/// per-edge confidence clears the env-var threshold.
fn is_heuristic_edge(t: &crate::schema::EdgeType) -> bool {
    matches!(
        t,
        crate::schema::EdgeType::DynamicDispatch
            | crate::schema::EdgeType::BusTopic
            | crate::schema::EdgeType::RouteMatches
    )
}

pub async fn get_blast_radius(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    include_coupling: bool,
    include_weak_edges: bool,
    ui_sessions: crate::server::tools::UiLink<'_>,
) -> Result<String, LainError> {
    let (node, other_defs) =
        crate::server::tools::utils::resolve_node_ambiguous(graph, overlay, symbol)?;

    // Overlay freshness indicator
    let overlay_age = overlay.last_update_age_secs();
    let freshness = if overlay_age < 5.0 {
        format!("live ({:.1}s ago)", overlay_age)
    } else if overlay_age < 60.0 {
        format!("recent ({:.0}s ago)", overlay_age)
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
    let mut heuristic_caller_count = 0u32;

    while let Some((id, depth)) = queue.pop_front() {
        if visited.contains(&id) {
            continue;
        }
        visited.insert(id.clone());

        // Walk the static-graph incoming edges (build-time callers) AND
        // the volatile-overlay incoming edges (live, uncommitted
        // callers). Pre-fix this loop only walked the static graph, so
        // a brand-new caller added in an uncommitted edit was invisible
        // to blast radius — a new symbol that calls `foo` would not
        // appear in `foo`'s blast-radius report until the next commit
        // and reindex. The overlay is the source of truth for "what
        // does the working tree currently have" — see `navigation.rs`
        // for the same dual-walk pattern.
        let mut enqueue_caller = |source_id: String,
                                  caller: Option<&crate::schema::GraphNode>,
                                  is_heuristic: bool,
                                  confidence: Option<f32>| {
            if visited.contains(&source_id) || !queued.insert(source_id.clone()) {
                return;
            }
            let Some(caller) = caller else {
                queue.push_back((source_id, depth + 1));
                return;
            };
            let is_direct = depth == 0;
            let prefix = if is_heuristic { "  ~ " } else { "  - " };
            let conf_tag = match confidence {
                Some(c) => format!(" [heuristic, conf={:.2}]", c),
                None => String::new(),
            };
            affected_names.push((
                depth + 1,
                format!(
                    "{}{} ({:?}) in {}{}",
                    prefix, caller.name, caller.node_type, caller.path, conf_tag
                ),
            ));
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
            if is_heuristic {
                heuristic_caller_count += 1;
            }
            queue.push_back((source_id, depth + 1));
        };

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
                let is_dependency = matches!(
                    e.edge_type,
                    crate::schema::EdgeType::Calls | crate::schema::EdgeType::Uses
                );
                let is_heuristic = is_heuristic_edge(&e.edge_type);

                if !is_dependency && !is_heuristic {
                    continue;
                }

                // Heuristic edges respect `LAIN_HEURISTIC_MIN_CONFIDENCE`
                // unless the caller asked for them explicitly. The
                // threshold defaults to 0.5; tighter raises precision,
                // looser widens the net.
                if is_heuristic {
                    let conf = e.weight.unwrap_or(0.0);
                    if !include_weak_edges && conf < heuristic_min_confidence() {
                        continue;
                    }
                }

                // Static graph only stores the node id, not the node
                // struct. Look up the node for the format fields.
                let source_id = e.source_id.clone();
                let caller_opt = graph.get_node(&source_id).ok().flatten();
                enqueue_caller(
                    source_id,
                    caller_opt.as_ref(),
                    is_heuristic,
                    if is_heuristic { e.weight } else { None },
                );
            }
        }

        // Overlay: live, uncommitted callers. `get_incoming_edges`
        // returns `(GraphNode, EdgeType)` directly so we don't need a
        // second node-id lookup. Filter to dependency edges here too.
        for (caller, edge_type) in overlay.get_incoming_edges(&id) {
            let is_dependency = matches!(
                edge_type,
                crate::schema::EdgeType::Calls | crate::schema::EdgeType::Uses
            );
            let is_heuristic = is_heuristic_edge(&edge_type);
            if !is_dependency && !is_heuristic {
                continue;
            }
            // Overlay carries no provenance yet. Treat as moderate
            // confidence and honour the same threshold so the default
            // view stays clean. Once Tier 3 attaches provenance to
            // overlay edges this branch can read it instead.
            if is_heuristic && !include_weak_edges && 0.5 < heuristic_min_confidence() {
                continue;
            }
            enqueue_caller(caller.id.clone(), Some(&caller), is_heuristic, None);
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
    if heuristic_caller_count > 0 {
        output.push_str(&format!(
            "\n\n~ {} heuristic caller(s) included (dynamic dispatch / bus / router). \
             Lines prefixed with `~` are pattern-matched, not type-resolved. \
             Raise `LAIN_HEURISTIC_MIN_CONFIDENCE` to narrow, or pass \
             include_weak_edges=false to suppress entirely.",
            heuristic_caller_count
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

        let indirect: Vec<&(u32, String)> = affected_names.iter().filter(|(d, _)| *d > 1).collect();
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
        output.push_str(&format!(
            "\n- Total transitively affected nodes: {}",
            total_affected
        ));
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
    if let Some((sessions, port, ttl)) = ui_sessions {
        let data = UiSessionData::BlastRadius {
            symbol: symbol.to_string(),
            nodes: session_nodes,
        };
        store_ui_session_and_append_link(
            sessions,
            port,
            "blast-radius",
            data,
            "blast-radius",
            &mut output,
            ttl,
        )
        .await;
        output
            .push_str("\nClick nodes to mark approved, then describe your selection to the agent.");
    }

    Ok(output)
}

pub async fn get_coupling_radar(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    ui_sessions: crate::server::tools::UiLink<'_>,
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
        partners
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, (p, c))| { format!("{}. {} (changed together {} times)", i + 1, p, c) })
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Store UI session if rich format requested
    if let Some((sessions, port, ttl)) = ui_sessions {
        let data = UiSessionData::Coupling {
            symbol: symbol.to_string(),
            matrix: vec![],
            files: partners.iter().map(|(p, _)| p.clone()).take(20).collect(),
        };
        store_ui_session_and_append_link(
            sessions,
            port,
            "coupling",
            data,
            "coupling",
            &mut output,
            ttl,
        )
        .await;
        output.push_str(
            "\nClick cells to see co-change details, then describe your selection to the agent.",
        );
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
    use crate::sensors::dynamic_dispatch_sensor::scan_workspace_dispatch;

    fn temp_graph() -> (tempfile::TempDir, GraphDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        (dir, graph)
    }

    fn file_node(path: &str, ns: &RepoNamespace) -> GraphNode {
        let id = GraphNode::generate_id(&NodeType::File, path, "", None, ns);
        let mut n = GraphNode::new(NodeType::File, String::new(), path.to_string());
        n.id = id;
        n
    }

    fn target_node(name: &str, ns: &RepoNamespace) -> GraphNode {
        let id = GraphNode::generate_id(&NodeType::Function, "src/api.py", name, None, ns);
        let mut n = GraphNode::new(
            NodeType::Function,
            name.to_string(),
            "src/api.py".to_string(),
        );
        n.id = id;
        n
    }

    #[tokio::test]
    async fn blast_radius_with_weak_edges_includes_heuristic_callers() {
        let (dir, graph) = temp_graph();
        std::fs::write(
            dir.path().join("orders.py"),
            "def publish():\n    bus.publish('orders', payload)\n",
        )
        .unwrap();
        let ns = RepoNamespace::for_test();

        // Run the sensor so the heuristic edge is in the graph.
        let _ = scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();

        let target = target_node("handle_order", &ns);
        graph.upsert_node(target.clone()).unwrap();
        // Force the heuristic edge to point at our target by replacing
        // its target_id with the target's id. The sensor emits edges
        // to a synthetic Hub, which is good for transitive coverage
        // but not for this focused test.
        let hub_name = "Hub:message_bus_publisher";
        let hub_id = GraphNode::generate_id(&NodeType::Function, "__hub__", hub_name, None, &ns);
        let edge = GraphEdge {
            edge_type: EdgeType::BusTopic,
            source_id: file_node("orders.py", &ns).id,
            target_id: target.id.clone(),
            weight: Some(0.7),
            cross_repo: false,
            provenance: Some(EdgeProvenance::Heuristic {
                detector: "message_bus_publisher".to_string(),
                confidence: 0.7,
            }),
        };
        graph.upsert_node(file_node("orders.py", &ns)).unwrap();
        graph
            .upsert_node({
                let mut n = GraphNode::new(NodeType::Function, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        graph.insert_edges_batch(&[edge]).unwrap();

        let overlay = VolatileOverlay::new();
        let output = get_blast_radius(&graph, &overlay, "handle_order", false, true, None)
            .await
            .unwrap();

        assert!(
            output.contains("heuristic") || output.contains("[heuristic"),
            "expected heuristic tag in output, got:\n{output}"
        );
        assert!(
            output.contains("conf=0.70"),
            "expected confidence tag in output, got:\n{output}"
        );
    }

    #[tokio::test]
    async fn blast_radius_without_weak_edges_suppresses_below_threshold_callers() {
        let (dir, graph) = temp_graph();
        std::fs::write(
            dir.path().join("orders.py"),
            "def publish():\n    bus.publish('orders', payload)\n",
        )
        .unwrap();
        let ns = RepoNamespace::for_test();

        let _ = scan_workspace_dispatch(&graph, dir.path(), &ns).unwrap();
        let target = target_node("handle_order", &ns);
        graph.upsert_node(target.clone()).unwrap();
        let hub_name = "Hub:serde_value";
        let hub_id = GraphNode::generate_id(&NodeType::Function, "__hub__", hub_name, None, &ns);
        graph.upsert_node(file_node("orders.py", &ns)).unwrap();
        graph
            .upsert_node({
                let mut n = GraphNode::new(NodeType::Function, hub_name.to_string(), String::new());
                n.id = hub_id.clone();
                n
            })
            .unwrap();
        // Below default threshold (0.5) so the default
        // include_weak_edges=false path should drop it.
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::DynamicDispatch,
                source_id: file_node("orders.py", &ns).id,
                target_id: target.id.clone(),
                weight: Some(0.3),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "serde_value".to_string(),
                    confidence: 0.3,
                }),
            }])
            .unwrap();

        let overlay = VolatileOverlay::new();
        let output = get_blast_radius(&graph, &overlay, "handle_order", false, false, None)
            .await
            .unwrap();

        assert!(
            !output.contains("[heuristic"),
            "below-threshold heuristic edges must be suppressed when include_weak_edges=false, got:\n{output}"
        );
        assert!(
            !output.contains("heuristic caller(s) included"),
            "summary line must be absent when no heuristic edges pass"
        );
    }
}
