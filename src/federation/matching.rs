use crate::federation::repo_id::GlobalId;
use crate::schema::GraphNode;

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
    let new_repo = GlobalId::parse(&new_node.id)
        .ok()
        .map(|global_id| global_id.repo_id().to_string());
    let new_sig = new_node.signature.as_deref().unwrap_or("");
    let new_tokens = signature_tokens(new_sig);
    let mut scored: Vec<(String, f32)> = candidates
        .iter()
        .filter_map(|candidate| {
            let candidate_repo = GlobalId::parse(&candidate.id)
                .ok()?
                .repo_id()
                .to_string();
            if Some(&candidate_repo) == new_repo.as_ref() {
                return None;
            }

            let candidate_sig = candidate.signature.as_deref().unwrap_or("");
            let candidate_tokens = signature_tokens(candidate_sig);
            let similarity = signature_similarity(&new_tokens, &candidate_tokens);
            if similarity >= threshold {
                Some((candidate.id.clone(), similarity))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(top_k);
    scored
}
