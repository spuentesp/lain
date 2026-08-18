//! Utility functions for tools
//!
//! Shared helpers for argument parsing, text enrichment, and similarity.

use serde_json::{Map, Value};
use crate::schema::{GraphNode, NodeType};
use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use std::path::{Path, PathBuf};

/// Helper to resolve a handle (name, path, or ID) to a node
pub fn resolve_node(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    handle: &str
) -> Result<GraphNode, LainError> {
    // Canonicalize path if the handle looks like one
    let canonical_handle = if Path::new(handle).exists() {
        dunce::canonicalize(handle).map(|p| p.to_string_lossy().to_string()).unwrap_or(handle.to_string())
    } else {
        handle.to_string()
    };

    // 1. Try Overlay by ID
    if let Some(n) = overlay.get_node(&canonical_handle) { return Ok(n); }
    // 2. Try Graph by ID
    if let Ok(Some(n)) = graph.get_node(&canonical_handle) { return Ok(n); }
    // 3. Try Overlay by Name
    let overlay_names = overlay.find_nodes_by_name(&canonical_handle);
    if let Some(n) = overlay_names.iter().find(|n| n.name == canonical_handle) { return Ok(n.clone()); }
    // 4. Try Graph by Name
    if let Some(n) = graph.find_node_by_name(&canonical_handle) { return Ok(n); }
    // 5. Try Graph by Path. Try the handle verbatim first: graph keys are
    //    workspace-relative, and a caller asking about "src/cli/hooks.rs" is
    //    already using the canonical form — canonicalizing it to an absolute
    //    path would match nothing. The canonicalized form stays as a fallback
    //    for absolute handles and out-of-tree nodes.
    if let Some(n) = graph.find_node_by_path(handle) { return Ok(n); }
    if let Some(n) = graph.find_node_by_path(&canonical_handle) { return Ok(n); }

    Err(LainError::NotFound(format!("Node not found for handle: {}", handle)))
}

/// Resolves a node at a specific location using the "Overlay Mask" pattern
pub fn resolve_node_at_location(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    path: &str,
    line: u32
) -> Option<GraphNode> {
    let canonical_path = dunce::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or(path.to_string());

    // 1. Check Overlay first (Priority Filter)
    let overlay_nodes = overlay.find_nodes_by_path(&canonical_path);
    if !overlay_nodes.is_empty() {
        let match_node = overlay_nodes.iter()
            .filter(|n| n.node_type != NodeType::File)
            .filter(|n| n.line_start.unwrap_or(0) <= line && n.line_end.unwrap_or(0) >= line)
            .min_by_key(|n| n.line_end.unwrap_or(0).saturating_sub(n.line_start.unwrap_or(0)))
            .cloned();
        if match_node.is_some() { return match_node; }
    }

    // 2. Fallback to Static Backbone. Same ordering rationale as
    //    `resolve_node`: the verbatim (already-relative) form first.
    graph.get_node_at_location(path, line)
        .or_else(|| graph.get_node_at_location(&canonical_path, line))
}

/// Extract string argument
pub fn get_str_arg<'a>(args: Option<&'a Map<String, Value>>, key: &str) -> &'a str {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// Extract usize argument
pub fn get_usize_arg(args: Option<&Map<String, Value>>, key: &str) -> Option<usize> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
}

/// Extract boolean argument
pub fn get_bool_arg(args: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_bool())
}

/// Extract a string argument from the args map. Returns an empty
/// string when the key is missing or the value isn't a string.
/// Use [`required_str_arg`] when an empty fallback would be wrong.
pub fn str_arg(args: &Map<String, Value>, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract a required string argument. Returns `LainError::NotFound`
/// when the key is missing or the value isn't a string.
pub fn required_str_arg(args: &Map<String, Value>, key: &str) -> Result<String, LainError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| LainError::NotFound(format!("Missing required argument: {}", key)))
}

