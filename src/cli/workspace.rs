use anyhow::Result;
use std::path::{Path, PathBuf};

/// Resolve the workspace root for `lain mcp`'s no-arg case. See
/// `find_git_workspace_root_resolved` for the policy; this wrapper
/// supplies the real process and parent-process working directories.
pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>> {
    find_git_workspace_root_resolved(
        start,
        std::env::current_dir().ok().as_deref(),
        parent_process_cwd().as_deref(),
    )
}

/// Resolve the workspace root for the `lain mcp` no-arg case.
///
/// Policy:
///   1. If `start` is `Some(p)`, walk up from `p` only — explicit
///      overrides everything (`--workspace PATH`, `LAIN_WORKSPACE`).
///   2. Otherwise, the process's **own** cwd: the directory the host
///      launched us in is the project it means. Every config `lain setup`
///      writes is a bare `lain mcp`, so a host that starts one server per
///      project by setting the child's cwd depends on this.
///   3. Otherwise the **parent** process's cwd (`/proc/$PPID/cwd` on
///      Linux) — for hosts that pin our cwd somewhere else. Kimi runs
///      plugin servers in the plugin's own directory, recognised by its
///      `kimi.plugin.json`; that directory is skipped in step 2.
///
/// Either candidate is skipped when the git root it resolves to contains
/// the running `lain` binary: that is the dev/test runner (`cargo test`,
/// `cargo run`), whose cwd is Lain's own source tree.
///
/// The parent's cwd used to come first, so a host that set our cwd to a
/// project but itself ran elsewhere got the wrong repository indexed.
fn find_git_workspace_root_resolved(
    start: Option<&Path>,
    process_cwd: Option<&Path>,
    parent_cwd: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(p) = start {
        return walk_up_for_git(p);
    }
    let own = process_cwd.filter(|c| !is_plugin_root(c));
    for c in [own, parent_cwd].into_iter().flatten() {
        if c.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = walk_up_for_git(c)? {
            if binary_lives_inside(&found) {
                continue;
            }
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// A directory an agent harness pins plugin servers to: Kimi's plugin
/// root holds `kimi.plugin.json`.
fn is_plugin_root(dir: &Path) -> bool {
    dir.join("kimi.plugin.json").is_file()
}

/// True when the running binary's canonical path is inside `root`
/// (which is expected to be a git workspace root). Returns `false`
/// on any IO error — the safe default is "we cannot prove the
/// binary is dev-env-resident, so behave as a normal client".
fn binary_lives_inside(root: &Path) -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let exe = match exe.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    exe.starts_with(&root)
}

/// Read the parent process's cwd via `/proc/$PPID/cwd`.
///
/// Linux only; returns `None` on every other platform, on permission
/// errors, or if the link is unreadable (sandboxed env, parent already
/// reaped). Callers fall back to the process's own cwd when this is
/// `None`.
pub fn parent_process_cwd() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let ppid = std::os::unix::process::parent_id();
        let link = format!("/proc/{ppid}/cwd");
        std::fs::read_link(&link).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Walk up from `start` until a directory containing `.git` is found,
/// or 16 levels are exhausted. Pure — no env, no /proc. Public so
/// tests and the resolver helper can call it directly.
pub fn walk_up_for_git(start: &Path) -> Result<Option<PathBuf>> {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    for _ in 0..16 {
        if current.join(".git").exists() {
            return Ok(Some(current));
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return Ok(None),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_some_when_dot_git_is_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let sub = root.join("src").join("nested");
        fs::create_dir_all(&sub).unwrap();
        let found = find_git_workspace_root(Some(&sub)).unwrap();
        // canonicalize normalizes /tmp -> /private/tmp on macOS; just
        // assert we walked up to *some* directory containing `.git`.
        assert!(found.unwrap().join(".git").exists());
    }

    #[test]
    fn returns_none_when_no_dot_git_within_16() {
        let tmp = tempfile::tempdir().unwrap();
        // No .git anywhere up the tempdir chain (tempdir parents don't
        // contain .git in practice; assert that explicitly.)
        let found = find_git_workspace_root(Some(tmp.path())).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn none_start_uses_current_dir() {
        // `find_git_workspace_root(None)` now also considers the parent
        // process's cwd on Linux, but it must still return Ok in any
        // environment — the function never errors on a missing .git,
        // only on env-lookup failures.
        let result = find_git_workspace_root(None);
        assert!(result.is_ok());
    }

    // --- Agent-harness (parent-process-cwd) resolution tests ---

    fn mk_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        tmp
    }

    fn canon(p: &Path) -> PathBuf {
        p.canonicalize().unwrap()
    }

    #[test]
    fn resolved_prefers_process_cwd_over_parent_cwd() {
        let own = mk_repo();
        let parent = mk_repo();
        let found = find_git_workspace_root_resolved(None, Some(own.path()), Some(parent.path()))
            .unwrap()
            .expect("own cwd resolves");
        assert_eq!(canon(&found), canon(own.path()));
    }

    #[test]
    fn resolved_uses_parent_cwd_from_a_plugin_root() {
        // Kimi pins our cwd to the plugin directory — even when that is
        // itself a git checkout.
        let plugin = mk_repo();
        fs::write(plugin.path().join("kimi.plugin.json"), "{}").unwrap();
        let project = mk_repo();
        let found =
            find_git_workspace_root_resolved(None, Some(plugin.path()), Some(project.path()))
                .unwrap()
                .expect("parent cwd resolves");
        assert_eq!(canon(&found), canon(project.path()));
    }

    #[test]
    fn resolved_falls_back_to_parent_when_own_cwd_is_not_a_repo() {
        let bare = tempfile::tempdir().unwrap();
        let parent = mk_repo();
        let found = find_git_workspace_root_resolved(None, Some(bare.path()), Some(parent.path()))
            .unwrap()
            .expect("parent resolves");
        assert_eq!(canon(&found), canon(parent.path()));
    }

    #[test]
    fn resolved_explicit_start_overrides_both() {
        let explicit = mk_repo();
        let own = mk_repo();
        let parent = mk_repo();
        let found = find_git_workspace_root_resolved(
            Some(explicit.path()),
            Some(own.path()),
            Some(parent.path()),
        )
        .unwrap()
        .expect("explicit start must resolve");
        assert_eq!(canon(&found), canon(explicit.path()));
    }

    #[test]
    fn resolved_returns_none_when_neither_has_git() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Tempdirs can sit inside a checkout (a shared TMPDIR); only assert
        // when they are genuinely outside any repository.
        if walk_up_for_git(a.path()).unwrap().is_none()
            && walk_up_for_git(b.path()).unwrap().is_none()
        {
            assert!(
                find_git_workspace_root_resolved(None, Some(a.path()), Some(b.path()))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn resolved_skips_the_repository_holding_the_binary() {
        // `cargo test` / `cargo run`: the binary lives in Lain's own tree.
        let bin = canon(&std::env::current_exe().unwrap());
        let Some(lain_root) = bin.ancestors().find(|a| a.join(".git").exists()) else {
            return; // binary built outside any checkout
        };
        let fixture = mk_repo();
        let found = find_git_workspace_root_resolved(None, Some(lain_root), Some(fixture.path()))
            .unwrap()
            .expect("the fixture resolves");
        assert_eq!(canon(&found), canon(fixture.path()));
    }

    // --- parent_process_cwd direct tests (Linux only meaningful) ---

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_process_cwd_reads_proc_link() {
        // Inside `cargo test`, the test binary's parent is `cargo`,
        // whose cwd is the workspace root (wherever the user ran
        // cargo). The function should return Some(path), and that
        // path should exist as a directory.
        let result = parent_process_cwd();
        match result {
            Some(p) => assert!(p.is_dir() || p.is_symlink()),
            None => {
                // Sandbox or unusual env — acceptable, just note it.
                eprintln!("parent_process_cwd returned None (likely sandboxed)");
            }
        }
    }
}
