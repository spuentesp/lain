//! Ingest pipeline + Lain server orchestration.
//!
//! `LainServer` wires together every component the MCP layer needs: the
//! persistent graph, the volatile overlay, the embedder + cross-encoder,
//! the git sensor, the LSP pool, and the tool executor. It owns the
//! federation handle (set by `with_federation*`) and the background-sync
//! job. The three sibling modules — `ingestion`, `jobs`, `scan` — carry
//! the ingest pipeline itself.

pub mod ingestion;
pub mod jobs;
pub mod resolve;
pub mod scan;

/// Per-process counter that disambiguates the staging dir used by
/// `LainServer::with_federation`. The placeholder `LainServer` builds a
/// throwaway git repo at `/tmp/lain-federation-{pid}-{counter}` so
/// parallel tests in the same process don't race on a shared path.
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

use crate::server::error::LainError;
use crate::server::federation::config::RepoConfig;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::repo_id::RepoId;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::graph::GraphDatabase;
use crate::server::lsp::LspPool;
use crate::server::nlp::{CrossEncoder, NlpEmbedder};
use crate::server::overlay::{OverlayDiff, VolatileOverlay};
use crate::server::presence::{save_pair as save_presence_pair, load_pair as load_presence_pair, OccupancyMap, PresenceEvent, PresenceRegistry};
use crate::server::reload::ReloadBus;
use crate::server::tools::ToolExecutor;
use crate::server::tuning::{load_tuning_config, TuningConfig};
use crate::server::git::GitSensor;
use crate::server::attribution::{AttributionBackend, AttributionWatcher, LsofBackend, NoopBackend, ProcFsBackend};
use crate::config::state_path_for_workspace;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tracing::info;

/// Server configuration
#[derive(Clone)]
/// Server configuration
pub struct LainConfig {
    /// Data-anchor directory. In single-workspace mode (built via
    /// `LainServer::new`) this is the user's actual workspace — what
    /// `tuning.toml` / `.git/` / `.lsp/` / the tool executor's
    /// workspace all point at. In federation mode (built via
    /// `with_federation*`) this is the placeholder staging dir at
    /// `/tmp/lain-federation-{pid}-{counter}` — federation tools
    /// don't read it (they go through the `FederatedIndex` handle),
    /// per-repo structural tools are bound to the single-repo's real
    /// graph via the binding fix, and git/LSP are placeholders too.
    ///
    /// The state-file stem (used by `state_path()`) is *not* derived
    /// from this field in federation mode — `repos_yaml` is preferred
    /// so restarts pick up the same state. See `state_path()`.
    pub workspace: PathBuf,
    /// Path to `<workspace>/.lain/graph.bin` — the sled database
    /// backing `ctx.graph`. Always `<workspace>/.lain/graph.bin` in
    /// both single-workspace and federation modes; `workspace` is
    /// the staging dir in federation mode (so this is the staging
    /// dir's graph path, which the placeholder executor opens).
    pub memory_path: PathBuf,
}

/// Capacity of the `PresenceEvent` broadcast bus. Generous for an
/// interactive session; if a slow consumer falls behind, `send`
/// returns Err and the event is dropped (the registry/occupancy
/// state itself remains consistent on the server side).
const PRESENCE_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Pick the platform-appropriate default [`AttributionBackend`] for
/// constructors that don't take an explicit backend (i.e. the
/// `LainServer::new` / `with_federation` / `with_federation_and_workspaces`
/// constructors — every test and embedder that isn't the `lain server`
/// CLI). The CLI uses the `_with_attribution` variants and picks its
/// own backend based on platform + `--no-process-attribution`.
fn default_attribution_backend() -> Arc<dyn AttributionBackend> {
    if cfg!(target_os = "linux") {
        Arc::new(ProcFsBackend)
    } else if cfg!(target_os = "macos") {
        Arc::new(LsofBackend)
    } else {
        Arc::new(NoopBackend)
    }
}

