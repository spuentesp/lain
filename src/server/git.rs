//! Git sensor integration using git2
//!
//! Handles file walking, change detection, and uncommitted diff tracking.

use crate::error::LainError;
pub use crate::sidecar::{SidecarGitSensor, SidecarHealth};
use git2::{DiffOptions, Repository, StatusOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info};

/// Git repository sensor
pub struct GitSensor {
    repo: Repository,
    workspace: PathBuf,
}

// SAFETY: `git2::Repository` is internally thread-safe per libgit2's
// design (it guards its own internals with refcounts and per-handle locks).
// git2 explicitly provides `unsafe impl Send for Repository` but no `Sync`
// impl; we extend the same trust to `GitSensor` (whose only other field,
// `PathBuf`, is `Sync`) so that `&GitSensor` is `Send` and can be passed
// across `.await` points in the federation runtime. Callers must still
// serialize concurrent method calls (e.g. via a `Mutex`) — libgit2 has
// no cross-call synchronization, but the data races we'd hit without
// the Mutex are about cache coherency on the `git2::Repository` handle,
// not about memory unsafety at the Rust level.
unsafe impl Sync for GitSensor {}

impl GitSensor {
    /// Open a Git repository at the given path
    pub fn new(workspace: &Path) -> Result<Self, LainError> {
        let repo = Repository::open(workspace)?;

        // Canonical, like the sidecar's: through a symlinked path (macOS
        // temp dirs: `/var` → `/private/var`) the two modes otherwise
        // reported the same tracked files under different spellings.
        Ok(Self {
            repo,
            workspace: dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf()),
        })
    }

    /// Check if this is a valid Git repository with a working HEAD
    pub fn is_valid(&self) -> bool {
        self.repo.head().is_ok()
    }

    /// Get all tracked files in the repository.
    ///
    /// Ignore rules are not applied: they only concern untracked files, and
    /// git keeps a tracked file tracked whatever `.gitignore` says. Filtering
    /// by them dropped real source — cJSON's `.gitignore` lists a bare
    /// `test` (a build artifact), which matches every directory named
    /// `test`, and 40-odd tracked files under `tests/unity/**/test/` were
    /// never indexed.
    pub fn get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> {
        let mut files = Vec::new();

        let mut index = self.repo.index()?;
        // libgit2 caches the index in memory; this sensor lives as long as
        // the server, so without a re-read a file added by a later commit
        // was missing here — and the orphan sweep then deleted the nodes
        // the same pass had just indexed for it.
        index.read(false)?;
        for entry in index.iter() {
            if let Ok(path) = std::str::from_utf8(&entry.path) {
                let full_path = self.workspace.join(path);
                if full_path.is_file() {
                    files.push(full_path);
                }
            }
        }

        info!("Found {} tracked files", files.len());
        Ok(files)
    }

    /// Check if a file is ignored by .gitignore. A tracked file never is —
    /// ignore rules only apply to untracked files — so the watcher still
    /// sees edits to tracked files an ignore pattern happens to match.
    pub fn is_ignored(&self, path: &Path) -> Result<bool, LainError> {
        // Relative to the workspace, through symlinks and 8.3 short names
        // (`/var` vs `/private/var` on macOS, `RUNNER~1` on Windows).
        // `Index::get_path` panics on an absolute path instead of returning
        // an error, so only a relative one may reach it.
        let rel = crate::server::graph::graph_path(&self.workspace, path);
        let rel_path = Path::new(&rel);
        if rel_path.is_relative() && !rel.contains(':') && {
            let mut index = self.repo.index()?;
            index.read(false)?;
            index.get_path(rel_path, 0).is_some()
        } {
            return Ok(false);
        }
        Ok(self.repo.is_path_ignored(path)?)
    }

    /// Get all uncommitted changes (staged and unstaged)
    pub fn get_uncommitted_changes(&self) -> Result<Vec<FileChange>, LainError> {
        let mut changes = Vec::new();

        // Get HEAD commit for comparison
        let head = self.repo.head().ok();
        let head_commit = head.as_ref().and_then(|h| h.peel_to_commit().ok());

        // Get staged changes
        let mut opts = DiffOptions::new();
        opts.include_untracked(true);

        // Compare index to HEAD for staged changes
        if let Some(commit) = head_commit {
            let diff =
                self.repo
                    .diff_tree_to_index(commit.tree().ok().as_ref(), None, Some(&mut opts))?;

            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        changes.push(FileChange {
                            path: self.workspace.join(path),
                            change_type: ChangeType::Modified,
                            staged: true,
                        });
                    }
                    true
                },
                None,
                None,
                None,
            )?;
        }

        // Get unstaged changes (workdir to index)
        let diff = self.repo.diff_index_to_workdir(None, Some(&mut opts))?;

        diff.foreach(
            &mut |delta, _| {
                if let Some(path) = delta.new_file().path() {
                    let full_path = self.workspace.join(path);
                    let staged = changes.iter().any(|c| c.path == full_path);
                    changes.push(FileChange {
                        path: full_path,
                        change_type: if delta.old_file().path().is_none() {
                            ChangeType::Added
                        } else {
                            ChangeType::Modified
                        },
                        staged,
                    });
                }
                true
            },
            None,
            None,
            None,
        )?;

        // Get untracked files
        let mut status_opts = StatusOptions::new();
        status_opts.include_untracked(true);
        status_opts.recurse_untracked_dirs(true);

        let statuses = self.repo.statuses(Some(&mut status_opts))?;

        for entry in statuses.iter() {
            if entry.status().is_wt_new() {
                if let Ok(path) = entry.path() {
                    let full_path = self.workspace.join(path);
                    changes.push(FileChange {
                        path: full_path,
                        change_type: ChangeType::Added,
                        staged: false,
                    });
                }
            }
        }

        debug!("Found {} uncommitted changes", changes.len());
        Ok(changes)
    }

    /// Get diff content for a specific file
    pub fn get_file_diff(&self, path: &Path) -> Result<String, LainError> {
        // Callers may spell the path through a symlink; the workspace is
        // canonical.
        let relative = crate::server::graph::graph_path(&self.workspace, path);

        let mut opts = DiffOptions::new();
        opts.pathspec(relative);

        let diff = self.repo.diff_index_to_workdir(None, Some(&mut opts))?;

        let mut diff_text = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            diff_text.push_str(prefix);
            if let Ok(content) = std::str::from_utf8(line.content()) {
                diff_text.push_str(content);
            }
            true
        })?;

        Ok(diff_text)
    }

    /// Get the current branch name
    pub fn get_current_branch(&self) -> Result<String, LainError> {
        let head = self.repo.head()?;
        let branch = head.shorthand().unwrap_or("unknown");
        Ok(branch.to_string())
    }

    /// Get the latest commit hash
    pub fn get_latest_commit(&self) -> Result<String, LainError> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    /// Get latest commit hash and its timestamp
    pub fn get_latest_commit_info(&self) -> Result<(String, i64), LainError> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok((commit.id().to_string(), commit.time().seconds()))
    }

    /// Get commit history for co-change analysis
    /// Returns a list of commits with their associated files
    pub fn get_commit_history(&self, count: usize) -> Result<Vec<CommitInfo>, LainError> {
        let mut commits = Vec::new();

        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;

        for oid in revwalk.flatten().take(count) {
            let commit = self.repo.find_commit(oid)?;
            let message = commit.message().unwrap_or("").to_string();

            // Get the parent commit tree to find changed files
            let tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };

            // Diff to find changed files
            let diff = self
                .repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

            let mut files = Vec::new();
            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    } else if let Some(path) = delta.old_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    }
                    true
                },
                None,
                None,
                None,
            )?;

            commits.push(CommitInfo {
                id: commit.id().to_string(),
                message: message.split('\n').next().unwrap_or("").to_string(),
                files,
                time: commit.time().seconds(),
            });
        }

        debug!("Retrieved {} commits for co-change analysis", commits.len());
        Ok(commits)
    }

    /// Analyze co-changes from commit history
    /// Returns pairs of files that frequently change together
    pub fn analyze_co_changes(
        &self,
        count: usize,
        threshold: usize,
        max_files: usize,
    ) -> Result<Vec<CoChangePair>, LainError> {
        let commits = self.get_commit_history(count)?;

        use std::collections::HashMap;
        let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();

        for commit in &commits {
            // Optimization: Skip commits that touch too many files
            // to avoid O(N^2) complexity explosions in pair generation.
            if commit.files.len() > max_files {
                debug!(
                    "Skipping commit {} for co-change: {} files exceeds max {}",
                    commit.id,
                    commit.files.len(),
                    max_files
                );
                continue;
            }

            // Sort files to ensure consistent pair ordering
            let mut files = commit.files.clone();
            files.sort();

            // Generate all pairs
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let pair = (files[i].clone(), files[j].clone());
                    *pair_counts.entry(pair).or_insert(0) += 1;
                }
            }
        }

        // Filter by threshold and convert to sorted pairs
        let mut co_changes: Vec<CoChangePair> = pair_counts
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .map(|((file1, file2), count)| CoChangePair {
                file1,
                file2,
                co_change_count: count,
            })
            .collect();

        // Sort by co-change count descending
        co_changes.sort_by_key(|b| std::cmp::Reverse(b.co_change_count));

        debug!(
            "Found {} co-change pairs above threshold {}",
            co_changes.len(),
            threshold
        );
        Ok(co_changes)
    }

    /// Get commits newer than the given commit hash
    /// Returns commits after (not including) the specified hash
    pub fn get_new_commits_since(&self, since_hash: &str) -> Result<Vec<CommitInfo>, LainError> {
        let mut commits = Vec::new();

        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;

        // A revwalk from HEAD yields newest-first, so the commits we want are
        // the ones *before* `since_hash` appears — stop as soon as we reach it.
        // This previously skipped until it saw `since_hash` and then collected
        // everything after, which is the whole history *older* than the last
        // indexed commit: the exact inverse of the intended set. Incremental
        // updates therefore never saw recent edits (a symbol deleted after the
        // last index kept answering queries) while re-scanning ancient history
        // on every pass.
        //
        // If `since_hash` is never found — a rebase, a branch switch, a
        // force-push — the walk runs to the root and every commit is returned,
        // which degrades to a full re-scan. The old code returned nothing in
        // that case, silently freezing the graph.
        for oid in revwalk.flatten() {
            let oid_str = oid.to_string();
            if oid_str == since_hash {
                break;
            }

            let commit = self.repo.find_commit(oid)?;
            let message = commit.message().unwrap_or("").to_string();

            let tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };

            let diff = self
                .repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

            let mut files = Vec::new();
            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    } else if let Some(path) = delta.old_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    }
                    true
                },
                None,
                None,
                None,
            )?;

            commits.push(CommitInfo {
                id: oid_str,
                message: message.split('\n').next().unwrap_or("").to_string(),
                files,
                time: commit.time().seconds(),
            });
        }

        debug!("Found {} new commits since {}", commits.len(), since_hash);
        Ok(commits)
    }

    /// Get all files that were changed since a specific commit hash
    pub fn get_changed_files_since(&self, since_hash: &str) -> Result<Vec<PathBuf>, LainError> {
        let commits = self.get_new_commits_since(since_hash)?;
        let mut files = std::collections::HashSet::new();
        for commit in commits {
            for file in commit.files {
                let full_path = self.workspace.join(&file);
                // Skip paths the commits touched that are no longer on disk:
                // deleted files, and the old half of a rename. Handing them to
                // the scanner just produces read errors, and their nodes are
                // reclaimed by the orphan sweep after a complete pass.
                if !full_path.is_file() {
                    continue;
                }
                // Committed, so tracked: ignore rules do not apply (see
                // `get_all_tracked_files`).
                files.insert(full_path);
            }
        }
        Ok(files.into_iter().collect())
    }
}

