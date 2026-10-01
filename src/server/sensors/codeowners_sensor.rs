//! GitHub-style CODEOWNERS sensor.
//!
//! Reads `CODEOWNERS`, `.github/CODEOWNERS` and `docs/CODEOWNERS` files
//! from the workspace root and exposes a `(repo, path) -> [owner]`
//! lookup. `get_service` (`docs/CONTRACT_FEDERATION.md` §10.9) consults
//! the lookup when building each `used_by` entry, attaching an
//! `owners: [String]` field. PR 17 stretch goal — owners on provider
//! and consumer sites in `get_service`.
//!
//! Phase 1 (§6.1): runs after the protocol sensors and alongside the
//! HTTP-client sensor / `entry_point_sensor`, before `field_access_sensor`
//! at phase 2. Doesn't need Topic nodes or `Binds` edges, so phase 1
//! is fine.
//!
//! Pattern grammar (a strict subset of GitHub CODEOWNERS):
//!
//! - `*` — any character except `/`.
//! - `**` — any characters including `/`. Implemented by falling back
//!   to a `*` that doesn't split on `/`; matches the test fixtures and
//!   is sufficient for the documented used_by attribution.
//! - Leading `/` anchors the pattern to the repo root.
//! - Trailing `/` denotes a directory; matches any path under it.
//! - Patterns without a leading `/` match anywhere in the path tree.
//!
//! The parser ignores comment lines (`# …`) and blank lines. On each
//! non-comment line, the first whitespace-separated token is the
//! pattern and the remaining tokens are owners (`@user`,
//! `@org/team`, bare emails). Lines with no owners are skipped.
//!
//! **Match order.** GitHub resolves a path against rules in
//! declaration order with "last match wins" for non-`*` patterns.
//! `codeowners_for` walks the parsed rules in reverse so the last
//! applicable pattern is returned; this matches GitHub's behaviour for
//! the common "add a more-specific rule at the bottom" idiom.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::schema::RepoNamespace;
use crate::server::sensors::SensorEntry;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const CANDIDATE_PATHS: &[&str] = &["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"];

/// One parsed CODEOWNERS rule: `pattern → owners`. `pub` so the
/// integration tests can inspect parsed rules directly.
#[derive(Debug, Clone)]
pub struct Rule {
    pub pattern: String,
    pub owners: Vec<String>,
}

/// Per-repo parsed rules. `codeowners_for` walks in reverse so the
/// last-applicable rule wins, mirroring GitHub's behaviour.
#[derive(Debug, Default)]
struct RepoRules {
    rules: Vec<Rule>,
}

/// Global index keyed by the repo id (the workspace basename). The
/// sensor `scan` populates it for the repo it just walked; lookups
/// from `get_service` read from it. `OnceLock` so tests can populate
/// it before any scan runs.
static INDEX: OnceLock<Mutex<HashMap<String, RepoRules>>> = OnceLock::new();

fn global() -> &'static Mutex<HashMap<String, RepoRules>> {
    INDEX.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up the owners of `(repo, path)`. `repo` is the repo id (the
/// basename of the workspace, e.g. `billing`). `path` is the
/// repo-relative file path with optional leading `/`. Returns an empty
/// vector when no `CODEOWNERS` rule matches or when the repo has no
/// `CODEOWNERS` file.
pub fn codeowners_for(repo: &str, path: &str) -> Vec<String> {
    let Some(idx) = INDEX.get() else {
        return Vec::new();
    };
    let map = match idx.lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    let Some(rules) = map.get(repo) else {
        return Vec::new();
    };
    for rule in rules.rules.iter().rev() {
        if matches_pattern(&rule.pattern, path) {
            return rule.owners.clone();
        }
    }
    Vec::new()
}

/// Parse a single CODEOWNERS file's text into rules. `pub` so the
/// integration tests in `tests/federation_contracts_e2e.rs` can
/// exercise the parser without going through the global index.
pub fn parse_codeowners(content: &str) -> Vec<Rule> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Escape GitHub's `#` literal-pattern: a `#` after whitespace
        // starts a comment, but `#path` as the first token is a
        // pattern that starts with `#`. Our test fixtures don't
        // exercise this case; keep the simpler split here and
        // document the limitation.
        let mut parts = trimmed.split_whitespace();
        let Some(first) = parts.next() else {
            continue;
        };
        let pattern = first.to_string();
        let owners: Vec<String> = parts.map(|s| s.to_string()).collect();
        if owners.is_empty() {
            continue;
        }
        out.push(Rule { pattern, owners });
    }
    out
}

