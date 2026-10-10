//! URL normalization (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §4.5).
//!
//! One pure function [`normalize`], used by every provider and consumer
//! sensor. Input is a URL argument as a sequence of [`UrlPart`]s
//! (`Literal(String) | Hole(String)`, §6.3); output is a
//! [`NormalizedUrl`](crate::federation::contracts::model::NormalizedUrl)
//! with the [`HostPart`](crate::federation::contracts::model::HostPart)
//! and the rendered route template.
//!
//! The five steps in §4.5 run in order:
//!
//! 1. **Host.** First literal containing `://` → host is the text
//!    after it up to the next `/`, `?`, `#`, or end (userinfo and port
//!    stripped, lower-cased) → `HostPart::Literal`. A hole before the
//!    first `/` of the path → `HostPart::Expr(hole)` (`HostPart::Env`
//!    is the §6.3 sensor's job, not this function's). URL starting
//!    with `/` → `HostPart::None`.
//! 2. **Cut.** Drop everything from the first `?` or `#` found in a
//!    literal, including later holes.
//! 3. **Segments.** Split the rest on `/`. A segment containing any
//!    hole → `{}`. A literal segment in parameter syntax (`:id`,
//!    `{id}`, `<id>`, `<int:id>`, `[id]`, `$id`) → `{}`. A wildcard
//!    segment (`*`, `*rest`, `{*rest}`, `{rest:path}`, `<path:rest>`)
//!    → `{**}`, which must be the last segment.
//! 4. **Clean.** Drop empty segments (collapses `//` and removes a
//!    trailing `/`). An empty result is `/`. Case is preserved.
//! 5. **Dynamic.** If the path part consisted only of holes, then
//!    `template = None`.
//!
//! Provider prefixes are prepended by the caller before step 3 (§6.2);
//! consumer templates are never changed by config (§7.4). This module
//! has no graph or config dependencies — it's pure functions only.

use crate::federation::contracts::model::{HostPart, NormalizedUrl};

/// One piece of a URL argument. A literal is verbatim text from the
/// source; a hole is a runtime expression (`settings.base_url`, `BASE`,
/// `id`, …) that the §6.3 sensor splits out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlPart {
    Literal(String),
    Hole(String),
}

/// Normalize a URL argument per §4.5. Returns a [`NormalizedUrl`]
/// carrying the host side and the rendered route template (or `None`
/// when the path is fully dynamic).
pub fn normalize(parts: &[UrlPart]) -> NormalizedUrl {
    let (host, rest) = extract_host(parts);
    let rest = cut_query_or_fragment(rest);
    let segments = segmentize(rest);
    let segments = clean(segments);
    let template = render_template(&segments);
    NormalizedUrl { host, template }
}

// ─── Step 1: host extraction ────────────────────────────────────────

