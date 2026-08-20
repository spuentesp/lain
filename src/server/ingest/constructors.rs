//! Constructor helpers for `LainServer`. The 5 public constructors
//! (`new`, `with_federation`, `with_federation_with_attribution`,
//! `with_federation_and_workspaces`,
//! `with_federation_and_workspaces_with_attribution`) and the
//! private `build_federation_server` that all federation variants
//! delegate to live here, alongside the staging-dir / graph-init /
//! single-repo-binding / embedder helpers they call.

use super::background::{default_attribution_backend, spawn_presence_expiry_loop, start_attribution_watcher};
use super::config::{LainConfig, Transport, PRESENCE_EVENT_CHANNEL_CAPACITY};
use super::server::LainServer;
use crate::config::state_path_for_workspace;
use crate::server::attribution::AttributionBackend;
use crate::server::auth::AuthState;
use crate::server::error::LainError;
use crate::server::events_log::EventsLog;
use crate::server::federation::config::FederationConfig;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::git::GitSensor;
use crate::server::graph::GraphDatabase;
use crate::server::lsp::LspPool;
use crate::server::nlp::{CrossEncoder, NlpEmbedder};
use crate::server::overlay::VolatileOverlay;
use crate::server::presence::{OccupancyMap, PresenceEvent, PresenceRegistry};
use crate::server::reload::ReloadBus;
use crate::server::tools::ToolExecutor;
use crate::server::tuning::{load_tuning_config, TuningConfig};
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tracing::info;

/// Allocate a unique staging dir for federation-mode servers. The
/// placeholder `LainServer` builds a throwaway git repo at
/// `/tmp/lain-federation-{pid}-{counter}` so parallel tests in the
/// same process don't race on a shared path.
fn allocate_staging_dir() -> Result<PathBuf, LainError> {
    let counter = super::STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "lain-federation-{}-{}",
        std::process::id(),
        counter
    ));
    std::fs::create_dir_all(&dir)?;
    if !dir.join(".git").exists() {
        git2::Repository::init(&dir)?;
    }
    Ok(dir)
}

/// Initialize `<ws>/.lain/graph.bin` and return the on-disk path.
/// Removes any stale file so a prior process's graph doesn't leak in.
fn init_workspace_state(ws: &Path) -> Result<PathBuf, LainError> {
    let mem_dir = ws.join(".lain");
    std::fs::create_dir_all(&mem_dir)?;
    let mem_path = mem_dir.join("graph.bin");
    let _ = std::fs::remove_file(&mem_path);
    Ok(mem_path)
}

/// When the federation has exactly one repo, return that repo's
/// indexed `GraphDatabase` so the per-repo structural tools work.
/// Otherwise return the placeholder. The single-repo path
/// unblocks `find_anchors` / `explain_symbol` / `get_blast_radius`
/// / `query_graph` / `get_function_callers` / `get_function_callees`
/// without waiting for the round-2 federation-aware handler refactor
/// (which is the open follow-up for multi-repo).
fn bind_to_single_repo_graph(
    federation: &FederatedIndex,
    placeholder: GraphDatabase,
) -> GraphDatabase {
    if federation.list_repos().len() != 1 {
        return placeholder;
    }
    let only_id = federation
        .list_repos()
        .into_iter()
        .next()
        .map(|(id, _)| id)
        .expect("single-repo federation has exactly one id");
    match federation.get_repo(&only_id) {
        Some(repo) => {
            info!(
                "single-repo federation: binding per-repo tools to {}",
                only_id.as_str()
            );
            repo.db().clone()
        }
        None => placeholder,
    }
}

/// Build the `NlpEmbedder` + `CrossEncoder` pair. When `model_path`
/// is `Some`, loads the ONNX bi-encoder; otherwise runs in stub
/// mode (no `semantic_search` results, but the rest of the tool
/// surface works). The cross-encoder is read from
/// `$LAIN_CROSS_ENCODER` or `~/.local/lain/models/cross-encoder`.
fn build_embedder_pair(
    model_path: Option<&Path>,
    tuning: &TuningConfig,
) -> Result<(NlpEmbedder, CrossEncoder), LainError> {
    let embedder = if let Some(p) = model_path {
        let (model, tokenizer) = NlpEmbedder::resolve_model_paths(p);
        NlpEmbedder::with_max_threads(&model, &tokenizer, tuning.ingestion.nlp_max_threads)?
    } else {
        NlpEmbedder::new_with_threads(tuning.ingestion.nlp_max_threads)?
    };
    if embedder.is_stub() {
        info!("NLP embedder running in stub mode (no --embedding-model set)");
    } else if let Some(p) = model_path {
        info!("NLP embedder loaded from {}", p.display());
    }
    let cross_dir = std::env::var("LAIN_CROSS_ENCODER")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/lain/models/cross-encoder")
        });
    let cross = CrossEncoder::from_dir_with_threads(&cross_dir, tuning.ingestion.nlp_max_threads);
    if cross.is_active() {
        info!("Cross-encoder reranker active (from {:?})", cross_dir);
    } else {
        info!("Cross-encoder reranker disabled (no model at {:?})", cross_dir);
    }
    Ok((embedder, cross))
}