/// Type of change detected
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

/// A file with its change information
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub staged: bool,
}

/// Information about a commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: String,
    pub message: String,
    pub files: Vec<String>,
    pub time: i64,
}

/// A pair of files that were changed together
#[derive(Debug, Clone)]
pub struct CoChangePair {
    pub file1: String,
    pub file2: String,
    pub co_change_count: usize,
}

/// GitHub repository identity parsed from git remote
#[derive(Debug, Clone)]
pub struct RepoIdentity {
    pub owner: String,
    pub name: String,
}

impl RepoIdentity {
    /// Parse GitHub repo identity from git remote URL
    pub fn from_remote(remote_url: &str) -> Option<Self> {
        // Handle SSH format: git@github.com:owner/repo.git
        if remote_url.contains("@github.com:") {
            if let Some(path) = remote_url.rsplit(':').next() {
                let path = path.trim_end_matches(".git");
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() >= 2 {
                    return Some(RepoIdentity {
                        owner: parts[parts.len() - 2].to_string(),
                        name: parts[parts.len() - 1].to_string(),
                    });
                }
            }
        }
        // Handle HTTPS format: https://github.com/owner/repo.git
        if remote_url.contains("github.com") {
            let path = remote_url.rsplit("github.com").nth(0)?;
            let path = path.trim_end_matches(".git");
            let path = path.trim_end_matches('/');
            let parts: Vec<&str> = path.rsplit('/').collect();
            if parts.len() >= 2 {
                // parts are reversed: [repo, owner, ...]
                return Some(RepoIdentity {
                    owner: parts[1].to_string(),
                    name: parts[0].to_string(),
                });
            }
        }
        None
    }
}

