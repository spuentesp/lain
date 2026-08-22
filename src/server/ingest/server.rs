//! `LainServer` — the struct itself plus its non-constructor methods.
//!
//! Constructors (`new`, `with_federation*`) live in
//! [`super::constructors`]; background tasks live in
//! [`super::background`]; config types live in [`super::config`].
//! This module is just the data definition + every accessor,
//! lifecycle, and persistence method.

use super::config::LainConfig;
use crate::config::state_path_for_workspace;
use crate::server::attribution::AttributionBackend;
use crate::server::auth::AuthState;
use crate::server::error::LainError;
use crate::server::events_log::EventsLog;
use crate::server::federation::config::FederationConfig;
use crate::server::federation::config::RepoConfig;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::repo_id::RepoId;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::git::GitSensor;
use crate::server::graph::GraphDatabase;
use crate::server::lsp::LspPool;
use crate::server::nlp::{CrossEncoder, NlpEmbedder};
use crate::server::overlay::{broadcast_overlay_diff, OverlayDiff, RevisionId, VolatileOverlay};
use crate::server::presence::{
    load_pair as load_presence_pair, save_pair as save_presence_pair, OccupancyMap,
    PresenceEvent, PresenceRegistry,
};
use crate::server::refresh::{RefreshOutcome, RefreshResult};
use crate::server::reload::ReloadBus;
use crate::server::sync_status::SyncStatus;
use crate::server::tools::ToolExecutor;
use crate::server::tuning::TuningConfig;
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tracing::info;

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
    pub(crate) overlay_revision: Arc<AtomicU64>,
    /// Federation handle. `Some` for federation-mode servers (constructed
    /// via `with_federation`); `None` for single-workspace servers
    /// (constructed via `new`).
    pub(crate) federation: Option<Arc<FederatedIndex>>,
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
    pub(crate) federation_workspaces: Option<Arc<RwLock<WorkspacesFile>>>,
    /// Transport chosen at `with_federation` time. Consumed by `serve`.
    pub(crate) federation_transport: Option<super::config::Transport>,
    /// Port chosen at `with_federation` time. Consumed by `serve`.
    pub(crate) federation_port: Option<u16>,
    /// Process start time, captured at construction. Immutable for the
    /// life of the server; surfaced via `get_server_status`.
    pub(crate) started_at: SystemTime,
    /// Sync attempt bookkeeping shared across the ingest/sync paths.
    /// Lives in its own module so new fields don't grow the LainServer
    /// impl block. See [`crate::server::sync_status::SyncStatus`].
    pub(crate) sync_status: SyncStatus,
    /// Path to the `repos.yaml` this server was launched with, if any.
    /// `None` for single-workspace servers (no federation config). Used
    /// to record the project in `~/.config/lain/recent_projects` and to
    /// tag the server status payload.
    pub(crate) repos_yaml: Option<PathBuf>,
    /// Hot-reload signal bus. Always allocated (single-workspace and
    /// federation-mode servers both hold one); the actual rebuild loop
    /// is only spawned in federation mode. Wrapped in `Arc` so the
    /// watcher, Unix socket listener, MCP handler, and rebuild task can
    /// all share a single bus.
    pub(crate) reload_bus: Arc<ReloadBus>,
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
    pub fn transport(&self) -> Option<super::config::Transport> {
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
    pub async fn serve(self) -> Result<(), crate::server::error::LainError> {
        // Clone the whole server up front so the MCP layer can hold a
        // shared `Arc<LainServer>` for the 8 multiplayer tools while the
        // current `self` is consumed building the MCP handler. Every
        // inner Arc (presence registry, occupancy map, broadcast
        // sender) is shared by the clone, so the cost is a handful of
        // Arc bumps rather than a deep copy.
        let server_arc = Arc::new(self.clone());
        let federation = self.federation.ok_or_else(|| {
            crate::server::error::LainError::Other(
                "LainServer::serve() called on a non-federation server (use LainServer::new for single-workspace)".into(),
            )
        })?;
        let transport = self.federation_transport.ok_or_else(|| {
            crate::server::error::LainError::Other("LainServer::serve(): missing transport (internal)".into())
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
            super::config::Transport::Http => mcp
                .run_http(port)
                .await
                .map_err(|e| crate::server::error::LainError::Mcp(format!("HTTP transport: {e}"))),
            super::config::Transport::Stdio => mcp
                .run_stdio()
                .await
                .map_err(|e| crate::server::error::LainError::Mcp(format!("stdio transport: {e}"))),
        }
    }

    pub fn clone_for_background(&self) -> Self {
        self.clone()
    }

    /// Allocate the next overlay-diff revision id. Sidecars use this to
    /// detect drops in the broadcast bus.
    pub fn next_revision(&self) -> RevisionId {
        self.overlay_revision.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Broadcast a single node insertion. Sidecars pull these diffs from
    /// the broadcast bus and merge them into their in-memory overlay.
    /// Only fires when this process is an *owner* — the sidecar has its
    /// graph opened read-only (Task 3) and never reaches the call sites
    /// below `check_writable`, so it cannot broadcast by accident.
    pub fn broadcast_overlay_insert(&self, node: crate::server::schema::GraphNode) {
        broadcast_overlay_diff(OverlayDiff {
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
    ) -> Result<(), crate::server::error::LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            crate::server::error::LainError::Other("LainServer::add_repo called on a non-federation server".into())
        })?;
        let source = crate::server::federation::config::FederationConfig::default()
            .build_source_for(repo)
            .map_err(|e| crate::server::error::LainError::Config(format!("build_source_for({}): {e}", repo.id)))?;
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
    pub fn remove_repo(&self, repo_id: &str) -> Result<(), crate::server::error::LainError> {
        let fed = self.federation.as_ref().ok_or_else(|| {
            crate::server::error::LainError::Other("LainServer::remove_repo called on a non-federation server".into())
        })?;
        let rid = RepoId::new(repo_id)
            .map_err(|e| crate::server::error::LainError::Config(format!("invalid repo id '{repo_id}': {e}")))?;
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
    pub fn state_dir_for_audit(&self) -> PathBuf {
        self.state_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::state_dir)
    }

    /// Directory that holds the per-server `events.jsonl` log (P1 #2:
    /// SSE replay). Same parent directory as `audit.jsonl` so both
    /// live in the same state dir and survive restarts together.
    pub fn events_log_path_from_config(mem_path: &Path) -> PathBuf {
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
    pub fn save_state(&self) -> Result<(), crate::server::error::LainError> {
        let path = self.state_path();
        save_presence_pair(&path, &self.presence, &self.occupancy)
            .map_err(|e| crate::server::error::LainError::Other(format!("save_state({}): {e}", path.display())))
    }

    /// Hydrate the live `PresenceRegistry` + `OccupancyMap` from the
    /// server's state file, if any. Idempotent: missing file is a
    /// no-op. Used by the three constructors right after the
    /// registries are built.
    pub fn load_state(&self) -> Result<(), crate::server::error::LainError> {
        let path = self.state_path();
        load_presence_pair(&path, &self.presence, &self.occupancy)
            .map_err(|e| crate::server::error::LainError::Other(format!("load_state({}): {e}", path.display())))
    }

    /// Run `f` inside the cross-process presence critical section:
    /// take the state-file lock, refresh the in-memory registries from
    /// disk, run `f`, then write the result back.
    ///
    /// This is what makes presence shared between processes on one
    /// machine. The stdio transport spawns a server *per client*, so
    /// two agents on the same repo previously ran two registries and
    /// could not see each other — every claim was granted, no conflict
    /// was ever reported, and nothing indicated the coordination layer
    /// was inert. The state file was already the shared medium; it was
    /// only ever written, never re-read.
    ///
    /// Reload-before-act matters as much as the lock: without it this
    /// process would save a snapshot built from its own stale memory
    /// and drop every peer's session.
    ///
    /// Advisory throughout — a lock timeout proceeds unlocked, and a
    /// failed load or save is logged rather than surfaced. Presence is
    /// a coordination hint; it must never be the thing that breaks a
    /// tool call.
    pub fn with_shared_presence<T>(&self, f: impl FnOnce() -> T) -> T {
        let path = self.state_path();
        let _lock = crate::server::state_lock::acquire(&path);
        if let Err(e) = self.load_state() {
            tracing::debug!("presence refresh skipped: {e}");
        }
        let out = f();
        if let Err(e) = self.save_state() {
            tracing::warn!("presence save failed: {e}");
        }
        out
    }

    /// Read-only half of [`Self::with_shared_presence`]: refresh from
    /// disk so a listing reflects peers, without taking the write lock
    /// or saving. Used by `list_active_agents`, `list_occupancy` and
    /// friends, where a stale read is the whole bug and a write would
    /// be pure contention.
    pub fn refresh_shared_presence(&self) {
        if let Err(e) = self.load_state() {
            tracing::debug!("presence refresh skipped: {e}");
        }
    }

    /// Install a persist callback on `presence` and `occupancy` that
    /// drives `save_state` on every mutation. Called once from each
    /// constructor, immediately after the registries are built and
    /// after `load_state` has hydrated them.
    pub(crate) fn install_persist_callback(&self) {
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
        // In federation mode `config.workspace` is a staging
        // placeholder, not a checkout — the real files live under the
        // registered repo roots. Register those too so a relative claim
        // path from an MCP caller canonicalizes to the same key the CLI
        // produces from an absolute path.
        if let Some(fed) = &self.federation {
            self.occupancy.add_claim_roots(&fed.repo_paths());
        }
        // Touch the first closure so the compiler does not warn about
        // an unused binding; both callbacks are installed above.
        let _ = cb;
    }
}
