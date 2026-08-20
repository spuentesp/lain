//! `list_recent_projects` and the `RecentProjectEntry` DTO it returns.
//! Each entry combines a recent-projects record with live repo/workspace
//! counts from the referenced `repos.yaml` (and `workspaces.yaml` next
//! to it).

use super::dto::RecentProjectEntry;
use crate::error::LainError;
use std::path::{Path, PathBuf};

/// Compute the workspace + repo counts for a recent project entry
/// based on its `repos.yaml` / `workspaces.yaml` paths. Failures
/// (missing files, parse errors) collapse to zero counts so a single
/// broken entry never blocks the whole list.
fn counts_for_project(repos_yaml: &Path) -> (usize, usize) {
    let cfg = crate::federation::config::FederationConfig::load(repos_yaml).ok();
    let repo_count = cfg.as_ref().map(|c| c.repos.len()).unwrap_or(0);
    let ws_path = repos_yaml
        .parent()
        .map(|p| p.join("workspaces.yaml"));
    let workspace_count = ws_path
        .as_ref()
        .and_then(|p| crate::federation::workspace::WorkspacesFile::load(p).ok())
        .map(|w| w.workspaces.len())
        .unwrap_or(0);
    (workspace_count, repo_count)
}

/// Build the `list_recent_projects` response. Each entry combines a
/// recent-projects record with live repo/workspace counts from the
/// referenced `repos.yaml` (and `workspaces.yaml` next to it).
pub fn list_recent_projects() -> Result<Vec<RecentProjectEntry>, LainError> {
    let raw = crate::config::recent_projects::list()
        .map_err(|e| LainError::Other(format!("recent_projects::list: {e}")))?;
    // The active workspace pointer is global (one per user); only the
    // entry whose `path` matches the pointer's `config_path` sees a
    // name. This is best-effort — if the file is missing or malformed
    // we just leave every entry's `active_workspace` as `None`.
    let active = crate::state::ActiveWorkspace::load().ok().flatten();
    Ok(raw
        .into_iter()
        .map(|r| {
            let (workspace_count, repo_count) = counts_for_project(&r.path);
            let active_workspace = active
                .as_ref()
                .and_then(|a| {
                    a.config_path
                        .as_ref()
                        .filter(|p| **p == r.path)
                        .map(|_| a.name.clone())
                });
            RecentProjectEntry {
                path: r.path,
                last_used: r.last_used,
                workspace_count,
                repo_count,
                active_workspace,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a `repos.yaml` + optional `workspaces.yaml` next to each
    /// other under `tmp`. Returns the path to `repos.yaml`.
    fn write_project(
        tmp: &std::path::Path,
        name: &str,
        repos: &[&str],
        workspaces: &[(&str, &[&str])],
    ) -> PathBuf {
        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let repos_path = dir.join("repos.yaml");
        let mut yaml = String::from("repos:\n");
        for r in repos {
            yaml.push_str(&format!(
                "  - id: {r}\n    source: {{ type: workspace_dir, path: /srv/{r} }}\n"
            ));
        }
        std::fs::write(&repos_path, yaml).unwrap();
        if !workspaces.is_empty() {
            let mut ws_yaml = String::from("workspaces:\n");
            for (n, members) in workspaces {
                let members_list = members.join(", ");
                ws_yaml.push_str(&format!("  - name: {n}\n    members: [{members_list}]\n"));
            }
            std::fs::write(dir.join("workspaces.yaml"), ws_yaml).unwrap();
        }
        repos_path
    }

    /// Redirect the recent-projects file to a tempdir for the duration
    /// of the test. Uses `_in` variants so we don't race with other
    /// tests that may set `XDG_CONFIG_HOME`.
    fn with_temp_recent<F: FnOnce(&std::path::Path)>(f: F) {
        let boxed: Box<dyn FnOnce(&std::path::Path)> = Box::new(f);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boxed(&path)));
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// RAII guard that points `XDG_CONFIG_HOME` at a tempdir for the
    /// duration of a test, restoring the previous value on drop. Used
    /// only by the production end-to-end test below; the other tests
    /// in this module prefer the `_in` helpers which don't touch the
    /// env var at all.
    struct XdgGuard {
        prev: Option<String>,
    }

    impl XdgGuard {
        fn new(dir: &std::path::Path) -> Self {
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

    #[test]
    fn list_recent_projects_returns_empty_when_file_missing() {
        with_temp_recent(|dir| {
            let list = crate::config::recent_projects::list_in(dir).unwrap();
            assert!(list.is_empty());
        });
    }

    #[test]
    fn list_recent_projects_enriches_with_counts() {
        with_temp_recent(|dir| {
            let a = write_project(dir, "a", &["r1", "r2", "r3"], &[("team", &["r1", "r2"])]);
            let b = write_project(dir, "b", &["r4"], &[]);
            crate::config::recent_projects::record_in(dir, &a).unwrap();
            crate::config::recent_projects::record_in(dir, &b).unwrap();

            let (ws_count_a, repo_count_a) = counts_for_project(&a);
            assert_eq!(repo_count_a, 3);
            assert_eq!(ws_count_a, 1);
            let (ws_count_b, repo_count_b) = counts_for_project(&b);
            assert_eq!(repo_count_b, 1);
            assert_eq!(ws_count_b, 0);
        });
    }

    #[test]
    fn counts_for_project_zeros_when_file_missing() {
        let bogus = PathBuf::from("/nonexistent/repos.yaml");
        let (ws, repo) = counts_for_project(&bogus);
        assert_eq!(ws, 0);
        assert_eq!(repo, 0);
    }

    /// End-to-end test for the production `list_recent_projects()`
    /// function. Exercises the full chain — `record()` writes through
    /// `config_dir()`, which reads `XDG_CONFIG_HOME`, so we point
    /// `XDG_CONFIG_HOME` at a tempdir.
    #[test]
    fn list_recent_projects_production_end_to_end() {
        let _g = crate::state::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());

        let a = write_project(
            tmp.path(),
            "a",
            &["r1", "r2", "r3"],
            &[("team", &["r1", "r2"])],
        );
        let b = write_project(tmp.path(), "b", &["r4"], &[]);

        crate::config::recent_projects::record(&a).unwrap();
        crate::config::recent_projects::record(&b).unwrap();

        let entries = list_recent_projects().expect("list_recent_projects");
        assert_eq!(
            entries.len(),
            2,
            "expected 2 recent projects, got {:?}",
            entries.iter().map(|e| &e.path).collect::<Vec<_>>()
        );

        assert_eq!(entries[0].path, b);
        assert_eq!(entries[1].path, a);

        assert_eq!(entries[0].workspace_count, 0);
        assert_eq!(entries[0].repo_count, 1);
        assert_eq!(entries[1].workspace_count, 1);
        assert_eq!(entries[1].repo_count, 3);

        assert!(entries[0].last_used > 0);
        assert!(entries[1].last_used > 0);
        assert!(entries[0].last_used >= entries[1].last_used);

        let json = serde_json::to_value(&entries).expect("serialize entries");
        let arr = json.as_array().expect("entries serialize as JSON array");
        assert_eq!(arr.len(), 2);
        for (i, item) in arr.iter().enumerate() {
            let obj = item.as_object().expect("each entry is a JSON object");
            for field in ["path", "last_used", "workspace_count", "repo_count", "active_workspace"] {
                assert!(
                    obj.contains_key(field),
                    "entry[{i}] missing required JSON field `{field}`; got keys {:?}",
                    obj.keys().collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(arr[0]["path"], serde_json::json!(b.to_string_lossy()));
        assert_eq!(arr[0]["workspace_count"], serde_json::json!(0));
        assert_eq!(arr[0]["repo_count"], serde_json::json!(1));
        assert_eq!(arr[1]["path"], serde_json::json!(a.to_string_lossy()));
        assert_eq!(arr[1]["workspace_count"], serde_json::json!(1));
        assert_eq!(arr[1]["repo_count"], serde_json::json!(3));
        assert!(arr[0]["last_used"].as_i64().unwrap() > 0);
        assert!(arr[1]["last_used"].as_i64().unwrap() > 0);
        assert!(arr[0]["active_workspace"].is_null());
        assert!(arr[1]["active_workspace"].is_null());
    }

    #[test]
    fn list_recent_projects_surfaces_active_workspace_for_matching_path() {
        let _g = crate::state::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());

        let a = write_project(tmp.path(), "a", &["r1", "r2"], &[("team", &["r1", "r2"])]);
        let b = write_project(tmp.path(), "b", &["r3"], &[]);
        crate::config::recent_projects::record(&a).unwrap();
        crate::config::recent_projects::record(&b).unwrap();

        let lain_dir = tmp.path().join("lain");
        std::fs::create_dir_all(&lain_dir).unwrap();
        std::fs::write(
            lain_dir.join("active_workspace"),
            format!("{}\n{}\n", a.display(), "team"),
        )
        .unwrap();

        let entries = list_recent_projects().expect("list_recent_projects");
        assert_eq!(entries.len(), 2);
        let entry_a = entries.iter().find(|e| e.path == a).expect("entry a");
        let entry_b = entries.iter().find(|e| e.path == b).expect("entry b");
        assert_eq!(entry_a.active_workspace.as_deref(), Some("team"));
        assert!(entry_b.active_workspace.is_none());
    }
}
