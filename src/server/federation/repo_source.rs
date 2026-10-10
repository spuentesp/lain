use crate::error::LainError;
use crate::federation::config::SourceConfig;
use crate::federation::repo_id::RepoId;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[async_trait]
pub trait RepoSource: Send + Sync {
    fn id(&self) -> &RepoId;
    fn local_path(&self) -> &Path;
    /// Stable, lowercase kind label for this source (e.g. `"workspace_dir"`,
    /// `"local_clone"`, `"shallow_clone"`). Used as the `source_kind` field in
    /// the cold-restart manifest. The set of returned values is closed: any
    /// new source type must add a new label here AND a new `SourceConfig`
    /// variant in `config.rs` so the YAML schema stays in sync.
    fn kind(&self) -> &'static str;
    /// Original `SourceConfig` the source was constructed from. The
    /// `FederationManifest` persists this verbatim so a cold restart can
    /// reconstruct the source — `repos.yaml` is still the load-time source
    /// of truth, but the manifest gives operators (and future tooling) a
    /// round-trippable record of "what was actually loaded". Each impl
    /// stores the value it received at construction time.
    fn source_config(&self) -> &SourceConfig;
    /// Best-effort content fingerprint. For git-backed sources the
    /// canonical answer is `git rev-parse HEAD` on the local checkout.
    /// Sources that don't have a local checkout return `Ok(None)` —
    /// the manifest stores `content_hash = ""` and downstream change-
    /// detection just skips that repo. The default impl returns `None`
    /// so a sensor-only source doesn't have to override it; a git-
    /// backed source overrides with a real `git rev-parse`.
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        Ok(None)
    }
    /// Per-repo UUID namespace used to derive `GraphNode::id`s for
    /// every node this source produces. Two sources in the same
    /// federation with identical `(type, path, name, line)` therefore
    /// produce distinct ids. URGENT FIXES #2.
    fn id_namespace(&self) -> &crate::schema::RepoNamespace;
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    /// Stale once `max_age` has passed since [`Self::last_refreshed`]
    /// (or when the clock reads before it). Sources with nothing to
    /// refresh override this.
    fn is_stale(&self, max_age: Duration) -> bool {
        self.last_refreshed()
            .elapsed()
            .map(|e| e > max_age)
            .unwrap_or(true)
    }
}

/// Run one `git` invocation, mapping a spawn failure or non-zero exit
/// to `LainError::Git` with `what` as the human-readable action.
fn run_git(cmd: &mut Command, what: &str) -> Result<(), LainError> {
    let status = cmd
        .status()
        .map_err(|e| LainError::Git(format!("{what} failed to start: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(LainError::Git(format!("{what} failed")))
    }
}

/// Clone `url` into `path` if there is no checkout yet, then (unless a
/// fresh shallow clone already landed on `git_ref`) fetch `git_ref` and
/// hard-reset to `FETCH_HEAD`. `FETCH_HEAD` rather than `origin/<ref>`
/// because the latter fails for tags. `shallow` limits history to one
/// commit. Blocking: call from `spawn_blocking`.
pub(crate) fn git_sync(
    path: &Path,
    url: &str,
    git_ref: &str,
    shallow: bool,
) -> Result<(), LainError> {
    let depth: &[&str] = if shallow { &["--depth", "1"] } else { &[] };
    if !path.join(".git").exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LainError::Io(e.to_string()))?;
        }
        let mut clone = Command::new("git");
        clone.args(["clone", "--quiet"]).args(depth);
        if shallow {
            clone.args(["--branch", git_ref]);
        }
        run_git(clone.arg(url).arg(path), &format!("git clone {url}"))?;
        if shallow {
            return Ok(());
        }
    }
    run_git(
        Command::new("git")
            .current_dir(path)
            .args(["fetch", "--quiet"])
            .args(depth)
            .args(["origin", git_ref]),
        &format!("git fetch origin {git_ref}"),
    )?;
    run_git(
        Command::new("git")
            .current_dir(path)
            .args(["reset", "--quiet", "--hard", "FETCH_HEAD"]),
        &format!("git reset to {git_ref}"),
    )
}

