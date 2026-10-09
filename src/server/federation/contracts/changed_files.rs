//! git2-backed changed-files source (§9.2 `ChangedWithoutSchema`).
//!
//! Snapshots store their per-repo mirrors under
//! `<data_dir>/mirrors/<repo>.git` (PR 10, §8.1). The contract
//! diff's `ChangedWithoutSchema` rule fires when a file in the
//! endpoint's `source_files` differs between two snapshot commits;
//! the lookup happens at diff-time on the cached git2 trees.
//!
//! `MirrorChangedFiles` keeps a [`git2::Repository`] handle per repo
//! and exposes both the [`ChangedFilesSource`] trait (for tests that
//! inject a single repo's diff into `diff_contracts`) and a
//! multi-repo entry point that walks the mirror graph on demand.
//!
//! The mirror is the `git clone --mirror` artifact from PR 10; a
//! workspace-dir mirror records the same committed state (no
//! worktree, no uncommitted edits).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use git2::{DiffOptions, Oid, Repository};

use super::diff::{ChangedFilesSource, ChangedLines};

/// Tri-state outcome of diffing a repository between two revisions (§9.2, Gap P1.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoDiffResult {
    /// Diff succeeded and found the specified changed paths (relative to repository root).
    Changed(BTreeSet<String>),
    /// Diff succeeded and found zero differences between base and head.
    Unchanged,
    /// Diff could not be computed (missing git mirror, corrupt repository, invalid SHA, etc.).
    Unavailable(String),
}

