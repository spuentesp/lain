//! Search domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::schema::{GraphNode, NodeType};
use crate::tools::utils::{build_enriched_text, cosine_similarity};

pub fn semantic_search(
    graph: &GraphDatabase, 
    embedder: &NlpEmbedder, 
    query: &str, 
    limit: usize
) -> Result<String, LainError> {
    let mut all_nodes = Vec::new();
    for node_type in [NodeType::File, NodeType::Module, NodeType::Class, NodeType::Function] {
        all_nodes.extend(graph.get_nodes_by_type(node_type)?);
    }

    if all_nodes.is_empty() {
        return Ok(format!("No nodes found for semantic search. Run 'run_enrichment' first to build the graph."));
    }

    // Build enriched text for each node (code-aware vocabulary)
    let enriched: Vec<(String, GraphNode)> = all_nodes.iter().map(|n| {
        let text = build_enriched_text(n);
        (text, n.clone())
    }).collect();

    // Compute query embedding once
    let query_emb = embedder.embed(query)?;

    // Score all candidates by cosine similarity to query
    let mut scored: Vec<(&GraphNode, f32)> = Vec::new();
    for (text, node) in &enriched {
        let cand_emb = embedder.embed(text)?;
        let sim = cosine_similarity(&query_emb, &cand_emb);
        if sim > 0.3 {
            scored.push((node, sim));
        }
    }

    // Sort by hybrid score: combine similarity with anchor score
    scored.sort_by(|a, b| {
        let anchor_a = a.0.anchor_score.unwrap_or(0.0);
        let anchor_b = b.0.anchor_score.unwrap_or(0.0);
        // Hybrid: similarity + 0.3 * anchor_score
        let hybrid_a = a.1 + 0.3 * anchor_a;
        let hybrid_b = b.1 + 0.3 * anchor_b;
        hybrid_b.partial_cmp(&hybrid_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    let results: Vec<_> = scored.into_iter().take(limit).collect();

    Ok(format!("Found {} semantic results for '{}':\n{}",
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
