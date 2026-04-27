//! Search domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::nlp::NlpEmbedder;
use crate::schema::{GraphNode, NodeType};
use crate::tools::utils::{build_enriched_text, cosine_similarity};
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn semantic_search(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    tuning: &TuningConfig,
    query: &str,
    limit: usize,
) -> Result<String, LainError> {
    // 1. Gather all nodes using Shadow Masking (Priority Filter)
    let mut all_nodes = Vec::new();
    let mut masked_ids = HashSet::new();

    // Overlay has priority
    let overlay_nodes = overlay.get_all_nodes();
    for on in overlay_nodes {
        masked_ids.insert(on.id.clone());
        all_nodes.push(on);
    }

    // Add static nodes only if not masked by overlay
    let static_nodes = graph.get_all_nodes();
    for sn in static_nodes {
        if !masked_ids.contains(&sn.id) {
            // Only search significant node types to reduce noise and O(N) pressure
            if matches!(sn.node_type, NodeType::File | NodeType::Class | NodeType::Function | NodeType::Struct | NodeType::Trait) {
                all_nodes.push(sn);
            }
        }
    }

    if all_nodes.is_empty() {
        return Ok("No nodes found for semantic search in Merged Brain. Run 'run_enrichment' first.".to_string());
    }

    // 2. Compute query embedding once
    let query_emb = embedder.embed(query)?;

    // 3. Batch Scoring with Shadow Masking
    let mut scored: Vec<(&GraphNode, f32)> = Vec::new();
    let mut volatile_embed_count = 0;
    let mut cache = embedding_cache.lock();

    for node in &all_nodes {
        // Try cache first, then parse and cache, then on-demand
        let emb_opt: Option<Vec<f32>> = if let Some(cached) = cache.get(&node.id) {
            Some(cached.clone())
        } else if let Some(ref e_json) = node.embedding {
            serde_json::from_str::<Vec<f32>>(e_json).ok()
        } else if volatile_embed_count < 50 {
            volatile_embed_count += 1;
            let text = build_enriched_text(node);
            embedder.embed(&text).ok()
        } else {
            None
        };

        if let Some(emb) = emb_opt {
            // Cache the embedding for future searches (even if it came from cache, re-cache is no-op)
            if !cache.contains_key(&node.id) {
                if let Some(ref e_json) = node.embedding {
                    if let Ok(emb) = serde_json::from_str::<Vec<f32>>(e_json) {
                        cache.insert(node.id.clone(), emb);
                    }
                }
            }
            let sim = cosine_similarity(&query_emb, &emb);
            if sim > tuning.semantic_similarity_threshold {
                scored.push((node, sim));
            }
        }
    }

    // 4. Sort by hybrid score: combine similarity with anchor score (Importance Sorting)
    scored.sort_by(|a, b| {
        let anchor_a = a.0.anchor_score.unwrap_or(0.0);
        let anchor_b = b.0.anchor_score.unwrap_or(0.0);
        // Hybrid: similarity + anchor_weight * anchor_score
        let hybrid_a = a.1 + tuning.anchor_weight * anchor_a;
        let hybrid_b = b.1 + tuning.anchor_weight * anchor_b;
        hybrid_b.partial_cmp(&hybrid_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    let results: Vec<_> = scored.into_iter().take(limit).collect();

    Ok(format!("Found {} semantic results in Merged Brain for '{}' (using Shadow Masking):\n{}",
        results.len(),
        query,
        results.iter().enumerate().map(|(i, (n, sim))| {
            let anchor = n.anchor_score.map(|s| format!("{:.2}", s)).unwrap_or_else(|| "N/A".to_string());
            let sig = n.signature.as_ref().map(|s| format!(" | {}", s)).unwrap_or_default();
            format!("{}. {} ({:?}){} — sim: {:.3}, anchor: {}",
                i + 1, n.name, n.node_type, sig, sim, anchor)
        }).collect::<Vec<_>>().join("\n")
    ))
}