impl RepoDiffResult {
    pub fn changed_files(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::Changed(f) => Some(f),
            _ => None,
        }
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// A `git2` mirror-backed source. Builds an in-memory cache keyed by
/// repo id; per-repo handles are opened lazily on first use.
///
/// `Debug` is implemented manually so the inner `git2::Repository`
/// (which does not derive `Debug`) does not appear in the output.
pub struct MirrorChangedFiles {
    data_dir: PathBuf,
    repos: RwLock<HashMap<String, Arc<Repository>>>,
}

impl std::fmt::Debug for MirrorChangedFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MirrorChangedFiles")
            .field("data_dir", &self.data_dir)
            .field(
                "repo_count",
                &self.repos.read().map(|g| g.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl MirrorChangedFiles {
    /// Build a source backed by the mirrors under
    /// `<data_dir>/mirrors`. Per-repo handles are opened lazily on
    /// first use.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            repos: RwLock::new(HashMap::new()),
        }
    }

    fn mirror_path(&self, repo: &str) -> PathBuf {
        mirror_path(&self.data_dir, repo)
    }

    /// Open or fetch the repo's mirror handle. Returns `None` when
    /// the mirror is missing (caller treats this as "no diff to
    /// report" — `ChangedWithoutSchema` will not fire).
    ///
    /// The `Arc<Repository>` cache is local to one
    /// `MirrorChangedFiles` value, which is created and consumed
    /// inside a single diff call; no two threads ever call into
    /// the same `git2::Repository` concurrently.
    #[allow(clippy::arc_with_non_send_sync)]
    fn repo(&self, repo: &str) -> Option<Arc<Repository>> {
        if let Some(r) = self.repos.read().ok()?.get(repo).cloned() {
            return Some(r);
        }
        let mut write = self.repos.write().ok()?;
        if let Some(r) = write.get(repo).cloned() {
            return Some(r);
        }
        let mirror = self.mirror_path(repo);
        let opened = Repository::open_bare(&mirror).ok()?;
        let arc = Arc::new(opened);
        write.insert(repo.to_string(), Arc::clone(&arc));
        Some(arc)
    }

    /// Compute the diff between two commits of the repo's mirror.
    /// `base_sha` and `head_sha` are 40-hex SHAs. The returned set
    /// contains every repo-relative path that differs (add, modify,
    /// delete, rename — both old and new names on rename).
    pub fn diff(&self, repo: &str, base_sha: &str, head_sha: &str) -> BTreeSet<String> {
        match self.diff_repo(repo, base_sha, head_sha) {
            RepoDiffResult::Changed(files) => files,
            _ => BTreeSet::new(),
        }
    }

    /// Compute the changed file set **and** the changed line spans in a
    /// single pass over the git diff. Callers that need both must use
    /// this rather than `diff_repo` + `diff_lines_repo`, which would
    /// walk the tree twice.
    ///
    /// `lines` is `None` when the diff could not be computed (missing
    /// mirror, bad SHA) and the caller must fall back to whole-file
    /// attribution. A changed file with no hunks (delete, rename, mode
    /// change) simply has no entry in `lines` — also "unknown" to the
    /// caller, which keeps the fallback sound.
    pub fn diff_repo_with_lines(
        &self,
        repo: &str,
        base_sha: &str,
        head_sha: &str,
    ) -> (RepoDiffResult, Option<ChangedLines>) {
        if !base_sha.is_empty() && base_sha == head_sha {
            return (RepoDiffResult::Unchanged, Some(ChangedLines::new()));
        }
        let r = match self.repo(repo) {
            Some(r) => r,
            None => {
                return (
                    RepoDiffResult::Unavailable(format!("git mirror missing for repo {repo}")),
                    None,
                );
            }
        };
        match self.diff_full_impl(&r, base_sha, head_sha) {
            Ok((files, lines)) => {
                let res = if files.is_empty() {
                    RepoDiffResult::Unchanged
                } else {
                    RepoDiffResult::Changed(files)
                };
                (res, Some(lines))
            }
            Err(e) => (
                RepoDiffResult::Unavailable(format!("git error diffing repo {repo}: {e}")),
                None,
            ),
        }
    }

    /// Compute tri-state diff for a single repository.
    pub fn diff_repo(&self, repo: &str, base_sha: &str, head_sha: &str) -> RepoDiffResult {
        self.diff_repo_with_lines(repo, base_sha, head_sha).0
    }

    /// Compute changed line ranges for a single repository: each
    /// changed file maps to merged `[start, end]` spans of the lines
    /// the diff touched (the *head* side).
    ///
    /// Returns `None` when the diff cannot be computed. A changed file
    /// with no hunks (delete, rename, mode change) simply has no entry
    /// — the caller treats an absent entry as "unknown" and falls back
    /// to whole-file attribution, which keeps the fallback sound.
    pub fn diff_lines_repo(
        &self,
        repo: &str,
        base_sha: &str,
        head_sha: &str,
    ) -> Option<ChangedLines> {
        self.diff_repo_with_lines(repo, base_sha, head_sha).1
    }

    fn diff_full_impl(
        &self,
        r: &Repository,
        base_sha: &str,
        head_sha: &str,
    ) -> Result<(BTreeSet<String>, ChangedLines), git2::Error> {
        let base_tree = lookup_tree(r, base_sha)?;
        let head_tree = lookup_tree(r, head_sha)?;
        let mut opts = DiffOptions::new();
        opts.include_typechange(true);
        let diff = r.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))?;

        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut per_file: ChangedLines = ChangedLines::new();
        diff.foreach(
            &mut |delta, _| {
                for p in [delta.new_file().path(), delta.old_file().path()]
                    .into_iter()
                    .flatten()
                {
                    files.insert(p.to_string_lossy().to_string());
                }
                true
            },
            None,
            // The hunk callback has to be present for the line callback
            // to fire; it has nothing to add.
            Some(&mut |_delta, _hunk| true),
            Some(&mut |delta, _hunk, line| {
                // Only the lines the diff actually touched. A hunk's
                // line count includes its context, so taking the hunk
                // range wholesale would implicate every sibling site
                // that happens to sit near an edit.
                let touched = matches!(
                    line.origin_value(),
                    git2::DiffLineType::Addition | git2::DiffLineType::Deletion
                );
                if touched {
                    // Attribute against the HEAD side where possible so
                    // the spans line up with head-side anchors; fall
                    // back to the old number for pure deletions.
                    let Some(no) = line.new_lineno().or(line.old_lineno()) else {
                        return true;
                    };
                    let path = delta
                        .new_file()
                        .path()
                        .or_else(|| delta.old_file().path())
                        .map(|p| p.to_string_lossy().to_string());
                    if let Some(path) = path {
                        per_file.entry(path).or_default().push((no, no));
                    }
                }
                true
            }),
        )?;

