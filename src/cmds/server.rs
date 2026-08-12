//! `lain server` subcommand - start a federation-mode MCP server.
//!
//! Loads the federation from a config file (one or more repos cloned or
//! pointed at), then serves the federation MCP tools over the chosen
//! transport. In HTTP mode the federation tool surface (`list_repos`,
//! `get_federation_health`, `search_org`, etc.) is exposed at
//! `POST /mcp` exactly like a single-workspace `lain --transport http`.

use anyhow::{anyhow, Result};
use lain::federation::health::RepoHealth;
use lain::federation::loader::{load_federation, load_federation_with_workspace};
use lain::server::{LainServer, Transport};
use lain::state::ActiveWorkspace;
use std::path::Path;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Start a federation-mode MCP server.
///
/// `config_path` is the path to a `repos.yaml` federation config (see
/// `src/federation/config.rs` for the schema). `transport` is one of
/// `"http"` or `"stdio"`. `port` is the TCP port for HTTP. `log_level`
/// is a tracing `EnvFilter` directive (e.g. `"info"`, `"debug"`).
/// `workspace_arg` selects the active workspace: "auto" resolves via
/// `~/.config/lain/active_workspace`, "" loads every repo in
/// `repos.yaml` (today's behavior), and any other value names a workspace
/// from `workspaces.yaml` next to `repos.yaml`.
pub async fn run_server(
    config_path: &Path,
    transport: &str,
    port: u16,
    log_level: &str,
    workspace_arg: &str,
) -> Result<()> {
    init_tracing(log_level);

    info!(
        "lain server: loading federation from {}",
        config_path.display()
    );
    let fed = load_federation_for_workspace(config_path, workspace_arg)
        .await
        .map_err(|e| anyhow!("federation load: {e}"))?;

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
                // `RepoIndex::index` already demotes its own health to
                // `Degraded` on failure, but we re-assert it here so the
                // demotion is independent of `index()`'s implementation
                // details (e.g. if a future refactor moves the demotion
                // out of `index()` callers won't silently lose it).
                repo.set_health(RepoHealth::Degraded);
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

    let transport_enum = match transport {
        "http" => Transport::Http,
        "stdio" => Transport::Stdio,
        other => {
            return Err(anyhow!(
                "unknown transport: {other} (expected 'http' or 'stdio')"
            ));
        }
    };

    // If a workspaces file exists next to repos.yaml, load it so the
    // workspace MCP tools are registered. Optional — a server with no
    // workspaces.yaml still works (no workspace tools, today's behavior).
    let workspaces = load_workspaces_for_server(config_path).ok().flatten();
    let server = if let Some(workspaces) = workspaces {
        LainServer::with_federation_and_workspaces(fed, transport_enum, port, workspaces)?
    } else {
        LainServer::with_federation(fed, transport_enum, port)?
    };
    info!(
        "lain server: starting on {:?} transport (port {})",
        transport_enum, port
    );
    server
        .serve()
        .await
        .map_err(|e| anyhow!("federation server: {e}"))
}

/// Load `workspaces.yaml` from the same directory as `repos.yaml`. Returns
/// `Ok(None)` if the file doesn't exist (no workspaces configured) or
/// can't be loaded for any reason — workspace tooling is opt-in, and a
/// server without it still works.
fn load_workspaces_for_server(
    config_path: &Path,
) -> Result<Option<Arc<lain::federation::workspace::WorkspacesFile>>, anyhow::Error> {
    let workspaces_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("workspaces.yaml");
    if !workspaces_path.exists() {
        return Ok(None);
    }
    let workspaces = lain::federation::workspace::WorkspacesFile::load(&workspaces_path)
        .map_err(|e| anyhow!("load {}: {e}", workspaces_path.display()))?;
    Ok(Some(Arc::new(workspaces)))
}

/// Resolve the `--workspace` arg and dispatch to the right loader.
/// Exposed at the file level so a unit test can exercise the resolution
/// without spinning up an MCP server.
async fn load_federation_for_workspace(
    config_path: &Path,
    workspace_arg: &str,
) -> Result<Arc<FederatedIndex>, anyhow::Error> {
    use lain::error::LainError;
    let arg = workspace_arg.trim();
    let resolved_name: Option<String> = match arg {
        "" | "none" => None,  // explicit "no workspace" — today's behavior
        "auto" => {
            match ActiveWorkspace::load() {
                Ok(Some(active)) => Some(active.name),
                Ok(None) => None,  // no pointer set → fall through to all-repos
                Err(e) => {
                    // Don't fail startup over a corrupt pointer file;
                    // log and fall through. The operator can re-run
                    // `lain workspaces use <name>` to repair.
                    warn!("could not read ~/.config/lain/active_workspace: {e}");
                    None
                }
            }
        }
        _ => Some(arg.to_string()),
    };
    match resolved_name {
        None => Ok(load_federation(config_path).await?),
        Some(name) => {
            let workspaces_path = config_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("workspaces.yaml");
            Ok(load_federation_with_workspace(config_path, &workspaces_path, &name).await?)
        }
    }
}

// Bring `FederatedIndex` into scope for the helper above.
use lain::federation::federated_index::FederatedIndex;
use std::sync::Arc;

fn init_tracing(log_level: &str) {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}