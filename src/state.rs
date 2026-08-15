//! Lain runtime state.
//!
//! After the multi-project consolidation, the only runtime state lain keeps
//! outside a workspace is the active-federation-workspace pointer — the
//! `~/.config/lain/active_workspace` file that the hot-reload layer reads
//! to know which workspace's repos to load. There is no project registry;
//! operators point lain at a `repos.yaml` directly.
//!
//! File format (one or two lines):
//! - 1 line: `<workspace-name>`                        (legacy / no config path)
//! - 2 lines: `<config-path>\n<workspace-name>`        (multi-project layout)

use crate::config::config_dir;
use crate::server::error::LainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `~/.config/lain/active_workspace` — the pointer the operator writes via
/// `lain workspaces use <name>`. `config_path` is the `workspaces.yaml`
/// that the workspace was defined in; the server reads it from disk to
/// resolve the named workspace. `None` means the legacy one-line format
/// was used (no source path recorded).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveWorkspace {
    pub name: String,
    pub config_path: Option<PathBuf>,
}

fn active_workspace_file() -> PathBuf {
    config_dir().join("active_workspace")
}

impl ActiveWorkspace {
    /// Load the active workspace pointer from disk.
    ///
    /// Returns `Ok(None)` if the file does not exist (no workspace ever
    /// set). Two formats are supported:
    /// - 1 line: `<workspace-name>` — config_path is `None`.
    /// - 2 lines: `<config-path>\n<workspace-name>` — config_path is `Some`.
    pub fn load() -> Result<Option<Self>, LainError> {
        let path = active_workspace_file();
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| LainError::Io(e.to_string()))?;
        let mut lines = text.lines();
        let first = match lines.next() {
            Some(s) => s,
            None => return Ok(None),
        };
        match lines.next() {
            Some(name) => Ok(Some(ActiveWorkspace {
                name: name.to_string(),
                config_path: Some(PathBuf::from(first)),
            })),
            None => Ok(Some(ActiveWorkspace {
                name: first.to_string(),
                config_path: None,
            })),
        }
    }

    /// Save this pointer to disk atomically (write to .tmp, rename).
    pub fn save(&self) -> Result<(), LainError> {
        if self.name.is_empty() {
            return Err(LainError::Config("active workspace name cannot be empty".into()));
        }
        let dir = config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
        let path = active_workspace_file();
        let text = match &self.config_path {
            Some(p) => format!("{}\n{}\n", p.display(), self.name),
            None => format!("{}\n", self.name),
        };
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|e| LainError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| LainError::Io(e.to_string()))?;
        Ok(())
    }

    /// Remove the active workspace pointer file. No-op if it doesn't exist.
    pub fn clear() -> Result<(), LainError> {
        let path = active_workspace_file();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LainError::Io(e.to_string())),
        }
    }
}

/// Look up a workspace by name in a `WorkspacesFile`. Returns
/// `LainError::Config` with a clear message if the name is not present.
pub fn resolve_active_workspace<'a>(
    spec: &'a crate::federation::workspace::WorkspacesFile,
    name: &str,
) -> Result<&'a crate::federation::workspace::WorkspaceSpec, LainError> {
    spec.workspaces.iter()
        .find(|w| w.name == name)
        .ok_or_else(|| LainError::Config(format!(
            "workspace '{name}' not found in workspaces.yaml"
        )))
}

// Test-only mutex shared by all `mod tests` and `mod active_workspace_tests`
// blocks in this file. cargo test runs tests in parallel; XDG_CONFIG_HOME
// and cwd are process-global state, so parallel tests would stomp on each
// other without serialization. Defined at file scope so both test mods
// (which are sibling test sub-modules) can see it.
#[cfg(test)]
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod active_workspace_tests {
    use super::*;

    /// Run a closure with XDG_CONFIG_HOME pointed at a tempdir, restoring
    /// the original env var on drop. Used so tests don't touch the user's
    /// real `~/.config/lain/active_workspace`.
    struct XdgGuard { prev: Option<String> }
    impl XdgGuard {
        fn new(dir: &Path) -> Self {
            let prev = std::env::var("XDG_CONFIG_HOME").ok();
            std::env::set_var("XDG_CONFIG_HOME", dir);
            Self { prev }
        }
    }
    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    /// Write the file directly into the effective config dir so it's
    /// picked up by `config_dir()`. `config_dir()` appends `lain` to
    /// `XDG_CONFIG_HOME`, so the file lives at `<xdg>/lain/active_workspace`.
    fn write_active_workspace(xdg: &Path, body: &str) {
        let dir = xdg.join("lain");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("active_workspace"), body).unwrap();
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let r = ActiveWorkspace::load().expect("load should not error on missing file");
        assert!(r.is_none());
    }

    #[test]
    fn active_workspace_loads_two_line_format() {
        // Two-line format: <config-path>\n<name>. config_path is the
        // workspaces.yaml this workspace was sourced from; name is the
        // workspace's logical name.
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        write_active_workspace(tmp.path(), "/srv/workspaces.yaml\nbackend-team\n");
        let aw = ActiveWorkspace::load().unwrap().expect("load should return Some");
        assert_eq!(aw.name, "backend-team");
        assert_eq!(aw.config_path, Some(PathBuf::from("/srv/workspaces.yaml")));
    }

    #[test]
    fn load_returns_one_line_format_as_name_only() {
        // Legacy / simple case: file contains just a name. config_path is None.
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        write_active_workspace(tmp.path(), "just-a-name\n");
        let aw = ActiveWorkspace::load().unwrap().expect("load should return Some");
        assert_eq!(aw.name, "just-a-name");
        assert_eq!(aw.config_path, None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let aw = ActiveWorkspace {
            name: "backend-team".into(),
            config_path: Some(PathBuf::from("/srv/workspaces.yaml")),
        };
        aw.save().expect("save should succeed");
        let loaded = ActiveWorkspace::load().unwrap().expect("load should return Some");
        assert_eq!(loaded, aw);
    }

    #[test]
    fn clear_removes_the_file() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let aw = ActiveWorkspace {
            name: "team".into(),
            config_path: Some(PathBuf::from("/srv/ws.yaml")),
        };
        aw.save().unwrap();
        assert!(active_workspace_file().exists());
        ActiveWorkspace::clear().unwrap();
        assert!(!active_workspace_file().exists());
        // Idempotent: clear on missing file is OK.
        ActiveWorkspace::clear().unwrap();
    }

    #[test]
    fn resolve_active_workspace_finds_known() {
        let f = crate::federation::workspace::WorkspacesFile {
            default: None,
            workspaces: vec![
                crate::federation::workspace::WorkspaceSpec {
                    name: "alpha".into(),
                    description: None,
                    source: None,
                    members: vec!["a".into(), "b".into()],
                },
                crate::federation::workspace::WorkspaceSpec {
                    name: "beta".into(),
                    description: None,
                    source: None,
                    members: vec!["c".into(), "d".into()],
                },
            ],
        };
        let ws = resolve_active_workspace(&f, "beta").unwrap();
        assert_eq!(ws.name, "beta");
    }

    #[test]
    fn resolve_active_workspace_errors_on_unknown() {
        let f = crate::federation::workspace::WorkspacesFile::default();
        let r = resolve_active_workspace(&f, "ghost");
        assert!(r.is_err());
    }
}
