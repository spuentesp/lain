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

    let mut server = crate::server::LainServer::new(&workspace, &mem_path, embedding_model)
        .with_context(|| format!("build LainServer for {}", workspace.display()))?;

    // Seed the overlay from what is already uncommitted in the working
    // tree. `sync_volatile_overlay` had no caller anywhere, so a server
    // starting on a dirty checkout began with an empty overlay and stayed
    // that way until the user happened to re-save one of those files —
    // work already on disk was invisible. Runs before the watcher starts
    // because it clears the overlay first; the reverse order would drop
    // whatever the watcher had already inserted.
    //
    // Single-repo only: it reads `self.git`, which is unambiguous here
    // and would be the wrong repo under federation.
    if let Err(e) = server.sync_volatile_overlay().await {
        tracing::warn!("initial overlay sync failed (continuing): {e}");
    }

    // Keep the volatile overlay fresh while the user edits. Without this
    // the overlay is never written to at all and every answer is only as
    // current as the last reindex.
    crate::server::ingest::background::start_source_watcher(workspace.clone(), server.clone());
    crate::server::ingest::background::spawn_ui_session_reaper(server.tool_executor.ctx.clone());

    // Re-index when the checkout moves to a new commit. Without this the
    // graph never advances past the commit it was first built from.
    crate::server::ingest::background::spawn_commit_sync(server.clone());

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


/// Run a read-only **sidecar** MCP server against an owner.
///
/// Every piece of this existed and was individually tested — the owner
/// serves `/overlay/subscribe` and `/overlay/get_snapshot`
/// (`mcp::handler`), `overlay::subscribe` consumes that stream with
/// snapshot re-hydration and backoff, `GraphDatabase::open_read_only`
/// gives a graph that refuses writes, and `ToolExecutor::new_read_only`
/// plus `LainMcpServer::new_read_only` serve tools over it. What did not
/// exist was any way to *start* one: none of those three constructors had
/// a caller anywhere, so the ingest pipeline's `is_read_only()` guards
/// defended against a mode the binary could not enter.
///
/// A sidecar never indexes: it answers from the owner's persisted graph
/// plus whatever the owner has streamed into the overlay since.
pub async fn run_sidecar(workspace_arg: Option<&Path>, owner_url: &str) -> Result<()> {
    let workspace = match workspace_arg {
        Some(p) => p.to_path_buf(),
        None => crate::cli::workspace::find_git_workspace_root(None)
            .and_then(|o| o.ok_or_else(|| anyhow!(
                "walk up for .git: no `.git` found in any parent directory"
            )))?,
    };

    let mem_path = workspace.join(".lain").join("graph.bin");
    if !mem_path.exists() {
        return Err(anyhow!(
            "no persisted graph at {} — a sidecar reads the owner's graph and \
             never builds one itself; start `lain server` (or `lain mcp`) on \
             this checkout first",
            mem_path.display()
        ));
    }

    let graph = crate::graph::GraphDatabase::open_read_only(&mem_path)
        .with_context(|| format!("open {} read-only", mem_path.display()))?;
    let overlay = crate::overlay::VolatileOverlay::new();

    // Follow the owner for the life of the process. `subscribe` never
    // returns: it re-hydrates from the snapshot endpoint and reconnects
    // with backoff whenever the stream drops.
    let follow_overlay = overlay.clone();
    let owner = owner_url.to_string();
    tokio::spawn(async move {
        crate::overlay::subscribe(owner, follow_overlay).await;
    });

    tracing::info!("sidecar mode: following {owner_url} for {}", workspace.display());

    let executor =
        crate::tools::ToolExecutor::new_read_only(graph, overlay, workspace.clone());
    crate::server::mcp::handler::LainMcpServer::new_read_only(executor)
        .run_stdio()
        .await
        .map_err(|e| anyhow!("MCP stdio run failed: {e}"))?;
    Ok(())
}
