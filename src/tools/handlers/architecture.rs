//! Architecture domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::NodeType;

pub fn explore_architecture(graph: &GraphDatabase, max_depth: usize) -> Result<String, LainError> {
    let files = graph.get_nodes_by_type(NodeType::File)?;
    
    let filtered: Vec<_> = files.iter()
        .filter(|f| f.depth_from_main.unwrap_or(u32::MAX) as usize <= max_depth)
        .take(20)
        .collect();

    Ok(format!("## Architecture Overview (Max Depth: {})\n\nFound {} total files. Showing top {}:\n\n{}",
        max_depth,
        files.len(),
        filtered.len(),
        filtered.iter().map(|f| {
            let depth = f.depth_from_main.map(|d| format!(" (depth: {})", d)).unwrap_or_default();
            format!("- {}{}", f.name, depth)
        }).collect::<Vec<_>>().join("\n")
    ))
}

pub fn list_entry_points(graph: &GraphDatabase) -> Result<String, LainError> {
    let entries = graph.find_entry_points()?;
    if entries.is_empty() {
        return Ok("No explicit entry points (main, App) found in graph.".to_string());
    }

    Ok(format!("## Entry Points\n\n{}",
        entries.iter().map(|n| format!("- {} ({})", n.name, n.path)).collect::<Vec<_>>().join("\n")
    ))
}

pub fn compare_modules(graph: &GraphDatabase, module_a: &str, module_b: &str) -> Result<String, LainError> {
    let nodes_a = graph.get_nodes_by_type(NodeType::File)?;
    let node_a = nodes_a.iter().find(|n| n.path == module_a || n.name == module_a)
        .ok_or_else(|| LainError::NotFound(format!("Module A not found: {}", module_a)))?;

    let nodes_b = graph.get_nodes_by_type(NodeType::File)?;
    let node_b = nodes_b.iter().find(|n| n.path == module_b || n.name == module_b)
        .ok_or_else(|| LainError::NotFound(format!("Module B not found: {}", module_b)))?;

    // Find children (functions, structs, etc.) for both
    let children_a = graph.get_edges_from(&node_a.id)?;
    let children_b = graph.get_edges_from(&node_b.id)?;

    let mut output = format!("## Comparison: {} vs {}\n\n", node_a.name, node_b.name);

    output.push_str("### Interface Overview\n");
    output.push_str(&format!("- **{}** has {} internal symbols.\n", node_a.name, children_a.len()));
    output.push_str(&format!("- **{}** has {} internal symbols.\n", node_b.name, children_b.len()));

    // Metrics comparison
    let anchor_a = node_a.anchor_score.unwrap_or(0.0);
    let anchor_b = node_b.anchor_score.unwrap_or(0.0);
    output.push_str("\n### Architectural Metrics\n");
    output.push_str(&format!("- **Anchor Score (Stability):** {} vs {}\n", format!("{:.3}", anchor_a), format!("{:.3}", anchor_b)));

    // Shared co-change partners (if any)
    let partners_a = graph.get_co_change_partners(&node_a.path)?;
    let partners_b = graph.get_co_change_partners(&node_b.path)?;
    
    let set_b: std::collections::HashSet<_> = partners_b.iter().map(|(p, _)| p).collect();
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

pub fn get_master_map(graph: &GraphDatabase) -> Result<String, LainError> {
    let modules = graph.get_nodes_by_type(NodeType::Namespace)?;
    let files = graph.get_nodes_by_type(NodeType::File)?;

    let mut output = "## Master Map: Staleness Report\n\n".to_string();
    output.push_str("Summary of knowledge staleness across the project:\n\n");

    output.push_str("| Module | Files | Last LSP Sync | Last Git Sync | Status |\n");
    output.push_str("| :--- | :---: | :--- | :--- | :---: |\n");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for m in modules {
        // Count files in this module based on path
        let module_files: Vec<_> = files.iter()
            .filter(|f| f.path.starts_with(&m.path))
            .collect();

        let last_lsp = module_files.iter()
            .filter_map(|f| f.last_lsp_sync)
            .max();
        
        let last_git = module_files.iter()
            .filter_map(|f| f.last_git_sync)
            .max();

        let lsp_time = last_lsp.map(|t| format_duration(now - t)).unwrap_or_else(|| "Never".to_string());
        let git_time = last_git.map(|t| format_duration(now - t)).unwrap_or_else(|| "Never".to_string());

        let status = if last_lsp.is_some() && last_git.is_some() {
            let staleness = (now - last_lsp.unwrap()).max(now - last_git.unwrap());
            if staleness < 3600 { "🟢 Fresh" } 
            else if staleness < 86400 { "🟡 Stale" }
            else { "🔴 Outdated" }
        } else {
            "⚪ Unknown"
        };

        output.push_str(&format!("| {} | {} | {} | {} | {} |\n", 
            m.name, module_files.len(), lsp_time, git_time, status));
    }

    Ok(output)
}

fn format_duration(seconds: i64) -> String {
    if seconds < 60 { format!("{}s ago", seconds) }
    else if seconds < 3600 { format!("{}m ago", seconds / 60) }
    else if seconds < 86400 { format!("{}h ago", seconds / 3600) }
    else { format!("{}d ago", seconds / 86400) }
}
