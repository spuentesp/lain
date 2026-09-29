use crate::federation::repo_id::GlobalId;
use crate::schema::GraphNode;

/// Tokens that don't carry parameter / type information and shouldn't
/// contribute to cross-repo signature similarity.
pub const SIGNATURE_STOP_WORDS: &[&str] = &[
    "pub",
    "private",
    "protected",
    "public",
    "export",
    "default",
    "static",
    "async",
    "const",
    "let",
    "var",
    "final",
    "abstract",
    "override",
    "virtual",
    "unsafe",
    "fn",
    "def",
    "function",
    "class",
    "struct",
    "enum",
    "interface",
    "trait",
    "impl",
    "type",
    "record",
    "data",
    "use",
    "import",
    "from",
    "module",
    "namespace",
    "extern",
    "crate",
    "self",
    "cls",
    "func",
    "method",
    "new",
];

/// Confidence tag for a single cross-repo match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MatchConfidence {
    Signature,
    NameOnly,
}

pub fn signature_tokens(sig: &str) -> Vec<String> {
    sig.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .filter(|s| !SIGNATURE_STOP_WORDS.contains(&s.as_str()))
        .collect()
}

fn non_stop_tokens(tokens: &[String]) -> impl Iterator<Item = &String> {
    tokens
        .iter()
        .filter(|t| !SIGNATURE_STOP_WORDS.contains(&t.as_str()))
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
    allow_name_only: bool,
) -> Vec<(String, f32, MatchConfidence)> {
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
        .map(|g| g.repo_id().to_string());

    let new_signature = new_node.signature.as_deref().filter(|s| !s.is_empty());
    let new_tokens: Vec<String> = match new_signature {
        Some(sig) => signature_tokens(sig),
        None if allow_name_only => vec![new_node.name.to_lowercase()],
        None => return Vec::new(),
    };
    let new_non_stop: Vec<String> = non_stop_tokens(&new_tokens).cloned().collect();

    let mut scored: Vec<(String, f32, MatchConfidence)> = candidates
        .iter()
        .filter_map(|candidate| {
            let candidate_repo = GlobalId::parse(&candidate.id).ok()?.repo_id().to_string();
            if Some(&candidate_repo) == new_repo.as_ref() {
                return None;
            }

            let candidate_signature = candidate.signature.as_deref().filter(|s| !s.is_empty());
            let candidate_tokens: Vec<String> = match candidate_signature {
                Some(sig) => signature_tokens(sig),
                None if allow_name_only => vec![candidate.name.to_lowercase()],
                None => return None,
            };
            let candidate_non_stop: Vec<&String> = non_stop_tokens(&candidate_tokens).collect();

            // Hard requirement: at least one shared non-stop-word token,
            // unless we're in name-only mode (in which case we accept any
            // candidate and tag confidence as NameOnly).
            let shares_token = new_non_stop.iter().any(|t| candidate_non_stop.contains(&t));
            let confidence = if new_signature.is_some() && candidate_signature.is_some() {
                if shares_token {
                    MatchConfidence::Signature
                } else if allow_name_only {
                    MatchConfidence::NameOnly
                } else {
                    return None;
                }
            } else if allow_name_only {
                MatchConfidence::NameOnly
            } else {
                return None;
            };

            let similarity = signature_similarity(&new_tokens, &candidate_tokens);
            if similarity >= threshold {
                Some((candidate.id.clone(), similarity, confidence))
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
