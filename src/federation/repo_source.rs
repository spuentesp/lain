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
