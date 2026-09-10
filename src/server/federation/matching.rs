use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeType, GraphNode};

pub fn signature_tokens(sig: &str) -> Vec<String> {
    sig.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .filter(|s| s != "fn")
        .collect()
}

pub fn signature_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    use std::collections::HashMap;
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for token in a {
        counts.entry(token.as_str()).or_insert((0, 0)).0 += 1;
    }
    for token in b {
        counts.entry(token.as_str()).or_insert((0, 0)).1 += 1;
    }

    let mut dot = 0usize;
    let mut norm_a = 0usize;
    let mut norm_b = 0usize;
    for (count_a, count_b) in counts.values() {
        dot += count_a * count_b;
        norm_a += count_a * count_a;
        norm_b += count_b * count_b;
    }

    let denom = (norm_a as f32).sqrt() * (norm_b as f32).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot as f32 / denom
    }
}

pub fn find_cross_repo_matches(
    new_node: &GraphNode,
    candidates: &[GraphNode],
    top_k: usize,
    threshold: f32,
) -> Vec<(String, f32)> {
    // `CrossRepoSameSymbol` is schema-restricted to Function/Method
    // endpoints (schema.rs's `source_types`/`target_types`) — this is
    // the matcher's "same *symbol*, not same *anything*" contract.
    // That restriction was never enforced here, which stayed invisible
    // while a separate bug (project_repo feeding this function
    // un-rewritten local ids) meant every candidate was dropped before
    // node type could matter. With that bug fixed, an unfiltered
    // candidate list would pair up every same-named non-function node
    // too — e.g. every repo's `Cargo.toml` `File` node matching every
    // other repo's, which both defeats the edge's purpose and (upstream
    // in `project_repo`) can target a node the backend schema never
    // intended this edge type to reach.
    if !EdgeType::CrossRepoSameSymbol
        .source_types()
        .contains(&new_node.node_type)
    {
        return Vec::new();
    }
    let candidates: Vec<&GraphNode> = candidates
        .iter()
        .filter(|c| {
            EdgeType::CrossRepoSameSymbol
                .target_types()
                .contains(&c.node_type)
        })
        .collect();

    let new_repo = GlobalId::parse(&new_node.id)
        .ok()
        .map(|global_id| global_id.repo_id().to_string());
    // Fall back to a name-only signal when the signature is empty:
    // rust-analyzer's documentSymbol doesn't always populate `detail`
    // (the signature field), so a function's name is the strongest
    // signal we have. Two functions with the same name in different
    // repos produce a single-token match, giving similarity 1.0 (above
    // the 0.5 threshold).
    let new_tokens = if let Some(sig) = new_node.signature.as_deref() {
        if !sig.is_empty() {
            signature_tokens(sig)
        } else {
            vec![new_node.name.to_lowercase()]
        }
    } else {
        vec![new_node.name.to_lowercase()]
    };
    let mut scored: Vec<(String, f32)> = candidates
        .iter()
        .filter_map(|candidate| {
            let candidate_repo = GlobalId::parse(&candidate.id).ok()?.repo_id().to_string();
            if Some(&candidate_repo) == new_repo.as_ref() {
                return None;
            }

            let candidate_tokens = if let Some(sig) = candidate.signature.as_deref() {
                if !sig.is_empty() {
                    signature_tokens(sig)
                } else {
                    vec![candidate.name.to_lowercase()]
                }
            } else {
                vec![candidate.name.to_lowercase()]
            };
            let similarity = signature_similarity(&new_tokens, &candidate_tokens);
            if similarity >= threshold {
                Some((candidate.id.clone(), similarity))
            } else {
                None
            }
        })
        .collect();

    // Break similarity ties by candidate id. `candidates` (and, upstream,
    // `FederatedIndex::project_repo`'s `other_nodes`) is built by iterating
    // a `HashMap<RepoId, Arc<RepoIndex>>`, whose order is randomized per
    // process — so for a name with many equally-similar matches (e.g. every
    // repo's `new` with no populated signature, all scoring the name-only
    // fallback's 1.0), an unordered tie-break here means `truncate(top_k)`
    // silently keeps a different subset of `CrossRepoSameSymbol` edges on
    // every server restart even though nothing about the indexed code
    // changed.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    scored.truncate(top_k);
    scored
}