/// Allocate a unique staging dir for federation-mode servers. The
/// placeholder `LainServer` builds a throwaway git repo at
/// `/tmp/lain-federation-{pid}-{counter}` so parallel tests in the
/// same process don't race on a shared path.
fn allocate_staging_dir() -> Result<PathBuf, LainError> {
    let counter = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
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

/// Spawn the background task that prunes expired sessions + claim
/// TTLs every 5 seconds and broadcasts `PresenceEvent` notifications.
/// The `JoinHandle` is intentionally dropped — the task lives for
/// the lifetime of the process. (For graceful shutdown we'd store
/// the handle and abort it; not needed for MVP.)
fn spawn_presence_expiry_loop(
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    tx: broadcast::Sender<(u64, PresenceEvent)>,
    events_log: Arc<crate::server::events_log::EventsLog>,
) {
    let p = presence.clone();
    let o = occupancy.clone();
    let t = tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            expiry_tick(&p, &o, &t, &events_log);
        }
    });
}

/// One tick of the expiry loop. Expiring a session must also release
/// every claim it held: previously `expire_stale` removed only the
/// session, so a dead agent's claims (default: no TTL) persisted
/// forever — found live, where claims from days-old smoke tests kept
/// conflicting with new claims. Emits `HeartbeatExpired` plus one
/// `ClaimReleased` per released path, all with durable events-log ids.
fn expiry_tick(
    presence: &PresenceRegistry,
    occupancy: &OccupancyMap,
    tx: &broadcast::Sender<(u64, PresenceEvent)>,
    events_log: &crate::server::events_log::EventsLog,
) {
    let emit = |ev: PresenceEvent| {
        let eid = events_log.append(&ev);
        let _ = tx.send((eid, ev));
    };
    for id in presence.expire_stale() {
        emit(PresenceEvent::HeartbeatExpired(id.clone()));
        for path in occupancy.release_all_for(&id) {
            emit(PresenceEvent::ClaimReleased {
                agent_id: id.clone(),
                path,
            });
        }
    }
    for (agent_id, path) in occupancy.expire_by_ttl() {
        emit(PresenceEvent::ClaimReleased { agent_id, path });
    }
}

/// Start the attribution watcher (inotify on each registered repo's
/// checkout for live edit attribution). The handle is dropped — the
/// thread lives until the channel closes. Previously this watched
/// `repos.yaml`'s parent dir, which auto-claimed unrelated files
/// (server logs, scratch files) under the single-agent heuristic;
/// now it watches exactly `FederatedIndex::repo_paths()`. Repos
/// added by a hot-reload after startup are not watched until the
/// next server restart.
fn start_attribution_watcher(
    attribution: Arc<dyn AttributionBackend>,
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    tx: broadcast::Sender<(u64, PresenceEvent)>,
    events_log: Arc<crate::server::events_log::EventsLog>,
    repo_roots: Vec<PathBuf>,
) {
    let _ = AttributionWatcher::new_with_backend(
        attribution,
        presence,
        occupancy,
        tx,
        events_log,
        repo_roots,
    )
    .start();
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
        crate::server::events_log::EventsLog::open(&events_log_path)
            .expect("open events.jsonl"),
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
        auth: Arc::new(crate::server::auth::AuthState::from_env()),
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

/// MCP transport for federation-mode servers. Stdio for local agents,
/// Http for network-reachable deployments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http,
}