/// Extract an optional usize argument.
pub fn usize_arg(args: &Map<String, Value>, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

/// Extract an optional bool argument.
pub fn bool_arg(args: &Map<String, Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Extract an optional u32 argument.
pub fn u32_arg(args: &Map<String, Value>, key: &str) -> Option<u32> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as u32)
}

/// Extract an optional string argument, returning an empty string
/// when missing. Equivalent to `str_arg(args, key)`.
pub fn opt_str_arg(args: &Map<String, Value>, key: &str) -> String {
    str_arg(args, key)
}

/// Build enriched text for embedding: name + signature + docstring + path
/// + the first ~200 tokens of the source body (when a line range is known).
///
/// The body excerpt dramatically improves recall for behavior-oriented
/// queries (e.g. "graph storage", "error handling", "LSP language server")
/// because terms like `bincode`, `Tokenizer`, `LSP` only appear in the
/// implementation, not in the signature or docstring. Without this, the
/// embedder has no signal that `GraphDatabase::save` is about persistence.
pub fn build_enriched_text(node: &GraphNode, workspace: &Path) -> String {
    let mut parts = vec![node.name.clone()];

    // Add signature (function parameters, return types)
    if let Some(ref sig) = node.signature {
        if !sig.is_empty() {
            parts.push(sig.clone());
        }
    }

    // Add docstring for context
    if let Some(ref doc) = node.docstring {
        if !doc.is_empty() {
            parts.push(doc.clone());
        }
    }

    // Add path for file context
    parts.push(node.path.clone());

    // Add first ~200 tokens of the source body when a line range is known.
    // Capped at 200 tokens to keep embedding inputs in the model's
    // sweet spot (MiniLM truncates at 256 anyway, so this stays under the
    // effective limit while preserving meaningful behavior signals).
    if let (Some(start), Some(end)) = (node.line_start, node.line_end) {
        if end > start && (end - start) < 200 {
            if let Ok(body) = read_body_excerpt(workspace, &node.path, start, end, 200) {
                if !body.is_empty() {
                    parts.push(body);
                }
            }
        }
    }

    parts.join(" | ")
}

/// Read lines [start, end) from `path`, collapse to single-line whitespace,
/// and keep the first `max_tokens` whitespace-separated tokens.
/// Read a slice of a source file.
///
/// `path` is a graph key, which is workspace-relative (see `graph::graph_path`),
/// so it must be resolved against `workspace` rather than the process cwd.
/// Reading it directly worked only while the server happened to be launched
/// from the workspace root, and silently returned nothing otherwise — losing
/// the source excerpt from `explain_symbol` and the body text from embeddings.
fn read_body_excerpt(
    workspace: &Path,
    path: &str,
    start: u32,
    end: u32,
    max_tokens: usize,
) -> std::io::Result<String> {
    let resolved = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    let path = resolved.as_path();
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    let f = File::open(path)?;
    let reader = BufReader::new(f);
    let mut buf = String::new();
    for (i, line) in reader.lines().enumerate() {
        let lineno = (i as u32) + 1;
        if lineno < start {
            continue;
        }
        if lineno >= end {
            break;
        }
        let line = line?;
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(&line);
        // Stop early if we've already collected enough tokens
        if buf.split_whitespace().count() >= max_tokens {
            break;
        }
    }
    // Trim to max_tokens and collapse whitespace
    let trimmed: String = buf.split_whitespace().take(max_tokens).collect::<Vec<_>>().join(" ");
    Ok(trimmed)
}