fn extract_host(parts: &[UrlPart]) -> (HostPart, Vec<UrlPart>) {
    if parts.is_empty() {
        return (HostPart::None, Vec::new());
    }

    // Rule: URL starts with "/" → HostPart::None.
    if matches!(&parts[0], UrlPart::Literal(s) if s.starts_with('/')) {
        return (HostPart::None, parts.to_vec());
    }

    // Find the first literal containing the scheme delimiter. If none,
    // the URL has no scheme: a leading hole becomes HostPart::Expr;
    // anything else stays a path-only URL.
    let scheme_idx = parts
        .iter()
        .position(|p| matches!(p, UrlPart::Literal(s) if s.contains("://")));

    let Some(scheme_idx) = scheme_idx else {
        if let UrlPart::Hole(h) = &parts[0] {
            return (HostPart::Expr(h.clone()), parts[1..].to_vec());
        }
        return (HostPart::None, parts.to_vec());
    };

    // The scheme-bearing literal splits into "before ://" (kept as
    // scheme text — not part of host) and "after ://" (starts the host
    // candidate). Walk forward through parts, accumulating host chunks
    // until the first `/`, `?`, `#`.
    //
    // `delim` is `(part_index, byte_offset_within_part)` of the first
    // path/query/fragment delimiter. The host consists of every chunk
    // before that point; the path consists of everything from that
    // point forward.
    let mut host_chunks: Vec<UrlPart> = Vec::new();
    let mut delim: Option<(usize, usize)> = None;

    if let UrlPart::Literal(s) = &parts[scheme_idx] {
        let scheme_pos = s.find("://").expect("contains ://");
        let after = &s[scheme_pos + 3..];
        if let Some(pos) = after.find(['/', '?', '#']) {
            host_chunks.push(UrlPart::Literal(after[..pos].to_string()));
            delim = Some((scheme_idx, scheme_pos + 3 + pos));
        } else {
            host_chunks.push(UrlPart::Literal(after.to_string()));
        }
    }

    if delim.is_none() {
        for (i, p) in parts.iter().enumerate().skip(scheme_idx + 1) {
            match p {
                UrlPart::Literal(s) => {
                    if let Some(pos) = s.find(['/', '?', '#']) {
                        host_chunks.push(UrlPart::Literal(s[..pos].to_string()));
                        delim = Some((i, pos));
                        break;
                    }
                    host_chunks.push(UrlPart::Literal(s.clone()));
                }
                UrlPart::Hole(h) => {
                    host_chunks.push(UrlPart::Hole(h.clone()));
                }
            }
        }
    }

    // Build the host from the chunks and split the path from the
    // delim part forward.
    let host = build_host(&host_chunks);

    let rest = match delim {
        Some((idx, offset)) => {
            // The delim part keeps its suffix (the path part starts
            // here); everything after it is appended verbatim.
            let mut rest: Vec<UrlPart> = Vec::new();
            if let UrlPart::Literal(s) = &parts[idx] {
                let suffix = &s[offset..];
                if !suffix.is_empty() {
                    rest.push(UrlPart::Literal(suffix.to_string()));
                }
            }
            for p in &parts[idx + 1..] {
                rest.push(p.clone());
            }
            rest
        }
        None => Vec::new(), // no path after host (host consumed the whole input)
    };

    (host, rest)
}

fn build_host(chunks: &[UrlPart]) -> HostPart {
    let mut first_hole: Option<String> = None;
    let mut host_text = String::new();
    for chunk in chunks {
        match chunk {
            UrlPart::Literal(s) => host_text.push_str(s),
            UrlPart::Hole(h) => {
                if first_hole.is_none() {
                    first_hole = Some(h.clone());
                }
            }
        }
    }
    match first_hole {
        Some(h) => HostPart::Expr(h),
        None => host_from_text(&host_text),
    }
}

/// Strip userinfo and port, then lowercase. The host is everything
/// after the `@` (userinfo dropped) and before the `:` (port dropped).
/// An empty host becomes `HostPart::None` — a scheme with no host
/// is malformed but we don't refuse it, we just say "no host".
fn host_from_text(s: &str) -> HostPart {
    if s.is_empty() {
        return HostPart::None;
    }
    let after_userinfo = s.rsplit('@').next().unwrap_or(s);
    let after_port = match after_userinfo.rfind(':') {
        Some(pos) => &after_userinfo[..pos],
        None => after_userinfo,
    };
    HostPart::Literal(after_port.to_ascii_lowercase())
}

// ─── Step 2: cut ────────────────────────────────────────────────────

fn cut_query_or_fragment(parts: Vec<UrlPart>) -> Vec<UrlPart> {
    let mut out: Vec<UrlPart> = Vec::new();
    let mut cut = false;
    for part in parts {
        if cut {
            break;
        }
        match part {
            UrlPart::Literal(s) => {
                if let Some(pos) = s.find(['?', '#']) {
                    let head = &s[..pos];
                    if !head.is_empty() {
                        out.push(UrlPart::Literal(head.to_string()));
                    }
                    cut = true;
                } else {
                    out.push(UrlPart::Literal(s));
                }
            }
            UrlPart::Hole(h) => out.push(UrlPart::Hole(h)),
        }
    }
    out
}

// ─── Step 3: segmentize ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Empty placeholder; dropped by `clean`.
    Empty,
    /// A literal segment, kept as-is.
    Literal(String),
    /// A single-segment parameter, rendered `{}`.
    Param,
    /// A wildcard segment, rendered `{**}`. Must be last.
    Wildcard,
}

