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
    // Locate the actual call lines. `Calls` edges connect caller to
    // callee and carry no position, so this listed each calling
    // function's own definition range under the heading "Call sites" —
    // `build_core_memory at ...:19-360` is a 341-line span, not a call
    // site, and a function calling the target three times still counted
    // as one. Scanning the caller's body for the name gives the real
    // positions and the real count.
    let mut lines_by_caller: Vec<(&crate::schema::GraphNode, Vec<usize>)> = Vec::new();
    let mut total_sites = 0usize;
    for caller in &callers {
        let sites = call_lines_in(workspace, caller, &node.name);
        total_sites += sites.len();
        lines_by_caller.push((caller, sites));
    }

    let heading = if total_sites > 0 && total_sites != callers.len() {
        format!(
            "Call sites for '{}' ({} call(s) across {} function(s)):\n\n",
            symbol,
            total_sites,
            callers.len()
        )
    } else {
        format!(
            "Call sites for '{}' ({} found):\n\n",
            symbol,
            total_sites.max(callers.len())
        )
    };
    result.push_str(&heading);

    for (caller, sites) in lines_by_caller {
        if sites.is_empty() {
            // Nothing matched textually — the file may have moved on
            // since indexing. Fall back to the enclosing function and
            // say that is what this is, rather than passing a
            // definition range off as a call position.
            let loc = if let (Some(ls), Some(le)) = (caller.line_start, caller.line_end) {
                format!("{}:{}-{}", caller.path, ls, le)
            } else {
                caller.path.clone()
            };
            result.push_str(&format!(
                "- **{}** — enclosing function at {} (exact call line not found in the file on disk)\n",
                caller.name, loc
            ));
        } else {
            let joined = sites
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let label = if sites.len() == 1 { "line" } else { "lines" };
            result.push_str(&format!(
                "- **{}** ({}) calls it at {} {}\n",
                caller.name, caller.path, label, joined
            ));
        }
    }

    Ok(result)
}

/// 1-based line numbers inside `caller`'s body where `callee` appears as
/// a whole word.
///
/// `line_start` is used only to bound the scan and is treated as
/// advisory: it has been observed one off from the definition line, so
/// the range is widened by one rather than trusted exactly.
fn call_lines_in(
    workspace: &std::path::Path,
    caller: &crate::schema::GraphNode,
    callee: &str,
) -> Vec<usize> {
    if callee.is_empty() {
        return Vec::new();
    }
    let path = workspace.join(&caller.path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let (lo, hi) = match (caller.line_start, caller.line_end) {
        (Some(ls), Some(le)) => (ls.saturating_sub(1) as usize, le as usize + 1),
        _ => (0usize, usize::MAX),
    };
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lineno = idx + 1;
        if lineno < lo || lineno > hi {
            continue;
        }
        let mut found = false;
        for (i, _) in line.match_indices(callee) {
            let before = line[..i].chars().next_back();
            let after = line[i + callee.len()..].chars().next();
            let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if boundary(before) && boundary(after) {
                found = true;
                break;
            }
        }
        // Skip the definition itself.
        if found && !line.trim_start().starts_with("fn ") && !line.contains(&format!("fn {callee}")) {
            out.push(lineno);
        }
    }
    out
}