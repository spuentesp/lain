use crate::schema::NodeType;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RepoId(String);

impl RepoId {
    pub fn new(s: &str) -> Result<Self, crate::error::LainError> {
        if s.is_empty() || s.contains(':') || s.contains('/') {
            return Err(crate::error::LainError::InvalidRepoId(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Percent-encode a `path` or `name` segment so the resulting global id
/// still has exactly five `:`-delimited pieces after encoding.
///
/// `GlobalId::new` percent-encodes two characters inside path and name
/// segments: `%` becomes `%25` and `:` becomes `%3A`. Order matters —
/// `%` first so a pre-existing `%3A` payload gets its `%` doubled and
/// stays decodable. The `repo` segment is built from a `RepoId` that
/// already forbids `:`, and the `kind` segment is a bare `NodeType`
/// variant name with no colons or percent characters, so neither needs
/// encoding.
///
/// Ids without those characters round-trip byte-identically: a
/// `GlobalId::new(..., "src/auth.rs", "verify_token", ...)` still
/// formats to `"repo:Function:src/auth.rs:verify_token:42"`, and
/// `parse` decodes it to the same input. This keeps the federation
/// prefix-scoping (which uses `repo:` and assumes `:` only appears as a
/// separator) honest under §5.2.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            ':' => out.push_str("%3A"),
            other => out.push(other),
        }
    }
    out
}

/// Inverse of [`encode_segment`]. Decodes `%25` → `%` and `%3A` → `:`
/// (case-insensitive on the hex digits). Unrecognised `%XX` sequences
/// are passed through unchanged so a future encoding addition doesn't
/// silently corrupt an already-written id — `parse` only emits an
/// `InvalidGlobalId` when the on-the-wire shape is wrong (not five
/// segments, or the kind is unknown), not when an unknown escape shows
/// up inside a payload.
///
/// Iterates by `char` so a path containing non-ASCII code points
/// (`Cargo.toml` paths, symbol names in international code) round-trips
/// faithfully. Encoding only touches ASCII bytes, so any non-ASCII
/// sequence in the input is forwarded as-is.
fn decode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut idx = 0;
    while idx < s.len() {
        let rest = &s[idx..];
        if rest.starts_with("%25") {
            out.push('%');
            idx += 3;
            continue;
        }
        if rest.starts_with("%3A") || rest.starts_with("%3a") {
            out.push(':');
            idx += 3;
            continue;
        }
        // Take one full UTF-8 char so multi-byte sequences aren't
        // torn apart (an earlier byte-by-byte version cast each byte
        // to `char` and corrupted non-ASCII paths).
        let ch = rest.chars().next().expect("non-empty remaining slice");
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GlobalId(String);

impl GlobalId {
    pub fn new(
        repo: &RepoId,
        kind: NodeType,
        path: &str,
        name: &str,
        line_start: Option<u32>,
    ) -> Self {
        // Encoding happens here (not in `Display`/`Debug`) so any
        // consumer that round-trips through `as_str()` sees the
        // canonical wire shape, and `parse` is the one decoder.
        Self(format!(
            "{}:{:?}:{}:{}:{}",
            repo.as_str(),
            kind,
            encode_segment(path),
            encode_segment(name),
            line_start.unwrap_or(0),
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn repo_id(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }
    pub fn parse(s: &str) -> Result<Self, crate::error::LainError> {
        let parts: Vec<&str> = s.split(':').collect();
        // Exactly five segments: `repo:Kind:path:name:line_start`.
        // With encoding in place, `path` and `name` cannot contain a
        // literal `:` — so anything that splits into fewer or more
        // than five pieces is malformed on its face. The pre-bump
        // 4-segment shape (no line_start) and a hand-built id with a
        // literal `:` in the name both fail this check, so the
        // accessors never see a string that doesn't conform.
        if parts.len() != 5 {
            return Err(crate::error::LainError::InvalidGlobalId(s.to_string()));
        }
        // Validate the `Kind` segment is a real NodeType. Without this
        // check, `repo:Foo:src/main.rs:hello` parses as a GlobalId that
        // no graph lookup will ever resolve — the caller sees a
        // `NotFound` that's indistinguishable from a genuinely missing
        // symbol, and debugging requires reading the helper.
        let kind_str = parts[1];
        if !NodeType::all().iter().any(|k| format!("{k:?}") == kind_str) {
            return Err(crate::error::LainError::InvalidGlobalId(format!(
                "{s}: unknown node kind `{kind_str}`"
            )));
        }
        // Line_start must be a valid `u32`. Catching it here (rather
        // than at access time) keeps the parsed `GlobalId` always
        // well-formed and means `line_start()` can be infallible.
        if parts[4].parse::<u32>().is_err() {
            return Err(crate::error::LainError::InvalidGlobalId(format!(
                "{s}: invalid line_start `{}`",
                parts[4]
            )));
        }
        Ok(Self(s.to_string()))
    }

    /// Parse out the node-type component of a global id, e.g.
    /// `Function` from `"auth-svc:Function:src/auth.rs:verify_token:0"`.
    /// `None` if the id is malformed (which `parse` would already have
    /// rejected, but the helper is independent so callers don't have
    /// to re-validate).
    pub fn node_kind_str(&self) -> Option<&str> {
        // Format: `repo:Kind:path:name:line_start`. With `NodeType`'s
        // `Debug` impl producing no colons (variants are bare
        // identifiers), the second `:` is the boundary between `Kind`
        // and `path`.
        let after_repo = self.0.split_once(':')?.1;
        let (kind, _rest) = after_repo.split_once(':')?;
        Some(kind)
    }

    /// Parse the workspace-relative file path (3rd segment) of a global
    /// id, decoding `%25` → `%` and `%3A` → `:` so the returned string
    /// is the original path the indexer saw. Returns `None` for any id
    /// that does not have the canonical 5-segment shape (only possible
    /// for ids that bypassed `parse`, e.g. hand-built test fixtures).
    pub fn path(&self) -> Option<String> {
        let parts: Vec<&str> = self.0.split(':').collect();
        if parts.len() != 5 {
            return None;
        }
        Some(decode_segment(parts[2]))
    }

    /// Parse the symbol name (4th segment) of a global id, decoding
    /// `%25` → `%` and `%3A` → `:` so the returned string is the
    /// original name the indexer saw (e.g. `"GET /orders/:id"` rather
    /// than the encoded `"GET /orders/%3Aid"`). Returns `None` for
    /// any id that does not have the canonical 5-segment shape.
    pub fn name(&self) -> Option<String> {
        let parts: Vec<&str> = self.0.split(':').collect();
        if parts.len() != 5 {
            return None;
        }
        Some(decode_segment(parts[3]))
    }

    /// Parse the line-start (5th segment) of a global id, e.g.
    /// `Some(42)` from `"auth-svc:Function:src/auth.rs:verify_token:42"`.
    /// Returns `None` for any id that does not have the canonical
    /// 5-segment shape, or whose 5th segment is not a valid `u32`.
    pub fn line_start(&self) -> Option<u32> {
        let parts: Vec<&str> = self.0.split(':').collect();
        parts.get(4)?.parse().ok()
    }
}

impl std::fmt::Display for GlobalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LainError;
    use proptest::prelude::*;

    #[test]
    fn repo_id_rejects_empty() {
        assert!(RepoId::new("").is_err());
    }

    #[test]
    fn repo_id_rejects_colon() {
        assert!(RepoId::new("foo:bar").is_err());
    }

    #[test]
    fn repo_id_rejects_slash() {
        assert!(RepoId::new("foo/bar").is_err());
    }

    #[test]
    fn repo_id_accepts_valid() {
        let id = RepoId::new("auth-svc").unwrap();
        assert_eq!(id.as_str(), "auth-svc");
        assert_eq!(id.to_string(), "auth-svc");
    }

    #[test]
    fn global_id_format_is_stable() {
        let repo = RepoId::new("auth-svc").unwrap();
        let id = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            None,
        );
        assert_eq!(id.as_str(), "auth-svc:Function:src/auth.rs:verify_token:0");
    }

    #[test]
    fn global_id_roundtrip() {
        let repo = RepoId::new("billing-svc").unwrap();
        let id = GlobalId::new(
            &repo,
            NodeType::Method,
            "src/invoice.py",
            "calc_total",
            Some(42),
        );
        let parsed = GlobalId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.repo_id(), "billing-svc");
    }

    #[test]
    fn global_id_parse_rejects_too_few_parts() {
        assert!(GlobalId::parse("foo:bar").is_err());
    }

    /// Regression for round-3 #9: a malformed Kind segment must be
    /// rejected at parse time, not silently round-trip into a graph
    /// lookup that surfaces as an indistinguishable `NotFound`.
    #[test]
    fn global_id_parse_rejects_unknown_kind() {
        // `Foo` is not a `NodeType` variant — round-3 #9 path.
        let err = GlobalId::parse("auth-svc:Foo:src/main.rs:hello:0").unwrap_err();
        assert!(
            matches!(err, crate::error::LainError::InvalidGlobalId(_)),
            "expected InvalidGlobalId, got {err:?}"
        );
        assert!(
            err.to_string().contains("unknown node kind"),
            "error message should mention unknown kind: {err}"
        );
    }

    /// Pin every `NodeType` the parser accepts. The id is built via
    /// `GlobalId::new` (per §5.1 step 1 — every site routes through
    /// `GlobalId`, no hand-built `format!("{...}:{...}")`) and the
    /// variant list comes from `NodeType::all()` (per step 2 — the
    /// hardcoded list used to duplicate `parse`'s `is_known_node_kind`
    /// set, which has since been replaced by `NodeType::all()` itself;
    /// iterating over `NodeType::all()` here keeps the test honest if
    /// either side ever drifts from the other).
    #[test]
    fn global_id_parse_accepts_every_node_type() {
        let repo = RepoId::new("svc").unwrap();
        let path = "src/x";
        let name = "fn";
        for kind in NodeType::all() {
            let gid = GlobalId::new(&repo, kind.clone(), path, name, None);
            GlobalId::parse(gid.as_str())
                .unwrap_or_else(|e| panic!("kind {kind:?} should parse: {e}"));
        }
    }
    #[test]
    fn global_id_new_with_line_start_round_trips() {
        let repo = RepoId::new("bytes").unwrap();
        let cases = [
            (None, 0u32),
            (Some(0), 0),
            (Some(1), 1),
            (Some(12345), 12345),
        ];
        for (input, expected_line) in cases {
            let gid = GlobalId::new(&repo, NodeType::Function, "src/lib.rs", "foo", input);
            let parsed = GlobalId::parse(gid.as_str()).unwrap();
            // Round-trip preserves the line-start via the `line_start`
            // accessor (the segment that lives behind the last `:`).
            assert_eq!(parsed.line_start(), Some(expected_line));
        }
    }

    #[test]
    fn global_id_parse_rejects_pre_bump_format() {
        // Pre-bump format: 4 segments (no line_start).
        let legacy = "bytes:Function:src/lib.rs:foo";
        let err = GlobalId::parse(legacy).unwrap_err();
        assert!(matches!(err, LainError::InvalidGlobalId(_)));
    }

    #[test]
    fn global_id_accessors_extract_path_name_line_start() {
        let repo = RepoId::new("auth-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            Some(42),
        );
        assert_eq!(gid.path().as_deref(), Some("src/auth.rs"));
        assert_eq!(gid.name().as_deref(), Some("verify_token"));
        assert_eq!(gid.line_start(), Some(42));

        let zero = GlobalId::new(&repo, NodeType::Module, "src/lib.rs", "root", None);
        assert_eq!(zero.path().as_deref(), Some("src/lib.rs"));
        assert_eq!(zero.name().as_deref(), Some("root"));
        assert_eq!(zero.line_start(), Some(0));
    }

    #[test]
    fn global_id_accessors_handle_noncanonical_shapes() {
        let repo = RepoId::new("auth-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            Some(42),
        );
        let parsed = GlobalId::parse(gid.as_str()).unwrap();

        // 5-segment ids parse; the accessors should agree with the
        // constructor's input.
        assert_eq!(parsed.path().as_deref(), Some("src/auth.rs"));
        assert_eq!(parsed.name().as_deref(), Some("verify_token"));

        // 4-segment legacy id (pre-bump) is rejected by parse, but if
        // a malformed id leaks through some other way the accessors
        // should return None rather than panic.
        let four_segment = "repo:Kind:path:name";
        let parsed_four = GlobalId::parse(four_segment).unwrap_err();
        assert!(matches!(parsed_four, LainError::InvalidGlobalId(_)));
    }

    // --- §5.1 F1 GlobalId encoding -------------------------------------

    /// `GlobalId::new` percent-encodes `:` inside path and name segments
    /// so the result still splits into exactly 5 `:`-delimited pieces.
    #[test]
    fn global_id_new_encodes_colon_in_name() {
        let repo = RepoId::new("orders-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::HttpRoute,
            "src/routes.py",
            "GET /orders/:id",
            Some(42),
        );
        // The literal `:` in the name is now `%3A`, so the on-the-wire
        // id has exactly five `:`-delimited segments (repo:Kind:path:name:line).
        assert_eq!(gid.as_str(), "orders-svc:HttpRoute:src/routes.py:GET /orders/%3Aid:42");
    }

    /// `GlobalId::new` percent-encodes `%` so that the percent-decoder
    /// cannot be tricked by a path containing a literal `%3A` (which
    /// would otherwise decode into a stray `:` and split the id into 6
    /// segments).
    #[test]
    fn global_id_new_encodes_percent_in_path() {
        let repo = RepoId::new("svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/x%3Aweird.py",
            "fn",
            Some(1),
        );
        // `%` becomes `%25`; the previously-encoded `%3A` payload is
        // preserved (we encoded the `%`, not the `:` it sits inside).
        assert_eq!(gid.as_str(), "svc:Function:src/x%253Aweird.py:fn:1");
    }

    /// `::` in a name (two consecutive colons) — neither one is a
    /// segment boundary after encoding.
    #[test]
    fn global_id_new_encodes_double_colon_in_name() {
        let repo = RepoId::new("svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/lib.rs",
            "tokio::spawn",
            None,
        );
        assert_eq!(gid.as_str(), "svc:Function:src/lib.rs:tokio%3A%3Aspawn:0");
    }

    /// Ids without `:` or `%` must remain byte-identical to today —
    /// the encoding is opt-in by character, not by default.
    #[test]
    fn global_id_new_is_byte_identical_for_safe_strings() {
        let repo = RepoId::new("auth-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            Some(42),
        );
        assert_eq!(gid.as_str(), "auth-svc:Function:src/auth.rs:verify_token:42");
    }

    /// Accessors `path()` and `name()` round-trip the decoded segments
    /// so callers see the literal `:` they put in, not the encoded form.
    #[test]
    fn global_id_accessors_decode_encoded_segments() {
        let repo = RepoId::new("orders-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::HttpRoute,
            "src/routes.py",
            "GET /orders/:id",
            Some(42),
        );
        assert_eq!(gid.name().as_deref(), Some("GET /orders/:id"));
        assert_eq!(gid.path().as_deref(), Some("src/routes.py"));
        assert_eq!(gid.line_start(), Some(42));

        let parsed = GlobalId::parse(gid.as_str()).unwrap();
        assert_eq!(parsed.name().as_deref(), Some("GET /orders/:id"));
        assert_eq!(parsed.path().as_deref(), Some("src/routes.py"));
        assert_eq!(parsed.line_start(), Some(42));
    }

    /// `parse` requires exactly 5 segments. With encoding in place, a
    /// pre-bump 4-segment id is rejected (already covered by
    /// `global_id_parse_rejects_pre_bump_format`) AND a 6+-segment id
    /// — e.g. a hand-built id with a literal `:` still in the name —
    /// is now rejected so the accessors never see a malformed string.
    #[test]
    fn global_id_parse_rejects_more_than_five_segments() {
        // Path with literal colon -> 6 segments when split on `:`.
        let too_many = "svc:Function:src/x:bad:name:0";
        let err = GlobalId::parse(too_many).unwrap_err();
        assert!(
            matches!(err, LainError::InvalidGlobalId(_)),
            "expected InvalidGlobalId, got {err:?}"
        );
    }

    /// Regression for §5.1: an `HttpRoute` named `GET /orders/:id`
    /// must (a) project into the graph with a well-formed global id,
    /// (b) have `name()` return the original literal (not the
    /// percent-encoded form), and (c) be findable by `resolve_node` —
    /// the per-tool resolver that takes a global id or a bare name and
    /// returns the node.
    #[test]
    fn http_route_with_colon_in_name_resolves_via_resolve_node() {
        use crate::graph::GraphDatabase;
        use crate::overlay::VolatileOverlay;
        use crate::schema::{GraphNode, NodeType};
        use crate::server::tools::utils::resolve_node;

        let repo = RepoId::new("orders-svc").unwrap();
        // Project the route the way the federation would: a HttpRoute
        // node whose name carries an Express-style path parameter.
        let gid = GlobalId::new(
            &repo,
            NodeType::HttpRoute,
            "src/routes.py",
            "GET /orders/:id",
            Some(42),
        );
        let mut node = GraphNode::new(
            NodeType::HttpRoute,
            "GET /orders/:id".to_string(),
            "src/routes.py".to_string(),
        );
        node.line_start = Some(42);
        node.id = gid.as_str().to_string();
        assert_eq!(gid.name().as_deref(), Some("GET /orders/:id"));

        // Build a minimal per-repo graph + overlay, exactly the shape
        // `resolve_node` consults.
        let db_path = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&db_path.path().join("g.bin")).unwrap();
        graph.upsert_node(node.clone()).unwrap();
        let overlay = VolatileOverlay::new();

        // (c) `resolve_node` finds the node both by the global id
        // (step 2b in the resolver) and by the bare name (step 4).
        let by_id = resolve_node(&graph, &overlay, gid.as_str()).expect("resolve by gid");
        assert_eq!(by_id.name, "GET /orders/:id");
        assert_eq!(by_id.path, "src/routes.py");
        let by_name = resolve_node(&graph, &overlay, "GET /orders/:id").expect("resolve by name");
        assert_eq!(by_name.name, "GET /orders/:id");
        assert_eq!(by_name.id, gid.as_str());
    }

    // Proptest round-trip of arbitrary path and name strings including
    // `:`, `%`, `::`, `%3A`. The encoder must be the exact inverse of
    // the decoder: for any input, `parse(new(x).as_str())` decodes to
    // the original `path` and `name` (and the line_start segment is
    // unaffected by encoding).
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn global_id_roundtrip_arbitrary_path_and_name(
            path in r"[\w/.\-%:]{0,40}",
            name in r"[\w/.\-%:]{0,40}",
            line in proptest::option::of(0u32..100_000),
        ) {
            let repo = RepoId::new("svc").unwrap();
            let gid = GlobalId::new(
                &repo,
                NodeType::Function,
                &path,
                &name,
                line,
            );
            // Exactly 5 segments: even with the worst-case encoded
            // payload, splitting on `:` must still produce 5 parts.
            let segment_count = gid.as_str().split(':').count();
            prop_assert_eq!(segment_count, 5);

            let parsed = GlobalId::parse(gid.as_str()).expect("parse");
            let parsed_path = parsed.path();
            let parsed_name = parsed.name();
            prop_assert_eq!(parsed_path.as_deref(), Some(path.as_str()));
            prop_assert_eq!(parsed_name.as_deref(), Some(name.as_str()));
            prop_assert_eq!(parsed.line_start(), line.or(Some(0)));
        }
    }
}
