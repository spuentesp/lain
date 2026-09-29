//! Route matching (`docs/CONTRACT_FEDERATION.md` §7.4).
//!
//! A pure-logic function [`match_route`] that decides whether a
//! consumer template matches a provider template, applying the method
//! rule, segment rule, and (as a fallback) prefix tolerance. Lives in
//! `federation/contracts/` so the joiner (PR 7) and the
//! `RouteMatch`/`EdgeDetail` types already defined for `GraphEdge` in
//! `crate::schema` are reachable from one place. No graph or config
//! dependencies — the matcher is pure logic.

use crate::federation::contracts::model::{HttpMethod, MethodSpec};
use crate::schema::RouteMatch;

// ─── Outcome ─────────────────────────────────────────────────────────

/// The matcher's verdict on a (consumer, provider) pair. `NoMatch`
/// is final; `Match` carries the kind (`Exact`, `Pattern`,
/// `PrefixStripped`) and the confidence the joiner will assign to
/// the resulting `Binds` edge.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOutcome {
    NoMatch,
    Match(MatchDetail),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchDetail {
    /// `Exact`, `Pattern`, or `PrefixStripped` — written onto
    /// `GraphEdge.detail.route_match` for the `Binds` edge (§4.3).
    pub kind: RouteMatch,
    /// Confidence for the joiner: `1.0` for direct matches, `0.6`
    /// when the consumer's method was `Unknown` (capped), `0.5`
    /// when the match required prefix stripping.
    pub confidence: f32,
    /// For `PrefixStripped`, the prefix that was stripped from the
    /// consumer's template (e.g. `/api/v1`). `None` otherwise.
    pub stripped_prefix: Option<String>,
}

impl MatchOutcome {
    pub fn is_match(&self) -> bool {
        matches!(self, MatchOutcome::Match(_))
    }
}

// ─── Public entry point ─────────────────────────────────────────────

/// Decide whether the consumer (`consumer_method` / `consumer_template`)
/// matches the provider (`provider_method` / `provider_template`).
/// Returns [`MatchOutcome::Match`] with kind/confidence/stripped_prefix
/// on success; [`MatchOutcome::NoMatch`] otherwise.
///
/// Steps in order:
/// 1. Method rule (§7.4): known equality, `HttpMethod::Any`, or
///    `MethodSpec::Unknown` rescue. The Unknown rescue caps
///    confidence at 0.6.
/// 2. Direct segment match (no prefix strip).
/// 3. Prefix tolerance: strip up to 3 leading literal segments from
///    the consumer template, one at a time, retrying after each.
///    A match found this way is `PrefixStripped` with confidence
///    0.5 and `stripped_prefix` set.
pub fn match_route(
    consumer_method: MethodSpec,
    consumer_template: &str,
    provider_method: HttpMethod,
    provider_template: &str,
) -> MatchOutcome {
    let method_conf = match method_confidence(consumer_method, provider_method) {
        Some(c) => c,
        None => return MatchOutcome::NoMatch,
    };

    let consumer_segs = split_template(consumer_template);
    let provider_segs = split_template(provider_template);

    // Try direct match first.
    if segments_match(&consumer_segs, &provider_segs) {
        let kind = if consumer_segs.iter().any(|s| *s == "{}" || *s == "{**}")
            || provider_segs.iter().any(|s| *s == "{}" || *s == "{**}")
        {
            RouteMatch::Pattern
        } else {
            RouteMatch::Exact
        };
        return MatchOutcome::Match(MatchDetail {
            kind,
            confidence: method_conf,
            stripped_prefix: None,
        });
    }

    // Prefix tolerance: strip leading literal segments from C, retry.
    if let Some((_n_stripped, stripped_text)) =
        try_prefix_strip(&consumer_segs, &provider_segs)
    {
        // 0.5 is the prefix-stripped confidence; the method cap can
        // only lower it further (Unknown → 0.6 is already ≥ 0.5).
        let confidence = method_conf.min(0.5);
        return MatchOutcome::Match(MatchDetail {
            kind: RouteMatch::PrefixStripped,
            confidence,
            stripped_prefix: Some(stripped_text),
        });
    }

    MatchOutcome::NoMatch
}

// ─── Method rule ────────────────────────────────────────────────────

fn method_confidence(consumer: MethodSpec, provider: HttpMethod) -> Option<f32> {
    let direct = match consumer {
        MethodSpec::Known(c) => c == provider,
        MethodSpec::Unknown => true,
    };
    if direct {
        // Known equality or Unknown rescue. Unknown caps confidence at 0.6.
        return Some(match consumer {
            MethodSpec::Unknown => 0.6,
            MethodSpec::Known(_) => 1.0,
        });
    }
    // P is `Any`? That rescues a Known-mismatch. Unknown is already handled.
    if provider == HttpMethod::Any {
        return Some(1.0);
    }
    None
}

// ─── Segment rule ───────────────────────────────────────────────────

