//! Workspace types and parsing.
//!
//! A workspace is a named subset of repos declared in `repos.yaml`. Operators
//! declare workspaces in `workspaces.yaml`; the federation server loads only
//! the workspace's members when started with `--workspace <name>`.
//!
//! This module is intentionally narrow: data types + parse + validate + the
//! `WorkspaceSource` trait for fetching workspace definitions from disk or
//! git. The CLI subcommand, MCP tools, server flag wiring, and dashboard
//! live in their own modules — this is the shared data contract.

use crate::error::LainError;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A named group of repos that the federation engine loads as a coherent unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub source: Option<WorkspaceSourceConfig>,
    pub members: Vec<String>,
}

/// Where a workspace's `workspaces.yaml` (or its member declarations) live.
/// Mirrors the shape of `RepoSourceConfig` for repos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceSourceConfig {
    WorkspaceDir {
        path: PathBuf,
    },
    WorkspaceClone {
        url: String,
        #[serde(default)]
        ref_: Option<String>,
        #[serde(default)]
        refresh_interval_secs: Option<u64>,
    },
}

/// Top-level structure of `workspaces.yaml`. Multiple workspaces in one file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspacesFile {
    /// Optional name of the workspace to activate by default
    /// (`lain workspaces use` without args, or `--workspace auto`).
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSpec>,
}

impl WorkspacesFile {
    /// Load and validate a `workspaces.yaml` from disk.
    pub fn load(path: &Path) -> Result<Self, LainError> {
        let text = std::fs::read_to_string(path).map_err(|e| LainError::Io(e.to_string()))?;
        let file: WorkspacesFile =
            serde_yaml::from_str(&text).map_err(|e| LainError::Config(format!("workspaces.yaml: {e}")))?;
        file.validate()?;
        Ok(file)
    }

    /// Validate structural invariants: unique workspace names, ≥2 members per
    /// workspace, valid repo id characters, default workspace exists if set.
    pub fn validate(&self) -> Result<(), LainError> {
        let mut seen_names = std::collections::HashSet::new();
        for ws in &self.workspaces {
            if !seen_names.insert(&ws.name) {
                return Err(LainError::Config(format!(
                    "duplicate workspace name '{name}'",
                    name = ws.name
                )));
            }
            if ws.members.len() < 2 {
                return Err(LainError::Config(format!(
                    "workspace '{name}' must contain >= 2 repos; got {n}",
                    name = ws.name,
                    n = ws.members.len()
                )));
            }
            for m in &ws.members {
                if m.is_empty() || m.contains(':') || m.contains('/') {
                    return Err(LainError::Config(format!(
                        "workspace '{ws_name}' contains invalid repo id '{member}'",
                        ws_name = ws.name,
                        member = m
                    )));
                }
            }
        }
        if let Some(default_name) = &self.default {
            if !self.workspaces.iter().any(|w| &w.name == default_name) {
                return Err(LainError::Config(format!(
                    "default workspace '{default_name}' not found in workspaces list"
                )));
            }
        }
        Ok(())
    }
}

/// Stable, lowercase kind label for workspace sources. Used as
/// `source_kind` in observability output. The set of returned values is
/// closed: any new source type must add a new label here AND a new
/// `WorkspaceSourceConfig` variant above so the schema stays in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSourceKind {
    WorkspaceDir,
    WorkspaceClone,
}

impl std::fmt::Display for WorkspaceSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceDir => write!(f, "workspace_dir"),
            Self::WorkspaceClone => write!(f, "workspace_clone"),
        }
    }
}

/// Mirror of `RepoSource` for workspace definitions. Same shape, separate
/// trait so callers can be explicit about which subsystem they're driving.
#[async_trait]
pub trait WorkspaceSource: Send + Sync {
    fn id(&self) -> &str;
    fn local_path(&self) -> &Path;
    fn kind(&self) -> WorkspaceSourceKind;
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    fn is_stale(&self, max_age: Duration) -> bool;
}

/// Back-compat source: workspace definition lives on disk at a known path.
/// No git ops — the path is the source of truth.
pub struct WorkspaceDirSource {
    id: String,
    path: PathBuf,
    last_refreshed: Arc<RwLock<SystemTime>>,
}