/// Build a federation-mode `LainServer`. Both
/// `with_federation_with_attribution` and
/// `with_federation_and_workspaces_with_attribution` delegate here
/// — the only difference is whether `workspaces` is `Some` (which
/// adds the workspace MCP tool surface) or `None` (all-repos mode).
/// The 4 thin public wrappers above are kept for backwards compat
/// with the public API; the real work is here.
fn build_federation_server(
    federation: Arc<FederatedIndex>,
    transport: Transport,
    port: u16,
    repos_yaml: Option<PathBuf>,
    attribution: Arc<dyn AttributionBackend>,
    embedding_model: Option<&Path>,
    workspaces: Option<Arc<WorkspacesFile>>,
    reindex_timeout: Option<std::time::Duration>,
) -> Result<LainServer, LainError> {
    // Staging workspace: a throwaway git repo under /tmp. The counter
    // ensures parallel tests in the same process don't race.
    let ws = allocate_staging_dir()?;
    let mem_path = init_workspace_state(&ws)?;

    // Graph: open the placeholder, then (single-repo federation) swap
    // it for the real repo's indexed graph. See `bind_to_single_repo_graph`
    // for the multi-repo caveat.
    let mut graph = GraphDatabase::new(&mem_path)?;
    graph = bind_to_single_repo_graph(&federation, graph);

    let overlay = VolatileOverlay::new();
    let tuning = Arc::new(load_tuning_config(&ws));
    let (embedder, cross_encoder) = build_embedder_pair(embedding_model, &tuning)?;
    let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
    let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);

    let tool_executor = ToolExecutor::new(
        graph.clone(),
        overlay.clone(),
        embedder.clone(),
        cross_encoder.clone(),
        Arc::clone(&git),
        Arc::clone(&lsp_pool),
        Arc::clone(&tuning),
        ws.to_path_buf(),
    );

    // Build the LainMcpServer eagerly so any wiring problems surface
    // at construction. The 8 multiplayer tools (presence, occupancy,
    // list_active_agents, who_am_i, list_subagents, claim_files,
    // release_files, my_claims) need a server handle; federation
    // tools (list_repos, search_org, get_federation_health,
    // get_cross_repo_blast_radius*) need the federation handle.
    let workspaces_lock = workspaces.map(|ws| Arc::new(RwLock::new((*ws).clone())));
    let mcp = match workspaces_lock.as_ref() {
        Some(lock) => crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
            tool_executor.clone(),
            Arc::clone(&federation),
            Arc::clone(lock),
        )
        .with_reindex_timeout(reindex_timeout),
        None => crate::server::mcp::handler::LainMcpServer::with_federation(
            tool_executor.clone(),
            Arc::clone(&federation),
        )
        .with_reindex_timeout(reindex_timeout),
    };
    let _mcp = mcp;
    if workspaces_lock.is_some() {
        info!("Lain federation server initialized with workspaces");
    } else {
        info!("Lain federation server initialized");
    }

    // Presence layer: registry + occupancy + broadcast channel.
    // The expiry loop prunes stale sessions + claim TTLs and
    // broadcasts `PresenceEvent` notifications.
    let presence = Arc::new(PresenceRegistry::new());
    let occupancy = Arc::new(OccupancyMap::new());
    let (presence_event_tx, _) = broadcast::channel(PRESENCE_EVENT_CHANNEL_CAPACITY);
    // P1 #2: open the events log before `mem_path` is moved into the
    // LainServer struct — the expiry loop and attribution watcher tag
    // every broadcast event with the durable id assigned here.
    // `events_log_path_from_config` only borrows the path; we clone the
    // Arc into the struct below.
    let events_log_path = LainServer::events_log_path_from_config(&mem_path);
    let events_log = Arc::new(
        EventsLog::open(&events_log_path).expect("open events.jsonl"),
    );
    spawn_presence_expiry_loop(
        presence.clone(),
        occupancy.clone(),
        presence_event_tx.clone(),
        events_log.clone(),
    );
    start_attribution_watcher(
        attribution,
        presence.clone(),
        occupancy.clone(),
        presence_event_tx.clone(),
        events_log.clone(),
        federation.repo_paths(),
    );

    let now = SystemTime::now();
    let server = LainServer {
        config: LainConfig {
            workspace: ws,
            memory_path: mem_path.clone(),
        },
        graph,
        overlay,
        embedder,
        cross_encoder,
        git,
        lsp_pool,
        tool_executor,
        tuning,
        overlay_revision: Arc::new(AtomicU64::new(0)),
        federation: Some(federation),
        federation_workspaces: workspaces_lock,
        federation_transport: Some(transport),
        federation_port: Some(port),
        started_at: now,
        sync_status: crate::server::sync_status::SyncStatus::new(now),
        repos_yaml,
        reload_bus: Arc::new(ReloadBus::new()),
        presence,
        occupancy,
        presence_event_tx,
        last_outcome: Arc::new(parking_lot::Mutex::new(
            crate::server::refresh::RefreshOutcome::skipped(),
        )),
        attribution: default_attribution_backend(),
        auth: Arc::new(AuthState::from_env()),
        events_log: events_log.clone(),
    };
    // Hydrate presence + occupancy from `~/.local/lain/state/<stem>.json`
    // when the file exists, and install a persist callback so every
    // subsequent mutation (claim, release, register, expire) flushes
    // back to disk. Same rationale as `LainServer::new`: errors here
    // propagate so a corrupted snapshot surfaces at construction
    // instead of half-hydrating the registry mid-session.
    server.load_state()?;
    server.install_persist_callback();
    Ok(server)
}