impl GitSensor {
    /// Get the GitHub repository identity from the git remote
    pub fn get_repo_identity(&self) -> Result<Option<RepoIdentity>, LainError> {
        let remotes = self.repo.remotes()?;
        for remote_name in remotes.iter().flatten() {
            let Some(remote_name) = remote_name else {
                continue;
            };
            if remote_name == "origin" {
                let remote = self.repo.find_remote(remote_name)?;
                if let Ok(url) = remote.url() {
                    return Ok(RepoIdentity::from_remote(url));
                }
            }
        }
        Ok(None)
    }
}

/// Execution mode for Git sensor operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitSensorMode {
    /// Direct in-process `libgit2` calls wrapped in a mutex. The default
    /// where the sidecar cannot run (no Unix domain sockets: Windows).
    #[cfg_attr(not(unix), default)]
    InProcess,
    /// Isolated child daemon process communicating via Unix domain socket IPC.
    #[cfg_attr(unix, default)]
    Sidecar,
}

impl std::str::FromStr for GitSensorMode {
    type Err = LainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "in_process" | "inprocess" | "in-process" => Ok(Self::InProcess),
            "sidecar" => Ok(Self::Sidecar),
            other => Err(LainError::Config(format!(
                "invalid git sensor mode '{other}': expected 'in_process' or 'sidecar'"
            ))),
        }
    }
}

