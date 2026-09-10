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
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    fn is_stale(&self, max_age: Duration) -> bool;
}

/// Run `git rev-parse HEAD` against `local_path`, returning the hash on
/// success or `Ok(None)` if the path isn't a git repository. Any other
/// git failure (corrupt `.git`, lock contention, etc.) is surfaced as an
/// `LainError` — silently swallowing it would make the manifest
/// fingerprint useless exactly when an operator most wants to see it.
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
        // `git` exited non-zero. The common case is "not a git
        // repository" — `git rev-parse` reports that with exit code
        // 128 and a message on stderr. Treat that as "no hash
        // available" rather than a hard error so a workspace_dir
        // pointing at a non-checkout directory still has a usable
        // manifest entry (with `content_hash = ""`).
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if o.status.code() == Some(128) && stderr.contains("not a git repository") {
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
            SourceConfig::LocalClone { url: url.to_string(), r#ref: git_ref.to_string() },
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
        Ok(Self {
            repo_id,
            url: url.to_string(),
            git_ref: git_ref.to_string(),
            local_path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
            source_config,
        })
    }
    pub fn mark_refreshed(&self, t: SystemTime) {
        *self.last_refreshed.write() = t;
    }
    pub fn url(&self) -> &str { &self.url }
    pub fn git_ref(&self) -> &str { &self.git_ref }
}

#[async_trait]
impl RepoSource for LocalCloneSource {
    fn id(&self) -> &RepoId { &self.repo_id }
    fn local_path(&self) -> &Path { &self.local_path }
    fn kind(&self) -> &'static str { "local_clone" }
    fn source_config(&self) -> &SourceConfig { &self.source_config }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        git_head_hash(&self.local_path)
    }
    async fn fetch(&self) -> Result<(), LainError> {
        let path = self.local_path.clone();
        let url = self.url.clone();
        let git_ref = self.git_ref.clone();
        let last_refreshed = self.last_refreshed.clone();
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            if !path.exists() {
                let status = Command::new("git")
                    .arg("clone").arg("--quiet").arg(&url).arg(&path)
                    .status()
                    .map_err(|e| LainError::Git(format!("git clone failed to start: {e}")))?;
                if !status.success() {
                    return Err(LainError::Git(format!("git clone {} failed", url)));
                }
            }
            let fetch = Command::new("git")
                .current_dir(&path)
                .arg("fetch").arg("--quiet").arg("--all")
                .status()
                .map_err(|e| LainError::Git(format!("git fetch failed: {e}")))?;
            if !fetch.success() {
                return Err(LainError::Git("git fetch failed".into()));
            }
            let reset = Command::new("git")
                .current_dir(&path)
                .arg("reset").arg("--hard").arg(format!("origin/{}", git_ref))
                .status()
                .map_err(|e| LainError::Git(format!("git reset failed: {e}")))?;
            if !reset.success() {
                return Err(LainError::Git(format!("git reset to origin/{} failed", git_ref)));
            }
            *last_refreshed.write() = SystemTime::now();
            Ok(())
        }).await.map_err(|e| LainError::Git(format!("join error: {e}")))?
    }
    fn last_refreshed(&self) -> SystemTime { *self.last_refreshed.read() }
    fn is_stale(&self, max_age: Duration) -> bool {
        self.last_refreshed().elapsed().map(|e| e > max_age).unwrap_or(true)
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
        let inner = LocalCloneSource::with_config(repo_id, url, git_ref, local_path, source_config)?;
        Ok(Self { inner, refresh_interval })
    }
    pub fn refresh_interval(&self) -> Duration { self.refresh_interval }
}

#[async_trait]
impl RepoSource for ShallowCloneSource {
    fn id(&self) -> &RepoId { self.inner.id() }
    fn local_path(&self) -> &Path { self.inner.local_path() }
    fn kind(&self) -> &'static str { "shallow_clone" }
    fn source_config(&self) -> &SourceConfig { self.inner.source_config() }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        self.inner.content_hash()
    }
    async fn fetch(&self) -> Result<(), LainError> {
        let path = self.inner.local_path.clone();
        let url = self.inner.url.clone();
        let git_ref = self.inner.git_ref.clone();
        let last_refreshed = self.inner.last_refreshed.clone();
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            if !path.exists() {
                let status = Command::new("git")
                    .arg("clone").arg("--quiet").arg("--depth").arg("1").arg("--branch").arg(&git_ref).arg(&url).arg(&path)
                    .status()
                    .map_err(|e| LainError::Git(format!("git clone --depth 1 failed to start: {e}")))?;
                if !status.success() {
                    return Err(LainError::Git(format!("git clone --depth 1 {} failed", url)));
                }
            } else {
                let fetch = Command::new("git")
                    .current_dir(&path)
                    .arg("fetch").arg("--quiet").arg("--depth").arg("1").arg("origin").arg(&git_ref)
                    .status()
                    .map_err(|e| LainError::Git(format!("git fetch --depth 1 failed: {e}")))?;
                if !fetch.success() {
                    return Err(LainError::Git("git fetch --depth 1 failed".into()));
                }
                let reset = Command::new("git")
                    .current_dir(&path)
                    .arg("reset").arg("--hard").arg(format!("origin/{}", git_ref))
                    .status()
                    .map_err(|e| LainError::Git(format!("git reset failed: {e}")))?;
                if !reset.success() {
                    return Err(LainError::Git(format!("git reset to origin/{} failed", git_ref)));
                }
            }
            *last_refreshed.write() = SystemTime::now();
            Ok(())
        }).await.map_err(|e| LainError::Git(format!("join error: {e}")))?
    }
    fn last_refreshed(&self) -> SystemTime { self.inner.last_refreshed() }
    fn is_stale(&self, max_age: Duration) -> bool {
        self.inner.is_stale(max_age)
    }
}

/// Back-compat source for today's single-workspace mode. The workspace
/// directory already contains a checkout on disk; the file watcher handles
/// live updates, so `fetch` is a no-op and the source is always fresh.
pub struct WorkspaceDirSource {
    repo_id: RepoId,
    local_path: PathBuf,
    source_config: SourceConfig,
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
            return Err(LainError::Config("WorkspaceDirSource path cannot be empty".into()));
        }
        Ok(Self { repo_id, local_path, source_config })
    }
}

#[async_trait]
impl RepoSource for WorkspaceDirSource {
    fn id(&self) -> &RepoId { &self.repo_id }
    fn local_path(&self) -> &Path { &self.local_path }
    fn kind(&self) -> &'static str { "workspace_dir" }
    fn source_config(&self) -> &SourceConfig { &self.source_config }
    fn content_hash(&self) -> Result<Option<String>, LainError> {
        git_head_hash(&self.local_path)
    }
    async fn fetch(&self) -> Result<(), LainError> { Ok(()) }
    fn last_refreshed(&self) -> SystemTime { SystemTime::now() }
    fn is_stale(&self, _max_age: Duration) -> bool { false }
}