        for spans in per_file.values_mut() {
            merge_spans(spans);
        }
        Ok((files, per_file))
    }

    /// Pre-fetch a repo's mirror (so the lazy open happens up
    /// front). Used by the `diff_contracts` tool to warm the cache
    /// before any per-repo diff is requested.
    pub fn warm(&self, repo: &str) {
        let _ = self.repo(repo);
    }
}

fn mirror_path(data_dir: &Path, repo: &str) -> PathBuf {
    data_dir.join("mirrors").join(format!("{repo}.git"))
}

/// Sort point-spans and fold the adjacent ones together, so a set of
/// touched lines becomes the smallest set of `[start, end]` ranges
/// covering them.
fn merge_spans(spans: &mut Vec<(u32, u32)>) {
    spans.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (s, e) in spans.drain(..) {
        match merged.last_mut() {
            Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    *spans = merged;
}

fn lookup_tree<'r>(repo: &'r Repository, sha: &str) -> Result<git2::Tree<'r>, git2::Error> {
    let oid = Oid::from_str(sha).map_err(|_| git2::Error::from_str("invalid sha"))?;
    let commit = repo.find_commit(oid)?;
    commit.tree()
}

/// Compute the diff between two snapshot records for a single
/// repo. Used by `diff_contracts` (`§9.2`) when both snapshots are
/// `ready` and the operator's selected `repo` matches the
/// snapshot's commit set.
pub fn diff_for(data_dir: &Path, repo: &str, base_sha: &str, head_sha: &str) -> BTreeSet<String> {
    MirrorChangedFiles::new(data_dir).diff(repo, base_sha, head_sha)
}

/// Multi-repo entry point: build the changed-files set for every
/// repo that appears in `repos` (with the matching `(base, head)`
/// pair). Repos without mirrors contribute nothing. The union is
/// the changed set `diff_contracts` queries against the endpoint's
/// `source_files`.
pub fn diff_multi(
    data_dir: &Path,
    repos: &[(String, String, String)], // (repo, base_sha, head_sha)
) -> BTreeSet<String> {
    let src = MirrorChangedFiles::new(data_dir);
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (repo, base_sha, head_sha) in repos {
        for f in src.diff(repo, base_sha, head_sha) {
            out.insert(f);
        }
    }
    out
}

/// Adapter for the test-only `ChangedFilesSource` trait: holds a
/// precomputed set keyed by repo id so a caller can inject the
/// result of [`MirrorChangedFiles::diff`] without doing the
/// snapshot machinery. Real tools build this adapter once per call
/// and pass it to `diff_contracts`.
pub struct RepoScopedChangedFiles {
    pub repo: String,
    pub files: BTreeSet<String>,
}

impl ChangedFilesSource for RepoScopedChangedFiles {
    fn changed_files(&self, _base: &str, _head: &str) -> BTreeSet<String> {
        self.files.clone()
    }

    fn changed_files_for_repo(&self, repo: &str, _base: &str, _head: &str) -> RepoDiffResult {
        if repo == self.repo {
            if self.files.is_empty() {
                RepoDiffResult::Unchanged
            } else {
                RepoDiffResult::Changed(self.files.clone())
            }
        } else {
            RepoDiffResult::Unchanged
        }
    }
}

