use std::path::{Path, PathBuf};
use anyhow::{Context, Result};

/// Walk up from `start` (defaulting to the current working directory)
/// until a directory containing `.git` is found, and return that
/// ancestor. Returns `Ok(None)` when no `.git` is found within 16
/// levels or the start path cannot be resolved.
///
/// `start = None` uses `std::env::current_dir()` and canonicalizes
/// it; `start = Some(p)` uses `p.canonicalize()` (falling back to
/// `p.to_path_buf()` on canonicalize failure, mirroring the prior
/// `hooks.rs::find_workspace_root` behavior so filesystem-only
/// paths under a not-yet-created worktree don't error out).
pub fn find_git_workspace_root(start: Option<&Path>) -> Result<Option<PathBuf>> {
    let mut current = match start {
        Some(p) => p
            .canonicalize()
            .unwrap_or_else(|_| p.to_path_buf()),
        None => std::env::current_dir()
            .context("get current dir")?
            .canonicalize()
            .context("canonicalize cwd")?,
    };
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
        // Cannot easily test the cwd path without mutating env, but we
        // can assert the call signature compiles and returns Ok.
        let result = find_git_workspace_root(None);
        assert!(result.is_ok());
    }
}
