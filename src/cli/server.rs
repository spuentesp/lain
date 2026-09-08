//! `lain server` subcommand - start a federation-mode MCP server.
//!
//! Loads the federation from a config file (one or more repos cloned or
//! pointed at), then serves the federation MCP tools over the chosen
//! transport. In HTTP mode the federation tool surface (`list_repos`,
//! `get_federation_health`, `search_org`, etc.) is exposed at
//! `POST /mcp` exactly like a single-workspace `lain --transport http`.

use crate::federation::health::RepoHealth;
use crate::federation::loader::{load_federation, load_federation_with_workspace};
use crate::server::{
    attribution::{AttributionBackend, LsofBackend, NoopBackend, ProcFsBackend},
    LainServer, Transport,
};
use crate::state::ActiveWorkspace;
use anyhow::{anyhow, Result};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Start a federation-mode MCP server.
///
/// `config_path` is the path to a `repos.yaml` federation config (see
/// `src/federation/config.rs` for the schema). `transport` is one of
/// `"http"` or `"stdio"`. `port` is the TCP port for HTTP. `bind` is
/// the address the HTTP listener accepts on — defaults to loopback;
/// non-loopback requires `LAIN_API_KEYS` (enforced inside `LainServer::serve`).
/// `log_level` is a tracing `EnvFilter` directive (e.g. `"info"`,
/// `"debug"`). `workspace_arg` selects the active workspace: "auto"
/// resolves via `~/.config/lain/active_workspace`, "" loads every repo
/// in `repos.yaml` (today's behavior), and any other value names a
/// workspace from `workspaces.yaml` next to `repos.yaml`.
/// `no_process_attribution` forces the no-op [`AttributionBackend`]
/// regardless of platform.
#[allow(clippy::too_many_arguments)]
pub async fn run_server(
    config_path: &Path,
    transport: &str,
    port: u16,
    bind: IpAddr,
    log_level: &str,
    workspace_arg: &str,
    no_process_attribution: bool,
    embedding_model: Option<&Path>,
) -> Result<()> {
    init_tracing(log_level);

    info!(
        "lain server: loading federation from {}",
        config_path.display()
    );
    let fed = load_federation_for_workspace(config_path, workspace_arg)
        .await
        .map_err(|e| anyhow!("federation load: {e}"))?;

    // Indexing runs in the background so the server listens at once;
    // see `index_federation`.
    tokio::spawn(index_federation(Arc::clone(&fed)));

    let transport_enum = match transport {
        "http" => Transport::Http,
        "stdio" => Transport::Stdio,
        other => {
            return Err(anyhow!(
                "unknown transport: {other} (expected 'http' or 'stdio')"
            ));
        }
    };

    // Pick the attribution backend based on the platform and the
    // `--no-process-attribution` flag. The CLI is the only place that
    // knows about the flag, so it's the only place that can decide
    // between `ProcFsBackend` (Linux default), `LsofBackend` (macOS
    // default), and `NoopBackend` (Windows default or explicit
    // opt-out). Tests and embedders that go through `with_federation*`
    // directly still get a cfg-based default and never see this
    // dispatch.
    let attribution: Arc<dyn AttributionBackend> = if no_process_attribution {
        info!("attribution: --no-process-attribution set; using NoopBackend");
        Arc::new(NoopBackend)
    } else if cfg!(target_os = "linux") {
        Arc::new(ProcFsBackend)
    } else if cfg!(target_os = "macos") {
        Arc::new(LsofBackend)
    } else {
        Arc::new(NoopBackend)
    };
    info!("attribution: using backend '{}'", attribution.name());

    // If a workspaces file exists next to repos.yaml, load it so the
    // workspace MCP tools are registered. Optional — a server with no
    // workspaces.yaml still works (no workspace tools, today's behavior).
    let workspaces = load_workspaces_for_server(config_path).ok().flatten();
    let repos_yaml = Some(config_path.to_path_buf());
    // Captured before `fed` is moved into the server: the source watcher
    // needs one root per indexed repo.
    let fed_repo_paths = fed.repo_paths();
    let server = if let Some(workspaces) = workspaces {
        LainServer::with_federation_and_workspaces_with_attribution(
            fed,
            transport_enum,
            port,
            bind,
            workspaces,
            repos_yaml.clone(),
            attribution,
            embedding_model,
        )?
    } else {
        LainServer::with_federation_with_attribution(
            fed,
            transport_enum,
            port,
            bind,
            repos_yaml.clone(),
            attribution,
            embedding_model,
        )?
    };

    // Record this project under `~/.config/lain/recent_projects` so the
    // dashboard's project switcher can find it. Failures are logged and
    // ignored — never block startup on a side-effect that is purely
    // operator convenience.
    if let Some(p) = repos_yaml.as_deref() {
        if let Err(e) = crate::config::recent_projects::record(p) {
            tracing::warn!("could not record recent project {}: {e}", p.display());
        }
    }

    // Hot-reload subsystem: file watcher, Unix socket, rebuild loop.
    spawn_hot_reload(config_path, &server).await;

    // Source-file watcher: keeps the volatile overlay fresh between
    // reindexes. One per indexed repo — `spawn_config_watcher` above
    // only watches `repos.yaml`/`workspaces.yaml`, not source.
    //
    // `start_source_watcher` waits (bounded 5s) for its watcher to
    // actually register before returning, so its caller knows the
    // watcher is subscribed rather than racing its startup. Awaiting
    // that sequentially, one repo at a time, meant a federation of N
    // repos could add up to 5*N seconds to every server boot; starting
    // all of them concurrently bounds the wait to the slowest one.
    let watcher_tasks: Vec<_> = fed_repo_paths
        .into_iter()
        .map(|root| {
            tokio::spawn(crate::server::ingest::background::start_source_watcher(
                root,
                server.clone(),
            ))
        })
        .collect();
    for task in watcher_tasks {
        if let Err(e) = task.await {
            tracing::warn!("source file watcher registration task panicked: {e}");
        }
    }

    // Reap expired `/ui/...` sessions; the HTTP transport creates one per
    // interactive blast-radius link and nothing ever removed them.
    crate::server::ingest::background::spawn_ui_session_reaper(
        server.ingest().tool_executor().ctx.clone(),
    );

    // OTLP runtime-trace listener: opt-in via LAIN_TRACE_RUNTIME=true.
    // Bound to a separate TcpListener so the runtime trace path
    // doesn't share the MCP bearer-token auth layer — OTLP collectors
    // don't ship bearer tokens. The port is configurable via
    // LAIN_TRACE_OTLP_PORT (default 4318, the OTel standard).
    if std::env::var("LAIN_TRACE_RUNTIME")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false)
    {
        let otlp_port: u16 = std::env::var("LAIN_TRACE_OTLP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4318);
        let otlp_addr = std::net::SocketAddr::from(([0, 0, 0, 0], otlp_port));
        match crate::server::runtime_trace::server::try_bind(otlp_addr).await {
            Ok(listener) => {
                let store = std::sync::Arc::new(
                    crate::server::runtime_trace::RuntimeTraceStore::global().clone(),
                );
                // Build a span→node_id resolver against the bound
                // single-repo graph. Federation mode wires a different
                // resolver that walks every registered repo; this
                // wiring is sufficient because `cli::server::run`
                // starts the listener before the federation-aware
                // dispatcher takes over. Federation mode uses the
                // dedicated federation resolver which walks every
                // registered repo and honors OTLP semconv hints
                // (`code.repo`, `service.name`) so cross-repo spans
                // mint correctly namespaced `RuntimeCall` edges
                // instead of leaking edges between repos.
                let resolver = server
                    .ingest()
                    .tool_executor()
                    .ctx
                    .federation
                    .clone()
                    .map(|fed| crate::server::runtime_trace::server::federation_resolver(fed))
                    .unwrap_or_else(|| {
                        // Single-workspace mode: the executor's graph
                        // is the only authoritative source.
                        let graph = server.ingest().tool_executor().ctx.graph.clone();
                        crate::server::runtime_trace::server::SpanResolver::from(
                            std::sync::Arc::new(
                                move |span: &crate::server::runtime_trace::SpanRecord| {
                                    graph.find_node_by_name(&span.name).map(|n| n.id)
                                },
                            )
                                as std::sync::Arc<
                                    dyn Fn(
                                            &crate::server::runtime_trace::SpanRecord,
                                        )
                                            -> Option<String>
                                        + Send
                                        + Sync,
                                >,
                        )
                    });
                let handle = crate::server::runtime_trace::server::start(listener, store, resolver);
                tracing::info!(
                    "OTLP runtime-trace listener bound on {otlp_addr} (POST /v1/traces)"
                );
                // Drop the handle to free resources if shutdown aborts
                // it. The listener runs until the process exits.
                std::mem::forget(handle);
            }
            Err(e) => {
                tracing::warn!(
                    "could not bind OTLP listener on {otlp_addr}: {e} — runtime tracing disabled"
                );
            }
        }
    }

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
) -> Result<Option<Arc<crate::federation::workspace::WorkspacesFile>>, anyhow::Error> {
    let workspaces_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("workspaces.yaml");
    if !workspaces_path.exists() {
        return Ok(None);
    }
    let workspaces = crate::federation::workspace::WorkspacesFile::load(&workspaces_path)
        .map_err(|e| anyhow!("load {}: {e}", workspaces_path.display()))?;
    Ok(Some(Arc::new(workspaces)))
}

