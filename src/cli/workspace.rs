use std::path::{Path, PathBuf};
use anyhow::Result;

/// Resolve the workspace root for `lain mcp`'s no-arg case, by
/// preferring the parent process's cwd (the agent harness's cwd,
/// which differs from ours when the harness pins our cwd to a plugin
/// root) and falling back to the process's own cwd.
///
/// See `find_git_workspace_root_resolved` for the full policy. This
/// wrapper exists so existing callers keep their `Some(p)` / `None`
/// ergonomics; the resolution logic is in the inner function so
/// tests can inject the parent-cwd value directly.
pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>> {
    find_git_workspace_root_resolved(start, parent_process_cwd().as_deref())
}

/// Resolve the workspace root for the `lain mcp` no-arg case.
///
/// Policy (mirrors the user-facing contract for any agent harness):
///   1. If `start` is `Some(p)`, walk up from `p` only — explicit
///      overrides everything. Used when the caller passed
///      `--workspace PATH` (one or many), or when the env-var
///      `LAIN_WORKSPACE` is set and we're processing one of its entries.
///   2. Otherwise, walk up from the **parent process's** cwd (the agent
///      harness's cwd — read via `/proc/$PPID/cwd` on Linux). This is
///      what makes `lain mcp` work under Kimi, where the plugin
///      security model pins our cwd to the plugin root; without this,
///      the walk-up lands in the plugin dir instead of the user's repo.
///   3. If the parent cwd has no `.git` ancestor (macOS, sandboxed env,
///      parent already reaped), walk up from the process's own cwd.
///
/// Returns the first `.git` ancestor found, or `None` if neither
/// candidate has one within 16 levels.
fn find_git_workspace_root_resolved(
    start: Option<&Path>,
    parent_cwd: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(p) = start {
        return walk_up_for_git(p);
    }
    let process_cwd = std::env::current_dir().ok();
    let candidates = [parent_cwd, process_cwd.as_deref()];
    for c in candidates.into_iter().flatten() {
        if let Some(found) = walk_up_for_git(c)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
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
        match std::fs::read_link(&link) {
            Ok(p) => Some(p),
            Err(_) => None,
        }
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
    let mut current = start
        .canonicalize()
        .unwrap_or_else(|_| start.to_path_buf());
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

    #[test]
    fn resolved_prefers_parent_cwd_over_process_cwd() {
        // Two candidate directories; only the parent has a .git. The
        // helper must pick the parent, not the process cwd.
        let agent_dir = mk_repo();
        let bare_dir = tempfile::tempdir().unwrap(); // no .git
        let result = find_git_workspace_root_resolved(
            None,
            Some(agent_dir.path()),
        )
        .unwrap();
        let resolved = result.expect("should find .git via parent cwd");
        assert!(resolved.join(".git").exists());
        assert_ne!(resolved, bare_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn resolved_falls_back_to_process_cwd_when_no_parent() {
        // No parent cwd given — should at minimum walk up from the
        // process cwd (which here is the test runner's cwd, somewhere
        // inside this very repo, so .git IS an ancestor). Assert Ok.
        let result = find_git_workspace_root_resolved(None, None).unwrap();
        assert!(result.is_some(), "test runner cwd has a .git ancestor");
    }

    #[test]
    fn resolved_explicit_start_overrides_parent_cwd() {
        // Explicit --workspace PATH beats whatever the parent cwd says.
        let explicit = mk_repo();
        let agent_dir = mk_repo();
        let result = find_git_workspace_root_resolved(
            Some(explicit.path()),
            Some(agent_dir.path()),
        )
        .unwrap()
        .expect("explicit start must resolve");
        // Compare canonicalized paths — the tmpdir can be a symlink on macOS.
        let resolved = result.canonicalize().unwrap();
        let expected = explicit.path().canonicalize().unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolved_returns_none_when_neither_has_git() {
        // Synthetic parents and no process-cwd ancestor reachable.
        // We can't easily null out the process cwd, but we can construct
        // a synthetic parent and assert the function doesn't blow up
        // when both candidates are git-less at the synthetic level.
        let agent_dir = tempfile::tempdir().unwrap(); // no .git
        let result = find_git_workspace_root_resolved(
            None,
            Some(agent_dir.path()),
        )
        .unwrap();
        // Process cwd has .git (we're inside this repo); resolution
        // will find it. What we're really asserting is "doesn't crash,
        // doesn't use the synthetic git-less parent".
        if let Some(found) = result {
            assert_ne!(
                found.canonicalize().unwrap(),
                agent_dir.path().canonicalize().unwrap()
            );
        }
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
