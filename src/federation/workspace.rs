//! Workspace types and parsing.
//!
//! A workspace is a named subset of repos declared in `repos.yaml`. Operators
//! declare workspaces in `workspaces.yaml`; the federation server loads only
//! the workspace's members when started with `--workspace <name>`.
//!
//! This module is intentionally narrow: data types + parse + validate. The
//! CLI subcommand, MCP tools, server flag wiring, and dashboard live in
//! their own modules — this is the shared data contract.

use crate::error::LainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
}