impl LainServer {
    /// Single-workspace constructor: build a `LainServer` whose tool
    /// surface is the single workspace's. `workspace` is the user's
    /// checkout; `memory_path` is the sled DB path (usually
    /// `<workspace>/.lain/graph.bin`). `embedding_model` is an optional
    /// path to an ONNX bi-encoder model dir; when `Some`,
    /// `semantic_search` becomes live; when `None`, the embedder
    /// runs in stub mode and the tool returns an empty result.
    ///
    /// Hydrates presence + occupancy from the state file under
    /// `state_path_for_workspace(workspace)`, then installs the
    /// persist callback so every mutation flushes back to disk.
    pub fn new(
        workspace: &Path,
        memory_path: &Path,
        embedding_model: Option<&Path>,
    ) -> Result<Self, LainError> {
        let config = LainConfig {
            workspace: workspace.to_path_buf(),
            memory_path: memory_path.to_path_buf(),
        };

        let tuning = Arc::new(load_tuning_config(workspace));

        let graph = GraphDatabase::new(memory_path)?;
        let overlay = VolatileOverlay::new();

        let embedder = if let Some(model_path) = embedding_model {
            let (model, tokenizer_path) = NlpEmbedder::resolve_model_paths(model_path);
            NlpEmbedder::with_max_threads(
                &model,
                &tokenizer_path,
                tuning.ingestion.nlp_max_threads,
            )?
        } else {
            // No --embedding-model CLI arg; fall back to LAIN_EMBEDDING_MODEL
            // env var (handled inside NlpEmbedder::new_with_threads).
            NlpEmbedder::new_with_threads(tuning.ingestion.nlp_max_threads)?
        };

        if embedder.is_stub() {
            info!("NLP embedder running in stub mode - semantic search unavailable");
        }

        // Cross-encoder sits beside the bi-encoder model by default.
        // Override via LAIN_CROSS_ENCODER env var.
        let cross_dir = std::env::var("LAIN_CROSS_ENCODER")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".local/lain/models/cross-encoder")
            });
        let cross_encoder = CrossEncoder::from_dir_with_threads(
            &cross_dir,
            tuning.ingestion.nlp_max_threads,
        );
        if cross_encoder.is_active() {
            info!("Cross-encoder reranker active (from {:?})", cross_dir);
        } else {
            info!("Cross-encoder reranker disabled (no model at {:?})", cross_dir);
        }

        let git = Arc::new(Mutex::new(GitSensor::new(workspace)?));
        let lsp_pool = Arc::new(LspPool::new(workspace, tuning.ingestion.lsp_pool_size)?);

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            cross_encoder.clone(),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            workspace.to_path_buf(),
        );

        info!("Lain server initialized");
        let now = SystemTime::now();
        let (presence_event_tx, _) = broadcast::channel(PRESENCE_EVENT_CHANNEL_CAPACITY);
        // P1 #2: open the events log before `memory_path` is moved into
        // the LainServer struct. `events_log_path_from_config` only
        // borrows the path; we clone the Arc into the struct below.
        let events_log_path = LainServer::events_log_path_from_config(memory_path);
        let events_log = Arc::new(
            EventsLog::open(&events_log_path).expect("open events.jsonl"),
        );
        let server = Self {
            config,
            graph,
            overlay,
            embedder,
            federation_workspaces: None,
            cross_encoder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: None,
            federation_transport: None,
            federation_port: None,
            started_at: now,
            sync_status: crate::server::sync_status::SyncStatus::new(now),
            repos_yaml: None,
            reload_bus: Arc::new(ReloadBus::new()),
            presence: Arc::new(PresenceRegistry::new()),
            occupancy: Arc::new(OccupancyMap::new()),
            presence_event_tx,
            last_outcome: Arc::new(parking_lot::Mutex::new(
                crate::server::refresh::RefreshOutcome::skipped(),
            )),
            attribution: default_attribution_backend(),
            auth: Arc::new(AuthState::from_env()),
            events_log: events_log.clone(),
        };
        // Hydrate presence + occupancy from `~/.local/lain/state/<stem>.json`
        // when the file exists. Idempotent: missing file is a no-op.
        // JSON / IO errors here propagate so a corrupted snapshot
        // surfaces at construction rather than half-hydrating the
        // registry mid-session.
        server.load_state()?;
        server.install_persist_callback();
        Ok(server)
    }

    /// Federation-aware constructor. Builds a `LainServer` whose tool surface
    /// is the federation's (`list_repos`, `get_repo_info`, `search_org`,
    /// `get_federation_health`, `get_cross_repo_blast_radius*`).
    ///
    /// Unlike `LainServer::new`, this does **not** point at a single
    /// workspace: the federation manages N repos, so per-repo
    /// git/lsp/embedder/graph components are placeholders seeded from a
    /// throwaway temp git repo (mirrors `cmds/server.rs`'s pre-Task-24
    /// behavior). Federation tools are dispatched in `mcp/handler.rs`
    /// before they touch the executor's underlying services, so the
    /// placeholder wiring is sufficient.
    ///
    /// Pair with `serve()` to start the MCP loop on the chosen transport.
    ///
    /// This shim picks a platform-default [`AttributionBackend`]
    /// (procfs on Linux, noop elsewhere). Callers that need a specific
    /// backend — notably the `lain server` CLI, which has to honor
    /// `--no-process-attribution` and pick `LsofBackend` on macOS —
    /// should use `with_federation_with_attribution` instead.
    pub fn with_federation(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        repos_yaml: Option<PathBuf>,
        embedding_model: Option<&Path>,
    ) -> Result<Self, LainError> {
        Self::with_federation_with_attribution(
            federation,
            transport,
            port,
            repos_yaml,
            default_attribution_backend(),
            embedding_model,
        )
    }

    /// Same as [`Self::with_federation`] but lets the caller supply an
    /// explicit [`AttributionBackend`]. The chosen backend is stored on
    /// the server (in `self.attribution`) and handed to the background
    /// [`AttributionWatcher`] so it has the same view as the CLI.
    /// `embedding_model` is an optional path to an ONNX bi-encoder
    /// model dir; when `Some`, `semantic_search` becomes live; when
    /// `None`, the embedder runs in stub mode and the tool returns an
    /// empty result.
    pub fn with_federation_with_attribution(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        repos_yaml: Option<PathBuf>,
        attribution: Arc<dyn AttributionBackend>,
        embedding_model: Option<&Path>,
    ) -> Result<Self, LainError> {
        build_federation_server(
            federation,
            transport,
            port,
            repos_yaml,
            attribution,
            embedding_model,
            None,
            None,
        )
    }

    /// Same as `with_federation` but also registers the workspace MCP
    /// tools (`list_workspaces`, `get_active_workspace`, `get_workspace`,
    /// `get_workspace_graph`) against the supplied `WorkspacesFile`. Used
    /// when the server is started with `--workspace <name>` and a
    /// `workspaces.yaml` is present next to `repos.yaml`.
    ///
    /// This shim picks a platform-default [`AttributionBackend`] (see
    /// [`Self::with_federation`] for details). Callers that need a
    /// specific backend — notably the `lain server` CLI — should use
    /// [`Self::with_federation_and_workspaces_with_attribution`].
    pub fn with_federation_and_workspaces(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        workspaces: Arc<WorkspacesFile>,
        repos_yaml: Option<PathBuf>,
        embedding_model: Option<&Path>,
    ) -> Result<Self, LainError> {
        Self::with_federation_and_workspaces_with_attribution(
            federation,
            transport,
            port,
            workspaces,
            repos_yaml,
            default_attribution_backend(),
            embedding_model,
        )
    }

    /// Same as [`Self::with_federation_and_workspaces`] but lets the
    /// caller supply an explicit [`AttributionBackend`]. See
    /// [`Self::with_federation_with_attribution`] for `embedding_model`.
    pub fn with_federation_and_workspaces_with_attribution(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        workspaces: Arc<WorkspacesFile>,
        repos_yaml: Option<PathBuf>,
        attribution: Arc<dyn AttributionBackend>,
        embedding_model: Option<&Path>,
    ) -> Result<Self, LainError> {
        build_federation_server(
            federation,
            transport,
            port,
            repos_yaml,
            attribution,
            embedding_model,
            Some(workspaces),
            None,
        )
    }
}