/// Index every repo, then link calls across repos, in the background.
///
/// This ran before the server started listening, so `lain server` answered
/// nothing — not even `/health` — until every repo was indexed: seconds
/// for a small repo with rust-analyzer installed, minutes for a large
/// federation. Repos start in `RepoHealth::Indexing` and only turn `Ready`
/// when their pass finishes, so tools and `get_health` report progress
/// honestly in the meantime.
async fn index_federation(fed: Arc<FederatedIndex>) {
    // With several repos, a repo is not ready when its own pass ends:
    // calls into repos indexed after it are linked by the second pass
    // below. Hold it in `Indexing` until then, or a client that waits for
    // `ready` queries the cross-repo graph before it exists.
    let multi_repo = fed.list_repos().len() > 1;
    if multi_repo {
        for (id, _) in fed.list_repos() {
            if let Some(repo) = fed.get_repo(&id) {
                repo.hold_ready(true);
            }
        }
    }

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

            // Re-index this repo when its checkout changes.
            // `RepoIndex::start_watcher` had a test but no production
            // caller, so a federated repo was frozen at whatever commit
            // it was first indexed at — the same staleness that left this
            // repo's own graph 29 commits behind. The comment above
            // ("the watcher would eventually pick up filesystem events")
            // described a watcher that nothing started.
            if let Err(e) = repo.start_watcher().await {
                tracing::warn!(
                    "lain server: could not watch repo '{}' for re-index: {e}",
                    id.as_str()
                );
            }
        }
    }

    // Second pass: every repo's symbols are now known, so calls from a
    // repo indexed early into one indexed later can finally resolve.
    let repo_ids: Vec<_> = fed.list_repos().into_iter().map(|(id, _)| id).collect();
    if repo_ids.len() > 1 {
        for id in &repo_ids {
            let Some(repo) = fed.get_repo(id) else {
                continue;
            };
            match repo.relink_cross_repo().await {
                Ok(0) => {}
                Ok(n) => {
                    info!(
                        "lain server: linked {n} cross-repo edge(s) from '{}'",
                        id.as_str()
                    );
                    if let Err(e) = fed.project_repo(id).await {
                        tracing::warn!(
                            "lain server: project_repo for '{}' after cross-repo link failed: {e}",
                            id.as_str()
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "lain server: cross-repo link for '{}' failed: {e}",
                    id.as_str()
                ),
            }
        }
    }
    for (id, _) in fed.list_repos() {
        if let Some(repo) = fed.get_repo(&id) {
            repo.hold_ready(false);
        }
    }

    // Keep the federation's global view current: a watcher-triggered
    // re-index changes one repo's graph, and until it is projected again
    // search_org and the cross-repo tools answer from the old one.
    // Clone sources are fetched on this cadence while the server runs (the
    // docs promised a refresh interval; only startup fetched). A fetch that
    // moves the checkout is picked up by the repo's watcher and re-indexed.
    let mut last_fetch: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // Drop entries for repos that were removed since the last tick.
        // Without this, `last_fetch` grows linearly with the cumulative
        // count of repos ever added across hot-add/hot-remove churn.
        let live_ids: std::collections::HashSet<String> = fed
            .list_repos()
            .into_iter()
            .map(|(id, _)| id.to_string())
            .collect();
        last_fetch.retain(|id, _| live_ids.contains(id));
        for (id, _) in fed.list_repos() {
            let Some(repo) = fed.get_repo(&id) else {
                continue;
            };
            let every = match repo.source().source_config() {
                crate::federation::config::SourceConfig::LocalClone { .. } => 300,
                crate::federation::config::SourceConfig::ShallowClone {
                    refresh_interval_secs,
                    ..
                } => (*refresh_interval_secs).max(30),
                crate::federation::config::SourceConfig::WorkspaceDir { .. } => continue,
            };
            let now = std::time::Instant::now();
            let due = last_fetch
                .get(id.as_str())
                .is_none_or(|t| now.duration_since(*t).as_secs() >= every);
            if !due {
                continue;
            }
            if !last_fetch.contains_key(id.as_str()) {
                // Startup already fetched; count from now.
                last_fetch.insert(id.as_str().to_string(), now);
                continue;
            }
            last_fetch.insert(id.as_str().to_string(), now);
            // Cap each fetch at 5 minutes. A remote that accepts the
            // TCP connection but never returns (slow proxy, hung CI
            // runner) would otherwise have the refresh loop blocked
            // indefinitely waiting on it — `tokio::spawn` doesn't
            // outlive the awaiter, so the whole `for (id, _)` loop
            // stalls and no other repo gets its periodic check.
            //
            // NOTE: this bounds the *wait* in this loop. The spawned
            // task is dropped on timeout, but `spawn_blocking` around
            // `Command::new("git")` is not cancellable — the leaked
            // `std::thread` keeps running until the OS-level TCP
            // timeout fires. Killing the child `git` process is the
            // only way to actually release the thread; doing so
            // safely needs a process-tree kill, which is bigger than
            // this change. Until then, treat `FETCH_TIMEOUT` as
            // bounded wait, not bounded resource use.
            const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
            tokio::spawn(async move {
                match tokio::time::timeout(FETCH_TIMEOUT, repo.source().fetch()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!("lain server: refreshing '{}' failed: {e}", id.as_str());
                    }
                    Err(_) => {
                        tracing::warn!(
                            "lain server: refreshing '{}' timed out after {}s",
                            id.as_str(),
                            FETCH_TIMEOUT.as_secs()
                        );
                    }
                }
            });
        }
        let stale: Vec<_> = fed
            .list_repos()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| fed.get_repo(id).is_some_and(|r| r.take_projection_stale()))
            .collect();
        if stale.is_empty() {
            continue;
        }
        // A change in one repo can add or move a callee that *other* repos
        // call: relink every repo, not just the changed one, or those
        // callers stay unlinked (or point at the old location) until they
        // happen to be re-indexed themselves.
        let to_relink: Vec<_> = if fed.list_repos().len() > 1 {
            fed.list_repos().into_iter().map(|(id, _)| id).collect()
        } else {
            stale.clone()
        };
        for id in &to_relink {
            let Some(repo) = fed.get_repo(id) else {
                continue;
            };
            let linked = if fed.list_repos().len() > 1 {
                match repo.relink_cross_repo().await {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!(
                            "lain server: cross-repo relink for '{}' failed: {e}",
                            id.as_str()
                        );
                        0
                    }
                }
            } else {
                0
            };
            if stale.contains(id) || linked > 0 {
                if let Err(e) = fed.project_repo(id).await {
                    tracing::warn!("lain server: re-projecting '{}' failed: {e}", id.as_str());
                }
            }
        }
    }
}