/// Variant that exposes the multi-repo case to `diff_contracts`:
/// maps each repo to its tri-state diff outcome.
pub struct MultiRepoChangedFiles {
    pub by_repo: BTreeMap<String, RepoDiffResult>,
    /// Per-repo changed line ranges. A repo with no entry, or a file
    /// with no entry inside one, is "unknown" at that granularity and
    /// the caller falls back to whole-file attribution.
    pub line_ranges: BTreeMap<String, ChangedLines>,
}

impl MultiRepoChangedFiles {
    pub fn unavailable_repos(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (repo, res) in &self.by_repo {
            if res.is_unavailable() {
                out.insert(repo.clone());
            }
        }
        out
    }

    pub fn from_sets(by_repo: BTreeMap<String, BTreeSet<String>>) -> Self {
        let mapped = by_repo
            .into_iter()
            .map(|(repo, set)| {
                let res = if set.is_empty() {
                    RepoDiffResult::Unchanged
                } else {
                    RepoDiffResult::Changed(set)
                };
                (repo, res)
            })
            .collect();
        Self {
            by_repo: mapped,
            line_ranges: BTreeMap::new(),
        }
    }
}

impl ChangedFilesSource for MultiRepoChangedFiles {
    fn changed_files(&self, _base: &str, _head: &str) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for s in self.by_repo.values() {
            if let RepoDiffResult::Changed(f) = s {
                for file in f {
                    out.insert(file.clone());
                }
            }
        }
        out
    }

    /// One repo's changed spans. Only computed when that repo
    /// contributed some — otherwise the caller falls back to whole-file
    /// attribution, which is the sound default.
    fn changed_line_ranges_for_repo(
        &self,
        repo: &str,
        _base: &str,
        _head: &str,
    ) -> Option<ChangedLines> {
        self.line_ranges.get(repo).cloned()
    }

    fn changed_files_for_repo(&self, repo: &str, _base: &str, _head: &str) -> RepoDiffResult {
        self.by_repo.get(repo).cloned().unwrap_or_else(|| {
            RepoDiffResult::Unavailable(format!("repo {repo} not found in snapshot diff"))
        })
    }

    fn unavailable_repos(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (repo, res) in &self.by_repo {
            if res.is_unavailable() {
                out.insert(repo.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(args: &[&str], cwd: &Path) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git failed");
        assert!(status.success(), "git {args:?} failed: {status:?}");
    }

    fn git_stdout(args: &[&str], cwd: &Path) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git failed");
        assert!(out.status.success(), "git {args:?} failed: {:?}", out);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn init_repo_with_history(dir: &Path, files: &[(&str, &str)]) -> (String, String, String) {
        std::fs::create_dir_all(dir).unwrap();
        git(&["init", "-q", "-b", "main"], dir);
        git(&["config", "user.email", "test@lain"], dir);
        git(&["config", "user.name", "lain"], dir);
        for (path, contents) in files {
            std::fs::write(dir.join(path), contents).unwrap();
        }
        git(&["add", "-A"], dir);
        git(&["commit", "--quiet", "-m", "init"], dir);
        let base = git_stdout(&["rev-parse", "HEAD"], dir);
        std::fs::write(dir.join(files[0].0), "modified\n").unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "--quiet", "-m", "edit"], dir);
        let head = git_stdout(&["rev-parse", "HEAD"], dir);
        (base, head, files[0].0.to_string())
    }

    fn setup_mirror(src: &Path, mirror: &Path) {
        git(
            &[
                "clone",
                "--quiet",
                "--mirror",
                &src.display().to_string(),
                &mirror.display().to_string(),
            ],
            src.parent().unwrap(),
        );
    }

    #[test]
    fn mirror_changed_files_returns_modified_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("src");
        let mirror = tmp.path().join("mirrors").join("src.git");
        let (base, head, path) =
            init_repo_with_history(&work, &[("a.txt", "hello\n"), ("b.txt", "world\n")]);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        setup_mirror(&work, &mirror);
        let src = MirrorChangedFiles::new(tmp.path());
        let changes = src.diff("src", &base, &head);
        assert!(changes.contains(&path), "expected {path:?} in {changes:?}");
        assert!(changes.contains("a.txt"));
        assert!(!changes.contains("b.txt"));
    }

    #[test]
    fn mirror_changed_files_returns_empty_when_no_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("src");
        let mirror = tmp.path().join("mirrors").join("src.git");
        let (base, _head, _path) =
            init_repo_with_history(&work, &[("a.txt", "hello\n"), ("b.txt", "world\n")]);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        setup_mirror(&work, &mirror);
        let src = MirrorChangedFiles::new(tmp.path());
        let same = src.diff("src", &base, &base);
        assert!(
            same.is_empty(),
            "expected no diff for identical SHAs: {same:?}"
        );
    }

    /// Changed line spans are what let a change inside a shared file be
    /// attributed to the site it touched. A whole-file "it changed" is
    /// not enough to distinguish a routing-table edit from a sibling
    /// handler's edit.
    #[test]
    fn mirror_changed_files_reports_changed_line_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("src");
        let mirror = tmp.path().join("mirrors").join("src.git");
        let (base, head, path) =
            init_repo_with_history(&work, &[("a.txt", "hello\n"), ("b.txt", "world\n")]);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        setup_mirror(&work, &mirror);
        let src = MirrorChangedFiles::new(tmp.path());

        let ranges = src
            .diff_lines_repo("src", &base, &head)
            .expect("line ranges should compute when the diff succeeds");
        let spans = ranges
            .get(&path)
            .unwrap_or_else(|| panic!("no spans for {path}: {ranges:?}"));
        assert!(
            spans.contains(&(1, 1)),
            "the edit rewrote line 1 of {path}, expected (1,1) in {spans:?}"
        );
        // Identical SHAs: nothing changed, empty map (not None — the
        // diff succeeded and found nothing).
        let same = src
            .diff_lines_repo("src", &base, &base)
            .expect("identical SHAs still compute");
        assert!(same.is_empty(), "expected no spans, got {same:?}");
        // Missing mirror: `None`, i.e. unknown — the caller must fall
        // back to whole-file attribution rather than silence.
        assert!(src.diff_lines_repo("missing", &base, &head).is_none());
    }

    /// The span must cover only the lines the diff *touched*, not the
    /// whole hunk. A hunk's line count includes its context, so
    /// `hunk.new_lines()` gives a multi-line span for a one-line edit
    /// and implicates every sibling site near it — which silently
    /// narrowed attribution until it was caught end-to-end in T1's
    /// `s6-rename-path`. Pinned here so a fixture change cannot
    /// unpin it.
    #[test]
    fn changed_line_spans_exclude_the_hunks_context_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("src");
        let mirror = tmp.path().join("mirrors").join("src.git");
        std::fs::create_dir_all(&work).unwrap();
        git(&["init", "-q", "-b", "main"], &work);
        git(&["config", "user.email", "test@lain"], &work);
        git(&["config", "user.name", "lain"], &work);
        // Seven lines so the edit is interior and the hunk carries
        // context on both sides.
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        std::fs::write(work.join("a.txt"), before).unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "--quiet", "-m", "init"], &work);
        let base = git_stdout(&["rev-parse", "HEAD"], &work);

        let after = "a\nb\nc\nD\ne\nf\ng\n"; // line 4 only
        std::fs::write(work.join("a.txt"), after).unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "--quiet", "-m", "edit"], &work);
        let head = git_stdout(&["rev-parse", "HEAD"], &work);

        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        setup_mirror(&work, &mirror);
        let src = MirrorChangedFiles::new(tmp.path());

        let ranges = src
            .diff_lines_repo("src", &base, &head)
            .expect("line ranges should compute");
        assert_eq!(
            ranges.get("a.txt"),
            Some(&vec![(4, 4)]),
            "only line 4 changed — the hunk's context lines (2,3,5,6) must \
             not be counted as changed: {ranges:?}"
        );
    }

    #[test]
    fn mirror_changed_files_returns_empty_when_mirror_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = MirrorChangedFiles::new(tmp.path());
        let out = src.diff("missing", "deadbeef", "deadbeef");
        assert!(out.is_empty());
        let res = src.diff_repo("missing", "deadbeef", "cafebabe");
        assert!(matches!(res, RepoDiffResult::Unavailable(_)));
    }

    #[test]
    fn diff_repo_reports_tri_state() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("src");
        let mirror = tmp.path().join("mirrors").join("src.git");
        let (base, head, path) =
            init_repo_with_history(&work, &[("a.txt", "hello\n"), ("b.txt", "world\n")]);
        std::fs::create_dir_all(mirror.parent().unwrap()).unwrap();
        setup_mirror(&work, &mirror);
        let src = MirrorChangedFiles::new(tmp.path());

        // Changed
        let res = src.diff_repo("src", &base, &head);
        match res {
            RepoDiffResult::Changed(files) => {
                assert!(files.contains(&path));
            }
            other => panic!("expected Changed, got {other:?}"),
        }

        // Unchanged (same SHAs)
        let same = src.diff_repo("src", &base, &base);
        assert_eq!(same, RepoDiffResult::Unchanged);

        // Unavailable (missing repo)
        let unavail = src.diff_repo("missing", &base, &head);
        assert!(unavail.is_unavailable());
    }

    #[test]
    fn diff_multi_unions_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let work_a = tmp.path().join("a");
        let work_b = tmp.path().join("b");
        let (a_base, a_head, a_path) = init_repo_with_history(&work_a, &[("a.txt", "x\n")]);
        let (b_base, b_head, b_path) = init_repo_with_history(&work_b, &[("b.txt", "y\n")]);
        std::fs::create_dir_all(tmp.path().join("mirrors/a.git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("mirrors/b.git")).unwrap();
        setup_mirror(&work_a, &tmp.path().join("mirrors/a.git"));
        setup_mirror(&work_b, &tmp.path().join("mirrors/b.git"));
        let out = diff_multi(
            tmp.path(),
            &[("a".into(), a_base, a_head), ("b".into(), b_base, b_head)],
        );
        assert!(out.contains(&a_path));
        assert!(out.contains(&b_path));
    }

    #[test]
    fn repo_scoped_changed_files_adapts_to_trait() {
        let mut files = BTreeSet::new();
        files.insert("src/a.rs".to_string());
        let src = RepoScopedChangedFiles {
            repo: "orders".into(),
            files,
        };
        let out = src.changed_files("base", "head");
        assert!(out.contains("src/a.rs"));
    }

    #[test]
    fn multi_repo_changed_files_unions_per_repo() {
        let mut by_repo = BTreeMap::new();
        let mut a = BTreeSet::new();
        a.insert("src/a.rs".to_string());
        by_repo.insert("orders".into(), a.clone());
        let mut b = BTreeSet::new();
        b.insert("src/b.rs".to_string());
        by_repo.insert("billing".into(), b.clone());
        let src = MultiRepoChangedFiles::from_sets(by_repo);
        let out = src.changed_files("base", "head");
        assert!(out.contains("src/a.rs"));
        assert!(out.contains("src/b.rs"));
        assert_eq!(
            src.changed_files_for_repo("orders", "base", "head"),
            RepoDiffResult::Changed(a)
        );
        assert_eq!(
            src.changed_files_for_repo("billing", "base", "head"),
            RepoDiffResult::Changed(b)
        );
        assert!(src
            .changed_files_for_repo("unknown", "base", "head")
            .is_unavailable());
    }
}
