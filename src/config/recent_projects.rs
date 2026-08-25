//! Recently-used project tracking.
//!
//! Each `lain server` invocation records the path of the `repos.yaml` it
//! was launched with in `~/.config/lain/recent_projects.json`. The
//! `list_recent_projects` MCP tool reads this file so the dashboard's
//! project switcher can surface past projects without asking the
//! operator to re-type paths.
//!
//! Format: a JSON array of `RecentProject { path, last_used }`, sorted
//! most-recent-first. Capped at 20 entries so the file stays small even
//! after years of use.
//!
//! The on-disk file is intentionally placed under the standard config
//! dir (`config_dir()`) so it follows `XDG_CONFIG_HOME` and matches the
//! `active_workspace` pointer next to it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "recent_projects.json";
const MAX_ENTRIES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentProject {
    pub path: PathBuf,
    /// Seconds since the UNIX epoch when this project was last used.
    pub last_used: i64,
}

fn file_path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Record `project_path` as the most-recently-used project. Removes any
/// existing entry for the same path so the new entry bumps it back to
/// the top, then truncates to `MAX_ENTRIES`.
pub fn record(project_path: &Path) -> Result<()> {
    record_in(&crate::config::config_dir(), project_path)
}

/// Like `record`, but writes to a caller-supplied directory. Used by the
/// tests to point at a tempdir without racing on `XDG_CONFIG_HOME`.
pub fn record_in(dir: &Path, project_path: &Path) -> Result<()> {
    let path = file_path_in(dir);
    let mut list = list_in(dir).unwrap_or_default();
    list.retain(|r| r.path != project_path);
    list.insert(
        0,
        RecentProject {
            path: project_path.to_path_buf(),
            last_used: crate::server::time::now_unix(),
        },
    );
    list.truncate(MAX_ENTRIES);

    std::fs::create_dir_all(dir)
        .with_context(|| format!("create config dir {}", dir.display()))?;
    let text = serde_json::to_string_pretty(&list)
        .context("serialize recent_projects.json")?;
    std::fs::write(&path, text)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Read the recent-projects file. Returns an empty vec when the file
/// doesn't exist yet (first run), and surfaces I/O / parse errors
/// otherwise so the caller can decide whether to fall back or fail.
pub fn list() -> Result<Vec<RecentProject>> {
    list_in(&crate::config::config_dir())
}

/// Like `list`, but reads from a caller-supplied directory.
pub fn list_in(dir: &Path) -> Result<Vec<RecentProject>> {
    let path = file_path_in(dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<RecentProject> = serde_json::from_str(&text)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    // The record/list functions take a directory argument in the `_in`
    // variants, so each test points at a distinct tempdir and the suite
    // remains safe under cargo's default parallel test runner.

    #[test]
    fn list_in_returns_empty_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let list = list_in(tmp.path()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn record_in_then_list_in_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a").join("repos.yaml");
        let b = tmp.path().join("b").join("repos.yaml");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();

        record_in(tmp.path(), &a).unwrap();
        record_in(tmp.path(), &b).unwrap();

        let list = list_in(tmp.path()).unwrap();
        assert_eq!(list.len(), 2);
        // Most recent first.
        assert_eq!(list[0].path, b);
        assert_eq!(list[1].path, a);
        // Timestamps populated.
        assert!(list[0].last_used > 0);
        assert!(list[1].last_used > 0);
        assert!(list[0].last_used >= list[1].last_used);
    }

    #[test]
    fn record_in_dedupes_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("repos.yaml");
        std::fs::write(&a, "").unwrap();

        record_in(tmp.path(), &a).unwrap();
        record_in(tmp.path(), &a).unwrap();
        record_in(tmp.path(), &a).unwrap();

        let list = list_in(tmp.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, a);
    }

    #[test]
    fn record_in_bumps_existing_entry_to_top() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a").join("repos.yaml");
        let b = tmp.path().join("b").join("repos.yaml");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();

        record_in(tmp.path(), &a).unwrap();
        record_in(tmp.path(), &b).unwrap();
        // Touch `a` again — it should jump to position 0.
        record_in(tmp.path(), &a).unwrap();

        let list = list_in(tmp.path()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, a);
        assert_eq!(list[1].path, b);
    }

    #[test]
    fn record_in_caps_at_max_entries() {
        let tmp = tempfile::tempdir().unwrap();
        // Insert MAX_ENTRIES + 5 distinct paths; expect MAX_ENTRIES total
        // and the oldest entries dropped.
        for i in 0..(MAX_ENTRIES + 5) {
            let p = tmp.path().join(format!("p{i}")).join("repos.yaml");
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            record_in(tmp.path(), &p).unwrap();
        }
        let list = list_in(tmp.path()).unwrap();
        assert_eq!(list.len(), MAX_ENTRIES);
        // The most recently inserted (p24) should be at the top.
        let last = tmp.path().join(format!("p{}", MAX_ENTRIES + 4)).join("repos.yaml");
        assert_eq!(list[0].path, last);
    }
}
