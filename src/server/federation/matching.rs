use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeType, GraphNode};

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

struct IndexedCandidate<'a> {
    node: &'a GraphNode,
    repo: String,
    norm: f32,
}

/// Pre-indexed cross-repo signature matcher for bulk projection
/// (`FederatedIndex::project_edges`). Tokenizes and parses candidate
/// `GlobalId`s once and builds an inverted token posting list so
/// matching `N` repo nodes against `M` foreign candidates runs in
/// `O(N + non-zero postings)` instead of `O(N * M)` tokenizations.
pub struct CrossRepoMatcher<'a> {
    candidates: Vec<IndexedCandidate<'a>>,
    token_to_id: std::collections::HashMap<String, usize>,
    postings: Vec<Vec<(usize, usize)>>,
    by_id: std::collections::HashMap<&'a str, &'a GraphNode>,
}

impl<'a> CrossRepoMatcher<'a> {
    pub fn new(candidates: &'a [GraphNode]) -> Self {
        use std::collections::HashMap;
        let mut by_id: HashMap<&'a str, &'a GraphNode> = HashMap::with_capacity(candidates.len());
        let mut indexed: Vec<IndexedCandidate<'a>> = Vec::new();
        let mut token_to_id: HashMap<String, usize> = HashMap::new();
        let mut postings: Vec<Vec<(usize, usize)>> = Vec::new();
        let target_types = EdgeType::CrossRepoSameSymbol.target_types();

        for cand in candidates {
            by_id.insert(cand.id.as_str(), cand);
            if !target_types.contains(&cand.node_type) {
                continue;
            }
            let Some(sig) = cand.signature.as_deref().filter(|s| !s.is_empty()) else {
                continue;
            };
            let Ok(gid) = GlobalId::parse(&cand.id) else {
                continue;
            };
            let tokens = signature_tokens(sig);
            if tokens.is_empty() {
                continue;
            }
            let mut counts: HashMap<String, usize> = HashMap::with_capacity(tokens.len());
            for tok in tokens {
                *counts.entry(tok).or_insert(0) += 1;
            }
            let norm_sq: usize = counts.values().map(|c| c * c).sum();
            let norm = (norm_sq as f32).sqrt();
            if norm == 0.0 {
                continue;
            }
            let cand_idx = indexed.len();
            for (tok, count) in counts {
                let tid = match token_to_id.get(&tok) {
                    Some(&id) => id,
                    None => {
                        let id = postings.len();
                        token_to_id.insert(tok, id);
                        postings.push(Vec::new());
                        id
                    }
                };
                postings[tid].push((cand_idx, count));
            }
            indexed.push(IndexedCandidate {
                node: cand,
                repo: gid.repo_id().to_string(),
                norm,
            });
        }

        Self {
            candidates: indexed,
            token_to_id,
            postings,
            by_id,
        }
    }