fn segmentize(parts: Vec<UrlPart>) -> Vec<Segment> {
    // A segment is the run of `UrlPart`s between two `/`s (or the
    // start/end of the path). Splitting on `/` inside literals gives
    // segment boundaries; a hole extends the current segment.
    let mut segments: Vec<Segment> = Vec::new();
    let mut buf: Vec<UrlPart> = Vec::new();

    let flush = |buf: &mut Vec<UrlPart>, segments: &mut Vec<Segment>| {
        if buf.is_empty() {
            segments.push(Segment::Empty);
            return;
        }
        let has_hole = buf.iter().any(|p| matches!(p, UrlPart::Hole(_)));
        if has_hole {
            segments.push(Segment::Param);
            buf.clear();
            return;
        }
        let text: String = buf
            .iter()
            .map(|p| {
                if let UrlPart::Literal(s) = p {
                    s.as_str()
                } else {
                    ""
                }
            })
            .collect();
        buf.clear();
        if text.is_empty() {
            segments.push(Segment::Empty);
        } else if is_wildcard(&text) {
            segments.push(Segment::Wildcard);
        } else if is_parameter_syntax(&text) {
            segments.push(Segment::Param);
        } else {
            segments.push(Segment::Literal(text));
        }
    };

    for part in parts {
        match part {
            UrlPart::Literal(s) => {
                for sub in s.split('/') {
                    flush(&mut buf, &mut segments);
                    if !sub.is_empty() {
                        buf.push(UrlPart::Literal(sub.to_string()));
                    }
                }
            }
            UrlPart::Hole(h) => {
                buf.push(UrlPart::Hole(h));
            }
        }
    }
    flush(&mut buf, &mut segments);
    segments
}

fn is_parameter_syntax(seg: &str) -> bool {
    // `:id`, `{id}`, `<id>`, `<int:id>`, `[id]`, `$id`.
    if seg.is_empty() {
        return false;
    }
    let bytes = seg.as_bytes();
    match bytes[0] {
        b':' => true,
        b'{' => {
            seg.len() >= 3
                && seg.ends_with('}')
                && !seg[1..seg.len() - 1].contains('{')
                && !seg[1..seg.len() - 1].contains('}')
        }
        b'<' => {
            seg.len() >= 3
                && seg.ends_with('>')
                && !seg[1..seg.len() - 1].contains('<')
                && !seg[1..seg.len() - 1].contains('>')
        }
        b'[' => {
            seg.len() >= 3
                && seg.ends_with(']')
                && !seg[1..seg.len() - 1].contains('[')
                && !seg[1..seg.len() - 1].contains(']')
        }
        b'$' => seg.len() >= 2 && !seg[1..].contains('$'),
        _ => false,
    }
}

fn is_wildcard(seg: &str) -> bool {
    // `*`, `*rest`, `{*rest}`, `{rest:path}`, `<path:rest>`.
    if seg == "*" {
        return true;
    }
    if let Some(rest) = seg.strip_prefix('*') {
        // `*` followed by an identifier (no `/`, no `{`, no `}`).
        return !rest.is_empty() && rest.chars().all(|c| c.is_alphanumeric() || c == '_');
    }
    if seg.starts_with("{*") && seg.ends_with('}') && seg.len() >= 4 {
        return !seg[2..seg.len() - 1].contains('{');
    }
    if seg.starts_with('{') && seg.ends_with(":path}") && seg.len() >= 8 {
        let inner = &seg[1..seg.len() - 6];
        return !inner.is_empty() && !inner.contains('{') && !inner.contains(':');
    }
    if seg.starts_with("<path:") && seg.ends_with('>') && seg.len() >= 8 {
        let inner = &seg[6..seg.len() - 1];
        return !inner.is_empty() && !inner.contains('<') && !inner.contains(':');
    }
    false
}

// ─── Step 4: clean ──────────────────────────────────────────────────

fn clean(segments: Vec<Segment>) -> Vec<Segment> {
    let cleaned: Vec<Segment> = segments
        .into_iter()
        .filter(|s| !matches!(s, Segment::Empty))
        .collect();
    if cleaned.is_empty() {
        vec![Segment::Literal(String::new())]
    } else {
        cleaned
    }
}

