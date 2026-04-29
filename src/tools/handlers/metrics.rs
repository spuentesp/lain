//! Metrics and explanation domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;
use crate::tools::utils::{build_enriched_text, cosine_similarity, resolve_node};
use std::sync::Arc;
use parking_lot::Mutex;
use std::collections::HashMap;

pub fn find_anchors(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    limit: usize
) -> Result<String, LainError> {
    let mut anchors = graph.find_anchors(limit)?;
    
    let overlay_anchors = overlay.get_all_nodes().into_iter()
        .filter(|n| n.anchor_score.is_some())
        .collect::<Vec<_>>();

    let mut seen_ids: std::collections::HashSet<String> = anchors.iter().map(|a| a.id.clone()).collect();
    for oa in overlay_anchors {
        if seen_ids.insert(oa.id.clone()) {
            anchors.push(oa);
        }
    }
    anchors.sort_by(|a, b| {
        b.anchor_score
            .unwrap_or(0.0)
            .total_cmp(&a.anchor_score.unwrap_or(0.0))
    });

    if anchors.is_empty() {
        return Ok("No anchors found in Merged Brain.".to_string());
    }

    Ok(format!("Top {} anchors (Merged Brain):\n{}",
        anchors.len(),
        anchors.iter().enumerate().take(limit).map(|(i, n)| {
            let score = n.anchor_score.map(|s| format!("{:.3}", s)).unwrap_or_else(|| "N/A".to_string());
            format!("{}. {} (score: {})", i + 1, n.name, score)
        }).collect::<Vec<_>>().join("\n")
    ))
}

pub fn get_anchor_score(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    match node.anchor_score {
        Some(s) => Ok(format!("Anchor score for '{}': {:.3}", symbol, s)),
        None => Ok(format!("Symbol '{}' has no anchor score in Merged Brain.", symbol)),
    }
}

pub fn get_context_depth(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    match node.depth_from_main {
        Some(d) => Ok(format!("Context depth for '{}': {} layers from entry", symbol, d)),
        None => Ok(format!("Symbol '{}' has no depth score in Merged Brain.", symbol)),
    }
}

/// Names that commonly indicate a false positive (trait defaults, constructors, etc.)
const FALSE_POSITIVE_PATTERNS: &[&str] = &[
    "default", "new", "clone", "from", "into", "as_ref", "as_mut",
    "to_string", "to_owned", "debug", "display", "fmt", "format",
    "from_str", "parse", "try_from", "try_into", "borrowed",
];

/// Check if a function name matches known false-positive patterns
fn is_false_positive_name(name: &str) -> bool {
    FALSE_POSITIVE_PATTERNS.iter().any(|p| name == *p || name.ends_with(p))
}

/// Check if function appears in a trait definition (heuristic: path contains "trait")
fn is_trait_context(path: &str) -> bool {
    path.contains("trait") || path.contains("_trait")
}

/// Check if a function is a likely false positive dead code candidate
fn is_likely_false_positive(node: &crate::schema::GraphNode) -> bool {
    // Trait default implementations are not dead code
    if is_trait_context(&node.path) {
        return true;
    }
    // Functions with common constructor/default names are likely false positives
    if is_false_positive_name(&node.name) {
        return true;
    }
    // Functions that call other functions are more likely to be utilities/helpers
    // not truly dead - they have a job even if not directly called
    if node.fan_out.unwrap_or(0) > 0 {
        return true;
    }
    false
}

pub fn find_dead_code(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    like: Option<&str>,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
) -> Result<String, LainError> {
    let functions = graph.get_nodes_by_type(NodeType::Function)?;

    // Primary filter: fan_in == 0 (no incoming calls)
    let candidates: Vec<_> = functions.into_iter()
        .filter(|f| f.fan_in.unwrap_or(0) == 0)
        .collect();

    // Filter out known false positives
    let truly_dead: Vec<_> = candidates.iter()
        .filter(|f| !is_likely_false_positive(f))
        .cloned()
        .collect();

    // Secondary signal: functions with BOTH fan_in == 0 AND fan_out == 0
    // are the most likely true dead code (leaf nodes with no callers)
    let highly_confident: Vec<_> = truly_dead.iter()
        .filter(|f| f.fan_out.unwrap_or(0) == 0)
        .cloned()
        .collect();

    // Return the highly confident set (true dead code)
    let results = highly_confident;

    // If user provided a "like" query, filter semantically
    let filtered = if let Some(query) = like {
        let query_emb = embedder.embed(query)?;
        let threshold = 0.3; // semantic similarity threshold

        results.into_iter().filter(|n| {
            let node_emb = get_embedding(n, embedder, embedding_cache);
            if let Some(emb) = node_emb {
                cosine_similarity(&query_emb, &emb) > threshold
            } else {
                false
            }
        }).collect()
    } else {
        results
    };

    let (label, items) = if filtered.is_empty() {
        ("likely dead", truly_dead.as_slice())
    } else {
        ("highly confident dead", filtered.as_slice())
    };

    Ok(format!(
        "Found {} {} symbols in Static Backbone:\n{}",
        items.len(),
        label,
        items.iter().take(20).map(|n| {
            let signals = {
                let mut s = Vec::new();
                if n.fan_in.unwrap_or(0) == 0 { s.push("no callers"); }
                if n.fan_out.unwrap_or(0) == 0 { s.push("no callees"); }
                if is_false_positive_name(&n.name) { s.push("common name"); }
                if is_trait_context(&n.path) { s.push("trait context"); }
                s.join(", ")
            };
            format!("- {} ({}) [{}]", n.name, n.path, signals)
        }).collect::<Vec<_>>().join("\n")
    ))
}

