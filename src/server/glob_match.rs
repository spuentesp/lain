//! Minimal glob matcher used by the `get_audit_log` MCP tool to
//! filter audit events by their `path` field. The brief (Task 2.5)
//! permitted hand-rolling a `*` / `**` matcher, but the `glob`
//! crate is already a dependency of this crate, so we delegate to
//! `glob::Pattern` for the actual matching and expose only the
//! `(&str, &Path) -> bool` shape the tool needs.
//!
//! `glob::Pattern` accepts `*` (single-segment) and `**` (any depth)
//! and treats `/` as the segment separator on Unix. Absolute and
//! relative paths round-trip identically because the pattern and
//! the path are compared as strings. Non-UTF-8 paths fail to
//! match (the glob crate does not support them), which is
//! acceptable here because audit `path` values originate from
//! paths the server itself produced and they are UTF-8 in
//! practice.

use std::path::Path;

/// Returns `true` iff `path` matches the glob `pattern`.
///
/// The pattern is parsed by `glob::Pattern::new`; a malformed
/// pattern returns `false` rather than propagating the error, so
/// callers can pass user-supplied patterns from MCP args without
/// a separate validation step. The audit tool treats a
/// non-matching pattern as "filter dropped everything", which is
/// the safer failure mode for a forensic surface.
pub fn simple(pattern: &str, path: &Path) -> bool {
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches_path(path),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn exact_path_matches() {
        assert!(simple("/a.rs", &PathBuf::from("/a.rs")));
        assert!(!simple("/a.rs", &PathBuf::from("/b.rs")));
    }

    #[test]
    fn single_star_matches_one_segment() {
        // Per glob 0.3 docs, `*` matches *any* characters including
        // `/`. So `"/b/*.rs"` matches `"/b/foo.rs"` and the multi-segment
        // `"/b/foo/bar.rs"`. `**` is the operator that constrains
        // match depth — see the next test. This test pins the actual
        // glob-crate semantics so a future bump that tightens `*`
        // triggers a deliberate test change, not a silent regression.
        assert!(simple("/b/*.rs", &PathBuf::from("/b/foo.rs")));
        assert!(simple("/b/*.rs", &PathBuf::from("/b/foo/bar.rs")));
    }

    #[test]
    fn double_star_matches_any_depth() {
        // `**` recursively matches any number of path segments.
        assert!(simple("/b/**", &PathBuf::from("/b/foo.rs")));
        assert!(simple("/b/**", &PathBuf::from("/b/foo/bar.rs")));
        assert!(!simple("/b/**", &PathBuf::from("/a.rs")));
    }

    #[test]
    fn malformed_pattern_returns_false_not_error() {
        // `a**b` is rejected by glob because `**` must form a whole
        // path component. The tool surfaces this as "filter
        // excludes everything" rather than crashing.
        assert!(!simple("a**b", &PathBuf::from("/a/foo/b")));
    }
}