impl std::fmt::Display for GitSensorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProcess => write!(f, "in_process"),
            Self::Sidecar => write!(f, "sidecar"),
        }
    }
}

impl GitSensorMode {
    /// Resolve git sensor mode from the `LAIN_GIT_SENSOR` environment variable,
    /// falling back to the default `Sidecar` mode.
    pub fn from_env() -> Self {
        if let Ok(val) = std::env::var("LAIN_GIT_SENSOR") {
            if let Ok(mode) = val.parse() {
                return mode;
            }
        }
        Self::default()
    }
}

/// Polymorphic abstraction over in-process and out-of-process Git sensors.
///
/// Both variants implement the complete set of git queries used throughout
/// Lain (commit inspection, uncommitted changes, co-change analysis, and gitignore).
#[derive(Clone)]
pub enum AnyGitSensor {
    InProcess(Arc<parking_lot::Mutex<GitSensor>>),
    Sidecar(Arc<SidecarGitSensor>),
}

impl std::fmt::Debug for AnyGitSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProcess(_) => write!(f, "AnyGitSensor::InProcess(..)"),
            Self::Sidecar(s) => write!(f, "AnyGitSensor::Sidecar({:?})", s.workspace()),
        }
    }
}

impl AnyGitSensor {
    /// Open a Git sensor for the given workspace with the requested mode.
    pub fn new(workspace: &Path, mode: GitSensorMode) -> Result<Self, LainError> {
        match mode {
            GitSensorMode::InProcess => {
                let sensor = GitSensor::new(workspace)?;
                Ok(Self::InProcess(Arc::new(parking_lot::Mutex::new(sensor))))
            }
            GitSensorMode::Sidecar if cfg!(not(unix)) => {
                tracing::warn!(
                    "git sidecar needs Unix domain sockets; using the in-process git sensor"
                );
                Self::new(workspace, GitSensorMode::InProcess)
            }
            GitSensorMode::Sidecar => {
                let sensor = SidecarGitSensor::new(workspace)?;
                Ok(Self::Sidecar(Arc::new(sensor)))
            }
        }
    }

