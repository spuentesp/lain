//! Search domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::{CrossEncoder, NlpEmbedder};
use crate::overlay::VolatileOverlay;
use crate::schema::{GraphNode, NodeType};
use crate::tools::utils::{build_enriched_text, cosine_similarity, read_body_summary, token_recall};
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub fn semantic_search(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    embedder: &NlpEmbedder,
    cross_encoder: &CrossEncoder,
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
    // Apply query_prefix if configured (BGE-style asymmetric retrieval).
    // Documents embedded during ingestion are NOT prefixed — only the
    // user's query string gets the instruction.
    let query_for_embedding = if tuning.query_prefix.is_empty() {
        query.to_string()
    } else {
        format!("{}{}", tuning.query_prefix, query)
    };
    let query_emb = embedder.embed(&query_for_embedding)?;

    // 3. Batch Scoring with Shadow Masking
    let mut scored: Vec<(&GraphNode, f32)> = Vec::new();
    let mut volatile_embed_count = 0;
    let mut cache = embedding_cache.lock();

    for node in &all_nodes {
        // Try cache first, then parse and cache, then on-demand.
        // The volatile-embed branch ALSO caches its result so subsequent
        // queries don't re-embed the same nodes — this is critical for
        // latency: with 1722 nodes and ~44ms per embed, re-running 500
        // embeds per query adds 22s. With caching, query 2+ runs in <1s.
        let emb_opt: Option<Vec<f32>> = if let Some(cached) = cache.get(&node.id) {
            Some(cached.clone())
        } else if let Some(ref e_json) = node.embedding {
            serde_json::from_str::<Vec<f32>>(e_json).ok()
        } else if volatile_embed_count < 200 {
            // Cap cold-query on-demand embeddings so a single search call
            // stays fast even on large corpora. The per-call cache (set on
            // line below) means subsequent calls within the same process
            // reuse these 200 instead of recomputing, so cold cost amortizes
            // across the session rather than every call.
            volatile_embed_count += 1;
            let text = build_enriched_text(node);
            embedder.embed(&text).ok()
        } else {
            None
        };

        if let Some(emb) = emb_opt {
            // Cache whatever we have (persisted or volatile) so subsequent
            // queries can reuse it. Before this fix, only persisted
            // embeddings were cached, and every cold query paid the
            // full embed cost (~22s for 500 nodes on this corpus).
            if !cache.contains_key(&node.id) {
                cache.insert(node.id.clone(), emb.clone());
            }
            // Persist volatile embeddings back to graph.bin so the next
            // process start doesn't have to re-embed the same nodes.
            // Only the cold-embed path triggers a write — already-
            // persisted embeddings are read as-is. Each embedding is
            // ~3 KB (384 floats * 8 bytes JSON), so 200 new writes add
            // ~600 KB to graph.bin. Cheap, and the alternative (re-
            // embedding on every cold start) costs 8-30 s.
            if node.embedding.is_none() {
                if let Ok(emb_json) = serde_json::to_string(&emb) {
                    let mut updated = node.clone();
                    updated.embedding = Some(emb_json);
                    let _ = graph.upsert_node(updated);
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

    // 4. Sort by hybrid score: combine similarity with anchor score (Importance Sorting).
    // anchor_score is normalized to [0, 1] within this candidate set via min-max
    // so the formula behaves consistently regardless of corpus size or
    // how many times the indexer has re-run. Without this, anchor scores
    // can grow to 1000+ across reindexes and the formula
    //   sim + anchor_weight * anchor
    // becomes anchor-dominated even with anchor_weight=0.05, burying
    // semantically better matches.
    let max_anchor = scored.iter()
        .map(|(n, _)| n.anchor_score.unwrap_or(0.0))
        .fold(0.0f32, f32::max);
    let norm = if max_anchor > 0.0 { max_anchor } else { 1.0 };
    scored.sort_by(|a, b| {
        let anchor_a = a.0.anchor_score.unwrap_or(0.0) / norm;
        let anchor_b = b.0.anchor_score.unwrap_or(0.0) / norm;
        // Hybrid: similarity + anchor_weight * normalized_anchor_score
        let hybrid_a = a.1 + tuning.anchor_weight * anchor_a;
        let hybrid_b = b.1 + tuning.anchor_weight * anchor_b;
        hybrid_b.partial_cmp(&hybrid_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    // 5. Optional cross-encoder reranking on the top-K bi-encoder candidates.
    // The cross-encoder scores each (query, document) pair jointly — much
    // more accurate than cosine similarity but ~50ms per candidate. We only
    // rerank the top-K, so the cost stays bounded.
    //
    // For each candidate we keep BOTH scores: the cross-encoder logit
    // (used for ordering) and the original bi-encoder sim+lex hybrid
    // (used for the response label and threshold check).
    #[derive(Clone, Copy)]
    enum ScoreKind { BiHybrid, CrossLogit }
    #[derive(Clone, Copy)]
    struct Scored<'a> {
        node: &'a crate::schema::GraphNode,
        hybrid: f32,             // always bi-encoder sim+lex hybrid
        score: f32,              // the value used for ranking
        kind: ScoreKind,
    }
    let results: Vec<Scored> = if tuning.cross_encoder_top_k > 0 && cross_encoder.is_active() {
        let k = tuning.cross_encoder_top_k.min(scored.len());
        let mut reranked: Vec<Scored> = Vec::with_capacity(k);
        for (node, hybrid) in scored.iter().take(k) {
            let text = build_enriched_text(node);
            let ce_score = cross_encoder.score(query, &text).unwrap_or(0.0);
            reranked.push(Scored { node, hybrid: *hybrid, score: ce_score, kind: ScoreKind::CrossLogit });
        }
        reranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        // Anything beyond the top-K keeps its bi-encoder score
        let mut tail: Vec<Scored> = scored.iter().skip(k).map(|(n, h)| Scored {
            node: n, hybrid: *h, score: *h, kind: ScoreKind::BiHybrid,
        }).collect();
        let mut combined = reranked;
        combined.append(&mut tail);
        combined.into_iter().take(limit).collect()
    } else {
        scored.iter().take(limit).map(|(n, h)| Scored {
            node: n, hybrid: *h, score: *h, kind: ScoreKind::BiHybrid,
        }).collect()
    };

    Ok(format!("Found {} semantic results in Merged Brain for '{}' (using Shadow Masking):\n{}",
        results.len(),
        query,
        results.iter().enumerate().map(|(i, s)| {
            let anchor = s.node.anchor_score.map(|x| format!("{:.2}", x)).unwrap_or_else(|| "N/A".to_string());
            let sig = s.node.signature.as_ref().map(|x| format!(" | {}", x)).unwrap_or_default();
            // Short body excerpt so behavior-shaped terms (e.g. `bincode`
            // in GraphDatabase::save_to_disk) appear in the response text.
            let body = read_body_summary(s.node, 80)
                .map(|b| format!(" | {}", b))
                .unwrap_or_default();
            // Label depends on which score drives the ranking. Cross-encoder
            // logits are unbounded (often -10..+10) and only meaningful for
            // relative ordering — they don't reflect "similarity" in any
            // user-facing sense, so we label them differently.
            let (label, value) = match s.kind {
                ScoreKind::BiHybrid => ("sim", s.hybrid),
                ScoreKind::CrossLogit => ("rerank", s.score),
            };
            format!("{}. {} ({:?}){}{} — {}: {:.3}, anchor: {}",
                i + 1, s.node.name, s.node.node_type, sig, body, label, value, anchor)
        }).collect::<Vec<_>>().join("\n")
    ))
}