/// Main Lain server
#[derive(Clone)]
pub struct LainServer {
    pub config: LainConfig,
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: CrossEncoder,
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tool_executor: ToolExecutor,
    pub tuning: Arc<TuningConfig>,
    /// Outcome of the most recent startup re-index. Written by the
    /// re-index spawn in `LainMcpServer::run_stdio` / `run_http`;
    /// read by `ToolExecutor::get_health` and (in step 3) by the tool
    /// dispatcher for the failure banner. Step 1 of the staleness
    /// fix: the failure was previously invisible because it went to
    /// `tracing::warn` and `lain mcp` never inits tracing.
    pub last_outcome: Arc<parking_lot::Mutex<crate::server::refresh::RefreshOutcome>>,
    /// Monotonic counter used as the `revision` field of every
    /// `OverlayDiff` this process broadcasts.
    overlay_revision: Arc<AtomicU64>,
    /// Federation handle. `Some` for federation-mode servers (constructed
    /// via `with_federation`); `None` for single-workspace servers
    /// (constructed via `new`).
    federation: Option<Arc<FederatedIndex>>,
    /// Per-key auth + rate limit (P0 #1). Populated from `LAIN_API_KEYS`
    /// and `LAIN_RATE_LIMIT_RPM` env vars at server startup. Cloned into
    /// the HTTP request handler so dev mode (no env) stays zero-cost.
    pub auth: Arc<crate::server::auth::AuthState>,
    /// Durable SSE event log (P1 #2). Captures every `PresenceEvent`
    /// broadcast on the SSE channel with a monotonic `event_id: u64`,
    /// supports replay-after-id via `events.jsonl` so SSE subscribers
    /// that reconnect with `Last-Event-ID: N` see every event since N.
    /// Lives in the same state dir as `audit.jsonl`.
    pub events_log: Arc<crate::server::events_log::EventsLog>,
    /// Workspaces file passed to `LainMcpServer` when `with_federation_and_workspaces`
    /// is used. `Some` when a workspace is active; `None` for the
    /// all-repos path (no workspaces.yaml).
    ///
    /// The cell is wrapped in `Arc<RwLock<WorkspacesFile>>` and the
    /// *same* `Arc` is handed to `LainMcpServer` during `serve()`. This
    /// is what makes hot-reload of `workspaces.yaml` visible to the
    /// in-flight MCP dispatcher without restarting the server:
    /// `run_rebuild` writes `*lock = new_workspaces`, and the next
    /// `list_workspaces` / `get_workspace` dispatch reads through the
    /// same lock. Without the shared `Arc`, the LainMcpServer captures
    /// its own `Arc<WorkspacesFile>` snapshot at construction time and
    /// stale data is served indefinitely.
    federation_workspaces: Option<Arc<RwLock<WorkspacesFile>>>,
    /// Transport chosen at `with_federation` time. Consumed by `serve`.
    federation_transport: Option<Transport>,
    /// Port chosen at `with_federation` time. Consumed by `serve`.
    federation_port: Option<u16>,
    /// Process start time, captured at construction. Immutable for the
    /// life of the server; surfaced via `get_server_status`.
    started_at: SystemTime,
    /// Sync attempt bookkeeping shared across the ingest/sync paths.
    /// Lives in its own module so new fields don't grow the LainServer
    /// impl block. See [`crate::server::sync_status::SyncStatus`].
    sync_status: crate::server::sync_status::SyncStatus,
    /// Path to the `repos.yaml` this server was launched with, if any.
    /// `None` for single-workspace servers (no federation config). Used
    /// to record the project in `~/.config/lain/recent_projects` and to
    /// tag the server status payload.
    repos_yaml: Option<PathBuf>,
    /// Hot-reload signal bus. Always allocated (single-workspace and
    /// federation-mode servers both hold one); the actual rebuild loop
    /// is only spawned in federation mode. Wrapped in `Arc` so the
    /// watcher, Unix socket listener, MCP handler, and rebuild task can
    /// all share a single bus.
    reload_bus: Arc<ReloadBus>,
    /// Presence registry: which agents are connected, plus their heartbeat.
    /// Wrapped in `Arc` so the MCP dispatcher, attribution watcher, and SSE
    /// endpoint can share the same registry without juggling lifetimes.
    /// Spawned via `PresenceRegistry::new()` (default 60s expiry).
    pub presence: Arc<PresenceRegistry>,
    /// Occupancy map: which files/symbols each agent has claimed. Wrapped
    /// in `Arc` for the same reason as `presence`.
    pub occupancy: Arc<OccupancyMap>,
    /// Broadcast sender for `PresenceEvent`s, tagged with the durable
    /// `event_id` assigned by [`crate::server::events_log::EventsLog`]
    /// at emit time. Subscribers (SSE handler, etc.) clone the receiver
    /// and stream events to clients; the id lets a reconnecting client
    /// resume via `Last-Event-ID` (see `serve_sse`). Capacity 256 is
    /// generous for an interactive session; if a slow consumer falls
    /// behind, `send` returns Err and the event is dropped (the
    /// registry/occupancy state itself remains consistent on the
    /// server side).
    pub presence_event_tx: broadcast::Sender<(u64, PresenceEvent)>,
    /// Strategy used by the background attribution watcher to map a
    /// workspace path to the PID that wrote it. The `lain server` CLI
    /// picks this at startup based on platform (`ProcFsBackend` on
    /// Linux, `LsofBackend` on macOS) and the `--no-process-attribution`
    /// flag (which forces `NoopBackend`). Stored here so it is shared
    /// with the [`AttributionWatcher`] the constructor spawns.
    pub attribution: Arc<dyn AttributionBackend>,
}