    /// Open a Git sensor using `LAIN_GIT_SENSOR` (defaults to `Sidecar`).
    pub fn from_env(workspace: &Path) -> Result<Self, LainError> {
        Self::new(workspace, GitSensorMode::from_env())
    }

    /// Construct an `AnyGitSensor` wrapping an existing in-process `GitSensor`.
    pub fn in_process(sensor: GitSensor) -> Self {
        Self::InProcess(Arc::new(parking_lot::Mutex::new(sensor)))
    }

    /// Construct an `AnyGitSensor` wrapping an existing `Arc<parking_lot::Mutex<GitSensor>>`.
    pub fn from_in_process_arc(arc: Arc<parking_lot::Mutex<GitSensor>>) -> Self {
        Self::InProcess(arc)
    }

    /// Construct an `AnyGitSensor` wrapping a `SidecarGitSensor`.
    pub fn sidecar(sensor: SidecarGitSensor) -> Self {
        Self::Sidecar(Arc::new(sensor))
    }

    /// Construct an `AnyGitSensor` wrapping an existing `Arc<SidecarGitSensor>`.
    pub fn from_sidecar_arc(arc: Arc<SidecarGitSensor>) -> Self {
        Self::Sidecar(arc)
    }

    /// Active operational mode.
    pub fn mode(&self) -> GitSensorMode {
        match self {
            Self::InProcess(_) => GitSensorMode::InProcess,
            Self::Sidecar(_) => GitSensorMode::Sidecar,
        }
    }

    /// Returns `true` if this sensor is running out-of-process via `lain-git-sidecar`.
    pub fn is_sidecar(&self) -> bool {
        matches!(self, Self::Sidecar(_))
    }

    /// Returns `true` if this sensor is running in-process via direct `libgit2`.
    pub fn is_in_process(&self) -> bool {
        matches!(self, Self::InProcess(_))
    }

    /// Health telemetry if backed by a sidecar daemon.
    pub fn sidecar_health(&self) -> Option<SidecarHealth> {
        match self {
            Self::InProcess(_) => None,
            Self::Sidecar(sensor) => Some(sensor.health()),
        }
    }

    /// Returns `true` if this sensor is operational and responsive.
    pub fn is_alive(&self) -> bool {
        match self {
            Self::InProcess(_) => true,
            Self::Sidecar(sensor) => sensor.health().alive,
        }
    }

    /// Format structured health and telemetry for this sensor.
    pub fn health_json(&self) -> serde_json::Value {
        match self {
            Self::InProcess(_) => serde_json::json!({
                "kind": "in_process",
            }),
            Self::Sidecar(sensor) => {
                let sh = sensor.health();
                serde_json::json!({
                    "kind": "sidecar",
                    "alive": sh.alive,
                    "child_pid": sh.child_pid,
                    "respawn_count": sh.respawns_in_window,
                    "respawns_in_window": sh.respawns_in_window,
                    "consecutive_failures": sh.consecutive_failures,
                    "last_call_duration_us": sh.last_call_duration_us,
                })
            }
        }
    }

    /// Borrow the underlying in-process mutex handle if in `InProcess` mode.
    pub fn as_in_process(&self) -> Option<&Arc<parking_lot::Mutex<GitSensor>>> {
        match self {
            Self::InProcess(arc) => Some(arc),
            Self::Sidecar(_) => None,
        }
    }

    /// Borrow the underlying sidecar handle if in `Sidecar` mode.
    pub fn as_sidecar(&self) -> Option<&Arc<SidecarGitSensor>> {
        match self {
            Self::InProcess(_) => None,
            Self::Sidecar(arc) => Some(arc),
        }
    }