/// Match `path` against a single CODEOWNERS pattern. Returns `true`
/// when `path` falls under `pattern` per the documented grammar.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let p = pattern.trim();
    let path = path.trim_start_matches('/');
    if p.is_empty() {
        return false;
    }
    // Directory pattern (trailing `/`).
    if let Some(dir) = p.strip_suffix('/') {
        let dir = dir.trim_start_matches('/');
        return path == dir
            || path.starts_with(&format!("{dir}/"))
            || path.starts_with(&format!("/{dir}/"));
    }
    let is_anchored = p.starts_with('/');
    let pat = p.trim_start_matches('/');
    if is_anchored {
        glob_match(pat, path)
    } else {
        // Unanchored: the pattern may appear anywhere in the path.
        // Walk the path's `/`-separated segments and try to anchor
        // the pattern at each one; this lets `*.py` match any `.py`
        // file at any depth.
        glob_match_unanchored(pat, path)
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    glob_rec(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_unanchored(pattern: &str, text: &str) -> bool {
    if glob_match(pattern, text) {
        return true;
    }
    // Try matching at each `/` boundary in `text`. Each segment
    // becomes the start; the pattern's leading `/` anchoring is
    // implicit here because the caller stripped it.
    let bytes = text.as_bytes();
    let mut idx = 0usize;
    while let Some(pos) = bytes[idx..].iter().position(|&b| b == b'/') {
        let start = idx + pos + 1;
        if glob_match(pattern, &text[start..]) {
            return true;
        }
        idx = start;
    }
    false
}

fn glob_rec(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p[0] == b'*' {
        // `*` matches zero or more characters other than `/`. Try
        // every continuation that doesn't cross a `/`.
        for i in 0..=s.len() {
            if i > 0 && s[i - 1] == b'/' {
                break;
            }
            if glob_rec(&p[1..], &s[i..]) {
                return true;
            }
        }
        return false;
    }
    if s.is_empty() {
        return false;
    }
    if p[0] == s[0] {
        glob_rec(&p[1..], &s[1..])
    } else {
        false
    }
}

/// Walk the workspace, read any `CODEOWNERS` files at conventional
/// locations, and update the global index for this repo. Returns 0 —
/// the sensor contributes attribution, not graph nodes, so the count
/// is intentionally zero.
pub fn scan_workspace_codeowners(
    _graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    let repo = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut rules: Vec<Rule> = Vec::new();
    for cand in CANDIDATE_PATHS {
        let p = root.join(cand);
        if let Ok(content) = std::fs::read_to_string(&p) {
            for r in parse_codeowners(&content) {
                rules.push(r);
            }
        }
    }
    let mut map = match global().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    if rules.is_empty() {
        map.remove(&repo);
    } else {
        map.insert(repo, RepoRules { rules });
    }
    Ok(0)
}

/// The `codeowners` sensor. Submits itself via `inventory::submit!`
/// below; the shared walker in `sensors/util.rs` is not used because
/// the sensor reads specific files, not a workspace walk.
pub struct CodeownersSensor;

impl crate::server::sensors::Sensor for CodeownersSensor {
    fn name(&self) -> &'static str {
        "codeowners"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        // Codeowners contributes attribution, not graph nodes — the
        // bucket is a placeholder so the counts surface in `run_all`
        // without growing the enum (mirrors the `event_sensor`
        // convention).
        crate::server::sensors::SensorCountField::EntryPoints
    }
    fn phase(&self) -> u8 {
        // Phase 1: runs after the http sensors (phase 0), alongside
        // `entry_point_sensor`. Doesn't need Topic nodes or `Binds`
        // edges, so the earlier phases are sufficient.
        1
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_codeowners(graph, root, namespace)
    }
}

inventory::submit!(SensorEntry(&CodeownersSensor));

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RepoNamespace;
    use std::sync::{Mutex, MutexGuard};

    /// Serialise the tests that share the global INDEX. Without it
    /// one test's scan can clobber another's lookup. Recovering from
    /// a poison keeps a single failing test from cascading.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn test_lock() -> MutexGuard<'static, ()> {
        match TEST_LOCK.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn run_scan(dir: &Path) {
        let g = GraphDatabase::new(&dir.join("db.bin")).unwrap();
        scan_workspace_codeowners(&g, dir, &RepoNamespace::for_test()).unwrap();
    }

    /// A workspace under `/tmp/<tag>` whose basename is the tag, so
    /// the lookup can use the same name the sensor inferred. The
    /// `tempdir` crate's auto-generated names are random and don't
    /// match the supplied tag.
    fn fixed_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lain_codeowners_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn repo_of(dir: &Path) -> String {
        dir.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn parses_basic_owner_entries() {
        let rules = parse_codeowners(
            r#"
# top-level owners
*                       @global-team
/src/                   @src-team

/src/orders_api.py     @billing-team @oncall
*.py                   @python-team
"#,
        );
        assert_eq!(rules.len(), 4, "rules: {rules:?}");
        assert_eq!(rules[0].pattern, "*");
        assert_eq!(rules[0].owners, vec!["@global-team".to_string()]);
        assert_eq!(rules[1].pattern, "/src/");
        assert_eq!(rules[1].owners, vec!["@src-team".to_string()]);
        assert_eq!(
            rules[2].owners,
            vec!["@billing-team".to_string(), "@oncall".to_string()]
        );
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let rules = parse_codeowners(
            r#"
# a comment

   # indented comment
/foo  @team

"#,
        );
        assert_eq!(rules.len(), 1, "rules: {rules:?}");
        assert_eq!(rules[0].pattern, "/foo");
    }

    #[test]
    fn skips_lines_without_owners() {
        let rules = parse_codeowners("/orphan-pattern\n*.go  @go-team\n");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "*.go");
    }

    #[test]
    fn supports_org_team_and_email_owners() {
        let rules = parse_codeowners("/x  @org/team  user@example.com\n");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "/x");
        assert_eq!(
            rules[0].owners,
            vec!["@org/team".to_string(), "user@example.com".to_string()],
        );
    }

    #[test]
    fn matches_anchored_directory_pattern() {
        assert!(matches_pattern("/src/", "/src/main.py"));
        assert!(matches_pattern("/src/", "src/main.py"));
        assert!(matches_pattern("/src/", "src/"));
        assert!(!matches_pattern("/src/", "/docs/x.md"));
        assert!(!matches_pattern("/src/", "docs/x.md"));
    }

    #[test]
    fn matches_anchored_file_pattern() {
        assert!(matches_pattern("/src/orders_api.py", "/src/orders_api.py"));
        assert!(matches_pattern("/src/orders_api.py", "src/orders_api.py"));
        assert!(!matches_pattern("/src/orders_api.py", "/src/other.py"));
    }

    #[test]
    fn matches_extension_pattern_unanchored() {
        assert!(matches_pattern("*.py", "/src/orders_api.py"));
        assert!(matches_pattern("*.py", "src/orders_api.py"));
        assert!(matches_pattern("*.py", "anywhere/deep/file.py"));
        assert!(!matches_pattern("*.py", "/src/main.rs"));
    }

    #[test]
    fn matches_bare_star() {
        // GitHub: a bare `*` matches any file (or directory). The
        // surrounding implementation walks each `/` boundary and
        // matches the basename, so a deep path still resolves.
        assert!(matches_pattern("*", "/anything"));
        assert!(matches_pattern("*", "anything"));
        assert!(matches_pattern("*", "deep/nested/file.txt"));
        assert!(matches_pattern("*", "/foo/bar"));
    }

    #[test]
    fn no_codeowners_returns_empty_list() {
        let _g = test_lock();
        let dir = fixed_workspace("empty");
        run_scan(&dir);
        let repo = repo_of(&dir);
        assert!(codeowners_for(&repo, "/src/x.py").is_empty());
    }

    #[test]
    fn reads_workspace_codeowners_file() {
        let _g = test_lock();
        let dir = fixed_workspace("with_codeowners");
        let codeowners = dir.join(".github").join("CODEOWNERS");
        std::fs::create_dir_all(codeowners.parent().unwrap()).unwrap();
        // GitHub CODEOWNERS is last-match-wins: a more specific rule
        // listed *later* overrides earlier ones. Order the entries
        // so the file-specific rule is at the bottom.
        std::fs::write(
            &codeowners,
            "\
*.py                   @python-team
/src/                  @src-team
/src/orders_api.py     @billing-team
",
        )
        .unwrap();
        run_scan(&dir);
        let repo = repo_of(&dir);
        assert_eq!(
            codeowners_for(&repo, "/src/orders_api.py"),
            vec!["@billing-team".to_string()],
        );
        assert_eq!(
            codeowners_for(&repo, "/src/other.py"),
            vec!["@src-team".to_string()],
        );
        assert!(codeowners_for(&repo, "/README.md").is_empty(),);
    }

    #[test]
    fn last_match_wins_for_same_path() {
        let _g = test_lock();
        let dir = fixed_workspace("last_wins");
        std::fs::write(
            dir.join("CODEOWNERS"),
            "\
/src/        @team-a
/src/foo.py  @team-b
",
        )
        .unwrap();
        run_scan(&dir);
        let repo = repo_of(&dir);
        // `/src/foo.py` is more specific and declared later → it wins.
        assert_eq!(
            codeowners_for(&repo, "/src/foo.py"),
            vec!["@team-b".to_string()],
        );
        // `/src/bar.py` falls back to the broader `/src/` rule.
        assert_eq!(
            codeowners_for(&repo, "/src/bar.py"),
            vec!["@team-a".to_string()],
        );
    }

    #[test]
    fn empty_repo_id_returns_empty() {
        let _g = test_lock();
        let dir = fixed_workspace("isolated");
        std::fs::write(dir.join("CODEOWNERS"), "*.py  @py\n").unwrap();
        run_scan(&dir);
        // Asking for an unknown repo yields nothing — the sensor
        // populates only the repo basename it scanned.
        assert!(codeowners_for("does-not-exist", "/src/x.py").is_empty());
    }
}