/// Whether the global active-workspace pointer belongs to the project
/// whose `repos.yaml` is `config_path`: same workspaces file when the
/// pointer records one, otherwise (older pointers) a workspace of that name
/// exists here.
fn active_pointer_applies(active: &ActiveWorkspace, config_path: &Path) -> bool {
    let here = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("workspaces.yaml");
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match active.config_path.as_deref() {
        Some(set_from) if set_from.is_absolute() => canon(set_from) == canon(&here),
        _ => crate::federation::workspace::WorkspacesFile::load(&here)
            .is_ok_and(|f| f.workspaces.iter().any(|w| w.name == active.name)),
    }
}

/// Resolve the `--workspace` arg and dispatch to the right loader.
/// Exposed at the file level so a unit test can exercise the resolution
/// without spinning up an MCP server.
async fn load_federation_for_workspace(
    config_path: &Path,
    workspace_arg: &str,
) -> Result<Arc<FederatedIndex>, anyhow::Error> {
    let arg = workspace_arg.trim();
    let resolved_name: Option<String> = match arg {
        "" | "none" => None, // explicit "no workspace" — today's behavior
        "auto" => {
            match ActiveWorkspace::load() {
                // The pointer is global (~/.config/lain); it only applies to
                // the project whose workspaces file it was set from. Using
                // it everywhere made `lain workspaces use` in one project
                // stop `lain server` in every other one.
                Ok(Some(active)) if !active_pointer_applies(&active, config_path) => {
                    warn!(
                        "active workspace '{}' was set for {}, not this project; serving all repos",
                        active.name,
                        active
                            .config_path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "another project".into())
                    );
                    None
                }
                Ok(Some(active)) => Some(active.name),
                Ok(None) => None, // no pointer set → fall through to all-repos
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
use crate::federation::federated_index::FederatedIndex;

use crate::server::reload::run_rebuild;

/// Spawn the long-lived hot-reload subsystem: a Unix socket listener
/// that the CLI can ping to request a reload, a file watcher that
/// picks up hand-edits to `repos.yaml` / `workspaces.yaml`, and a
/// rebuild task that consumes the bus. Returns immediately after
/// spawning — failures are logged and non-fatal so the MCP server
/// can still come up even if the socket dir is unwritable.
async fn spawn_hot_reload(config_path: &Path, server: &LainServer) {
    let bus = server.reload_bus();

    // File watcher — fires `request_reload` on hand-edits.
    // The handle pair (`_watcher_join` + `_watcher_handle`) is held
    // for the lifetime of the hot-reload task; dropping the handle
    // signals the watcher thread to exit and releases the OS watch
    // handles.
    let (_watcher_join, _watcher_handle) =
        crate::server::watcher::spawn_config_watcher(config_path, Arc::clone(&bus));

    // Unix socket — CLI signals. Unix only; on Windows the file watcher
    // is still the reload path (CLI-prompted reloads via the
    // `lain repos add` / `lain workspaces create` socket are unavailable).
    #[cfg(unix)]
    {
        let sock_path = crate::cli::signal::socket_path_for(config_path);
        if let Err(e) =
            crate::cli::signal::spawn_signal_listener_at(&sock_path, Arc::clone(&bus)).await
        {
            tracing::warn!(
                "hot reload: could not bind signal socket at {}: {e}",
                sock_path.display()
            );
            // Continue: the file watcher is still up; only CLI-prompted
            // reloads are unavailable.
        } else {
            tracing::info!("hot reload: signal listener at {}", sock_path.display());
        }
    }

    // Rebuild loop: subscribes to the bus and runs `run_rebuild` on
    // every signal.
    let server_for_loop = server.clone_for_background();
    let bus_for_loop = Arc::clone(&bus);
    tokio::spawn(async move {
        let mut sub = bus_for_loop.subscribe();
        loop {
            match sub.try_recv() {
                Ok(()) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    if let Err(e) = run_rebuild(&server_for_loop, &bus_for_loop).await {
                        tracing::warn!("hot reload: rebuild failed: {e}");
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("hot reload: bus closed, rebuild loop exiting");
                    break;
                }
            }
        }
    });
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

#[cfg(test)]
mod active_pointer_tests {
    use super::*;

    #[test]
    fn a_pointer_from_another_project_does_not_apply() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        for d in [here.path(), there.path()] {
            std::fs::write(d.join("repos.yaml"), "repos: []\n").unwrap();
            std::fs::write(
                d.join("workspaces.yaml"),
                "workspaces:\n- name: stack\n  members: [a]\n",
            )
            .unwrap();
        }
        let set_there = ActiveWorkspace {
            name: "stack".into(),
            config_path: Some(there.path().join("workspaces.yaml")),
        };
        assert!(!active_pointer_applies(
            &set_there,
            &here.path().join("repos.yaml")
        ));
        assert!(active_pointer_applies(
            &set_there,
            &there.path().join("repos.yaml")
        ));
        // A legacy pointer (no path) applies where the name exists.
        let legacy = ActiveWorkspace {
            name: "stack".into(),
            config_path: None,
        };
        assert!(active_pointer_applies(
            &legacy,
            &here.path().join("repos.yaml")
        ));
        let unknown = ActiveWorkspace {
            name: "nope".into(),
            config_path: None,
        };
        assert!(!active_pointer_applies(
            &unknown,
            &here.path().join("repos.yaml")
        ));
    }
}