impl LainServer {
    pub fn new(workspace: &Path, memory_path: &Path, embedding_model: Option<&Path>) -> Result<Self, LainError> {
        let config = LainConfig {
            workspace: workspace.to_path_buf(),
            memory_path: memory_path.to_path_buf(),
        };

        let tuning = Arc::new(load_tuning_config(workspace));

        let graph = GraphDatabase::new(memory_path)?;
        let overlay = VolatileOverlay::new();

        let embedder = if let Some(model_path) = embedding_model {
            let (model, tokenizer_path) =
                crate::server::nlp::NlpEmbedder::resolve_model_paths(model_path);
            crate::server::nlp::NlpEmbedder::with_max_threads(
                &model,
                &tokenizer_path,
                tuning.ingestion.nlp_max_threads,
            )?
        } else {
            // No --embedding-model CLI arg; fall back to LAIN_EMBEDDING_MODEL
            // env var (handled inside NlpEmbedder::new_with_threads).
            crate::server::nlp::NlpEmbedder::new_with_threads(tuning.ingestion.nlp_max_threads)?
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
            crate::server::events_log::EventsLog::open(&events_log_path)
                .expect("open events.jsonl"),
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
            auth: Arc::new(crate::server::auth::AuthState::from_env()),
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

    /// Federation accessor. Returns `None` for single-workspace servers.
    pub fn federation(&self) -> Option<&Arc<FederatedIndex>> {
        self.federation.as_ref()
    }

    /// Shared reload bus accessor. Always returns a bus; the bus is
    /// lazily initialized on the first call when the field is missing
    /// (only the case for `LainServer::new` before this commit — we
    /// construct it eagerly today but the accessor is the contract).
    pub fn reload_bus(&self) -> Arc<ReloadBus> {
        Arc::clone(&self.reload_bus)
    }

    /// Borrowed handle to the presence registry. The field itself is
    /// already `pub`, but this accessor keeps the contract consistent
    /// with the other `Arc`-sharing accessors (`reload_bus`,
    /// `workspaces_handle`).
    pub fn presence(&self) -> &Arc<PresenceRegistry> {
        &self.presence
    }

    /// Borrowed handle to the occupancy map. Same rationale as
    /// `presence()`.
    pub fn occupancy(&self) -> &Arc<OccupancyMap> {
        &self.occupancy
    }

    /// Borrowed handle to the `PresenceEvent` broadcast sender. Subscribers
    /// clone the receiver side (`tx.subscribe()`) to stream events to
    /// their own consumers. Events are tagged with their durable
    /// `EventsLog` id (see [`Self::emit_presence_event`]).
    pub fn presence_event_tx(&self) -> &broadcast::Sender<(u64, PresenceEvent)> {
        &self.presence_event_tx
    }

    /// Emit a presence event: append it to the durable events log
    /// (assigning its monotonic `event_id`) and broadcast the
    /// `(event_id, event)` pair to all subscribers. This is the only
    /// path live events should take — sending on `presence_event_tx`
    /// directly would skip durability and break SSE `Last-Event-ID`
    /// resume.
    pub fn emit_presence_event(&self, event: PresenceEvent) {
        let id = self.events_log.append(&event);
        let _ = self.presence_event_tx.send((id, event));
    }

    /// Borrowed handle to the [`AttributionBackend`] this server was
    /// constructed with. Surfaced for diagnostics and tests; the
    /// background [`AttributionWatcher`] already shares the same `Arc`
    /// so callers don't need to plumb it further.
    pub fn attribution(&self) -> &Arc<dyn AttributionBackend> {
        &self.attribution
    }

    /// How long a presence session stays valid after the last heartbeat.
    /// The MCP `register_agent` tool surfaces this in its `expires_at_unix`
    /// reply so agents know when to renew.
    pub fn presence_expires_after(&self) -> std::time::Duration {
        self.presence.expires_after()
    }

    /// Process start time, captured at construction. Used by
    /// `get_server_status` to report uptime.
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// Last successful sync time. Updated via [`Self::record_sync`];
    /// consumed by `get_server_status`.
    pub fn last_sync_at(&self) -> SystemTime {
        self.sync_status.last_sync_at()
    }

    /// Most recent ingest/sync error message, if any.
    pub fn last_error(&self) -> Option<String> {
        self.sync_status.last_error()
    }

    /// Mark a sync attempt as successful: clear `last_error` and bump
    /// `last_sync_at` to now. Called by ingest/sync paths that finish
    /// without an error; errors should call [`Self::record_last_error`]
    /// instead.
    pub fn record_sync(&self) {
        self.sync_status.record_ok();
    }

    /// Record an error message from the ingest/sync paths and refresh
    /// `last_sync_at` to the current time so the operator can see when
    /// the last attempt was.
    pub fn record_last_error(&self, msg: impl Into<String>) {
        self.sync_status.record_error(msg);
    }

    /// Transport for the active MCP server. `None` for single-workspace
    /// servers (not federation-mode); some for federation-mode.
    pub fn transport(&self) -> Option<Transport> {
        self.federation_transport
    }

    /// TCP port for HTTP federation-mode servers; `None` for stdio or
    /// single-workspace servers.
    pub fn port(&self) -> Option<u16> {
        self.federation_port
    }

    /// Path to the `repos.yaml` this server was launched with, if any.
    /// `None` for single-workspace servers.
    pub fn repos_yaml(&self) -> Option<&Path> {
        self.repos_yaml.as_deref()
    }

    /// Number of repos in the live federation, or 0 for single-workspace
    /// servers.
    pub fn repo_count(&self) -> usize {
        self.federation.as_ref().map(|f| f.list_repos().len()).unwrap_or(0)
    }

    /// Number of workspaces in the loaded `workspaces.yaml`, or 0 when
    /// none was supplied. Reads through the `Arc<RwLock<...>>` slot so a
    /// hot-reload that swaps `set_workspace` is reflected on the next
    /// call.
    pub fn workspace_count(&self) -> usize {
        self.federation_workspaces
            .as_ref()
            .map(|w| w.read().workspaces.len())
            .unwrap_or(0)
    }

    /// Consume the server and run the federation-mode MCP loop on the
    /// transport chosen at construction time. Errors out if the server
    /// was not built via `with_federation`.
    pub async fn serve(self) -> Result<(), LainError> {
        // Clone the whole server up front so the MCP layer can hold a
        // shared `Arc<LainServer>` for the 8 multiplayer tools while the
        // current `self` is consumed building the MCP handler. Every
        // inner Arc (presence registry, occupancy map, broadcast
        // sender) is shared by the clone, so the cost is a handful of
        // Arc bumps rather than a deep copy.
        let server_arc = Arc::new(self.clone());
        let federation = self.federation.ok_or_else(|| {
            LainError::Other(
                "LainServer::serve() called on a non-federation server (use LainServer::new for single-workspace)".into(),
            )
        })?;
        let transport = self.federation_transport.ok_or_else(|| {
            LainError::Other("LainServer::serve(): missing transport (internal)".into())
        })?;
        let port = self.federation_port.unwrap_or(9999);

        let workspaces = self.federation_workspaces.as_ref().map(Arc::clone);
        let mcp = match workspaces {
            Some(ws) => crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
                self.tool_executor, federation, ws,
            ),
            None => crate::server::mcp::handler::LainMcpServer::with_federation(self.tool_executor, federation),
        }
        .with_status(
            Some(transport),
            Some(port),
            self.started_at,
            self.sync_status.last_sync_at_handle(),
            self.sync_status.last_error_handle(),
        )
        .with_reload_bus(Arc::clone(&self.reload_bus))
        .with_server(server_arc);
        match transport {
            Transport::Http => mcp
                .run_http(port)
                .await
                .map_err(|e| LainError::Mcp(format!("HTTP transport: {e}"))),
            Transport::Stdio => mcp
                .run_stdio()
                .await
                .map_err(|e| LainError::Mcp(format!("stdio transport: {e}"))),
        }
    }

    pub fn clone_for_background(&self) -> Self {
        self.clone()
    }

    /// Allocate the next overlay-diff revision id. Sidecars use this to
    /// detect drops in the broadcast bus.
    pub fn next_revision(&self) -> crate::server::overlay::RevisionId {
        self.overlay_revision.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Broadcast a single node insertion. Sidecars pull these diffs from
    /// the broadcast bus and merge them into their in-memory overlay.
    /// Only fires when this process is an *owner* — the sidecar has its
    /// graph opened read-only (Task 3) and never reaches the call sites
    /// below `check_writable`, so it cannot broadcast by accident.
    pub fn broadcast_overlay_insert(
        &self,
        node: crate::server::schema::GraphNode,
    ) {
        crate::server::overlay::broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![node],
            removed: vec![],
            updated: vec![],
        });
    }