/// Split a normalized template into its `/`-separated segments. The
/// empty segments from a leading `/` are dropped.
fn split_template(template: &str) -> Vec<String> {
    template
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn segments_match(consumer_segs: &[String], provider_segs: &[String]) -> bool {
    let p_len = provider_segs.len();
    let c_len = consumer_segs.len();
    if p_len == 0 || c_len == 0 {
        return p_len == 0 && c_len == 0;
    }

    let p_has_catchall = provider_segs[p_len - 1] == "{**}";
    let p_core_len = if p_has_catchall { p_len - 1 } else { p_len };

    // Without `{**}` on P, segment counts must match exactly.
    if !p_has_catchall && c_len != p_len {
        return false;
    }
    // With `{**}` on P, C must have at least the core length.
    if p_has_catchall && c_len < p_core_len {
        return false;
    }

    // Pairwise compare the core segments.
    for i in 0..p_core_len {
        let c = &consumer_segs[i];
        let p = &provider_segs[i];
        if !segment_pair_matches(c, p) {
            return false;
        }
    }
    true
}

/// One segment pair: C segment vs P segment. `{}` matches any C
/// segment but a C `{}` matches only a P `{}` — a literal route must
/// be proven, and a runtime value can never prove it. `**` is handled
/// at the segment-array level, not here.
fn segment_pair_matches(consumer: &str, provider: &str) -> bool {
    if consumer == provider {
        return true;
    }
    if provider == "{}" {
        return true;
    }
    if consumer == "{}" {
        // Consumer placeholder; provider is not `{}`. A runtime value
        // proves nothing about a literal route, so this is no match.
        return false;
    }
    false
}

// ─── Prefix tolerance ───────────────────────────────────────────────

/// Strip up to 3 leading literal segments from the consumer template
/// one at a time and retry the segment match. A literal segment is
/// neither `{}` nor `{**}`. Returns `Some((n, text))` on success
/// where `text` is the stripped prefix and `n` is the number of
/// stripped segments.
fn try_prefix_strip(
    consumer_segs: &[String],
    provider_segs: &[String],
) -> Option<(usize, String)> {
    for n in 1..=3 {
        if n > consumer_segs.len() {
            break;
        }
        // Every segment being stripped must be literal (no hole or
        // wildcard — we never strip dynamic positions, since that
        // would invent information we do not have).
        if consumer_segs[..n]
            .iter()
            .any(|s| s == "{}" || s == "{**}")
        {
            continue;
        }
        let stripped: Vec<String> = consumer_segs[n..].to_vec();
        if segments_match(&stripped, provider_segs) {
            let prefix = format!("/{}", consumer_segs[..n].join("/"));
            return Some((n, prefix));
        }
    }
    None
}

// ─── Specificity (helper for the joiner) ─────────────────────────────

/// Specificity of a template: the most specific provider wins
/// (§7.4). Counts literals > placeholders > wildcards, left to
/// right; the first difference decides. Templates with equal
/// specificity are the same endpoint.
///
/// The matcher does not use this — it answers "does this pair
/// match?". The joiner asks "of all matches, which is best?" and
/// uses [`template_specificity`] to compare.
pub fn template_specificity(template: &str) -> Vec<u8> {
    let mut ranks = Vec::new();
    for seg in split_template(template) {
        let rank = if seg == "{**}" {
            0
        } else if seg == "{}" {
            1
        } else {
            2
        };
        ranks.push(rank);
    }
    ranks
}

/// Two templates compared left to right by [`template_specificity`].
/// Returns `Ordering::Greater` if `a` is more specific than `b`.
pub fn compare_specificity(a: &str, b: &str) -> std::cmp::Ordering {
    template_specificity(a).cmp(&template_specificity(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::HttpMethod;
    use std::cmp::Ordering;

    fn match_get(c_t: &str, p_t: &str) -> MatchOutcome {
        match_route(
            MethodSpec::Known(HttpMethod::Get),
            c_t,
            HttpMethod::Get,
            p_t,
        )
    }

    // ─── Method rule ──────────────────────────────────────────────

    #[test]
    fn matching_methods_match_with_full_confidence() {
        let m = match_route(
            MethodSpec::Known(HttpMethod::Get),
            "/api/orders",
            HttpMethod::Get,
            "/api/orders",
        );
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::Exact);
        assert_eq!(d.confidence, 1.0);
        assert_eq!(d.stripped_prefix, None);
    }

    #[test]
    fn provider_any_matches_any_known_method() {
        let m = match_route(
            MethodSpec::Known(HttpMethod::Post),
            "/x",
            HttpMethod::Any,
            "/x",
        );
        assert!(matches!(m, MatchOutcome::Match(ref d) if d.confidence == 1.0));
    }

    #[test]
    fn consumer_unknown_matches_any_provider_method_but_caps_confidence() {
        let m = match_route(
            MethodSpec::Unknown,
            "/api/orders",
            HttpMethod::Get,
            "/api/orders",
        );
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.confidence, 0.6, "Unknown caps at 0.6");
    }

    #[test]
    fn mismatching_known_methods_do_not_match() {
        let m = match_route(
            MethodSpec::Known(HttpMethod::Get),
            "/x",
            HttpMethod::Post,
            "/x",
        );
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    // ─── Segment rule ─────────────────────────────────────────────

    #[test]
    fn same_segments_match() {
        let m = match_get("/api/orders/{}", "/api/orders/{}");
        assert!(matches!(m, MatchOutcome::Match(ref d) if d.kind == RouteMatch::Pattern));
    }

    #[test]
    fn provider_param_matches_any_consumer_segment() {
        let m = match_get("/api/orders/42", "/api/orders/{}");
        assert!(matches!(m, MatchOutcome::Match(_)));
    }

    #[test]
    fn consumer_param_matches_only_provider_param() {
        // Consumer {} doesn't prove a literal provider route.
        let m = match_get("/api/orders/{}", "/api/orders/42");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    #[test]
    fn case_sensitive_literal_match() {
        let m = match_get("/api/Orders", "/api/orders");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    #[test]
    fn provider_catchall_matches_consumer_with_more_segments() {
        let m = match_get("/files/a/b/c", "/files/{**}");
        assert!(matches!(m, MatchOutcome::Match(_)));
    }

    #[test]
    fn provider_catchall_with_no_consumer_segments_above_minimum_matches() {
        let m = match_get("/files/x", "/files/{**}");
        assert!(matches!(m, MatchOutcome::Match(_)));
    }

    #[test]
    fn different_segment_counts_without_catchall_do_not_match() {
        let m = match_get("/api/orders/a/b", "/api/orders/{}");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    #[test]
    fn consumer_has_fewer_segments_than_provider_core() {
        let m = match_get("/api/orders", "/api/orders/{}");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    // ─── Specificity ──────────────────────────────────────────────

    #[test]
    fn a_literal_route_is_more_specific_than_a_parameterised_one() {
        // /api/orders/me is more specific than /api/orders/{}.
        let a = compare_specificity("/api/orders/me", "/api/orders/{}");
        let b = compare_specificity("/api/orders/{}", "/api/orders/me");
        assert_eq!(a, Ordering::Greater);
        assert_eq!(b, Ordering::Less);
    }

    #[test]
    fn a_param_is_more_specific_than_a_catchall() {
        let a = compare_specificity("/files/{}", "/files/{**}");
        assert_eq!(a, Ordering::Greater);
    }

    #[test]
    fn identical_templates_have_equal_specificity() {
        assert_eq!(
            compare_specificity("/api/orders/{}", "/api/orders/{}"),
            Ordering::Equal
        );
    }

    #[test]
    fn the_leftmost_difference_decides() {
        // /a/b/c (literal) vs /a/{}/c (placeholder at position 1).
        assert_eq!(
            compare_specificity("/a/b/c", "/a/{}/c"),
            Ordering::Greater
        );
    }

    // ─── Prefix tolerance ─────────────────────────────────────────

    #[test]
    fn stripping_one_leading_literal_segment_finds_a_match() {
        let m = match_get("/api/orders/42", "/orders/{}");
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::PrefixStripped);
        assert_eq!(d.confidence, 0.5);
        assert_eq!(d.stripped_prefix.as_deref(), Some("/api"));
    }

    #[test]
    fn stripping_two_segments_finds_a_match() {
        let m = match_get("/api/v1/orders/42", "/orders/{}");
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::PrefixStripped);
        assert_eq!(d.stripped_prefix.as_deref(), Some("/api/v1"));
    }

    #[test]
    fn stripping_three_segments_is_the_cap() {
        let m = match_get("/a/b/c/orders/42", "/orders/{}");
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::PrefixStripped);
        assert_eq!(d.stripped_prefix.as_deref(), Some("/a/b/c"));
    }

    #[test]
    fn more_than_three_segments_to_strip_does_not_match() {
        // Four leading literals → strip cap of 3 is not enough.
        let m = match_get("/a/b/c/d/orders/42", "/orders/{}");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    #[test]
    fn stripping_a_dynamic_segment_does_not_count() {
        // The first segment is `{}` — that's a hole, not a literal,
        // so the prefix tolerance rule refuses to strip it. Falling
        // through to no match is the right answer (we'd be inventing
        // information we don't have).
        let m = match_get("/{}/orders/42", "/orders/{}");
        assert_eq!(m, MatchOutcome::NoMatch);
    }

    #[test]
    fn direct_match_wins_over_prefix_strip() {
        // /api/orders/42 matches /api/orders/{} directly — no prefix
        // strip, so confidence is 1.0.
        let m = match_get("/api/orders/42", "/api/orders/{}");
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::Pattern);
        assert_eq!(d.confidence, 1.0);
        assert_eq!(d.stripped_prefix, None);
    }

    #[test]
    fn prefix_strip_caps_confidence_at_0_5_for_unknown_method() {
        // C method=Unknown → base 0.6; prefix strip → min(0.6, 0.5) = 0.5.
        let m = match_route(
            MethodSpec::Unknown,
            "/api/orders/42",
            HttpMethod::Get,
            "/orders/{}",
        );
        let d = match m {
            MatchOutcome::Match(d) => d,
            MatchOutcome::NoMatch => panic!("expected match"),
        };
        assert_eq!(d.kind, RouteMatch::PrefixStripped);
        assert_eq!(d.confidence, 0.5);
    }
}