/// Run `git rev-parse HEAD` against `local_path`, returning the hash on
/// success or `Ok(None)` if the path isn't a git repository or has no
/// commits yet. Any other git failure (corrupt `.git`, lock
/// contention, etc.) is surfaced as an `LainError` — silently
/// swallowing it would make the manifest fingerprint useless
/// exactly when an operator most wants to see it.
fn git_head_hash(local_path: &Path) -> Result<Option<String>, LainError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(local_path)
        .arg("rev-parse")
        .arg("HEAD")
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let hash = String::from_utf8(o.stdout)
                .map_err(|e| LainError::Git(format!("git rev-parse utf8: {e}")))?
                .trim()
                .to_string();
            Ok(Some(hash))
        }
        // `git` exited non-zero. Three benign cases all surface as
        // "no hash available" rather than a hard error so a
        // workspace_dir over a non-checkout, a non-git, or an
        // unborn-HEAD repo still has a usable manifest entry (with
        // `content_hash = ""`):
        //
        //   1. Not a git repository — `git rev-parse` reports that
        //      with exit code 128 and `fatal: not a git repository…`
        //      on stderr.
        //   2. Unborn HEAD — `git rev-parse HEAD` reports that with
        //      exit code 128 and `fatal: ambiguous argument 'HEAD'…`
        //      on stderr. The repo is real but has no commits yet;
        //      there's nothing to hash until the first commit.
        //   3. Detached HEAD — same exit code, same "unknown
        //      revision" wording in practice.
        //
        // Pre-fix, only case (1) was treated as `Ok(None)`. A fresh
        // `git init` repo with no commits falls into case (2) and
        // returned `Err`, which `FederatedIndex::persist_manifest`
        // then `continue`s on — silently dropping the whole entry
        // from the on-disk manifest (URGENT FIXES #5 regression).
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if o.status.code() == Some(128)
                && (stderr.contains("not a git repository")
                    || stderr.contains("ambiguous argument 'HEAD'")
                    || stderr.contains("unknown revision"))
            {
                Ok(None)
            } else {
                Err(LainError::Git(format!(
                    "git rev-parse failed (status {:?}): {}",
                    o.status.code(),
                    stderr.trim()
                )))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LainError::Git(format!("git rev-parse: {e}"))),
    }
}

pub struct LocalCloneSource {
    repo_id: RepoId,
    url: String,
    git_ref: String,
    local_path: PathBuf,
    last_refreshed: Arc<RwLock<SystemTime>>,
    source_config: SourceConfig,
    id_namespace: crate::schema::RepoNamespace,
}

impl LocalCloneSource {
    /// Convenience constructor that auto-derives a `SourceConfig` from
    /// the URL and ref. Use this in tests and any caller that doesn't
    /// have a `RepoConfig` in hand; `config.rs::build_source_for` uses
    /// `with_config` to round-trip the verbatim YAML.
    pub fn new(
        repo_id: RepoId,
        url: &str,
        git_ref: &str,
        local_path: PathBuf,
    ) -> Result<Self, LainError> {
        Self::with_config(
            repo_id,
            url,
            git_ref,
            local_path,
            SourceConfig::LocalClone {
                url: url.to_string(),
                r#ref: git_ref.to_string(),
            },
        )
    }

    /// Full-control constructor that accepts the exact `SourceConfig`
    /// the source should advertise. `config.rs` calls this with the
    /// value deserialized from `repos.yaml` so the manifest can
    /// round-trip without losing original formatting.
    pub fn with_config(
        repo_id: RepoId,
        url: &str,
        git_ref: &str,
        local_path: PathBuf,
        source_config: SourceConfig,
    ) -> Result<Self, LainError> {
        if url.is_empty() {
            return Err(LainError::Config("RepoSource url cannot be empty".into()));
        }
        let id_namespace = crate::schema::RepoNamespace::from_repo_id(&repo_id);
        Ok(Self {
            repo_id,
            url: url.to_string(),
            git_ref: git_ref.to_string(),
            local_path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
            source_config,
            id_namespace,
        })
    }
    pub fn mark_refreshed(&self, t: SystemTime) {
        *self.last_refreshed.write() = t;
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn git_ref(&self) -> &str {
        &self.git_ref
    }
}

#[async_trait]
impl RepoSource for LocalCloneSource {
    fn id(&self) -> &RepoId {
        &self.repo_id
    }
    fn local_path(&self) -> &Path {
        &self.local_path
    }
    fn kind(&self) -> &'static str {
        "local_clone"
    }
    fn source_config(&self) -> &SourceConfig {
        &self.source_config
    }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        git_head_hash(&self.local_path)
    }
    fn id_namespace(&self) -> &crate::schema::RepoNamespace {
        &self.id_namespace
    }
    async fn fetch(&self) -> Result<(), LainError> {
        let path = self.local_path.clone();
        let url = self.url.clone();
        let git_ref = self.git_ref.clone();
        let last_refreshed = self.last_refreshed.clone();
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            git_sync(&path, &url, &git_ref, false)?;
            *last_refreshed.write() = SystemTime::now();
            Ok(())
        })
        .await
        .map_err(|e| LainError::Git(format!("join error: {e}")))?
    }
    fn last_refreshed(&self) -> SystemTime {
        *self.last_refreshed.read()
    }
}

pub struct ShallowCloneSource {
    inner: LocalCloneSource,
    refresh_interval: Duration,
}