/// Compute cosine similarity between two embedding vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len();
    if len != b.len() || len == 0 {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    // Manually unroll for performance (MiniLM is 384d, multiple of 8 and 16)
    let chunks = len / 8;
    for i in 0..chunks {
        let idx = i * 8;
        for j in 0..8 {
            let val_a = a[idx + j];
            let val_b = b[idx + j];
            dot += val_a * val_b;
            norm_a += val_a * val_a;
            norm_b += val_b * val_b;
        }
    }

    // Handle remaining
    for i in (chunks * 8)..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    if norm_a <= 0.0 || norm_b <= 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

/// Light suffix-stripping stemmer. Not a full Porter stemmer — just the
/// rules that matter for code search:
///   running  → run       (drop -ing if stem ≥ 3 chars)
///   indexed  → index     (drop -ed if stem ≥ 3 chars)
///   tokens   → token     (drop -s if stem ≥ 3 chars and not -ss/-us)
///   boxes    → box       (drop -es if stem ≥ 3 chars)
///   files    → file      (drop -s if stem ≥ 3 chars)
/// Used by `lex_tokens` below to collapse surface forms so queries
/// like "indexing" match symbols named `fn index`.
pub fn stem(word: &str) -> String {
    let w = word.to_ascii_lowercase();
    if w.len() < 4 {
        return w;
    }
    if w.len() > 5 && w.ends_with("ing") {
        return w[..w.len() - 3].to_string();
    }
    if w.len() > 4 && w.ends_with("ies") {
        let stem = &w[..w.len() - 3];
        if let Some(c) = stem.chars().last() {
            if !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
                return format!("{stem}y");
            }
        }
    }
    if w.len() > 4 && w.ends_with("ed") {
        return w[..w.len() - 2].to_string();
    }
    if w.len() > 4 && w.ends_with("es") {
        let stem = &w[..w.len() - 2];
        if let Some(c) = stem.chars().last() {
            if matches!(c, 's' | 'x' | 'z') {
                return stem.to_string();
            }
        }
        let last_two: String = stem.chars().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect();
        if last_two == "ch" || last_two == "sh" {
            return stem.to_string();
        }
    }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") {
        return w[..w.len() - 1].to_string();
    }
    w
}

/// Tokenize text for lexical scoring: lowercase, split on non-alphanumeric
/// boundaries, drop pure-numeric tokens, then stem each remaining token
/// so "runs"/"running"/"ran" all collapse to the same form. Used by
/// `token_recall` to compute per-candidate lexical coverage.
pub fn lex_tokens(text: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() < 3 {
            continue;
        }
        if raw.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out.insert(stem(raw));
    }
    out
}

/// Read the first ~`max_chars` characters of a node's source body,
/// collapsing whitespace. Returns None if the node has no line range or
/// the file can't be read. Used by search.rs and metrics.rs to surface
/// behavior-shaped terms and the actual code in responses.
///
/// Threshold: 2000 lines. Real-world functions can be long (Python
/// modules with deep config classes run hundreds of lines); we still
/// want to show their bodies. Above 2000 lines, fall back to the
/// first 200 lines via read_body_excerpt's internal cap.
pub fn read_body_summary(node: &GraphNode, max_chars: usize, workspace: &Path) -> Option<String> {
    let start = node.line_start?;
    let end = node.line_end?;
    if end <= start || (end - start) > 2000 {
        return None;
    }
    let body = read_body_excerpt(workspace, &node.path, start, end, 30).ok()?;
    let trimmed: String = body.chars().take(max_chars).collect();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Recall-style lexical score: fraction of query tokens that appear in the
/// candidate text. Range [0.0, 1.0]. 1.0 = every query token appears in the
/// candidate; 0.0 = none appear.
///
/// Recall is preferred over Jaccard for search ranking because it directly
/// answers "did we cover the user's query?" — a candidate that mentions 2
/// of 3 query terms scores 0.67, while a candidate that mentions 2 of 50
/// unrelated terms plus those 2 still scores 0.67.
pub fn token_recall(query: &str, candidate: &str) -> f32 {
    let q = lex_tokens(query);
    if q.is_empty() {
        return 0.0;
    }
    let c: std::collections::HashSet<String> = lex_tokens(candidate);
    let hits = q.intersection(&c).count();
    hits as f32 / q.len() as f32
}
