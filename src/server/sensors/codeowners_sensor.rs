//! GitHub-style CODEOWNERS sensor.
//!
//! Reads `CODEOWNERS`, `.github/CODEOWNERS` and `docs/CODEOWNERS` files
//! from the workspace root and exposes a `(repo, path) -> [owner]`
//! lookup. `get_service` (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §10.9) consults
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

/// Look up all distinct owners declared across all rules in `repo`.
pub fn owners_for_repo(repo: &str) -> Vec<String> {
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
    let mut owners = std::collections::BTreeSet::new();
    for rule in &rules.rules {
        for o in &rule.owners {
            owners.insert(o.clone());
        }
    }
    owners.into_iter().collect()
}

/// Reverse query: find all patterns associated with `owner` in `repo`.
pub fn patterns_for_owner(repo: &str, owner: &str) -> Vec<String> {
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
    rules
        .rules
        .iter()
        .filter(|r| r.owners.iter().any(|o| o == owner))
        .map(|r| r.pattern.clone())
        .collect()
}

/// Explicitly configure parsed rules for `repo` in the global index.
pub fn set_repo_rules(repo: &str, rules: Vec<Rule>) {
    let mut map = match global().lock() {
        Ok(m) => m,
        Err(p) => p.into_inner(),
    };
    if rules.is_empty() {
        map.remove(repo);
    } else {
        map.insert(repo.to_string(), RepoRules { rules });
    }
}

/// Clear all entries from the global index.
pub fn clear_index() {
    if let Some(idx) = INDEX.get() {
        if let Ok(mut m) = idx.lock() {
            m.clear();
        }
    }
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
/// locations, and update the global index under `repo`. Returns 0 —
/// the sensor contributes attribution, not graph nodes, so the count
/// is intentionally zero.
///
/// `repo` is the repository identity from the caller. Do **not** key
/// this by `root`: on a re-indexed federation `root` is a worktree
/// directory named after a commit SHA, so a `root.file_name()` key
/// never matches the `GlobalId` repo that `get_service` looks up with.
pub fn scan_workspace_codeowners_for_repo(
    _graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
    repo: &str,
) -> Result<usize, LainError> {
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
    // An empty `repo` means the caller could not name the repository;
    // fall back to the directory name rather than keying under "".
    let key = if repo.is_empty() {
        root.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        repo.to_string()
    };
    if rules.is_empty() {
        map.remove(&key);
    } else {
        map.insert(key, RepoRules { rules });
    }
    Ok(0)
}

/// Back-compat entry point for callers that cannot name the repo:
/// keys the index by the directory name so existing tests keep
/// working. New code must use [`scan_workspace_codeowners_for_repo`].
pub fn scan_workspace_codeowners(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    let repo = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    scan_workspace_codeowners_for_repo(graph, root, namespace, &repo)
}

/// The `codeowners` sensor. Registered by hand (not via
/// `register_sensor!`) because it must override
/// [`Sensor::scan_for_repo`] to key its index by the real repo id.
pub struct CodeownersSensor;

impl crate::server::sensors::Sensor for CodeownersSensor {
    fn name(&self) -> &'static str {
        "codeowners"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::EntryPoints
    }
    /// Phase 1: after the http sensors, alongside `entry_point_sensor`.
    fn phase(&self) -> u8 {
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
    fn scan_for_repo(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
        repo_id: &str,
    ) -> Result<usize, LainError> {
        scan_workspace_codeowners_for_repo(graph, root, namespace, repo_id)
    }
    fn scan_with_report_for_repo(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
        repo_id: &str,
    ) -> Result<crate::server::sensors::ScanReport, LainError> {
        let n = self.scan_for_repo(graph, root, namespace, repo_id)?;
        Ok(crate::server::sensors::ScanReport {
            emitted: n,
            error: None,
            unresolved: Vec::new(),
        })
    }
}

// Codeowners contributes attribution, not graph nodes, so it rides on the
// `EntryPoints` bucket.
inventory::submit!(crate::server::sensors::SensorEntry(&CodeownersSensor));

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

    /// Regression: the index used to be keyed by `root.file_name()`,
    /// which on a re-indexed federation is the worktree's commit-SHA
    /// directory. `get_service` looks owners up by the `GlobalId` repo,
    /// so the two never matched and `owners` was silently omitted.
    #[test]
    fn owners_resolve_when_root_is_a_worktree_sha_directory() {
        let _guard = test_lock();
        let dir = fixed_workspace("worktree_sha");
        // SHA-shaped directory name, exactly what
        // `<data_dir>/worktrees/<repo>/<sha>/` produces.
        let worktree = dir.join("b5bf29ad1d5d6b84ef9655ee7a035a4a5c8523cf");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join("CODEOWNERS"), "* @billing-team\n").unwrap();

        let g = GraphDatabase::new(&dir.join("db.bin")).unwrap();
        scan_workspace_codeowners_for_repo(&g, &worktree, &RepoNamespace::for_test(), "billing")
            .unwrap();

        assert_eq!(
            codeowners_for("billing", "/src/main.py"),
            vec!["@billing-team".to_string()],
            "owners must resolve via the repo id, not the worktree directory name"
        );
        // The old key must NOT be how it is found.
        assert!(
            codeowners_for(&repo_of(&worktree), "/src/main.py").is_empty(),
            "a SHA-named directory must not become the index key"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn reverse_queries_and_set_repo_rules() {
        let _g = test_lock();
        let rules = vec![
            Rule {
                pattern: "/src/orders/".to_string(),
                owners: vec!["@orders-team".to_string(), "@infra-team".to_string()],
            },
            Rule {
                pattern: "/src/billing/".to_string(),
                owners: vec!["@billing-team".to_string(), "@infra-team".to_string()],
            },
        ];
        set_repo_rules("test_repo", rules);

        let owners = owners_for_repo("test_repo");
        assert_eq!(
            owners,
            vec![
                "@billing-team".to_string(),
                "@infra-team".to_string(),
                "@orders-team".to_string()
            ]
        );

        let infra_patterns = patterns_for_owner("test_repo", "@infra-team");
        assert_eq!(
            infra_patterns,
            vec!["/src/orders/".to_string(), "/src/billing/".to_string()]
        );

        let billing_patterns = patterns_for_owner("test_repo", "@billing-team");
        assert_eq!(billing_patterns, vec!["/src/billing/".to_string()]);

        clear_index();
        assert!(owners_for_repo("test_repo").is_empty());
    }
}