// ─── Step 5: render & dynamic detection ─────────────────────────────

fn render_template(segments: &[Segment]) -> Option<String> {
    if segments.is_empty() {
        return Some("/".to_string());
    }
    // Dynamic: every segment is a parameter (`{}`) or a wildcard (`{**}`).
    let all_dynamic = segments
        .iter()
        .all(|s| matches!(s, Segment::Param | Segment::Wildcard));
    if all_dynamic {
        return None;
    }
    let mut out = String::new();
    out.push('/');
    let mut first = true;
    for seg in segments {
        if !first {
            out.push('/');
        }
        first = false;
        match seg {
            Segment::Empty => unreachable!("cleaned before render"),
            Segment::Literal(s) => out.push_str(s),
            Segment::Param => out.push_str("{}"),
            Segment::Wildcard => out.push_str("{**}"),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::HostPart;
    use proptest::prelude::*;

    fn lit(s: &str) -> UrlPart {
        UrlPart::Literal(s.to_string())
    }
    fn hole(s: &str) -> UrlPart {
        UrlPart::Hole(s.to_string())
    }

    // ─── Host step ────────────────────────────────────────────────

    #[test]
    fn host_literal_strips_userinfo_and_port_and_lowercases() {
        let url = normalize(&[lit("https://User@Example.COM:8080/api/users")]);
        assert_eq!(url.host, HostPart::Literal("example.com".to_string()));
        assert_eq!(url.template.as_deref(), Some("/api/users"));
    }

    #[test]
    fn host_literal_without_userinfo_keeps_the_host() {
        let url = normalize(&[lit("https://orders.svc/api/orders")]);
        assert_eq!(url.host, HostPart::Literal("orders.svc".to_string()));
        assert_eq!(url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn host_expr_when_first_part_is_a_hole() {
        let url = normalize(&[hole("BASE"), lit("/api/users")]);
        assert_eq!(url.host, HostPart::Expr("BASE".to_string()));
        assert_eq!(url.template.as_deref(), Some("/api/users"));
    }

    #[test]
    fn host_none_when_url_starts_with_a_slash() {
        let url = normalize(&[lit("/api/users")]);
        assert_eq!(url.host, HostPart::None);
        assert_eq!(url.template.as_deref(), Some("/api/users"));
    }

    #[test]
    fn host_none_when_url_has_no_scheme_and_no_hole() {
        let url = normalize(&[lit("api/users")]);
        assert_eq!(url.host, HostPart::None);
        assert_eq!(url.template.as_deref(), Some("/api/users"));
    }

    #[test]
    fn host_literal_when_no_path() {
        let url = normalize(&[lit("https://orders.svc")]);
        assert_eq!(url.host, HostPart::Literal("orders.svc".to_string()));
        // No path → empty result collapses to root "/".
        assert_eq!(url.template.as_deref(), Some("/"));
    }

    #[test]
    fn host_literal_when_only_query() {
        let url = normalize(&[lit("https://orders.svc?token=abc")]);
        assert_eq!(url.host, HostPart::Literal("orders.svc".to_string()));
        // Query is cut; the empty path becomes root.
        assert_eq!(url.template.as_deref(), Some("/"));
    }

    #[test]
    fn host_expr_when_a_hole_sits_after_the_scheme() {
        // "https://{BASE}/api/users" — BASE is the host expression.
        let url = normalize(&[lit("https://"), hole("BASE"), lit("/api/users")]);
        assert_eq!(url.host, HostPart::Expr("BASE".to_string()));
        assert_eq!(url.template.as_deref(), Some("/api/users"));
    }

    // ─── Cut step ─────────────────────────────────────────────────

    #[test]
    fn cut_drops_everything_from_query() {
        let url = normalize(&[lit("/api/orders?limit=10&offset=0")]);
        assert_eq!(url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn cut_drops_everything_from_fragment() {
        let url = normalize(&[lit("/api/orders#top")]);
        assert_eq!(url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn cut_drops_holes_after_the_query() {
        let url = normalize(&[lit("/api/orders?"), hole("q"), lit("/more")]);
        assert_eq!(url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn cut_takes_the_first_delim_only() {
        // A literal that contains both ? and # cuts at the first one.
        let url = normalize(&[lit("/a?b=1#c=2")]);
        assert_eq!(url.template.as_deref(), Some("/a"));
    }

    // ─── Segments step ────────────────────────────────────────────

    #[test]
    fn segment_with_a_hole_collapses_to_placeholder() {
        let url = normalize(&[lit("/api/orders/"), hole("id"), lit("/label")]);
        assert_eq!(url.template.as_deref(), Some("/api/orders/{}/label"));
    }

    #[test]
    fn parameter_syntax_colon_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/:id")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn parameter_syntax_braces_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/{id}")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn parameter_syntax_angle_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/<id>")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn parameter_syntax_typed_angle_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/<int:id>")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn parameter_syntax_brackets_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/[id]")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn parameter_syntax_dollar_becomes_placeholder() {
        let url = normalize(&[lit("/api/users/$id")]);
        assert_eq!(url.template.as_deref(), Some("/api/users/{}"));
    }

    #[test]
    fn wildcard_star_becomes_double_placeholder() {
        let url = normalize(&[lit("/files/*")]);
        assert_eq!(url.template.as_deref(), Some("/files/{**}"));
    }

    #[test]
    fn wildcard_named_star_becomes_double_placeholder() {
        let url = normalize(&[lit("/files/*rest")]);
        assert_eq!(url.template.as_deref(), Some("/files/{**}"));
    }

    #[test]
    fn wildcard_brace_star_becomes_double_placeholder() {
        let url = normalize(&[lit("/files/{*rest}")]);
        assert_eq!(url.template.as_deref(), Some("/files/{**}"));
    }

    #[test]
    fn wildcard_brace_path_becomes_double_placeholder() {
        let url = normalize(&[lit("/files/{rest:path}")]);
        assert_eq!(url.template.as_deref(), Some("/files/{**}"));
    }

    #[test]
    fn wildcard_angle_path_becomes_double_placeholder() {
        let url = normalize(&[lit("/files/<path:rest>")]);
        assert_eq!(url.template.as_deref(), Some("/files/{**}"));
    }

    // ─── Clean step ───────────────────────────────────────────────

    #[test]
    fn empty_segments_collapse_double_slash() {
        let url = normalize(&[lit("//api//orders//")]);
        assert_eq!(url.template.as_deref(), Some("/api/orders"));
    }

    #[test]
    fn an_empty_path_renders_as_root() {
        let url = normalize(&[lit("/")]);
        assert_eq!(url.template.as_deref(), Some("/"));
    }

    #[test]
    fn empty_input_renders_as_root() {
        let url = normalize(&[]);
        assert_eq!(url.template.as_deref(), Some("/"));
    }

    #[test]
    fn case_is_preserved() {
        let url = normalize(&[lit("/api/Orders/Me")]);
        assert_eq!(url.template.as_deref(), Some("/api/Orders/Me"));
    }

    // ─── Dynamic step ─────────────────────────────────────────────

    #[test]
    fn all_holes_path_yields_template_none() {
        let url = normalize(&[lit("/"), hole("a"), lit("/"), hole("b")]);
        assert_eq!(url.template, None);
        assert_eq!(url.host, HostPart::None);
    }

    #[test]
    fn all_wildcards_yields_template_none() {
        let url = normalize(&[lit("/"), hole("a"), lit("/*")]);
        assert_eq!(url.template, None);
    }

    #[test]
    fn one_literal_segment_keeps_template() {
        let url = normalize(&[lit("/api/"), hole("a"), lit("/")]);
        assert_eq!(url.template.as_deref(), Some("/api/{}"));
    }

    // ─── Property tests (§4.5) ────────────────────────────────────

    /// Idempotence: normalizing a rendered template returns the same
    /// template. The template is always a literal string, so we wrap
    /// it as a single Literal part and re-normalize.
    #[test]
    fn normalize_is_idempotent_on_rendered_templates() {
        let cases = [
            "/api/orders",
            "/api/orders/{}",
            "/api/orders/{}/label",
            "/api/orders/{}/items/{}/sku",
            "/files/{**}",
            "/",
            "/Me",
        ];
        for tmpl in cases {
            let once = normalize(&[lit(tmpl)]);
            let once_str = once.template.as_deref().unwrap();
            let twice = normalize(&[lit(once_str)]);
            let twice_str = twice.template.as_deref().unwrap();
            assert_eq!(
                once_str, twice_str,
                "normalize({tmpl:?}) = {once_str:?}; re-normalizing = {twice_str:?}"
            );
        }
    }

    /// Idempotence on the host side: a URL with no host (`/api/...`)
    /// stays `HostPart::None`. A URL with a `https://orders.svc/...`
    /// stays `HostPart::Literal("orders.svc")`.
    #[test]
    fn host_side_is_idempotent() {
        let n = normalize(&[lit("https://Orders.SVC/api/x")]);
        assert_eq!(n.host, HostPart::Literal("orders.svc".to_string()));
        let again = normalize(&[lit("https://"), lit("orders.svc"), lit("/api/x")]);
        assert_eq!(again.host, HostPart::Literal("orders.svc".to_string()));
    }

    /// Property: for every framework's provider declaration in the
    /// fixture, the provider template the normalizer produces is what
    /// the joiner (PR 7) would compare a matching consumer call
    /// against. Today the consumer side has no sensor; this asserts
    /// the provider half and the equivalence property holds for any
    /// input the fixture would emit.
    ///
    /// The other half — the consumer call producing the same template
    /// — is PR 6 (`http_client_sensor`) and is intentionally out of
    /// scope here.
    #[test]
    fn every_framework_provider_renders_a_stable_template() {
        // Each row: a single-literal path the framework produces, and
        // the template the normalizer must yield. The consumer side
        // (§6.3) will, when it lands, normalize its URL the same way.
        let cases: &[(&str, &str)] = &[
            // Rust axum: .route("/api/orders/:id", get(get_order))
            (r#"/api/orders/:id"#, "/api/orders/{}"),
            // Rust actix: #[get("/api/users")]
            (r#"/api/users"#, "/api/users"),
            // Python FastAPI: @app.get("/api/widgets/{widget_id}")
            (r#"/api/widgets/{widget_id}"#, "/api/widgets/{}"),
            // Python Flask: @app.route("/api/orders", methods=["POST"])
            (r#"/api/orders"#, "/api/orders"),
            // TypeScript Express: router.post("/api/login", h)
            (r#"/api/login"#, "/api/login"),
            // Go net/http: http.HandleFunc("/healthz", h)
            (r#"/healthz"#, "/healthz"),
            // Go Gin: r.GET("/api/users", h)
            (r#"/api/users"#, "/api/users"),
        ];
        for (input, want) in cases {
            let url = normalize(&[lit(input)]);
            assert_eq!(
                url.template.as_deref(),
                Some(*want),
                "input {input:?} should normalize to {want:?}"
            );
        }
    }

    /// Equivalence property: any input that round-trips through a hole
    /// yields the same template as the same input without the hole.
    /// E.g. "/api/{id}" and "/api/orders" are different templates
    /// (one is dynamic), but "/api/{id}" with hole "id" and "/api/:id"
    /// with literal ":id" must produce the same template — because the
    /// consumer's URL was a literal `:id` and the provider's was a
    /// hole at the same position.
    #[test]
    fn literal_parameter_and_hole_in_same_position_match() {
        let from_literal = normalize(&[lit("/api/orders/:id")]);
        let from_hole = normalize(&[lit("/api/orders/"), hole("id")]);
        assert_eq!(from_literal.template, from_hole.template);
        assert_eq!(from_literal.template.as_deref(), Some("/api/orders/{}"));
    }

    proptest! {
        // Property test: a non-empty path that contains only
        // alphanumerics and slashes is stable under
        // normalize(normalize(x)).template.
        #[test]
        fn normalize_template_idempotent_proptest(s in "[a-zA-Z0-9/_-]{0,32}") {
            let once = normalize(&[lit(&s)]);
            if let Some(once_str) = once.template.as_deref() {
                let twice = normalize(&[lit(once_str)]);
                let twice_str = twice.template.as_deref().unwrap();
                proptest::prop_assert_eq!(once_str, twice_str);
            }
        }
    }
}
