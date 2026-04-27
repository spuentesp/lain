//! Architecture domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;
use crate::tools::utils::resolve_node;
use std::collections::HashSet;

pub fn explore_architecture(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    max_depth: usize
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
        b.anchor_score.unwrap_or(0.0)
            .partial_cmp(&a.anchor_score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let depths_computed = files.iter().any(|f| f.depth_from_main.is_some());
    let filtered: Vec<_> = files.iter()
        .filter(|f| {
            if depths_computed {
                f.depth_from_main.unwrap_or(u32::MAX) as usize <= max_depth
            } else {
                true // show all if enrichment hasn't run yet
            }
        })
        .take(20)
        .collect();

    Ok(format!("## Architecture Overview (Max Depth: {})\n\nFound {} total files in Merged Brain. Showing top {} (sorted by importance):\n\n{}",
        max_depth,
        files.len(),
        filtered.len(),
        filtered.iter().map(|f| {
            let depth = f.depth_from_main.map(|d| format!(" (depth: {})", d)).unwrap_or_default();
            format!("- {}{}", f.name, depth)
        }).collect::<Vec<_>>().join("\n")
    ))
}

pub fn list_entry_points(graph: &GraphDatabase, overlay: &VolatileOverlay) -> Result<String, LainError> {
    let mut entries = graph.find_entry_points()?;
    
    // Entry points might be in overlay if recently added
    let overlay_entries = overlay.get_all_nodes().into_iter()
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

    Ok(format!("## Entry Points\n\n{}",
        entries.iter().map(|n| format!("- {} ({})", n.name, n.path)).collect::<Vec<_>>().join("\n")
    ))
}

pub fn compare_modules(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    module_a: &str, 
    module_b: &str
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
    output.push_str(&format!("- **{}** has {} internal symbols.\n", node_a.name, count_a));
    output.push_str(&format!("- **{}** has {} internal symbols.\n", node_b.name, count_b));

    // Metrics comparison
    let anchor_a = node_a.anchor_score.unwrap_or(0.0);
    let anchor_b = node_b.anchor_score.unwrap_or(0.0);
    output.push_str("\n### Architectural Metrics\n");
    output.push_str(&format!("- **Anchor Score (Stability):** {:.3} vs {:.3}\n", anchor_a, anchor_b));

    // Shared co-change partners
    let partners_a = graph.get_co_change_partners(&node_a.path)?;
    let partners_b = graph.get_co_change_partners(&node_b.path)?;
    
    let set_b: HashSet<_> = partners_b.iter().map(|(p, _)| p).collect();
    let shared: Vec<_> = partners_a.iter().filter(|(p, _)| set_b.contains(p)).collect();

    if !shared.is_empty() {
        output.push_str("\n### Shared Temporal Coupling\n");
        output.push_str("These modules often change alongside the same set of files:\n");
        for (p, _) in shared.iter().take(5) {
            output.push_str(&format!("- {}\n", p));
        }
    }

    Ok(output)
}

pub fn get_master_map(graph: &GraphDatabase, overlay: &VolatileOverlay) -> Result<String, LainError> {
    let mut modules = graph.get_nodes_by_type(NodeType::Namespace)?;
    let mut files = graph.get_nodes_by_type(NodeType::File)?;

    // Optimized Merge
    let mut seen_mod_ids: HashSet<String> = modules.iter().map(|m| m.id.clone()).collect();
    for n in overlay.find_nodes_by_type(&NodeType::Namespace) {
        if seen_mod_ids.insert(n.id.clone()) { modules.push(n); }
    }

    let mut seen_file_ids: HashSet<String> = files.iter().map(|f| f.id.clone()).collect();
    for f in overlay.find_nodes_by_type(&NodeType::File) {
        if seen_file_ids.insert(f.id.clone()) { files.push(f); }
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

        let module_files: Vec<_> = files.iter()
            .filter(|f| f.path == m.path || f.path.starts_with(&module_path_with_sep))
            .collect();
        
        let volatile_nodes: Vec<_> = module_files.iter()
            .filter_map(|f| overlay.get_node(&f.id))
            .collect();
        
        let volatile_count = volatile_nodes.len();
        
        // Table Bloat Prevention: Cap names and add suffix
        let volatile_names: Vec<_> = volatile_nodes.iter()
            .map(|n| n.name.clone())
            .take(3)
            .collect();
            
        let volatile_str = if volatile_count > 3 {
            format!("{} ({}, ...+{} more)", volatile_count, volatile_names.join(", "), volatile_count - 3)
        } else if volatile_count > 0 {
            format!("{} ({})", volatile_count, volatile_names.join(", "))
        } else {
            "0".to_string()
        };

        let last_lsp = module_files.iter()
            .filter_map(|f| f.last_lsp_sync)
            .max();
        
        let last_git = module_files.iter()
            .filter_map(|f| f.last_git_sync)
            .max();

        let lsp_time = last_lsp.map(|t| format_duration(now - t)).unwrap_or_else(|| "Never".to_string());
        let git_time = last_git.map(|t| format_duration(now - t)).unwrap_or_else(|| "Never".to_string());

        let status = match (last_lsp, last_git) {
            (Some(lsp), Some(git)) => {
                let staleness = (now - lsp).max(now - git);
                if staleness < 3600 { "🟢 Fresh" }
                else if staleness < 86400 { "🟡 Stale" }
                else { "🔴 Outdated" }
            }
            _ => "⚪ Unknown"
        };

        output.push_str(&format!("| {} | {} | {} | {} | {} | {} |\n", 
            m.name, module_files.len(), volatile_str, lsp_time, git_time, status));
    }

    Ok(output)
}

fn format_duration(seconds: i64) -> String {
    if seconds < 60 { format!("{}s ago", seconds) }
    else if seconds < 3600 { format!("{}m ago", seconds / 60) }
    else if seconds < 86400 { format!("{}h ago", seconds / 3600) }
    else { format!("{}d ago", seconds / 86400) }
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
    let mut file_fan_outs: Vec<_> = files.iter()
        .filter_map(|f| {
            let edges = graph.get_edges_from(&f.id).unwrap_or_default();
            // Count all non-Contains edges (Calls, Uses, Imports, etc.)
            let outgoing = edges.iter()
                .filter(|e| !matches!(e.edge_type, crate::schema::EdgeType::Contains))
                .count();
            if outgoing >= min_fan_out {
                Some((f.clone(), outgoing))
            } else {
                None
            }
        })
        .collect();

    file_fan_outs.sort_by(|a, b| b.1.cmp(&a.1));

    output.push_str("### High Fan-Out Modules\n\n");
    output.push_str(&format!("*Modules referencing {} or more other modules*\n\n", min_fan_out));

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
                if let Ok(target_nodes) = graph.get_node(&edge.target_id) {
                    if let Some(target) = target_nodes {
                        if let Some(parent) = std::path::Path::new(&target.path).parent() {
                            dirs.insert(parent.to_string_lossy().to_string());
                        }
                    }
                }
            }
            let dir_count = dirs.len();
            output.push_str(&format!(
                "| `{}` | {} | {} |\n",
                file.name,
                count,
                dir_count
            ));
        }
        output.push_str("\n");
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
                    if let Ok(target) = graph.get_node(&edge.target_id) {
                        if let Some(t) = target {
                            let boundary_key = format!(
                                "{} <-> {}",
                                std::path::Path::new(&file.path).parent()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                std::path::Path::new(&t.path).parent()
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
    }

    // Find cross-boundary patterns with highest fan-out
    let mut cross_boundary: Vec<_> = pattern_boundaries.iter()
        .filter(|(_, files)| files.len() >= 2)
        .collect();
    cross_boundary.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    if cross_boundary.is_empty() {
        output.push_str("No significant cross-boundary patterns detected.\n");
    } else {
        output.push_str("| Boundary Pair | Shared Files |\n");
        output.push_str("| :--- | :--- |\n");
        for (boundary, files) in cross_boundary.iter().take(10) {
            output.push_str(&format!("| `{}` | {} |\n", boundary, files.len()));
        }
        output.push_str("\n");
    }

    // ── Observations Summary ───────────────────────────────────────────────
    output.push_str("### Summary\n\n");
    output.push_str(&format!(
        "- **{}** files analyzed\n",
        files.len()
    ));
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