impl WorkspaceDirSource {
    pub fn new(id: String, path: PathBuf) -> Result<Self, LainError> {
        if id.is_empty() {
            return Err(LainError::Config("WorkspaceDirSource id cannot be empty".into()));
        }
        if path.as_os_str().is_empty() {
            return Err(LainError::Config("WorkspaceDirSource path cannot be empty".into()));
        }
        // We don't require the path to exist at construction time — the
        // workspace may be defined in a workspaces.yaml that points at a
        // not-yet-created directory. The validation happens in `fetch`.
        Ok(Self {
            id,
            path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::now())),
        })
    }
}

#[async_trait]
impl WorkspaceSource for WorkspaceDirSource {
    fn id(&self) -> &str { &self.id }
    fn local_path(&self) -> &Path { &self.path }
    fn kind(&self) -> WorkspaceSourceKind { WorkspaceSourceKind::WorkspaceDir }
    async fn fetch(&self) -> Result<(), LainError> {
        if !self.path.is_dir() {
            return Err(LainError::Config(format!(
                "workspace_dir path does not exist or is not a directory: {}",
                self.path.display()
            )));
        }
        *self.last_refreshed.write() = SystemTime::now();
        Ok(())
    }
    fn last_refreshed(&self) -> SystemTime { *self.last_refreshed.read() }
    fn is_stale(&self, _max_age: Duration) -> bool { false }
}

/// Git-backed source: clone (or refresh) a workspace definition repo.
/// Like `ShallowCloneSource` for repos.
pub struct WorkspaceCloneSource {
    id: String,
    url: String,
    git_ref: String,
    refresh_interval: Duration,
    local_path: PathBuf,
    last_refreshed: Arc<RwLock<SystemTime>>,
}

impl WorkspaceCloneSource {
    pub fn new(
        id: String,
        url: String,
        git_ref: Option<String>,
        refresh_interval_secs: Option<u64>,
        local_root: PathBuf,
    ) -> Result<Self, LainError> {
        if id.is_empty() {
            return Err(LainError::Config("WorkspaceCloneSource id cannot be empty".into()));
        }
        if url.is_empty() {
            return Err(LainError::Config("WorkspaceCloneSource url cannot be empty".into()));
        }
        let git_ref = git_ref.unwrap_or_else(|| "main".to_string());
        let refresh_interval = Duration::from_secs(refresh_interval_secs.unwrap_or(300));
        let local_path = local_root.join("workspaces").join(&id);
        Ok(Self {
            id,
            url,
            git_ref,
            refresh_interval,
            local_path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        })
    }

    pub fn refresh_interval(&self) -> Duration { self.refresh_interval }
    pub fn url(&self) -> &str { &self.url }
    pub fn git_ref(&self) -> &str { &self.git_ref }
}

