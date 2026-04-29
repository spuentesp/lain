//! Enricher that uses the knowledge graph

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, NodeType};

use crate::tools::handlers::decoration::types::{
    EnrichedError, EnrichedReport, FailureSummary, ParsedError,
};

/// Trait for enriching parsed errors with graph context
pub trait ErrorEnricher: Send + Sync {
    fn enrich(
        &self,
        errors: &[ParsedError],
        graph: &GraphDatabase,
        overlay: &VolatileOverlay,
    ) -> EnrichedReport;
}

/// Enricher that uses the knowledge graph
pub struct GraphEnricher;

impl ErrorEnricher for GraphEnricher {
    fn enrich(
        &self,
        errors: &[ParsedError],
        graph: &GraphDatabase,
        overlay: &VolatileOverlay,
    ) -> EnrichedReport {
        let mut enriched_errors = Vec::new();
        let mut affected_files: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut affected_symbols: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Phase 1: Per-error enrichment (cheap)
        for error in errors {
            let (symbol, anchor_score) = resolve_symbol(error, graph, overlay);

            if let Some(ref s) = symbol {
                affected_symbols.insert(s.clone());
            }
            affected_files.insert(error.path.clone());

            enriched_errors.push(EnrichedError {
                error: error.clone(),
                symbol,
                anchor_score,
            });
        }

        // Phase 2: Summary enrichment (expensive, batch)
        let combined_blast_radius = compute_combined_blast_radius(&affected_symbols, graph);
        let co_change_partners = compute_co_change_partners(&affected_files, graph);
        let architectural_note = generate_architectural_note(&enriched_errors, &affected_symbols);

        let summary = FailureSummary {
            affected_files: affected_files.into_iter().collect(),
            affected_symbols: affected_symbols.into_iter().collect(),
            combined_blast_radius,
            co_change_partners,
            architectural_note,
        };

        EnrichedReport {
            errors: enriched_errors,
            summary,
        }
    }
}

/// Resolve a symbol name from file:line using the graph
fn resolve_symbol(
    error: &ParsedError,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> (Option<String>, Option<f32>) {
    let path_str = error.path.to_string_lossy();

    // Try overlay first (uncommitted changes), then persistent graph
    let node = overlay
        .find_nodes_by_path(&path_str)
        .first()
        .cloned()
        .or_else(|| graph.find_node_by_path(&path_str));

    if let Some(node) = node {
        (Some(node.name.clone()), node.anchor_score)
    } else {
        (None, None)
    }
}

/// Compute combined blast radius for all affected symbols
fn compute_combined_blast_radius(
    symbols: &std::collections::HashSet<String>,
    graph: &GraphDatabase,
) -> Vec<String> {
    let mut all_reachable: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Build a name -> node mapping once (O(m) where m = total function nodes)
    let Ok(all_func_nodes) = graph.get_nodes_by_type(NodeType::Function) else {
        return Vec::new();
    };

    // Create a lookup map: symbol name -> node IDs
    // This reduces O(n*m) to O(n+m)
    let mut name_to_nodes: std::collections::HashMap<&str, Vec<_>> =
        std::collections::HashMap::new();
    for node in &all_func_nodes {
        name_to_nodes
            .entry(node.name.as_str())
            .or_default()
            .push(&node.id);
    }

    // For each symbol, look up nodes and traverse edges
    for symbol_name in symbols {
        if let Some(node_ids) = name_to_nodes.get(symbol_name.as_str()) {
            for node_id in node_ids {
                if let Ok(edges) = graph.get_edges_from(node_id) {
                    for edge in edges.iter().filter(|e| e.edge_type == EdgeType::Calls) {
                        if let Some(target) = graph.get_node(&edge.target_id).ok().flatten() {
                            all_reachable.insert(target.name.clone());
                        }
                    }
                }
            }
        }
    }

    let mut result: Vec<String> = all_reachable.into_iter().take(20).collect();
    result.sort();
    result
}

/// Find files that co-change with the affected files
fn compute_co_change_partners(
    files: &std::collections::HashSet<std::path::PathBuf>,
    graph: &GraphDatabase,
) -> Vec<(String, usize)> {
    let mut partners: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for file in files {
        if let Ok(pairs) = graph.get_co_change_partners(&file.to_string_lossy()) {
            for (name, count) in pairs {
                *partners.entry(name).or_insert(0) += count;
            }
        }
    }

    let mut result: Vec<(String, usize)> = partners.into_iter().collect();
    result.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    result.truncate(5);
    result
}

/// Generate an architectural note if there's a pattern in the failures
fn generate_architectural_note(
    errors: &[EnrichedError],
    symbols: &std::collections::HashSet<String>,
) -> Option<String> {
    if errors.len() >= 3 && !symbols.is_empty() {
        let mut symbol_error_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for e in errors {
            if let Some(ref s) = e.symbol {
                *symbol_error_counts.entry(s).or_insert(0) += 1;
            }
        }

        if let Some((most_repeated, count)) =
            symbol_error_counts.iter().max_by_key(|(_, c)| *c)
        {
            if *count >= 2 && errors.len() > 1 {
                return Some(format!(
                    "{} of {} failures are in `{}` — likely a single root cause",
                    count,
                    errors.len(),
                    most_repeated
                ));
            }
        }
    }
    None
}