    pub fn is_git_repo(&self) -> bool {
        self.git.lock().is_valid()
    }

    /// Add a repo to the live federation, then project its nodes/edges
    /// into the global backend. No-op if `self.federation` is `None`
    /// (single-workspace mode).
    ///
    /// `repo` is the `RepoConfig` entry as written to `repos.yaml`. The
    /// data directory for the per-repo indexer is computed from the
    /// server's stored `repos.yaml` path (or, when none is set, from
    /// the default `./.lain/federation`). The `data_dir` resolution is
    /// performed by the caller (`run_rebuild`) and passed in via the
    /// `data_dir` argument; here we just call through to the
    /// federation.
    pub async fn add_repo(
        &self,
        repo: &RepoConfig,
        data_dir: &Path,
    ) -> Result<(), LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            LainError::Other("LainServer::add_repo called on a non-federation server".into())
        })?;
        let source = crate::server::federation::config::FederationConfig::default()
            .build_source_for(repo)
            .map_err(|e| LainError::Config(format!("build_source_for({}): {e}", repo.id)))?;
        // `WorkspaceDirSource::fetch` is a no-op; `LocalCloneSource` and
        // `ShallowCloneSource` actually clone. Hot-reload only sees
        // already-on-disk sources (`workspace_dir`), but we still call
        // `fetch` so adding a freshly-written `local_clone` entry also
        // works end-to-end.
        source.fetch().await?;
        let repo_id = source.id().clone();
        fed.add_repo(source, data_dir).await?;
        fed.project_repo(&repo_id).await?;
        self.record_sync();
        Ok(())
    }

    /// Remove a repo from the live federation. No-op if `self.federation`
    /// is `None` (single-workspace mode).
    pub fn remove_repo(&self, repo_id: &str) -> Result<(), LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            LainError::Other("LainServer::remove_repo called on a non-federation server".into())
        })?;
        let rid = RepoId::new(repo_id)
            .map_err(|e| LainError::Config(format!("invalid repo id '{repo_id}': {e}")))?;
        fed.remove_repo(&rid)?;
        self.record_sync();
        Ok(())
    }

    /// Replace the workspace file the server exposes through MCP. Called
    /// by `run_rebuild` after re-reading `workspaces.yaml`. Writes
    /// through the SAME `Arc<RwLock<WorkspacesFile>>` the
    /// `LainMcpServer` constructed by `serve` is holding, so the very
    /// next dispatch of `list_workspaces` / `get_workspace` /
    /// `get_workspace_graph` observes the new contents without a
    /// server restart.
    ///
    /// No-op for single-workspace servers (no workspaces file).
    pub fn set_workspace(&self, workspaces: Arc<WorkspacesFile>) {
        if let Some(slot) = &self.federation_workspaces {
            *slot.write() = (*workspaces).clone();
        }
    }

    /// Cheap snapshot of the loaded workspaces file. Used by
    /// `run_rebuild` to seed a `set_workspace` no-op diff when nothing
    /// changed and to test the slot swap. Clones the inner
    /// `WorkspacesFile`, so callers should avoid calling this in tight
    /// loops; reads are cheap on the lock side.
    pub fn workspaces_snapshot(&self) -> Option<Arc<WorkspacesFile>> {
        self.federation_workspaces
            .as_ref()
            .map(|slot| Arc::new(slot.read().clone()))
    }

    /// Shared handle to the live workspaces lock, or `None` for
    /// single-workspace servers. This is the SAME
    /// `Arc<RwLock<WorkspacesFile>>` the `LainMcpServer` constructed
    /// by `serve()` holds inside its `workspaces` field, so callers
    /// can verify the hot-reload fix end-to-end: a `set_workspace`
    /// followed by a `handle.read()` on any thread (including from a
    /// JSON-RPC dispatch) sees the new value without any
    /// synchronization beyond the rwlock's own barriers. Compared with
    /// `Arc::ptr_eq` against the `LainMcpServer`'s `workspaces` field
    /// it confirms the single-source-of-truth wiring.
    pub fn workspaces_handle(&self) -> Option<Arc<RwLock<WorkspacesFile>>> {
        self.federation_workspaces.as_ref().map(Arc::clone)
    }

    pub async fn shutdown(&self) {
        info!("Shutting down Lain server...");
        self.lsp_pool.shutdown_all().await;
    }

    /// Path on disk where this server's `PresenceRegistry` +
    /// `OccupancyMap` JSON snapshot is persisted. Derived from a
    /// *stable* identifier — for federation servers, the
    /// `repos.yaml` path the operator launched with; for single-workspace
    /// servers, the workspace dir itself.
    ///
    /// Wishlist #4 fix: previously this derived from the per-process
    /// `/tmp/lain-federation-{pid}-{counter}` staging dir, which meant
    /// every restart picked a brand-new state file and `load_state`
    /// silently hydrated nothing. `persistence_e2e.rs` still passed
    /// because it hardcodes the stem in `save_pair` / `load_pair`.
    /// Operators on this machine accumulated 370 stale `.json` files
    /// in `~/.local/lain/state/` before the fix landed.
    pub fn state_path(&self) -> PathBuf {
        if let Some(repos) = self.repos_yaml.as_deref() {
            state_path_for_workspace(repos)
        } else {
            state_path_for_workspace(&self.config.workspace)
        }
    }

    /// Directory that holds the per-server `audit.jsonl` log
    /// (Task 2.3, PR 2). Sibling of the persisted-state JSON file
    /// returned by `state_path()`: both live under `state_dir()` from
    /// `crate::config`, so the audit log survives across restarts
    /// alongside the occupancy snapshot.
    ///
    /// Falls back to `state_dir()` itself when the state path has no
    /// parent (degenerate but possible if a future caller constructed
    /// the server from a workspace path that resolves to a bare
    /// filename). `append_edit_event` writes to this directory via
    /// the audit module, which `mkdir`s nothing on its own — the
    /// state dir is created lazily by the persist callback when the
    /// first presence change happens, so the audit directory is
    /// already present by the time any claim reaches us in practice.
    pub fn state_dir_for_audit(&self) -> std::path::PathBuf {
        self.state_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::state_dir)
    }

    /// Directory that holds the per-server `events.jsonl` log (P1 #2:
    /// SSE replay). Same parent directory as `audit.jsonl` so both
    /// live in the same state dir and survive restarts together.
    pub fn events_log_path_from_config(mem_path: &std::path::Path) -> std::path::PathBuf {
        mem_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::state_dir)
    }

    /// Static-graph generation as a Unix-epoch seconds value, or `None`
    /// if no successful re-index has happened in this process. Used by
    /// the JSON-RPC `_meta.static_graph_generation` envelope field (P1
    /// #1) so the LLM knows how fresh the static graph is without
    /// needing a separate `list_repos` round-trip.
    pub fn static_graph_generation_unix(&self) -> Option<i64> {
        use crate::server::refresh::RefreshResult;
        use std::time::UNIX_EPOCH;
        let outcome = self.last_outcome.lock();
        if !matches!(outcome.result, RefreshResult::Ok) {
            return None;
        }
        outcome.started_at.duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }

    /// Persist the live `PresenceRegistry` + `OccupancyMap` to the
    /// server's state file. Called by the persist callback installed
    /// in the three constructors, so callers do not need to invoke
    /// it directly — but it remains a `pub` method so `cron`-style
    /// background ops can force a flush.
    pub fn save_state(&self) -> Result<(), LainError> {
        let path = self.state_path();
        save_presence_pair(&path, &self.presence, &self.occupancy)
            .map_err(|e| LainError::Other(format!("save_state({}): {e}", path.display())))
    }

    /// Hydrate the live `PresenceRegistry` + `OccupancyMap` from the
    /// server's state file, if any. Idempotent: missing file is a
    /// no-op. Used by the three constructors right after the
    /// registries are built.
    pub fn load_state(&self) -> Result<(), LainError> {
        let path = self.state_path();
        load_presence_pair(&path, &self.presence, &self.occupancy)
            .map_err(|e| LainError::Other(format!("load_state({}): {e}", path.display())))
    }

    /// Install a persist callback on `presence` and `occupancy` that
    /// drives `save_state` on every mutation. Called once from each
    /// constructor, immediately after the registries are built and
    /// after `load_state` has hydrated them.
    fn install_persist_callback(&self) {
        let path = self.state_path();
        let presence = Arc::clone(&self.presence);
        let occupancy = Arc::clone(&self.occupancy);
        let cb = move || {
            if let Err(e) = save_presence_pair(&path, &presence, &occupancy) {
                tracing::warn!("persist failed: {e}");
            }
        };
        self.presence.set_persist_callback(cb.clone());
        // `set_persist_callback` takes `impl Fn()`, so rebuild for the
        // second setter rather than trying to share an Arc<dyn Fn()>;
        // each closure captures only `Send + Sync` data so both
        // callbacks are independent and cheap to construct.
        let presence2 = Arc::clone(&self.presence);
        let occupancy2 = Arc::clone(&self.occupancy);
        let path2 = self.state_path();
        let cb2 = move || {
            if let Err(e) = save_presence_pair(&path2, &presence2, &occupancy2) {
                tracing::warn!("persist failed: {e}");
            }
        };
        self.occupancy.set_persist_callback(cb2);
        // Filesystem-as-lock side-effect: anchor the occupancy map to
        // the workspace so `claim_with_session` can write
        // `<workspace>/.lain/locks/<file>.json`. Mirrors the persist
        // callback above; called from all three constructors via
        // `install_persist_callback`.
        self.occupancy.set_workspace_root(&self.config.workspace);
        // Touch the first closure so the compiler does not warn about
        // an unused binding; both callbacks are installed above.
        let _ = cb;
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use crate::server::presence::{AgentKind, AgentMode, ClaimRequest, ClaimIntent};

    /// A session expiring must release its claims: the pre-fix behavior
    /// left dead agents' no-TTL claims in the occupancy map forever
    /// (observed live: days-old smoke-test claims kept conflicting).
    #[test]
    fn expiry_tick_releases_claims_of_expired_sessions() {
        let presence = PresenceRegistry::with_expiry(std::time::Duration::from_millis(20));
        let occupancy = OccupancyMap::new();
        let (tx, _rx) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let log = crate::server::events_log::EventsLog::open(tmp.path()).unwrap();

        let sess = presence.register(
            "ghost".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        let granted = occupancy.claim(
            &sess.id,
            vec![ClaimRequest {
                path: PathBuf::from("a.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None, // no TTL — the case that used to leak
                plan_revision: None,
            }],
        );
        assert_eq!(granted.granted.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(40));
        expiry_tick(&presence, &occupancy, &tx, &log);

        assert!(presence.list_active(true).is_empty(), "session expired");
        assert!(
            occupancy.list_all().is_empty(),
            "claims of an expired session must be released, got: {:?}",
            occupancy.list_all()
        );
    }

    /// Claims WITH a TTL still expire via their own path; sessions whose
    /// claims expire keep living (heartbeat-independent).
    #[test]
    fn expiry_tick_keeps_ttl_claim_path() {
        let presence = PresenceRegistry::new();
        let occupancy = OccupancyMap::new();
        let (tx, _rx) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let log = crate::server::events_log::EventsLog::open(tmp.path()).unwrap();

        let sess = presence.register(
            "ttl-agent".into(),
            AgentKind::Kimi,
            AgentMode::Interactive,
            None,
            None,
        );
        occupancy.claim(
            &sess.id,
            vec![ClaimRequest {
                path: PathBuf::from("b.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: Some(0), // expires immediately
                plan_revision: None,
            }],
        );

        expiry_tick(&presence, &occupancy, &tx, &log);

        assert!(occupancy.list_all().is_empty(), "TTL claim expired");
        assert_eq!(presence.list_active(true).len(), 1, "session alive");
    }
}
