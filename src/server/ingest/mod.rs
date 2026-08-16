//! Ingest pipeline + Lain server orchestration.
//!
//! `LainServer` wires together every component the MCP layer needs: the
//! persistent graph, the volatile overlay, the embedder + cross-encoder,
//! the git sensor, the LSP pool, and the tool executor. It owns the
//! federation handle (set by `with_federation*`) and the background-sync
//! job. The three sibling modules — `ingestion`, `jobs`, `scan` — carry
//! the ingest pipeline itself.

pub mod ingestion;
pub mod scan;
pub mod jobs;

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
use crate::server::presence::{OccupancyMap, PresenceEvent, PresenceRegistry};
use crate::server::reload::ReloadBus;
use crate::server::tools::ToolExecutor;
use crate::server::tuning::{load_tuning_config, TuningConfig};
use crate::server::git::GitSensor;
use crate::server::attribution::AttributionWatcher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tracing::info;

/// Server configuration
#[derive(Clone)]
pub struct LainConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
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
    /// Monotonic counter used as the `revision` field of every
    /// `OverlayDiff` this process broadcasts.
    overlay_revision: Arc<AtomicU64>,
    /// Federation handle. `Some` for federation-mode servers (constructed
    /// via `with_federation`); `None` for single-workspace servers
    /// (constructed via `new`).
    federation: Option<Arc<FederatedIndex>>,
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
    /// Last successful sync time, updated by ingest/sync paths. The
    /// `get_server_status` tool surfaces this so operators can see how
    /// fresh the federation is. Wrapped in `Arc<Mutex<_>>` so `LainServer`
    /// stays `Clone` (the executor sidecar clones the server).
    last_sync_at: Arc<Mutex<SystemTime>>,
    /// Most recent sync/ingest error message, if any. Cleared by
    /// `record_sync()` and set by `record_last_error()`. Surfaced via
    /// `get_server_status`. Wrapped in `Arc<Mutex<_>>` for the same
    /// `Clone` reason as `last_sync_at`.
    last_error: Arc<Mutex<Option<String>>>,
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
    /// Broadcast sender for `PresenceEvent`s. Subscribers (SSE handler in
    /// Task 6, attribution watcher, etc.) clone the receiver and stream
    /// events to clients. Capacity 256 is generous for an interactive
    /// session; if a slow consumer falls behind, `send` returns Err and the
    /// event is dropped (the registry/occupancy state itself remains
    /// consistent on the server side).
    pub presence_event_tx: broadcast::Sender<PresenceEvent>,
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
            let tokenizer_path = model_path.parent().map(|p| p.join("tokenizer.json"))
                .unwrap_or_else(|| PathBuf::from("tokenizer.json"));
            crate::server::nlp::NlpEmbedder::with_max_threads(
                model_path,
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
        let (presence_event_tx, _) = broadcast::channel(256);
        Ok(Self {
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
            last_sync_at: Arc::new(Mutex::new(now)),
            last_error: Arc::new(Mutex::new(None)),
            repos_yaml: None,
            reload_bus: Arc::new(ReloadBus::new()),
            presence: Arc::new(PresenceRegistry::new()),
            occupancy: Arc::new(OccupancyMap::new()),
            presence_event_tx,
        })
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
    pub fn with_federation(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        repos_yaml: Option<PathBuf>,
    ) -> Result<Self, LainError> {
        // Build a minimal executor — same trick as `cmds/server.rs`'s
        // `build_minimal_executor`. Federation tools never reach the
        // executor's underlying services, but `LainMcpServer::with_federation`
        // still requires a constructed one.
        //
        // The staging dir name includes an atomic counter so parallel
        // tests in the same process (which all share `process::id()`)
        // don't race on the same `/tmp/lain-federation-{pid}` path.
        let counter = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!(
            "lain-federation-{}-{}",
            std::process::id(),
            counter
        ));
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
            info!("NLP embedder running in stub mode (federation placeholder)");
        }
        let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
        let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);
        let tuning = Arc::new(load_tuning_config(&ws));

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            crate::server::nlp::CrossEncoder::from_dir(&ws),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            ws.to_path_buf(),
        );

        // Build the federation-aware MCP server eagerly so any wiring
        // problems surface at construction time. We don't store the
        // `LainMcpServer` itself — it's a thin wrapper over `tool_executor`
        // plus `federation`, both of which we already hold — and `serve()`
        // rebuilds it before consuming self.
        let _mcp = crate::server::mcp::handler::LainMcpServer::with_federation(
            tool_executor.clone(),
            Arc::clone(&federation),
        );

        info!("Lain federation server initialized");
        let cross_encoder = crate::server::nlp::CrossEncoder::from_dir(&ws);
        let now = SystemTime::now();

        // Presence layer: allocate registry + occupancy map, build a
        // broadcast channel for `PresenceEvent`s, and spawn the heartbeat
        // expiry loop. The expiry task fires every 5 seconds; on each tick
        // it drops stale sessions from the registry and emits a
        // `HeartbeatExpired` event on the broadcast bus. The `JoinHandle`
        // is intentionally dropped — the task lives for the lifetime of
        // the process. (For graceful shutdown we'd store the handle and
        // abort it; not needed for MVP.)
        let presence = Arc::new(PresenceRegistry::new());
        let occupancy = Arc::new(OccupancyMap::new());
        let (presence_event_tx, _) = broadcast::channel(256);
        let presence_for_expiry = presence.clone();
        let expiry_tx = presence_event_tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                let released = presence_for_expiry.expire_stale();
                for id in &released {
                    let _ = expiry_tx.send(PresenceEvent::HeartbeatExpired(id.clone()));
                }
            }
        });

        // Attribution watcher: subscribe to the workspace (the parent of
        // `repos.yaml`, which is the directory the operator launched us
        // from) for inotify events and auto-claim edits to the agent that
        // wrote them. The handle is intentionally dropped — the watcher
        // thread lives for the lifetime of the process and exits when its
        // `notify::RecommendedWatcher` is dropped (which happens when the
        // channel closes on server shutdown). For graceful shutdown we'd
        // store the handle and abort it; not needed for MVP.
        let attribution_root = repos_yaml
            .as_deref()
            .and_then(Path::parent)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let _attribution_handle = AttributionWatcher::new(
            presence.clone(),
            occupancy.clone(),
            presence_event_tx.clone(),
            attribution_root,
        )
        .start();

        Ok(Self {
            config: LainConfig {
                workspace: ws,
                memory_path: mem_path,
            },
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            cross_encoder,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: Some(federation),
            federation_workspaces: None,
            federation_transport: Some(transport),
            federation_port: Some(port),
            started_at: now,
            last_sync_at: Arc::new(Mutex::new(now)),
            last_error: Arc::new(Mutex::new(None)),
            repos_yaml,
            reload_bus: Arc::new(ReloadBus::new()),
            presence,
            occupancy,
            presence_event_tx,
        })
    }

    /// Same as `with_federation` but also registers the workspace MCP
    /// tools (`list_workspaces`, `get_active_workspace`, `get_workspace`,
    /// `get_workspace_graph`) against the supplied `WorkspacesFile`. Used
    /// when the server is started with `--workspace <name>` and a
    /// `workspaces.yaml` is present next to `repos.yaml`.
    pub fn with_federation_and_workspaces(
        federation: Arc<FederatedIndex>,
        transport: Transport,
        port: u16,
        workspaces: Arc<WorkspacesFile>,
        repos_yaml: Option<PathBuf>,
    ) -> Result<Self, LainError> {
        // Mostly the same wiring as `with_federation`. The differences:
        // we store the workspaces file in `federation_workspaces`, and we
        // build the LainMcpServer with the workspaces-aware constructor
        // eagerly so any wiring problems surface at construction time.
        //
        // Use the same pid+counter staging-path pattern as
        // `with_federation` so this constructor plays nicely with
        // parallel tests in the same process — a previous test may
        // have torn the dir down between calls. See the comment on
        // `STAGING_COUNTER` for context.
        let counter = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!(
            "lain-federation-{}-{}",
            std::process::id(),
            counter
        ));
        std::fs::create_dir_all(&ws)?;
        if !ws.join(".git").exists() {
            git2::Repository::init(&ws)?;
        }
        let mem_dir = ws.join(".lain");
        std::fs::create_dir_all(&mem_dir)?;
        let mem_path = mem_dir.join("graph.bin");
        let _ = std::fs::remove_file(&mem_path);

        let graph = GraphDatabase::new(&mem_path)?;
        let overlay = VolatileOverlay::new();
        let embedder = NlpEmbedder::new()?;
        if embedder.is_stub() {
            info!("NLP embedder running in stub mode (federation placeholder)");
        }
        let git = Arc::new(Mutex::new(GitSensor::new(&ws)?));
        let lsp_pool = Arc::new(LspPool::new(&ws, 1)?);
        let tuning = Arc::new(load_tuning_config(&ws));

        let tool_executor = ToolExecutor::new(
            graph.clone(),
            overlay.clone(),
            embedder.clone(),
            crate::server::nlp::CrossEncoder::from_dir(&ws),
            Arc::clone(&git),
            Arc::clone(&lsp_pool),
            Arc::clone(&tuning),
            ws.to_path_buf(),
        );

        // Wrap the workspaces file in an `Arc<RwLock<...>>` and share
        // the SAME `Arc` with both `federation_workspaces` below and
        // the `LainMcpServer` we build next. This is the single source
        // of truth for the workspace MCP tools: `set_workspace` (Task
        // 6.2 rebuild flow) writes through this lock, and every
        // dispatch the in-flight MCP server handles reads through the
        // same lock. Without sharing the lock, the LainMcpServer would
        // capture its own snapshot of `WorkspacesFile` at construction
        // time and the rebuild path's updates would never reach the
        // dispatcher — leading to the long-running "stale workspace"
        // bug.
        let workspaces_lock: Arc<RwLock<WorkspacesFile>> =
            Arc::new(RwLock::new((*workspaces).clone()));

        let _mcp = crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
            tool_executor.clone(),
            Arc::clone(&federation),
            Arc::clone(&workspaces_lock),
        );

        info!("Lain federation server initialized with workspaces");
        let cross_encoder = crate::server::nlp::CrossEncoder::from_dir(&ws);
        let now = SystemTime::now();

        // Presence layer: same wiring as `with_federation`. See that
        // constructor for the rationale on each piece.
        let presence = Arc::new(PresenceRegistry::new());
        let occupancy = Arc::new(OccupancyMap::new());
        let (presence_event_tx, _) = broadcast::channel(256);
        let presence_for_expiry = presence.clone();
        let expiry_tx = presence_event_tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                let released = presence_for_expiry.expire_stale();
                for id in &released {
                    let _ = expiry_tx.send(PresenceEvent::HeartbeatExpired(id.clone()));
                }
            }
        });

        // Attribution watcher: same wiring as `with_federation`. See that
        // constructor for the rationale on the path resolution.
        let attribution_root = repos_yaml
            .as_deref()
            .and_then(Path::parent)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let _attribution_handle = AttributionWatcher::new(
            presence.clone(),
            occupancy.clone(),
            presence_event_tx.clone(),
            attribution_root,
        )
        .start();

        Ok(Self {
            config: LainConfig {
                workspace: ws,
                memory_path: mem_path,
            },
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            cross_encoder,
            overlay_revision: Arc::new(AtomicU64::new(0)),
            federation: Some(federation),
            federation_workspaces: Some(workspaces_lock),
            federation_transport: Some(transport),
            federation_port: Some(port),
            started_at: now,
            last_sync_at: Arc::new(Mutex::new(now)),
            last_error: Arc::new(Mutex::new(None)),
            repos_yaml,
            reload_bus: Arc::new(ReloadBus::new()),
            presence,
            occupancy,
            presence_event_tx,
        })
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
    /// their own consumers.
    pub fn presence_event_tx(&self) -> &broadcast::Sender<PresenceEvent> {
        &self.presence_event_tx
    }

    /// Process start time, captured at construction. Used by
    /// `get_server_status` to report uptime.
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// Last successful sync time. Updated via `record_sync`; consumed by
    /// `get_server_status`.
    pub fn last_sync_at(&self) -> SystemTime {
        *self.last_sync_at.lock()
    }

    /// Most recent ingest/sync error message, if any.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    /// Mark a sync attempt as successful: clear `last_error` and bump
    /// `last_sync_at` to now. Called by ingest/sync paths that finish
    /// without an error; errors should call `record_last_error` instead.
    pub fn record_sync(&self) {
        *self.last_sync_at.lock() = SystemTime::now();
        *self.last_error.lock() = None;
    }

    /// Record an error message from the ingest/sync paths and refresh
    /// `last_sync_at` to the current time so the operator can see when
    /// the last attempt was.
    pub fn record_last_error(&self, msg: impl Into<String>) {
        *self.last_sync_at.lock() = SystemTime::now();
        *self.last_error.lock() = Some(msg.into());
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

    /// Borrowed accessor for `repos_yaml_path`. Alias of `repos_yaml`
    /// kept distinct so callers that read the docs of one don't have
    /// to scan the other.
    pub fn repos_yaml_path(&self) -> Option<&Path> {
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
            Arc::clone(&self.last_sync_at),
            Arc::clone(&self.last_error),
        )
        .with_reload_bus(Arc::clone(&self.reload_bus));
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
}
