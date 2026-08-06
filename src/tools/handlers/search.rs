//! Search domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::nlp::NlpEmbedder;
use crate::schema::{GraphNode, NodeType};
use crate::tools::utils::{build_enriched_text, cosine_similarity, token_recall};
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
            // Search all symbol-bearing node types so queries like "Tokenizer"
            // can hit Enum variants, "GraphDatabase::save" can hit Impl methods,
            // and "graph storage" can hit Module/Constant nodes too.
            // Cross-runtime nodes (HttpRoute, Topic, Resource, Schema) are
            // excluded — they're document-shaped, not code-shaped.
            if matches!(sn.node_type,
                NodeType::File
                | NodeType::Namespace
                | NodeType::Module
                | NodeType::Package
                | NodeType::Class
                | NodeType::Interface
                | NodeType::Struct
                | NodeType::Enum
                | NodeType::Trait
                | NodeType::Function
                | NodeType::Method
                | NodeType::Property
                | NodeType::Variable
                | NodeType::Constant
            ) {
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
        } else if volatile_embed_count < 500 {
            // Raised from 50 → 500 so cold queries can embed a meaningful
            // slice of the corpus on the fly. The previous cap meant
            // anything beyond the first 50 nodes was effectively invisible
            // until the background prewarm queue caught up.
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
            // Hybrid scoring: combine semantic similarity with lexical token
            // recall. lex_weight = 0.0 falls back to pure cosine (default).
            // With lex_weight > 0, exact-term queries ("Tokenizer",
            // "GraphDatabase") surface even when the cosine score alone
            // is borderline.
            let lex = if tuning.lexical_weight > 0.0 {
                let text = build_enriched_text(node);
                token_recall(query, &text)
            } else {
                0.0
            };
            let hybrid = (1.0 - tuning.lexical_weight) * sim + tuning.lexical_weight * lex;
            if hybrid > tuning.semantic_similarity_threshold {
                scored.push((node, hybrid));
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