    pub fn node_by_id(&self, id: &str) -> Option<&'a GraphNode> {
        self.by_id.get(id).copied()
    }

    pub fn find_matches(
        &self,
        new_node: &GraphNode,
        top_k: usize,
        threshold: f32,
    ) -> Vec<(String, f32, MatchConfidence)> {
        use std::collections::HashMap;
        if !EdgeType::CrossRepoSameSymbol
            .source_types()
            .contains(&new_node.node_type)
        {
            return Vec::new();
        }
        let Some(sig) = new_node.signature.as_deref().filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let new_tokens = signature_tokens(sig);
        if new_tokens.is_empty() {
            return Vec::new();
        }
        let new_repo = GlobalId::parse(&new_node.id)
            .ok()
            .map(|g| g.repo_id().to_string());

        let mut counts_a: HashMap<&str, usize> = HashMap::with_capacity(new_tokens.len());
        for tok in &new_tokens {
            *counts_a.entry(tok.as_str()).or_insert(0) += 1;
        }
        let norm_a_sq: usize = counts_a.values().map(|c| c * c).sum();
        let norm_a = (norm_a_sq as f32).sqrt();
        if norm_a == 0.0 {
            return Vec::new();
        }

        let mut dots: HashMap<usize, usize> = HashMap::new();
        for (tok, count_a) in &counts_a {
            if let Some(&tid) = self.token_to_id.get(*tok) {
                for &(cand_idx, count_b) in &self.postings[tid] {
                    *dots.entry(cand_idx).or_insert(0) += count_a * count_b;
                }
            }
        }

        let mut scored: Vec<(String, f32, MatchConfidence)> = dots
            .into_iter()
            .filter_map(|(cand_idx, dot)| {
                let cand = &self.candidates[cand_idx];
                if Some(&cand.repo) == new_repo.as_ref() {
                    return None;
                }
                let denom = norm_a * cand.norm;
                if denom == 0.0 {
                    return None;
                }
                let similarity = dot as f32 / denom;
                if similarity >= threshold {
                    Some((cand.node.id.clone(), similarity, MatchConfidence::Signature))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(top_k);
        scored
    }
}

#[cfg(test)]
mod mutant_kills {
    use super::*;
    use crate::schema::NodeType;

    fn node(id: &str, sig: Option<&str>) -> GraphNode {
        let mut n = GraphNode::new(NodeType::Function, "f".into(), "src/f.rs".into());
        n.id = id.into();
        n.signature = sig.map(str::to_string);
        n
    }

    #[test]
    fn node_by_id_finds_only_known_candidates() {
        let cands = vec![node("repoB:Function:src/b.rs:g:3", Some("alpha gamma"))];
        let m = CrossRepoMatcher::new(&cands);
        assert!(m.node_by_id("repoB:Function:src/b.rs:g:3").is_some());
        assert!(m.node_by_id("missing").is_none());
    }

    #[test]
    fn identical_signature_from_another_repo_scores_one_and_same_repo_is_excluded() {
        let cands = vec![
            node("repoB:Function:src/b.rs:g:3", Some("alpha beta")),
            node("repoA:Function:src/a.rs:g:3", Some("alpha beta")),
        ];
        let m = CrossRepoMatcher::new(&cands);
        let source = node("repoA:Function:src/src.rs:f:1", Some("alpha beta"));
        let hits = m.find_matches(&source, 5, 0.5);
        assert_eq!(hits.len(), 1, "same-repo candidate is excluded: {hits:?}");
        assert_eq!(hits[0].0, "repoB:Function:src/b.rs:g:3");
        assert!((hits[0].1 - 1.0).abs() < 1e-6, "similarity {}", hits[0].1);
    }

    #[test]
    fn similarity_is_cosine_over_token_counts_and_threshold_is_inclusive() {
        let cands = vec![node("repoB:Function:src/b.rs:g:3", Some("alpha gamma"))];
        let m = CrossRepoMatcher::new(&cands);
        // One shared token out of two on each side: cosine 1 / (√2 · √2) = 0.5.
        let source = node("repoA:Function:src/src.rs:f:1", Some("alpha beta"));
        let at = m.find_matches(&source, 5, 0.5);
        assert_eq!(at.len(), 1, "a score equal to the threshold is kept");
        assert!((at[0].1 - 0.5).abs() < 1e-6, "similarity {}", at[0].1);
        assert!(m.find_matches(&source, 5, 0.51).is_empty());
    }

    #[test]
    fn repeated_tokens_weigh_by_count() {
        let cands = vec![node("repoB:Function:src/b.rs:g:3", Some("alpha beta"))];
        let m = CrossRepoMatcher::new(&cands);
        // Counts: source alpha=2, beta=1 (‖a‖ = √5); candidate alpha=1, beta=1 (‖b‖ = √2).
        // Dot product 2·1 + 1·1 = 3, so cosine = 3 / √10 ≈ 0.9487.
        let source = node("repoA:Function:src/src.rs:f:1", Some("alpha alpha beta"));
        let hits = m.find_matches(&source, 5, 0.0);
        assert_eq!(hits.len(), 1);
        let expected = 3.0_f32 / 10.0_f32.sqrt();
        assert!(
            (hits[0].1 - expected).abs() < 1e-5,
            "similarity {}",
            hits[0].1
        );
    }

    #[test]
    fn repeated_tokens_on_the_candidate_side_weigh_by_count() {
        let cands = vec![node(
            "repoB:Function:src/b.rs:g:3",
            Some("alpha alpha beta"),
        )];
        let m = CrossRepoMatcher::new(&cands);
        // Source alpha=1, beta=1 (‖a‖ = √2); candidate alpha=2, beta=1 (‖b‖ = √5).
        // Dot product 1·2 + 1·1 = 3, so cosine = 3 / √10 ≈ 0.9487.
        let source = node("repoA:Function:src/src.rs:f:1", Some("alpha beta"));
        let hits = m.find_matches(&source, 5, 0.0);
        assert_eq!(hits.len(), 1);
        let expected = 3.0_f32 / 10.0_f32.sqrt();
        assert!(
            (hits[0].1 - expected).abs() < 1e-5,
            "similarity {}",
            hits[0].1
        );
    }

    #[test]
    fn nodes_without_a_signature_or_of_another_type_do_not_match() {
        let cands = vec![node("repoB:Function:src/b.rs:g:3", Some("alpha beta"))];
        let m = CrossRepoMatcher::new(&cands);
        assert!(m
            .find_matches(&node("repoA:Function:src/src.rs:f:1", None), 5, 0.0)
            .is_empty());
        assert!(m
            .find_matches(&node("repoA:Function:src/src.rs:f:1", Some("")), 5, 0.0)
            .is_empty());
    }
}