#[async_trait]
impl WorkspaceSource for WorkspaceCloneSource {
    fn id(&self) -> &str { &self.id }
    fn local_path(&self) -> &Path { &self.local_path }
    fn kind(&self) -> WorkspaceSourceKind { WorkspaceSourceKind::WorkspaceClone }
    async fn fetch(&self) -> Result<(), LainError> {
        use std::process::Command;
        let path = self.local_path.clone();
        let url = self.url.clone();
        let git_ref = self.git_ref.clone();
        let last_refreshed = self.last_refreshed.clone();
        let git_dir = path.join(".git");
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            if !git_dir.exists() {
                let parent = path.parent().ok_or_else(|| LainError::Config("workspace_clone local_path has no parent".into()))?;
                std::fs::create_dir_all(parent).map_err(|e| LainError::Io(e.to_string()))?;
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
    fn last_refreshed(&self) -> SystemTime { *self.last_refreshed.read() }
    fn is_stale(&self, max_age: Duration) -> bool {
        self.last_refreshed().elapsed().map(|e| e > max_age).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_workspaces_yaml() {
        let yaml = "\
workspaces:
  - name: backend-team
    members: [auth-svc, billing-svc]
";
        let f: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.workspaces.len(), 1);
        assert_eq!(f.workspaces[0].name, "backend-team");
        assert_eq!(f.workspaces[0].members, vec!["auth-svc", "billing-svc"]);
    }

    #[test]
    fn parses_with_default_and_description() {
        let yaml = "\
default: backend-team
workspaces:
  - name: backend-team
    description: Core backend services
    members: [auth-svc, billing-svc, db-client]
";
        let f: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.default.as_deref(), Some("backend-team"));
        assert_eq!(f.workspaces[0].description.as_deref(), Some("Core backend services"));
        assert_eq!(f.workspaces[0].members.len(), 3);
    }

    #[test]
    fn parses_workspace_source_config() {
        let yaml = "\
workspaces:
  - name: payments-ws
    members: [payments, billing]
    source:
      type: workspace_clone
      url: https://github.com/acme/payments-ws.git
      ref_: main
      refresh_interval_secs: 600
";
        let f: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
        match &f.workspaces[0].source {
            Some(WorkspaceSourceConfig::WorkspaceClone { url, ref_, refresh_interval_secs }) => {
                assert_eq!(url, "https://github.com/acme/payments-ws.git");
                assert_eq!(ref_.as_deref(), Some("main"));
                assert_eq!(*refresh_interval_secs, Some(600));
            }
            other => panic!("expected WorkspaceClone, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_sub_two_members() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![WorkspaceSpec {
                name: "tiny".into(),
                description: None,
                source: None,
                members: vec!["only".into()],
            }],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_invalid_repo_id_chars() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![WorkspaceSpec {
                name: "ws".into(),
                description: None,
                source: None,
                members: vec!["ok".into(), "bad/id".into()],
            }],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)));
    }

    #[test]
    fn validate_rejects_duplicate_workspace_names() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "dup".into(),
                    description: None,
                    source: None,
                    members: vec!["a".into(), "b".into()],
                },
                WorkspaceSpec {
                    name: "dup".into(),
                    description: None,
                    source: None,
                    members: vec!["c".into(), "d".into()],
                },
            ],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)));
    }

    #[test]
    fn validate_rejects_default_not_in_list() {
        let f = WorkspacesFile {
            default: Some("nonexistent".into()),
            workspaces: vec![WorkspaceSpec {
                name: "real".into(),
                description: None,
                source: None,
                members: vec!["a".into(), "b".into()],
            }],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)));
    }

    #[test]
    fn validate_accepts_empty_workspaces_file() {
        // Useful when an operator hasn't created any workspaces yet.
        let f = WorkspacesFile::default();
        assert!(f.validate().is_ok());
    }

    #[test]
    fn workspace_dir_source_kind_label_matches_config() {
        // The kind label is part of the public contract — the source kind
        // string in the config must match what `.kind()` returns.
        let s = WorkspaceDirSource::new("team".into(), PathBuf::from("/tmp/whatever")).unwrap();
        assert_eq!(s.kind(), WorkspaceSourceKind::WorkspaceDir);
        assert_eq!(format!("{}", s.kind()), "workspace_dir");
    }

    #[test]
    fn workspace_dir_source_rejects_empty_id() {
        let r = WorkspaceDirSource::new("".into(), PathBuf::from("/tmp"));
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn workspace_dir_source_fetch_succeeds_when_path_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let s = WorkspaceDirSource::new("team".into(), tmp.path().to_path_buf()).unwrap();
        s.fetch().await.expect("fetch on existing dir should succeed");
    }

    #[tokio::test]
    async fn workspace_dir_source_fetch_fails_when_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let s = WorkspaceDirSource::new("team".into(), missing).unwrap();
        let r = s.fetch().await;
        assert!(r.is_err(), "expected fetch on missing path to fail");
    }

    #[test]
    fn workspace_clone_source_defaults_main_and_300s() {
        let s = WorkspaceCloneSource::new(
            "team".into(),
            "https://example.com/repo.git".into(),
            None,
            None,
            PathBuf::from("/tmp"),
        ).unwrap();
        assert_eq!(s.git_ref(), "main");
        assert_eq!(s.refresh_interval(), Duration::from_secs(300));
        assert_eq!(format!("{}", s.kind()), "workspace_clone");
    }

    #[test]
    fn workspace_clone_source_rejects_empty_url() {
        let r = WorkspaceCloneSource::new(
            "team".into(),
            "".into(),
            None,
            None,
            PathBuf::from("/tmp"),
        );
        assert!(r.is_err());
    }
}