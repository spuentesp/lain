//! `lain server` subcommand - start a federation-mode MCP server.
//!
//! Loads the federation from a config file (one or more repos cloned or
//! pointed at), then serves the federation MCP tools over the chosen
//! transport. In HTTP mode the federation tool surface (`list_repos`,
//! `get_federation_health`, `search_org`, etc.) is exposed at
//! `POST /mcp` exactly like a single-workspace `lain --transport http`.
//!
//! Note: this implements only the subset of Task 24 required to smoke-test
//! the federation end-to-end. Federation-mode routing for the existing
//! per-repo tool surface is out of scope here.

use anyhow::{anyhow, Result};
use lain::federation::loader::load_federation;
use lain::git::GitSensor;
use lain::graph::GraphDatabase;
use lain::lsp::LspPool;
use lain::mcp::LainMcpServer;
use lain::nlp::NlpEmbedder;
use lain::overlay::VolatileOverlay;
use lain::tools::ToolExecutor;
use lain::tuning::load_tuning_config;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Start a federation-mode MCP server.
///
/// `config_path` is the path to a `repos.yaml` federation config (see
/// `src/federation/config.rs` for the schema). `transport` is one of
/// `"http"` or `"stdio"`. `port` is the TCP port for HTTP. `log_level`
/// is a tracing `EnvFilter` directive (e.g. `"info"`, `"debug"`).
pub async fn run_server(
    config_path: &Path,
    transport: &str,
    port: u16,
    log_level: &str,
) -> Result<()> {
    init_tracing(log_level);

    info!(
        "lain server: loading federation from {}",
        config_path.display()
    );
    let fed = load_federation(config_path)
        .await
        .map_err(|e| anyhow!("load_federation({}): {e}", config_path.display()))?;

    // `load_federation` adds each repo to the federation and projects whatever
    // nodes are already in the per-repo DB, but it does NOT run the indexing
    // pipeline (`tree-sitter` extract → LSP hydrate → git co-change). For a
    // freshly-loaded federation the per-repo DB is empty, so federation tools
    // that read from `repo.nodes()` (e.g. `search_org`) would return zero hits
    // until something else kicks off indexing. The watcher would eventually
    // pick up filesystem events, but the initial `git clone` won't fire any —
    // so we explicitly run `repo.index()` on every registered repo here.
    // Failures are logged and demoted to `Degraded`; the federation still comes
    // up so partial results remain queryable.
    for (id, _) in fed.list_repos() {
        if let Some(repo) = fed.get_repo(&id) {
            info!("lain server: indexing repo '{}'", id.as_str());
            if let Err(e) = repo.index().await {
                tracing::warn!(
                    "lain server: indexing repo '{}' failed: {e} (marking Degraded)",
                    id.as_str()
                );
            } else {
                // After indexing, re-project so the global backend sees the
                // newly-extracted nodes/edges.
                if let Err(e) = fed.project_repo(&id).await {
                    tracing::warn!(
                        "lain server: project_repo for '{}' after indexing failed: {e}",
                        id.as_str()
                    );
                }
            }
        }
    }

    let executor = build_minimal_executor()?;

    let mcp = LainMcpServer::with_federation(executor, fed);

    match transport {
        "http" => {
            info!("lain server: HTTP transport on port {}", port);
            mcp.run_http(port)
                .await
                .map_err(|e| anyhow!("HTTP transport: {e}"))?;
        }
        "stdio" => {
            info!("lain server: stdio transport");
            mcp.run_stdio()
                .await
                .map_err(|e| anyhow!("stdio transport: {e}"))?;
        }
        other => {
            return Err(anyhow!(
                "unknown transport: {other} (expected 'http' or 'stdio')"
            ));
        }
    }
    Ok(())
}

fn init_tracing(log_level: &str) {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

/// Build a minimal `ToolExecutor` so `LainMcpServer::with_federation` can be
/// constructed. Federation tools are dispatched in `mcp/handler.rs` before
/// the executor is touched, so for a federation-only server the executor
/// is just a placeholder — none of its underlying services are exercised.
///
/// The components it wires up still require a working directory and a git
/// repository to point at (GitSensor opens the workspace with git2). Use a
/// temp directory seeded by `std::process::id()` (unique per invocation) and
/// `git init` it. The graph memory path is a fresh path under that dir so
/// `load_from_disk` is never triggered.
fn build_minimal_executor() -> Result<ToolExecutor> {
    let ws = std::env::temp_dir().join(format!("lain-federation-{}", std::process::id()));
    std::fs::create_dir_all(&ws)?;
    if !ws.join(".git").exists() {
        git2::Repository::init(&ws)?;
    }

    let mem_dir = ws.join(".lain");
    std::fs::create_dir_all(&mem_dir)?;
    let mem_path = mem_dir.join("graph.bin");
    // Ensure no stale file from a previous run leaks in.
    let _ = std::fs::remove_file(&mem_path);

    let graph = GraphDatabase::new(&mem_path)?;
    let overlay = VolatileOverlay::new();
    let embedder = NlpEmbedder::new()?;
    if embedder.is_stub() {
        info!("NLP embedder running in stub mode (no ONNX model found)");
    }
    let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
    let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);
    let tuning = Arc::new(load_tuning_config(&ws));

    Ok(ToolExecutor::new(
        graph,
        overlay,
        embedder,
        git,
        lsp_pool,
        tuning,
    ))
}
