//! Utility functions for tools
//!
//! Shared helpers for argument parsing, text enrichment, and similarity.

use serde_json::{Map, Value};
use crate::schema::{GraphNode, NodeType};
use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use std::path::Path;

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
    // 5. Try Graph by Path
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

    // 2. Fallback to Static Backbone
    graph.get_node_at_location(&canonical_path, line)
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

/// Build enriched text for embedding: name + signature + docstring + path
pub fn build_enriched_text(node: &GraphNode) -> String {
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

    parts.join(" | ")
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
