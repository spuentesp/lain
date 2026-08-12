use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::{LocalCloneSource, RepoSource, ShallowCloneSource, WorkspaceDirSource};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct FederationConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_max_concurrent_indexers")]
    pub max_concurrent_indexers: usize,
    #[serde(default = "default_ready_threshold")]
    pub ready_threshold: f32,
    pub repos: Vec<RepoConfig>,
}

fn default_data_dir() -> PathBuf { PathBuf::from("./.lain/federation") }
fn default_max_concurrent_indexers() -> usize { 8 }
fn default_ready_threshold() -> f32 { 0.8 }

#[derive(Debug, Clone, Deserialize)]
pub struct RepoConfig {
    pub id: String,
    pub source: SourceConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    LocalClone { url: String, #[serde(default = "default_ref")] r#ref: String },
    ShallowClone { url: String, #[serde(default = "default_ref")] r#ref: String, #[serde(default = "default_refresh_interval_secs")] refresh_interval_secs: u64 },
    WorkspaceDir { path: PathBuf },
}

fn default_ref() -> String { "main".into() }
fn default_refresh_interval_secs() -> u64 { 300 }

impl FederationConfig {
    pub fn load(path: &Path) -> Result<Self, LainError> {
        let s = std::fs::read_to_string(path).map_err(|e| LainError::Io(format!("read config: {e}")))?;
        Self::load_from_str(&s)
    }
    pub fn load_from_str(s: &str) -> Result<Self, LainError> {
        serde_yaml::from_str(s).map_err(|e| LainError::Config(format!("yaml: {e}")))
    }
    pub fn build_sources(&self) -> Result<Vec<Box<dyn RepoSource>>, LainError> {
        let mut out = Vec::with_capacity(self.repos.len());
        for r in &self.repos {
            out.push(self.build_source_for(r)?);
        }
        Ok(out)
    }

    /// Build a single `RepoSource` from one `RepoConfig`. Pulled out of
    /// `build_sources` so the workspace loader can construct one source
    /// at a time as it iterates the workspace's filtered member set.
    pub fn build_source_for(&self, repo: &RepoConfig) -> Result<Box<dyn RepoSource>, LainError> {
        let id = RepoId::new(&repo.id)?;
        let src: Box<dyn RepoSource> = match &repo.source {
            SourceConfig::LocalClone { url, r#ref } => Box::new(LocalCloneSource::new(id, url, r#ref, self.data_dir.join(&repo.id))?),
            SourceConfig::ShallowClone { url, r#ref, refresh_interval_secs } => Box::new(ShallowCloneSource::new(id, url, r#ref, self.data_dir.join(&repo.id), Duration::from_secs(*refresh_interval_secs))?),
            SourceConfig::WorkspaceDir { path } => Box::new(WorkspaceDirSource::new(id, path.clone())?),
        };
        Ok(src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let yaml = r#"
data_dir: /var/lib/lain
max_concurrent_indexers: 4
ready_threshold: 0.8
repos:
  - id: a
    source:
      type: workspace_dir
      path: /srv/a
  - id: b
    source:
      type: local_clone
      url: https://example.com/b.git
      ref: main
"#;
        let cfg: FederationConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(cfg.max_concurrent_indexers, 4);
    }

    #[test]
    fn build_sources_returns_correct_impls() {
        let yaml = r#"
data_dir: /tmp
repos:
  - id: ws
    source: { type: workspace_dir, path: /srv/ws }
  - id: lc
    source: { type: local_clone, url: "https://example.com/lc.git", ref: main }
"#;
        let cfg = FederationConfig::load_from_str(yaml).unwrap();
        let sources = cfg.build_sources().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id().as_str(), "ws");
        assert_eq!(sources[1].id().as_str(), "lc");
    }

    #[test]
    fn rejects_unknown_source_type() {
        let yaml = r#"
data_dir: /tmp
repos:
  - id: x
    source: { type: nonsense }
"#;
        assert!(FederationConfig::load_from_str(yaml).is_err());
    }
}
