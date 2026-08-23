//! Context domain handlers - build LLM-optimized context

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::server::tools::utils::resolve_node;

pub fn get_context_for_prompt(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    max_tokens: Option<usize>,
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    let max_toks = max_tokens.unwrap_or(2000);

    let mut parts = Vec::new();

    // Node identity
    parts.push(format!("## {} ({:?})\n", node.name, node.node_type));
    parts.push(format!("Path: {}\n", node.path));

    // Signature
    if let Some(ref sig) = node.signature {
        parts.push(format!("Signature: `{}`\n", sig));
    }

    // Docstring
    if let Some(ref doc) = node.docstring {
        parts.push(format!("Documentation: {}\n", doc));
    }

    // Relationships (callers and callees)
    let callers = graph.get_edges_to(&node.id)?.into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.source_id).ok().flatten())
        .map(|n| n.name)
        .collect::<Vec<_>>();

    let callees = graph.get_edges_from(&node.id)?.into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.target_id).ok().flatten())
        .map(|n| n.name)
        .collect::<Vec<_>>();

    if !callers.is_empty() {
        parts.push(format!("Called by: {}\n", callers.join(", ")));
    }
    if !callees.is_empty() {
        parts.push(format!("Calls: {}\n", callees.join(", ")));
    }

    // Type context (for structs/enums)
    if matches!(node.node_type, crate::schema::NodeType::Struct | crate::schema::NodeType::Enum) {
        let uses = graph.get_edges_from(&node.id)?.into_iter()
            .filter(|e| e.edge_type == crate::schema::EdgeType::Uses)
            .filter_map(|e| graph.get_node(&e.target_id).ok().flatten())
            .map(|n| format!("{} ({:?})", n.name, n.node_type))
            .collect::<Vec<_>>();
        if !uses.is_empty() {
            parts.push(format!("Uses types: {}\n", uses.join(", ")));
        }
    }

    // Co-change partners
    let partners = graph.get_co_change_partners(&node.path)?;
    if !partners.is_empty() {
        parts.push(format!("Frequently co-changes with: {}\n",
            partners.iter().take(3).map(|(p, _)| p.clone()).collect::<Vec<_>>().join(", ")));
    }

    // Join and truncate
    let mut context = parts.join("\n");
    let token_count = context.split_whitespace().count() * 2; // rough estimate
    if token_count > max_toks {
        let words: Vec<&str> = context.split_whitespace().collect();
        let truncated = words.into_iter().take(max_toks / 2).collect::<Vec<_>>().join(" ");
        context = format!("{}...\n[truncated - {} tokens]", truncated, token_count);
    }

    Ok(context)
}

/// Resolve a graph-relative path against the repo it belongs to.
///
/// Graph paths are workspace-relative, and `std::fs` resolves a relative
/// path against the *process* working directory. In single-workspace
/// mode those coincide often enough that nothing showed; in a multi-repo
/// federation they never do, and `get_code_snippet` on a symbol in one
/// repo happily returned the same-named file from wherever the server
/// happened to be launched — `src/lib.rs` from lain's own checkout
/// instead of the repo that was asked about. Wrong file, no error.
fn resolve_against_workspace(workspace: &std::path::Path, path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    let joined = workspace.join(p);
    if joined.exists() {
        return joined.to_string_lossy().into_owned();
    }
    // Fall back to the caller's spelling: it may be relative to the
    // process cwd (single-workspace mode) or simply not exist, and the
    // read error should name what they actually asked for.
    path.to_string()
}

pub fn get_code_snippet(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    path: &str,
    line: Option<u32>,
    context_lines: Option<usize>,
) -> Result<String, LainError> {
    let ctx = context_lines.unwrap_or(10);
    let line_num = line.unwrap_or(1) as usize;
    let disk_path = resolve_against_workspace(workspace, path);

    // Try overlay first
    if let Some(node) = overlay.get_node(path) {
        if let (Some(ls), Some(le)) = (node.line_start, node.line_end) {
            return read_file_range(&disk_path, ls as usize, le as usize, ctx);
        }
    }

    // Fall back to graph
    if let Some(node) = graph.get_node_at_location(path, line.unwrap_or(1)) {
        if let (Some(ls), Some(le)) = (node.line_start, node.line_end) {
            return read_file_range(&disk_path, ls as usize, le as usize, ctx);
        }
    }

    // Just read the file with context around the line
    read_file_range(&disk_path, line_num.saturating_sub(ctx), line_num + ctx, ctx)
}

fn read_file_range(path: &str, start: usize, end: usize, _ctx: usize) -> Result<String, LainError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| LainError::Io(e.to_string()))?;
    let lines: Vec<&str> = content.lines().collect();

    let start = start.saturating_sub(1).min(lines.len());
    let end = end.min(lines.len());

    if start >= end {
        return Err(LainError::NotFound(format!("Invalid range: {} to {}", start + 1, end)));
    }

    let snippet: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:4}: {}", start + i + 1, l))
        .collect();

    Ok(format!("File: {}\nShowing lines {}-{}\n\n{}\n",
        path, start + 1, end, snippet.join("\n")))
}

pub fn get_call_sites(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    let freshness = graph.freshness(workspace, &node.path);
    let target_id = &node.id;

    // Find all callers (edges of type Calls pointing to this node)
    let callers = graph.get_edges_to(target_id)?.into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.source_id).ok().flatten())
        .collect::<Vec<_>>();

    if callers.is_empty() {
        // A leaf really has no callers; a stale file only looks like one. These
        // read identically to a caller acting on the answer, so distinguish them.
        return Ok(match freshness.note(&node.path) {
            Some(note) => format!(
                "{note}\nNo call sites found for '{symbol}' in the graph — \
                 callers added since the last index would not appear."
            ),
            None => format!("No call sites found for '{symbol}' ({symbol} is a leaf)"),
        });
    }

    let mut result = String::new();
    if let Some(note) = freshness.note(&node.path) {
        result.push_str(&note);
        result.push('\n');
    }
    result.push_str(&format!("Call sites for '{}' ({} found):\n\n", symbol, callers.len()));

    for caller in callers {
        let loc = if let (Some(ls), Some(le)) = (caller.line_start, caller.line_end) {
            format!("{}:{}-{}", caller.path, ls, le)
        } else {
            caller.path.clone()
        };
        result.push_str(&format!("- **{}** at {}\n", caller.name, loc));
    }

    Ok(result)
}