    /// Check if this is a valid Git repository with a working HEAD.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::InProcess(m) => m.lock().is_valid(),
            Self::Sidecar(s) => s.is_valid(),
        }
    }

    /// Get latest commit hash and its timestamp.
    pub fn get_latest_commit_info(&self) -> Result<(String, i64), LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_latest_commit_info(),
            Self::Sidecar(s) => s.get_latest_commit_info(),
        }
    }

    /// Get the latest commit hash.
    pub fn get_latest_commit(&self) -> Result<String, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_latest_commit(),
            Self::Sidecar(s) => s.get_latest_commit(),
        }
    }

    /// Get all files that were changed since a specific commit hash.
    pub fn get_changed_files_since(&self, since_hash: &str) -> Result<Vec<PathBuf>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_changed_files_since(since_hash),
            Self::Sidecar(s) => s.get_changed_files_since(since_hash),
        }
    }

    /// Get all tracked files in the repository (ignore rules do not apply to
    /// tracked files).
    pub fn get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_all_tracked_files(),
            Self::Sidecar(s) => s.get_all_tracked_files(),
        }
    }

    /// Analyze co-changes from commit history.
    pub fn analyze_co_changes(
        &self,
        count: usize,
        threshold: usize,
        max_files: usize,
    ) -> Result<Vec<CoChangePair>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().analyze_co_changes(count, threshold, max_files),
            Self::Sidecar(s) => s.analyze_co_changes(count, threshold, max_files),
        }
    }

    /// Get all uncommitted changes (staged and unstaged).
    pub fn get_uncommitted_changes(&self) -> Result<Vec<FileChange>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_uncommitted_changes(),
            Self::Sidecar(s) => s.get_uncommitted_changes(),
        }
    }

    /// Check if a file is ignored by .gitignore.
    pub fn is_ignored(&self, path: &Path) -> Result<bool, LainError> {
        match self {
            Self::InProcess(m) => m.lock().is_ignored(path),
            Self::Sidecar(s) => s.is_ignored(path),
        }
    }

    /// Get diff content for a specific file.
    pub fn get_file_diff(&self, path: &Path) -> Result<String, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_file_diff(path),
            Self::Sidecar(s) => s.get_file_diff(path),
        }
    }

    /// Get the current branch name.
    pub fn get_current_branch(&self) -> Result<String, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_current_branch(),
            Self::Sidecar(s) => s.get_current_branch(),
        }
    }

    /// Get commit history for co-change or timeline analysis.
    pub fn get_commit_history(&self, count: usize) -> Result<Vec<CommitInfo>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_commit_history(count),
            Self::Sidecar(s) => s.get_commit_history(count),
        }
    }

    /// Get the GitHub repository identity from the git remote.
    pub fn get_repo_identity(&self) -> Result<Option<RepoIdentity>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_repo_identity(),
            Self::Sidecar(s) => s.get_repo_identity(),
        }
    }

    /// Get commits newer than the given commit hash.
    pub fn get_new_commits_since(&self, since_hash: &str) -> Result<Vec<CommitInfo>, LainError> {
        match self {
            Self::InProcess(m) => m.lock().get_new_commits_since(since_hash),
            Self::Sidecar(s) => s.get_new_commits_since(since_hash),
        }
    }

    /// Try to get latest commit info, failing fast if in `InProcess` mode and the mutex is locked.
    /// In `Sidecar` mode, dispatches directly without acquiring any in-process lock.
    pub fn try_get_latest_commit_info(&self) -> Result<(String, i64), LainError> {
        match self {
            Self::InProcess(m) => {
                let guard = m.try_lock().ok_or_else(git_sensor_busy_error)?;
                guard.get_latest_commit_info()
            }
            Self::Sidecar(s) => s.get_latest_commit_info(),
        }
    }

    /// Try to get changed files since commit, failing fast if in `InProcess` mode and the mutex is locked.
    /// In `Sidecar` mode, dispatches directly without acquiring any in-process lock.
    pub fn try_get_changed_files_since(&self, since_hash: &str) -> Result<Vec<PathBuf>, LainError> {
        match self {
            Self::InProcess(m) => {
                let guard = m.try_lock().ok_or_else(git_sensor_busy_error)?;
                guard.get_changed_files_since(since_hash)
            }
            Self::Sidecar(s) => s.get_changed_files_since(since_hash),
        }
    }

    /// Try to get all tracked files, failing fast if in `InProcess` mode and the mutex is locked.
    /// In `Sidecar` mode, dispatches directly without acquiring any in-process lock.
    pub fn try_get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> {
        match self {
            Self::InProcess(m) => {
                let guard = m.try_lock().ok_or_else(git_sensor_busy_error)?;
                guard.get_all_tracked_files()
            }
            Self::Sidecar(s) => s.get_all_tracked_files(),
        }
    }

    /// Try to analyze co-changes, failing fast if in `InProcess` mode and the mutex is locked.
    /// In `Sidecar` mode, dispatches directly without acquiring any in-process lock.
    pub fn try_analyze_co_changes(
        &self,
        count: usize,
        threshold: usize,
        max_files: usize,
    ) -> Result<Vec<CoChangePair>, LainError> {
        match self {
            Self::InProcess(m) => {
                let guard = m.try_lock().ok_or_else(git_sensor_busy_error)?;
                guard.analyze_co_changes(count, threshold, max_files)
            }
            Self::Sidecar(s) => s.analyze_co_changes(count, threshold, max_files),
        }
    }
}

