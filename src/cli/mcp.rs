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
use std::path::Path;

/// Run a single-repo MCP server on stdio. If `workspace_arg` is
/// `None`, walks up from the current directory until it finds a
/// `.git` marker, then uses that parent as the workspace root.
/// `embedding_model` is passed through to `LainServer::new` — see
/// `lain server --help` for the model directory format.
/// `reindex_timeout` overrides `LAIN_REINDEX_TIMEOUT`; step 1 of the
/// staleness fix.
pub async fn run_mcp(
    workspace_arg: Option<&Path>,
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
) -> Result<()> {
    let workspace = match workspace_arg {
        Some(p) => p.to_path_buf(),
        None => crate::cli::workspace::find_git_workspace_root(None)
            .and_then(|o| o.ok_or_else(|| anyhow!(
                "walk up for .git: no `.git` found in any parent directory"
            )))?,
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
    // against `server.tool_executor.graph` directly. The re-index
    // timeout is wired through to run_stdio so the spawn honors
    // it (or the env var if None).
    let mcp = crate::server::mcp::handler::LainMcpServer::new(server.tool_executor.clone())
        .with_server(std::sync::Arc::new(server))
        .with_reindex_timeout(reindex_timeout);
    mcp.run_stdio()
        .await
        .map_err(|e| anyhow!("MCP stdio run failed: {e}"))?;
    Ok(())
}

