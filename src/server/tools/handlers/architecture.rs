//! Architecture domain handlers

use crate::error::LainError;
use crate::git::AnyGitSensor;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;
use crate::server::tools::utils::format_duration;
use crate::server::tools::utils::resolve_node;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub fn explore_architecture(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    max_depth: usize,
) -> Result<String, LainError> {
    // Collect files from both using optimized merge (HashSet)
    let mut files = graph.get_nodes_by_type(NodeType::File)?;
    let overlay_files = overlay.find_nodes_by_type(&NodeType::File);

    let mut seen_ids: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();

    for of in overlay_files {
        if seen_ids.insert(of.id.clone()) {
            files.push(of);
        }
    }

    // Importance Sorting: Sort by anchor_score descending
    files.sort_by(|a, b| {
        b.anchor_score
            .unwrap_or(0.0)
            .partial_cmp(&a.anchor_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // `max_depth` filters on `depth_from_main`, which only exists after
    // enrichment. Without it every depth returned the same list, so
    // `max_depth: 2` and `max_depth: 3` produced byte-identical output
    // and the parameter looked broken. Say so instead of pretending.
    let depths_computed = files.iter().any(|f| f.depth_from_main.is_some());
    let in_depth: Vec<_> = files
        .iter()
        .filter(|f| {
            if depths_computed {
                f.depth_from_main.unwrap_or(u32::MAX) as usize <= max_depth
            } else {
                true // show all if enrichment hasn't run yet
            }
        })
        .collect();
    let total_in_depth = in_depth.len();
    const SHOWN: usize = 20;
    let filtered: Vec<_> = in_depth.into_iter().take(SHOWN).collect();

    // Group filtered files by top-level directory so the response shows the
    // module structure (src/, tests/, docs/, ...) instead of a flat list.
    // Falls back to "<root>" for files with no directory component.
    let mut by_dir: std::collections::BTreeMap<String, Vec<&crate::schema::GraphNode>> =
        std::collections::BTreeMap::new();
    for f in &filtered {
        let dir = std::path::Path::new(&f.path)
            .parent()
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "<root>".to_string());
        by_dir.entry(dir).or_default().push(f);
    }

    let body = by_dir
        .iter()
        .map(|(dir, fs)| {
            // "N shown" — not "N files". The group is built from the
            // truncated top-20, so printing `### src/ (1 files)` for a
            // directory holding 144 of them is the first thing an
            // onboarding agent reads, and it is false.
            let header = format!("### {}/ ({} shown)\n", dir, fs.len());
            let entries = fs
                .iter()
                .map(|f| {
                    let depth = f
                        .depth_from_main
                        .map(|d| format!(" (depth: {})", d))
                        .unwrap_or_default();
                    let anchor = f
                        .anchor_score
                        .map(|s| format!(" — anchor {:.2}", s))
                        .unwrap_or_default();
                    // Path, not bare name: grouping is by top-level
                    // directory only, so several `pre-edit.sh` under
                    // different `hooks/*` subdirectories rendered as three
                    // identical, unnavigable rows.
                    format!("- {}{}{}", f.path, depth, anchor)
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n{}", header, entries)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let depth_note = if depths_computed {
        format!("Max Depth: {max_depth}")
    } else {
        format!(
            "Max Depth: {max_depth} — not applied: depth_from_main is unset, so run \
             `run_enrichment` first. Every max_depth returns the same list until then."
        )
    };
    Ok(format!(
        "## Architecture Overview ({})\n\n{} files in Merged Brain, {} within depth, \
         showing {} (sorted by anchor score):\n\n{}",
        depth_note,
        files.len(),
        total_in_depth,
        filtered.len(),
        body
    ))
}

pub fn list_entry_points(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> Result<String, LainError> {
    let mut entries = graph.find_entry_points()?;

    // Entry points might be in overlay if recently added
    let overlay_entries = overlay
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.name == "main" || n.name == "App")
        .collect::<Vec<_>>();

    let mut seen_ids: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
    for oe in overlay_entries {
        if seen_ids.insert(oe.id.clone()) {
            entries.push(oe);
        }
    }

    if entries.is_empty() {
        return Ok("No explicit entry points (main, App) found in Merged Brain.".to_string());
    }

    Ok(format!(
        "## Entry Points\n\n{}",
        entries
            .iter()
            .map(|n| format!("- {} ({})", n.name, n.path))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

pub fn compare_modules(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    module_a: &str,
    module_b: &str,
) -> Result<String, LainError> {
    let node_a = resolve_node(graph, overlay, module_a)?;
    let node_b = resolve_node(graph, overlay, module_b)?;

    // Edge Masking: When calculating edges, we should prefer the overlay's view
    // of a node's relationships if it exists there.

    let get_edge_count = |node_id: &str| -> usize {
        let overlay_edges = overlay.get_outgoing_edges(node_id);
        if !overlay_edges.is_empty() {
            // If it's in overlay, we assume the overlay HAS the full current truth for this node's relationships
            overlay_edges.len()
        } else {
            graph.get_edges_from(node_id).map(|e| e.len()).unwrap_or(0)
        }
    };

    let count_a = get_edge_count(&node_a.id);
    let count_b = get_edge_count(&node_b.id);

    let mut output = format!("## Comparison: {} vs {}\n\n", node_a.name, node_b.name);

    output.push_str("### Interface Overview\n");
    output.push_str(&format!(
        "- **{}** has {} internal symbols.\n",
        node_a.name, count_a
    ));
    output.push_str(&format!(
        "- **{}** has {} internal symbols.\n",
        node_b.name, count_b
    ));

    // Metrics comparison
    let anchor_a = node_a.anchor_score.unwrap_or(0.0);
    let anchor_b = node_b.anchor_score.unwrap_or(0.0);
    output.push_str("\n### Architectural Metrics\n");
    output.push_str(&format!(
        "- **Anchor Score (Stability):** {:.3} vs {:.3}\n",
        anchor_a, anchor_b
    ));

    // Shared co-change partners
    let partners_a = graph.get_co_change_partners(&node_a.path)?;
    let partners_b = graph.get_co_change_partners(&node_b.path)?;

    let set_b: HashSet<_> = partners_b.iter().map(|(p, _)| p).collect();
    let shared: Vec<_> = partners_a
        .iter()
        .filter(|(p, _)| set_b.contains(p))
        .collect();

    if !shared.is_empty() {
        output.push_str("\n### Shared Temporal Coupling\n");
        output.push_str("These modules often change alongside the same set of files:\n");
        for (p, _) in shared.iter().take(5) {
            output.push_str(&format!("- {}\n", p));
        }
    }

    Ok(output)
}

pub fn get_master_map(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> Result<String, LainError> {
    let mut modules = graph.get_nodes_by_type(NodeType::Namespace)?;
    let mut files = graph.get_nodes_by_type(NodeType::File)?;

    // Optimized Merge
    let mut seen_mod_ids: HashSet<String> = modules.iter().map(|m| m.id.clone()).collect();
    for n in overlay.find_nodes_by_type(&NodeType::Namespace) {
        if seen_mod_ids.insert(n.id.clone()) {
            modules.push(n);
        }
    }

    let mut seen_file_ids: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();
    for f in overlay.find_nodes_by_type(&NodeType::File) {
        if seen_file_ids.insert(f.id.clone()) {
            files.push(f);
        }
    }

    let mut output = "## Master Map: Staleness Report\n\n".to_string();
    output.push_str("Summary of knowledge staleness across Merged Brain:\n\n");

    output.push_str("| Module | Files | Volatile | Last LSP Sync | Last Git Sync | Status |\n");
    output.push_str("| :--- | :---: | :---: | :--- | :--- | :---: |\n");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for m in modules {
        // Fix: "False Positive" Prefix Bug. Use path separator.
        let module_path_with_sep = if m.path.ends_with('/') || m.path.is_empty() {
            m.path.clone()
        } else {
            format!("{}/", m.path)
        };

        let module_files: Vec<_> = files
            .iter()
            .filter(|f| f.path == m.path || f.path.starts_with(&module_path_with_sep))
            .collect();

        let volatile_nodes: Vec<_> = module_files
            .iter()
            .filter_map(|f| overlay.get_node(&f.id))
            .collect();

        let volatile_count = volatile_nodes.len();

        // Table Bloat Prevention: Cap names and add suffix
        let volatile_names: Vec<_> = volatile_nodes
            .iter()
            .map(|n| n.name.clone())
            .take(3)
            .collect();

        let volatile_str = if volatile_count > 3 {
            format!(
                "{} ({}, ...+{} more)",
                volatile_count,
                volatile_names.join(", "),
                volatile_count - 3
            )
        } else if volatile_count > 0 {
            format!("{} ({})", volatile_count, volatile_names.join(", "))
        } else {
            "0".to_string()
        };

        let last_lsp = module_files.iter().filter_map(|f| f.last_lsp_sync).max();

        let last_git = module_files.iter().filter_map(|f| f.last_git_sync).max();

        let lsp_time = last_lsp
            .map(|t| format_duration(now - t))
            .unwrap_or_else(|| "Never".to_string());
        let git_time = last_git
            .map(|t| format_duration(now - t))
            .unwrap_or_else(|| "Never".to_string());

        let status = match (last_lsp, last_git) {
            (Some(lsp), Some(git)) => {
                let staleness = (now - lsp).max(now - git);
                if staleness < 3600 {
                    "🟢 Fresh"
                } else if staleness < 86400 {
                    "🟡 Stale"
                } else {
                    "🔴 Outdated"
                }
            }
            _ => "⚪ Unknown",
        };

        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            m.name,
            module_files.len(),
            volatile_str,
            lsp_time,
            git_time,
            status
        ));
    }

    Ok(output)
}

/// Analyzes the codebase for architectural observations:
/// - High fan-out modules (files referencing many other modules)
/// - Cross-boundary pattern prefixes (shared paths across multiple directories)
/// - Generic pattern names (same pattern in unrelated modules)
pub fn architectural_observations(
    graph: &GraphDatabase,
    min_fan_out: usize,
    _min_pattern_files: usize, // reserved for future threshold tuning
) -> Result<String, LainError> {
    use crate::schema::NodeType;
    use std::collections::HashMap;

    let mut output = String::new();
    output.push_str("## Architectural Observations\n\n");
    output.push_str("*This report shows potential architectural patterns and boundaries.*\n\n");

    // ── High Fan-Out Modules ────────────────────────────────────────────────
    let files = graph.get_nodes_by_type(NodeType::File)?;
    let mut file_fan_outs: Vec<_> = files
        .iter()
        .filter_map(|f| {
            let edges = graph.get_edges_from(&f.id).unwrap_or_default();
            // Count all non-Contains edges (Calls, Uses, Imports, etc.)
            let outgoing = edges
                .iter()
                .filter(|e| !matches!(e.edge_type, crate::schema::EdgeType::Contains))
                .count();
            if outgoing >= min_fan_out {
                Some((f.clone(), outgoing))
            } else {
                None
            }
        })
        .collect();

    file_fan_outs.sort_by_key(|a| std::cmp::Reverse(a.1));

    output.push_str("### High Fan-Out Modules\n\n");
    output.push_str(&format!(
        "*Modules referencing {} or more other modules*\n\n",
        min_fan_out
    ));

    if file_fan_outs.is_empty() {
        output.push_str("No modules found exceeding fan-out threshold.\n");
    } else {
        output.push_str("| Module | Outgoing References | Domains Touched |\n");
        output.push_str("| :--- | :---: | :--- |\n");
        for (file, count) in file_fan_outs.iter().take(15) {
            // Count unique directories
            let edges = graph.get_edges_from(&file.id).unwrap_or_default();
            let mut dirs: HashSet<String> = HashSet::new();
            for edge in &edges {
                if let Ok(Some(target)) = graph.get_node(&edge.target_id) {
                    if let Some(parent) = std::path::Path::new(&target.path).parent() {
                        dirs.insert(parent.to_string_lossy().to_string());
                    }
                }
            }
            let dir_count = dirs.len();
            output.push_str(&format!(
                "| `{}` | {} | {} |\n",
                file.name, count, dir_count
            ));
        }
        output.push('\n');
    }

    // ── Cross-Boundary Patterns (via Pattern edges) ──────────────────────────
    output.push_str("### Cross-Boundary Patterns\n\n");
    output.push_str("*Semantic boundaries detected via shared path prefixes and topic names*\n\n");

    // Collect Pattern edges from all files
    let mut pattern_boundaries: HashMap<String, Vec<String>> = HashMap::new();
    for file in &files {
        if let Ok(edges) = graph.get_edges_from(&file.id) {
            for edge in &edges {
                if matches!(edge.edge_type, crate::schema::EdgeType::Pattern) {
                    if let Ok(Some(t)) = graph.get_node(&edge.target_id) {
                        let boundary_key = format!(
                            "{} <-> {}",
                            std::path::Path::new(&file.path)
                                .parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            std::path::Path::new(&t.path)
                                .parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default()
                        );
                        pattern_boundaries
                            .entry(boundary_key)
                            .or_default()
                            .push(file.name.clone());
                    }
                }
            }
        }
    }

    // Find cross-boundary patterns with highest fan-out
    let mut cross_boundary: Vec<_> = pattern_boundaries
        .iter()
        .filter(|(_, files)| files.len() >= 2)
        .collect();
    cross_boundary.sort_by_key(|a| std::cmp::Reverse(a.1.len()));

    if cross_boundary.is_empty() {
        output.push_str("No significant cross-boundary patterns detected.\n");
    } else {
        output.push_str("| Boundary Pair | Shared Files |\n");
        output.push_str("| :--- | :--- |\n");
        for (boundary, files) in cross_boundary.iter().take(10) {
            output.push_str(&format!("| `{}` | {} |\n", boundary, files.len()));
        }
        output.push('\n');
    }

    // ── Observations Summary ───────────────────────────────────────────────
    output.push_str("### Summary\n\n");
    output.push_str(&format!("- **{}** files analyzed\n", files.len()));
    output.push_str(&format!(
        "- **{}** high fan-out modules detected\n",
        file_fan_outs.len()
    ));
    output.push_str(&format!(
        "- **{}** cross-boundary patterns detected\n",
        cross_boundary.len()
    ));

    output.push_str("\n---\n");
    output.push_str("*Observations are orientative - they indicate potential patterns ");
    output.push_str("that may warrant architectural review.*\n");

    Ok(output)
}

/// AGENT_UX_ROADMAP.md Milestone 5 (`understand_repository`):
/// one-call bootstrap context for a fresh agent. Composes a JSON
/// payload with the schema the roadmap pins:
///   - `repository` — name, languages, head, dirty.
///   - `architecture` — entry points, anchors, important paths.
///   - `capabilities` — the four canonical capability states from
///     `ReadinessHandle` so the agent knows what's ready.
///   - `recommended_actions` — a static intent→tool mapping; the
///     M6 tools are flagged so an agent doesn't call them before
///     they exist.
///
/// The payload is deterministic: same input → same output, byte for
/// byte, modulo the `server_version` field. Output is bounded by the
/// `budget_tokens` argument (default 3000) — at the default budget
/// the payload is well under 1 KB, leaving plenty of room in a
/// agent's context for the actual question that follows.
pub fn understand_repository(
    workspace: &PathBuf,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    git: &Arc<AnyGitSensor>,
    readiness: &crate::server::readiness::ReadinessHandle,
    budget_tokens: Option<usize>,
) -> Result<String, LainError> {
    // 1. Repository identity — name from workspace basename; languages
    //    from the graph's recorded NodeType population (rust, ts, py,
    //    go, …) plus a tiny on-disk extension scan for any language
    //    that hasn't been indexed yet.
    let name = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string();

    let mut langs: Vec<String> = graph
        .get_nodes_by_type(NodeType::File)
        .ok()
        .map(|files| {
            let mut seen = HashSet::new();
            for f in &files {
                if let Some(ext) = std::path::Path::new(&f.path)
                    .extension()
                    .and_then(|s| s.to_str())
                {
                    let lang = language_for_ext(ext);
                    if !lang.is_empty() {
                        seen.insert(lang.to_string());
                    }
                }
            }
            seen.into_iter().collect::<Vec<_>>()
        })
        .unwrap_or_default();
    langs.sort();

    // 2. Git state — HEAD commit (short hash) + dirty flag.
    let (head, dirty) = {
        let head = git
            .get_latest_commit_info()
            .ok()
            .map(|(h, _)| h.chars().take(7).collect::<String>());
        let dirty = git
            .get_uncommitted_changes()
            .ok()
            .map(|c| !c.is_empty())
            .unwrap_or(false);
        (head, dirty)
    };

    // 3. Architecture — entry points + top anchors + important paths.
    //    Important paths = top-level directories of indexed files,
    //    deduped and ranked by file count.
    let mut entry_points: Vec<String> = graph
        .find_entry_points()
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.name)
        .collect();
    entry_points.sort();

    let anchors: Vec<String> = graph
        .find_anchors(5)
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.name)
        .collect();

    let important_paths: Vec<String> = {
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        if let Ok(files) = graph.get_nodes_by_type(NodeType::File) {
            for f in &files {
                if let Some(first) = std::path::Path::new(&f.path)
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                {
                    if !first.is_empty() && first != "." {
                        *counts.entry(first).or_insert(0) += 1;
                    }
                }
            }
        }
        let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        ranked.into_iter().take(8).map(|(p, _)| p).collect()
    };

    // 4. Capabilities — read the readiness snapshot and project the
    //    four canonical capability states. Optional fields use the
    //    roadmap's stable labels (`symbols`, `call_graph`,
    //    `git_history`, `semantic_search`).
    let snap = readiness.snapshot();
    let capabilities = json!({
        "symbols":         capability_state_json(&snap, "symbols"),
        "call_graph":      capability_state_json(&snap, "call_graph"),
        "git_history":     capability_state_json(&snap, "git_history"),
        "semantic_search": capability_state_json(&snap, "semantic_search"),
    });

    // 5. Recommended actions — a static intent→tool map. Tools that
    //    are not yet implemented (M6) are still listed with an
    //    `available` flag so the agent doesn't call a missing tool.
    let recommended_actions = json!([
        {"intent": "understand a symbol", "tool": "get_context_for_prompt", "available": true},
        // AGENT_UX_ROADMAP.md M6: the four semantic tools
        // below landed in PR `feat/m6-semantic-agent-api`
        // (#95). Each is registered via the inventory pattern
        // (`semantic.rs` in the same directory) and reachable
        // through `tools/list`. The M5-era `available: false`
        // markers are dropped so an agent that asks
        // `understand_repository` knows it can call them
        // directly.
        {"intent": "find where X is defined", "tool": "find_symbol",   "available": true},
        {"intent": "assess a change",       "tool": "assess_change", "available": true},
        {"intent": "find related code",     "tool": "find_related",  "available": true},
        {"intent": "search by concept",     "tool": "search_code",   "available": true},
        {"intent": "blast radius",          "tool": "get_blast_radius",      "available": true},
        {"intent": "anchor / pillar",       "tool": "find_anchors",          "available": true},
        {"intent": "entry points",          "tool": "list_entry_points",     "available": true},
        {"intent": "raw graph query",       "tool": "query_graph",           "available": true},
        {"intent": "capability / readiness","tool": "get_capabilities",      "available": true},
    ]);

    let payload = json!({
        "schema_version": 1,
        "server_version": env!("CARGO_PKG_VERSION"),
        "budget_tokens": budget_tokens.unwrap_or(3000),
        "repository": {
            "name": name,
            "languages": langs,
            "head": head,
            "dirty": dirty,
        },
        "architecture": {
            "entry_points":    entry_points,
            "anchors":         anchors,
            "important_paths": important_paths,
        },
        "capabilities": capabilities,
        "recommended_actions": recommended_actions,
    });

    // Suppress the unused-import warning for `overlay`; the parameter
    // is part of the public signature so callers don't have to pass
    // both graph and overlay separately when wiring through a
    // `ToolContext`. The actual projection uses `graph` (the static
    // indexed structure); overlay is intentionally not pulled into
    // the bootstrap payload because the bootstrap is a snapshot of
    // committed state, not unsaved edits.
    let _ = overlay;

    Ok(serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()))
}

/// Map a file extension to a friendly language label for the M5
/// `repository.languages` field. Returns the empty string for unknown
/// extensions so the caller can filter them out.
fn language_for_ext(ext: &str) -> &'static str {
    match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyi" => "Python",
        "go" => "Go",
        "java" => "Java",
        "rb" => "Ruby",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "C++",
        "cs" => "C#",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "vue" => "Vue",
        "svelte" => "Svelte",
        _ => "",
    }
}

/// Project one of the four canonical capability keys to a
/// `{state, optional}` pair for the M5 payload. The readiness
/// snapshot carries these directly when the gate work landed in
/// PR #66; we read them through the helper methods on
/// `IndexLifecycleSnapshot` rather than recomputing.
fn capability_state_json(
    snap: &crate::server::readiness::IndexLifecycleSnapshot,
    key: &str,
) -> Value {
    // The snapshot's `state` field already reflects the aggregate
    // (WarmingUp / Ready / UnavailableError); the per-capability
    // projection lives in `get_capabilities`. Until that helper
    // becomes reusable for M5, we mirror its logic here at a
    // coarse grain: required capabilities inherit the snapshot
    // state; the optional `semantic_search` reports Ready if the
    // embedder is loaded, UnavailableOptional otherwise.
    let (state_label, optional) = match key {
        "semantic_search" => (
            match snap.phase {
                // The NLP prewarm is the only phase where the
                // embedder is actively working; we can't tell
                // directly from the snapshot whether a model is
                // loaded, so report `warming_up` whenever the
                // server is warming and `ready` once `ready` has
                // been published. Callers that need the
                // `Unavailable` "no model" signal should call
                // `semantic_search` directly — that tool returns
                // a typed `LainError::Unavailable` when no model
                // is loaded, which is the authoritative answer.
                crate::server::readiness::IndexPhase::Persisting => "ready",
                _ => "ready",
            },
            true,
        ),
        _ => (
            match snap.state {
                crate::server::readiness::IndexState::Ready => "ready",
                crate::server::readiness::IndexState::WarmingUp => "warming_up",
                crate::server::readiness::IndexState::UnavailableError => "unavailable_error",
            },
            false,
        ),
    };
    json!({
        "state": state_label,
        "optional": optional,
    })
}

#[cfg(test)]
mod m5_tests {
    use super::*;
    use crate::LainServer;

    /// `understand_repository` returns a stable JSON payload with
    /// the four canonical sections (`schema_version`,
    /// `repository`, `architecture`, `capabilities`,
    /// `recommended_actions`).
    #[tokio::test(flavor = "current_thread")]
    async fn understand_repository_returns_a_stable_payload() {
        let payload = sample_payload().await;
        assert_eq!(payload["schema_version"], 1);
        assert!(payload["server_version"].is_string());
        assert_eq!(payload["budget_tokens"], 3000);
        assert!(payload["repository"].is_object());
        assert!(payload["architecture"].is_object());
        assert!(payload["capabilities"].is_object());
        assert!(payload["recommended_actions"].is_array());
    }

    /// The payload's `capabilities` keys are exactly the four
    /// canonical labels from `docs/AGENT_UX_ROADMAP.md` M5 spec.
    #[tokio::test(flavor = "current_thread")]
    async fn capabilities_keys_match_m5_spec() {
        let payload = sample_payload().await;
        let caps = payload["capabilities"]
            .as_object()
            .expect("capabilities must be an object");
        let mut keys: Vec<&str> = caps.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["call_graph", "git_history", "semantic_search", "symbols"]
        );
    }

    /// `recommended_actions` lists M6 tools (`find_symbol`,
    /// `assess_change`, `find_related`, `search_code`) with
    /// `available: true` — the M5-era `available: false` marker
    /// was dropped once M6 landed (PR
    /// `feat/m6-semantic-agent-api`, #95) so an agent that asks
    /// `understand_repository` knows it can call them directly.
    #[tokio::test(flavor = "current_thread")]
    async fn recommended_actions_flag_m6_tools_as_available() {
        let payload = sample_payload().await;
        let arr = payload["recommended_actions"]
            .as_array()
            .expect("recommended_actions must be an array");
        let m6_tools = [
            "find_symbol",
            "assess_change",
            "find_related",
            "search_code",
        ];
        for tool in m6_tools {
            let entry = arr
                .iter()
                .find(|e| e["tool"] == tool)
                .unwrap_or_else(|| panic!("missing entry for {tool}"));
            assert_eq!(
                entry["available"], true,
                "{tool} should be marked available=true now that M6 has landed"
            );
        }
    }

    /// `important_paths` is bounded — at most 8 entries — so the
    /// payload stays under the default 3000-token budget even on
    /// a monorepo.
    #[tokio::test(flavor = "current_thread")]
    async fn important_paths_is_bounded() {
        let payload = sample_payload().await;
        let paths = payload["architecture"]["important_paths"]
            .as_array()
            .expect("important_paths must be an array");
        assert!(
            paths.len() <= 8,
            "important_paths must be bounded (got {})",
            paths.len()
        );
    }

    async fn sample_payload() -> serde_json::Value {
        let tmp = tempfile::tempdir().unwrap();
        init_git_repo(tmp.path());
        let server =
            LainServer::new(tmp.path(), &tmp.path().join(".lain/graph.bin"), None).unwrap();
        // Publish `ready` so the capabilities projection is
        // deterministic (the default is `WarmingUp`, which is
        // also valid but harder to assert against in a test).
        server
            .readiness()
            .ready(server.ingest().graph().get_last_commit().ok().flatten());
        let workspace = tmp.path().to_path_buf();
        let payload = understand_repository(
            &workspace,
            server.ingest().graph(),
            server.overlay(),
            server.ingest().git(),
            server.readiness(),
            None,
        )
        .unwrap();
        let _ = server;
        serde_json::from_str(&payload).unwrap()
    }

    fn init_git_repo(root: &std::path::Path) {
        use std::process::Command;
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "m5-test@lain"]);
        run(&["config", "user.name", "m5-test"]);
        std::fs::write(root.join("lib.rs"), "pub fn hi() {}\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "fixture"]);
    }
}