/// Bug #2 (2026-09-18 Tauri postmortem): every operation that
/// requires the parking_lot `GitSensor` mutex in fail-fast mode
/// builds the same "mutex held by another thread" error.
pub fn git_sensor_busy_error() -> LainError {
    LainError::Other(
        "GitSensor mutex held by another thread; a prior index() \
         call may be wedged in libgit2 (Bug #2, 2026-09-18 postmortem)"
            .into(),
    )
}

#[cfg(test)]
mod tracked_files_tests {
    use super::*;

    /// A tracked file stays tracked whatever `.gitignore` says — cJSON
    /// ignores a bare `test` yet tracks `tests/unity/test/tests/*.c`.
    #[test]
    fn tracked_files_matching_an_ignore_rule_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "test\n").unwrap();
        std::fs::create_dir_all(root.join("tests/unity/test")).unwrap();
        std::fs::write(
            root.join("tests/unity/test/testunity.c"),
            "void t(void) {}\n",
        )
        .unwrap();
        std::fs::write(root.join("lib.c"), "void l(void) {}\n").unwrap();
        let repo = git2::Repository::init(root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(".gitignore")).unwrap();
        index.add_path(Path::new("lib.c")).unwrap();
        // `git add -f`: libgit2's add_path ignores ignore rules too.
        index
            .add_path(Path::new("tests/unity/test/testunity.c"))
            .unwrap();
        index.write().unwrap();
        // An untracked file under the ignored name stays out.
        std::fs::write(root.join("tests/unity/test/scratch.c"), "").unwrap();

        let sensor = GitSensor::new(root).unwrap();
        let files: Vec<String> = sensor
            .get_all_tracked_files()
            .unwrap()
            .iter()
            .map(|p| crate::server::graph::graph_path(root, p))
            .collect();
        assert!(
            files.contains(&"tests/unity/test/testunity.c".to_string()),
            "{files:?}"
        );
        assert!(files.contains(&"lib.c".to_string()), "{files:?}");
        assert!(!files.iter().any(|f| f.ends_with("scratch.c")), "{files:?}");

        // The watcher's ignore check agrees: edits to the tracked file count.
        let ws = dunce::canonicalize(root).unwrap();
        assert!(!sensor
            .is_ignored(&ws.join("tests/unity/test/testunity.c"))
            .unwrap());
        assert!(!sensor
            .is_ignored(Path::new("tests/unity/test/testunity.c"))
            .unwrap());
        assert!(sensor
            .is_ignored(Path::new("tests/unity/test/scratch.c"))
            .unwrap());
        // A path outside the workspace must not panic inside libgit2.
        let elsewhere = tempfile::tempdir().unwrap();
        let _ = sensor.is_ignored(&elsewhere.path().join("x.rs"));
    }
}
