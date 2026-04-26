//! Utility functions for tools
//!
//! Shared helpers for argument parsing, text enrichment, and similarity.

use serde_json::{Map, Value};
use crate::schema::GraphNode;

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
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}