impl ShallowCloneSource {
    /// Convenience constructor that auto-derives a `SourceConfig` from
    /// the URL, ref, and refresh interval. Mirrors `LocalCloneSource::new`.
    pub fn new(
        repo_id: RepoId,
        url: &str,
        git_ref: &str,
        local_path: PathBuf,
        refresh_interval: Duration,
    ) -> Result<Self, LainError> {
        Self::with_config(
            repo_id,
            url,
            git_ref,
            local_path,
            refresh_interval,
            SourceConfig::ShallowClone {
                url: url.to_string(),
                r#ref: git_ref.to_string(),
                refresh_interval_secs: refresh_interval.as_secs(),
            },
        )
    }

    /// Full-control constructor that accepts the verbatim `SourceConfig`.
    pub fn with_config(
        repo_id: RepoId,
        url: &str,
        git_ref: &str,
        local_path: PathBuf,
        refresh_interval: Duration,
        source_config: SourceConfig,
    ) -> Result<Self, LainError> {
        let inner =
            LocalCloneSource::with_config(repo_id, url, git_ref, local_path, source_config)?;
        Ok(Self {
            inner,
            refresh_interval,
        })
    }
    pub fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }
}

#[async_trait]
impl RepoSource for ShallowCloneSource {
    fn id(&self) -> &RepoId {
        self.inner.id()
    }
    fn local_path(&self) -> &Path {
        self.inner.local_path()
    }
    fn kind(&self) -> &'static str {
        "shallow_clone"
    }
    fn source_config(&self) -> &SourceConfig {
        self.inner.source_config()
    }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        self.inner.content_hash()
    }
    fn id_namespace(&self) -> &crate::schema::RepoNamespace {
        self.inner.id_namespace()
    }
    async fn fetch(&self) -> Result<(), LainError> {
        let path = self.inner.local_path.clone();
        let url = self.inner.url.clone();
        let git_ref = self.inner.git_ref.clone();
        let last_refreshed = self.inner.last_refreshed.clone();
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            git_sync(&path, &url, &git_ref, true)?;
            *last_refreshed.write() = SystemTime::now();
            Ok(())
        })
        .await
        .map_err(|e| LainError::Git(format!("join error: {e}")))?
    }
    fn last_refreshed(&self) -> SystemTime {
        self.inner.last_refreshed()
    }
}

/// Back-compat source for today's single-workspace mode. The workspace
/// directory already contains a checkout on disk; the file watcher handles
/// live updates, so `fetch` is a no-op here. The contract layer does not
/// read the working tree — it reads `<data_dir>/mirrors/<repo>.git`, and
/// that mirror is refreshed at ref resolution (`contracts/mirrors.rs`),
/// so workspace_dir sources track committed state there too.
pub struct WorkspaceDirSource {
    repo_id: RepoId,
    local_path: PathBuf,
    source_config: SourceConfig,
    id_namespace: crate::schema::RepoNamespace,
}

impl WorkspaceDirSource {
    /// Convenience constructor that auto-derives a `SourceConfig` from
    /// the path. Use this in tests; `config.rs::build_source_for` uses
    /// `with_config` for verbatim YAML round-trip.
    pub fn new(repo_id: RepoId, local_path: PathBuf) -> Result<Self, LainError> {
        Self::with_config(
            repo_id,
            local_path.clone(),
            SourceConfig::WorkspaceDir { path: local_path },
        )
    }

    /// Full-control constructor that accepts the exact `SourceConfig`
    /// the source should advertise. `config.rs` calls this with the
    /// value deserialized from `repos.yaml`.
    pub fn with_config(
        repo_id: RepoId,
        local_path: PathBuf,
        source_config: SourceConfig,
    ) -> Result<Self, LainError> {
        if local_path.as_os_str().is_empty() {
            return Err(LainError::Config(
                "WorkspaceDirSource path cannot be empty".into(),
            ));
        }
        let id_namespace = crate::schema::RepoNamespace::from_repo_id(&repo_id);
        Ok(Self {
            repo_id,
            local_path,
            source_config,
            id_namespace,
        })
    }
}

#[async_trait]
impl RepoSource for WorkspaceDirSource {
    fn id(&self) -> &RepoId {
        &self.repo_id
    }
    fn local_path(&self) -> &Path {
        &self.local_path
    }
    fn kind(&self) -> &'static str {
        "workspace_dir"
    }
    fn source_config(&self) -> &SourceConfig {
        &self.source_config
    }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        git_head_hash(&self.local_path)
    }
    fn id_namespace(&self) -> &crate::schema::RepoNamespace {
        &self.id_namespace
    }
    async fn fetch(&self) -> Result<(), LainError> {
        Ok(())
    }
    fn last_refreshed(&self) -> SystemTime {
        SystemTime::now()
    }
    fn is_stale(&self, _max_age: Duration) -> bool {
        false
    }
}
