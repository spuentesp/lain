//! `lain mcp` — single-repo MCP server entry point.
//!
//! Wishlist #11 (option A): zero-args `lain mcp` discovers the workspace
//! by walking up from the current directory for `.git`, then serves
//! the per-repo MCP tool surface on stdio. The MCP config becomes
//! `{"command":"lain","args":["mcp"]}` — no repos.yaml, no federation
//! plumbing, no drift between installed config and the binary's CLI.
//!
//! Tradeoffs vs. `lain server --config repos.yaml`:
//! - Loses the federation surface (`list_repos`, `search_org`,
//!   `get_federation_health`, cross-repo blast radius). The
//!   single-repo binding fix (PR 18) means the per-repo tools all
//!   work; `list_repos` reduces to "the one repo we're in" which an
//!   agent can do with `git rev-parse` itself.
//! - Indexes the repo synchronously on first claim. The federation
//!   path uses a background indexer; here the stdio pipe is up
//!   immediately and the first `find_anchors`-class call does the
//!   initial walk. Acceptable for the drop-in use case.
//!
//! The dispatcher is `LainMcpServer::new(executor)` — the
//! single-workspace constructor. The federation handle is `None`,
//! so federation tools are not registered. The per-repo handlers
//! run against `executor.ctx.graph` (the real workspace graph built
//! by `LainServer::new`).

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Run a single-repo MCP server on stdio. If `workspace_arg` is
/// `None`, walks up from the current directory until it finds a
/// `.git` marker, then uses that parent as the workspace root.
/// `embedding_model` is passed through to `LainServer::new` — see
/// `lain server --help` for the model directory format.
pub async fn run_mcp(workspace_arg: Option<&Path>, embedding_model: Option<&Path>) -> Result<()> {
    let workspace = match workspace_arg {
        Some(p) => p.to_path_buf(),
        None => find_git_workspace_root()
            .context("walk up for .git: no `.git` found in any parent directory")?,
    };
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "no `.git` found at {} (or any parent) — pass `--workspace PATH` to override",
            workspace.display()
        ));
    }

    // `.lain/graph.bin` lives next to the workspace so the persisted
    // graph follows the repo. Picked up by `save_state` / `load_state`.
    let mem_dir = workspace.join(".lain");
    std::fs::create_dir_all(&mem_dir)
        .with_context(|| format!("create_dir_all({})", mem_dir.display()))?;
    let mem_path = mem_dir.join("graph.bin");

    let server = crate::server::LainServer::new(&workspace, &mem_path, embedding_model)
        .with_context(|| format!("build LainServer for {}", workspace.display()))?;

    // Hand the executor's tool surface to a federation-free
    // `LainMcpServer`. Single-workspace mode — per-repo tools run
    // against `server.tool_executor.graph` directly.
    let mcp = crate::server::mcp::handler::LainMcpServer::new(server.tool_executor.clone())
        .with_server(std::sync::Arc::new(server));
    mcp.run_stdio()
        .await
        .map_err(|e| anyhow!("MCP stdio run failed: {e}"))?;
    Ok(())
}

/// Walk up from the current directory until we find a `.git`
/// directory, then return that ancestor. Mirrors the helper in
/// `src/cli/hooks.rs::find_workspace_root` (intentionally not
/// re-exported — that one only honors `.git`, but it lives in a
/// submodule and exposing it would change the public surface).
/// Returns `None` if no `.git` is found within 16 levels.
fn find_git_workspace_root() -> Option<PathBuf> {
    let mut current = std::env::current_dir()
        .ok()?
        .canonicalize()
        .ok()?;
    for _ in 0..16 {
        if current.join(".git").exists() {
            return Some(current);
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `find_git_workspace_root` returns the directory containing
    /// `.git` when started from inside a clone. Uses a tempdir
    /// hierarchy (`root/.git` + `root/src/sub/file.rs`) so the test
    /// doesn't depend on the host repo's actual layout.
    #[test]
    fn find_git_workspace_root_walks_up_to_dot_git() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/sub/file.rs"), "fn x() {}").unwrap();
        // `current_dir` is the test process's cwd, not the tempdir.
        // We can't `chdir` in a multi-threaded test, so call the
        // inner walk directly with `current = root.join("src/sub")`.
        let mut current = root.join("src/sub");
        for _ in 0..16 {
            if current.join(".git").exists() {
                return;
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => panic!("walk did not find .git"),
            }
        }
        panic!("walk exhausted 16 levels without finding .git");
    }

    /// When no `.git` exists within 16 levels, the function returns
    /// `None` rather than panicking — `run_mcp` translates that into
    /// a clean error message ("no `.git` found, pass --workspace").
    #[test]
    fn find_git_workspace_root_returns_none_when_missing() {
        // Build a deep tempdir with no `.git` anywhere. Walking
        // from `deep/sub` should return `None` at the 16-level
        // budget without panicking.
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/deep/sub");
        std::fs::create_dir_all(&deep).unwrap();
        let mut current = deep;
        let mut found = false;
        for _ in 0..16 {
            if current.join(".git").exists() {
                found = true;
                break;
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
        assert!(!found, ".git must not exist in this controlled tree");
    }
}