// Helper to get embedding for a node (cache-first, then on-demand)
fn get_embedding(
    node: &crate::schema::GraphNode,
    embedder: &NlpEmbedder,
    cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
) -> Option<Vec<f32>> {
    // Check cache
    if let Some(emb) = cache.lock().get(&node.id).cloned() {
        return Some(emb);
    }
    // Check stored embedding
    if let Some(ref e_json) = node.embedding {
        if let Ok(emb) = serde_json::from_str::<Vec<f32>>(e_json) {
            cache.lock().insert(node.id.clone(), emb.clone());
            return Some(emb);
        }
    }
    // On-demand embed
    let text = build_enriched_text(node);
    embedder.embed(&text).ok()
}

pub fn explain_symbol(
    graph: &GraphDatabase, 
    overlay: &VolatileOverlay,
    symbol: &str
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;

    let mut lines = Vec::new();
    lines.push(format!("## Explanation for '{}' ({:?})", symbol, node.node_type));
    lines.push(format!("**Path:** {}", node.path));
    
    if let Some(sig) = &node.signature {
        lines.push(format!("**Signature:** `{}`", sig));
    }
    
    if let Some(doc) = &node.docstring {
        lines.push(format!("**Documentation:**\n{}", doc));
    }

    lines.push(String::new());
    lines.push("### Structural Context".to_string());
    
    let depth = node.depth_from_main.map(|d| d.to_string()).unwrap_or_else(|| "N/A".to_string());
    let anchor = node.anchor_score.map(|s| format!("{:.3}", s)).unwrap_or_else(|| "N/A".to_string());
    
    lines.push(format!("- **Context Depth:** {} (Lower is closer to entry point)", depth));
    lines.push(format!("- **Anchor Score:** {} (Higher means more foundational)", anchor));
    
    let partners = graph.get_co_change_partners(&node.path)?;
    if !partners.is_empty() {
        lines.push(String::new());
        lines.push("### Frequently Co-Changed With (Git History)".to_string());
        for (p, c) in partners.iter().take(5) {
            lines.push(format!("- {} ({} times)", p, c));
        }
    }

    Ok(lines.join("\n"))
}

pub fn suggest_refactor_targets(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    limit: usize
) -> Result<String, LainError> {
    let node_types = [NodeType::File, NodeType::Module, NodeType::Class, NodeType::Function];
    let all_nodes = graph.get_nodes_by_types(&node_types)?;

    if all_nodes.is_empty() {
        return Ok("No nodes found in Static Backbone to analyze. Run enrichment first.".to_string());
    }

    let mut targets: Vec<_> = all_nodes.into_iter().map(|n| {
        let fan_in = n.fan_in.unwrap_or(0);
        let fan_out = n.fan_out.unwrap_or(0);
        let co_change = n.co_change_count.unwrap_or(0);
        let anchor = n.anchor_score.unwrap_or(0.0);

        let debt_score = (fan_in as f32 * fan_out as f32) + (co_change as f32 / (anchor + 0.1));
        
        let mut reasons = Vec::new();
        if fan_in > 10 && fan_out > 10 { reasons.push("Potential 'God Object' (high fan-in/fan-out)"); }
        if co_change > 5 && anchor < 0.2 { reasons.push("Fragile/Spaghetti logic (high coupling, low stability)"); }
        if fan_out > 20 { reasons.push("High complexity/fan-out"); }

        (n, debt_score, reasons)
    })
    .filter(|(_, _, reasons)| !reasons.is_empty())
    .collect();

    targets.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if targets.is_empty() {
        return Ok("Architecture appears healthy! No high-debt refactor targets identified in Static Backbone.".to_string());
    }

    let mut output = "## Refactor Target Suggestions\n\n".to_string();
    output.push_str("Identified the following areas of high architectural debt:\n\n");

    for (node, _, reasons) in targets.iter().take(limit) {
        output.push_str(&format!("### {} ({:?})\n", node.name, node.node_type));
        output.push_str(&format!("- **Path:** {}\n", node.path));
        for reason in reasons {
            output.push_str(&format!("- **⚠️ {}**\n", reason));
        }
        output.push('\n');
    }

    Ok(output)
}
