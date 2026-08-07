use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[async_trait]
pub trait RepoSource: Send + Sync {
    fn id(&self) -> &RepoId;
    fn local_path(&self) -> &Path;
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    fn is_stale(&self, max_age: Duration) -> bool;
}

pub struct LocalCloneSource {
    repo_id: RepoId,
    url: String,
    git_ref: String,
    local_path: PathBuf,
    last_refreshed: Arc<RwLock<SystemTime>>,
}

impl LocalCloneSource {
    pub fn new(repo_id: RepoId, url: &str, git_ref: &str, local_path: PathBuf) -> Result<Self, LainError> {
        if url.is_empty() {
            return Err(LainError::Config("RepoSource url cannot be empty".into()));
        }
        Ok(Self {
            repo_id,
            url: url.to_string(),
            git_ref: git_ref.to_string(),
            local_path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
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
    async fn fetch(&self) -> Result<(), LainError> {
        use std::process::Command;
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
    pub fn new(repo_id: RepoId, url: &str, git_ref: &str, local_path: PathBuf, refresh_interval: Duration) -> Result<Self, LainError> {
        let inner = LocalCloneSource::new(repo_id, url, git_ref, local_path)?;
        Ok(Self { inner, refresh_interval })
    }
    pub fn refresh_interval(&self) -> Duration { self.refresh_interval }
}

#[async_trait]
impl RepoSource for ShallowCloneSource {
    fn id(&self) -> &RepoId { self.inner.id() }
    fn local_path(&self) -> &Path { self.inner.local_path() }
    async fn fetch(&self) -> Result<(), LainError> {
        use std::process::Command;
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
}

impl WorkspaceDirSource {
    pub fn new(repo_id: RepoId, local_path: PathBuf) -> Result<Self, LainError> {
        if local_path.as_os_str().is_empty() {
            return Err(LainError::Config("WorkspaceDirSource path cannot be empty".into()));
        }
        Ok(Self { repo_id, local_path })
    }
}

#[async_trait]
impl RepoSource for WorkspaceDirSource {
    fn id(&self) -> &RepoId { &self.repo_id }
    fn local_path(&self) -> &Path { &self.local_path }
    async fn fetch(&self) -> Result<(), LainError> { Ok(()) }
    fn last_refreshed(&self) -> SystemTime { SystemTime::now() }
    fn is_stale(&self, _max_age: Duration) -> bool { false }
}
