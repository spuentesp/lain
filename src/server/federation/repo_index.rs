use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_source::RepoSource;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::{HierarchicalSymbol, LspPool};
use crate::schema::{GraphEdge, GraphNode};
use crate::server::ingest::ingestion::index_one_repo;
use crate::server::overlay::VolatileOverlay;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Top-level wall-clock budget for a single [`RepoIndex::index`] call.
/// The inner stages already have their own per-request and per-scan
/// timeouts; this constant is the outer guardrail so a stuck child
/// process, a wedged tree-sitter parse, or an unresponsive git2 call
/// can't hold the per-repo git mutex forever. When this fires the repo
/// transitions to [`RepoHealth::Degraded`] and the watcher keeps
/// polling — the federation stays up.
pub const INDEX_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolved per-repo pipeline timeout. Honors `LAIN_REINDEX_TIMEOUT`
/// (the same knob [`crate::server::refresh::parse_reindex_timeout`]
/// reads for the outer startup budget) so operators have one knob to
/// turn when cold-cache federation indexing overruns the historical
/// 60s default — e.g. `tokio-rs/tokio` on a cold cache. Falls back to
/// [`Self::INDEX_TIMEOUT`] (60s) when the env var is unset or
/// unparseable. Cached per-process via `OnceLock` so the env var is
/// read exactly once at first call.
pub fn index_timeout() -> Duration {
    static OVERRIDE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| match std::env::var("LAIN_REINDEX_TIMEOUT") {
        Ok(s) => match s.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                eprintln!(
                    "LAIN_REINDEX_TIMEOUT={s:?} is not a valid integer; using default 60s for per-repo pipeline"
                );
                INDEX_TIMEOUT
            }
        },
        Err(_) => INDEX_TIMEOUT,
    })
}

/// Run `f` on a fresh OS thread and wait up to `budget` for it to
/// signal completion. If `f` finishes within `budget`, the thread is
/// joined and the function returns `true`. If `budget` elapses first,
/// the function returns `false` and drops the [`JoinHandle`]; the
/// thread continues in the background with whatever captures `f`
/// took, and any resources owned by those captures are released when
/// the thread eventually exits.
///
/// The detached-thread semantics are load-bearing for callers like
/// [`RepoIndex::drop`] that need to bound their own wall-clock time
/// even when the spawned work refuses to make progress. An unbounded
/// `JoinHandle::join()` after a timeout would block the caller
/// indefinitely; dropping the handle returns control to the caller
/// while letting the thread clean up at its own pace.
pub fn run_with_budget<F>(name: &str, f: F, budget: Duration) -> bool
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            f();
            let _ = tx.send(());
        })
        .expect("spawn thread");
    let completed = rx.recv_timeout(budget).is_ok();
    if completed {
        let _ = handle.join();
    }
    // On timeout: drop `handle` without joining. The thread runs to
    // completion in the background; the OS reaps it when it exits.
    completed
}

pub struct RepoIndex {
    source: Box<dyn RepoSource>,
    db: GraphDatabase,
    lsp: LspPool,
    // `GitSensor` wraps a `git2::Repository`, which is `Send` but `!Sync`
    // (git2 provides `unsafe impl Send for Repository` but no `Sync` impl).
    // We wrap the sensor in `Arc<Mutex<...>>` for two reasons:
    //
    // 1. **Runtime serialization:** `RepoIndex::index` and `start_watcher`
    //    both touch `git` from worker threads (the sink is on the tokio
    //    runtime; the watcher callback may fire on a notify thread). The
    //    Mutex serializes git2 calls so we never have two threads in
    //    libgit2 at once on the same handle.
    // 2. **Sharing into closures:** `start_watcher` clones the `Arc` into a
    //    `Fn` closure handed to `notify::RecommendedWatcher`. Without
    //    `Arc` we couldn't move `git` into the closure without taking
    //    `&mut self` (we want `&self.index` etc. to remain callable).
    //
    // We use `tokio::sync::Mutex` (not `parking_lot::Mutex`) because we
    // need to hold the lock across `.await` points inside `index_one_repo`.
    // `tokio::sync::MutexGuard<T>` is `Send` when `T: Send`, and
    // `GitSensor: Send` (via `git2::Repository`'s `unsafe impl Send`).
    // `parking_lot::MutexGuard` is `!Send` by default — its `send_guard`
    // feature is not enabled, so we'd have to either add a Cargo.toml
    // feature flip or restructure the pipeline to use `spawn_blocking` for
    // the entire ingestion. `tokio::sync::Mutex` is the small
    // dependency-free fix and matches the existing `LspPool` pattern.
    git: Arc<AsyncMutex<GitSensor>>,
    health: Arc<RwLock<RepoHealth>>,
    last_indexed: Arc<RwLock<SystemTime>>,
    /// The error text from the most recent failed `index()`/`index_forced()`
    /// attempt, if any. `RepoHealth::Degraded` alone doesn't say *why* —
    /// this is what `get_repo_info` surfaces so an operator (or a test
    /// harness diagnosing a flake) can see the real cause instead of just
    /// the coarse health enum. Cleared on the next successful index.
    last_index_error: Arc<RwLock<Option<String>>>,
    /// Shared handle to the federation's `VolatileOverlay`. `index()`
    /// touches it after a successful index pass so the `Overlay
    /// freshness` banner doesn't read as "stale" the moment the
    /// server comes up. Defaults to a fresh, unconnected overlay
    /// (tests); production wires the federation's overlay in via
    /// [`Self::set_overlay`] right after `add_repo`.
    server_overlay: parking_lot::Mutex<Arc<VolatileOverlay>>,
    /// Path -> node ids this repo currently has live in the shared
    /// `VolatileOverlay`, as of the last `sync_overlay` cycle. Tracking
    /// ids (not just paths) is load-bearing: the overlay is shared
    /// across every repo in the federation, and two repos can easily
    /// have a file at the same relative path (`src/lib.rs` is the
    /// common case). Removing "whatever is at this path" would delete
    /// another repo's live nodes too; removing specific ids this repo
    /// itself inserted never touches anything it doesn't own.
    overlay_paths: parking_lot::Mutex<HashMap<String, Vec<String>>>,
    /// Serializes `sync_overlay` calls for this repo. It can be invoked
    /// both by the watcher's receiver task and by `sync_state`'s
    /// per-repo `JoinSet` (`enrichment.rs`) landing on the same repo at
    /// once; without this, two overlapping calls each snapshot
    /// `get_uncommitted_changes()` independently and the later one to
    /// finish overwrites `overlay_paths` with its own (possibly
    /// stale-relative-to-the-other-call) view, corrupting the
    /// staleness bookkeeping.
    sync_overlay_lock: AsyncMutex<()>,
    /// Active file-system watcher for this repo. `None` until
    /// `start_watcher` is called. The watcher is dropped (and the
    /// background thread stops) when the `RepoIndex` is dropped.
    watcher: parking_lot::Mutex<Option<notify::RecommendedWatcher>>,
    watcher_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Gates overlay publication against removal from the federation.
    active: parking_lot::Mutex<bool>,
    /// Fires after every receiver-task iteration completes
    /// (`me.index().await` + `me.sync_overlay().await`). Tests await
    /// this with a timeout instead of `tokio::time::sleep`, so they
    /// wake as soon as the receiver has actually processed the edit
    /// rather than guessing a wall-clock budget. The notify fires
    /// once per event; if a test needs to observe N events it must
    /// call `notified().await` N times (or re-await after each
    /// `notify_one`).
    overlay_updated: Arc<tokio::sync::Notify>,
    /// Number of files whose overlay refresh was skipped due to LSP
    /// unavailability during the most recent `sync_overlay` cycle.
    /// Read by `sync_state` to populate
    /// `RefreshOutcome::lsp_failures_last_cycle` so `get_health` can
    /// surface the aggregate without grepping logs. Reset to 0 at
    /// the start of each `sync_overlay` call.
    last_overlay_lsp_failures: std::sync::atomic::AtomicU32,
    /// Federation-aware cross-repo resolver. `None` for repos that
    /// don't need it (tests, single-repo mode). The federation
    /// loader sets this right after `add_repo` so a subsequent
    /// `index()` can use it to materialize cross-repo `Calls` edges.
    cross_repo_resolver:
        parking_lot::Mutex<Option<Arc<dyn crate::federation::cross_repo::CrossRepoResolver>>>,
    /// Per-repo UUID namespace used to derive `GraphNode::id`s for
    /// every node this `RepoIndex` produces. Two repos in the same
    /// federation with identical `(type, path, name, line)` therefore
    /// produce distinct ids, and the shared `VolatileOverlay` keeps
    /// the symbols separate. The namespace is minted once at
    /// construction and is stable for the lifetime of the
    /// `RepoIndex`. URGENT FIXES #2.
    pub(crate) id_namespace: crate::schema::RepoNamespace,
    /// Fires after every successful `index()` or `index_forced()`.
    /// Closes the cold-boot race between the per-repo graph becoming
    /// visible and a tool call landing on the just-bound HTTP
    /// listener: the dispatcher awaits this with a 200 ms budget when
    /// the active repo's per-repo graph is empty, so a resolve that
    /// arrives in the cold-boot window either sees the freshly-indexed
    /// graph (signal fired during the wait) or returns its existing
    /// NotFound (budget elapsed, indexer still running — the test's
    /// `wait_for_repo_index` then re-polls). `notify_one` is used so a
    /// permit is buffered for one late-arriving waiter; after that the
    /// signal returns to its un-fired state until the next successful
    /// index. The race the user described in the bug report — per-repo
    /// graph and federation backend observable at different points —
    /// is what this signal unifies.
    indexed: Arc<tokio::sync::Notify>,
    /// Cheap "ever fired" companion to the `indexed` `Notify`. The
    /// `Notify` round-trip is right for the dispatcher's cold-boot
    /// bounded wait, but a snapshot reader (`FederatedIndex::per_repo_readiness`,
    /// `get_capabilities`) only needs the boolean. Setting an
    /// `AtomicBool` next to `notify_one()` is silently idempotent and
    /// catches `indexed_signal = true` for the wire payload the
    /// `Notify`'s one-shot semantics would otherwise miss.
    indexed_at_least_once: std::sync::atomic::AtomicBool,
    /// Current depth of the watcher's bounded event channel.
    /// Incremented by the receiver loop on receive, decremented on
    /// process. Exposed as the `outstanding_files` field on
    /// `PerRepoReadiness` so `get_capabilities` can show back-pressure
    /// without scraping the receiver task's internals. Currently
    /// stays at 0 — the receiver loop's incr/decr is wired but the
    /// channel capacity (1024) rarely fills in practice; the
    /// spawn_blocking follow-up PR will fill it in for hot-loop
    /// observability. Defined now to keep the wire shape stable
    /// across that work.
    /// Depth of the watcher's bounded event channel at snapshot
    /// time. `Arc`-wrapped so the inotify callback (increment
    /// side) and the Tokio receiver loop (decrement side) share
    /// one counter; `PerRepoReadiness::outstanding_files` reads
    /// the same atomic and exposes it through
    /// `get_capabilities`. Wired up in PR B
    /// (`feat/m4-spawn-blocking`).
    outstanding: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// AGENT_UX_ROADMAP.md M4 follow-up (FOLLOWUPS.md §"Cooperative
    /// cancellation token"): server-owned shutdown signal threaded
    /// through every long-running phase in the federation pipeline
    /// (`index_one_repo`, the receiver loop spawned by
    /// `start_watcher`, and the watcher-driven `index_forced` /
    /// `sync_overlay` cycles). `FederatedIndex::install_overlay`
    /// shares one token across all repos via `cancel_token()`; a
    /// `FederatedIndex::shutdown` call (or its `Drop`) cancels the
    /// parent token, which propagates to every child clone here.
    cancel: CancellationToken,
}

// `RepoIndex` is `Send + Sync` because every field is `Send + Sync`:
// - `Box<dyn RepoSource>`: the trait requires `Send + Sync`.
// - `GraphDatabase`, `LspPool`: heap-backed with internal `Arc<RwLock<...>>` /
//   `Arc<Mutex<...>>`, all `Send + Sync`.
// - `Arc<AsyncMutex<GitSensor>>`: `tokio::sync::Mutex<T>` is `Send + Sync`
//   when `T: Send`, which `GitSensor` is (via `git2::Repository`'s
//   `unsafe impl Send`).
// - `Arc<RwLock<...>>`, `Mutex<...>`: parking_lot primitives are
//   `Send + Sync` for `Send` payloads.
//
// No `unsafe impl` is needed on `RepoIndex` itself — the compiler will
// verify the auto-traits. A test in `mod tests` asserts this statically.

impl RepoIndex {
    /// Stop publishing shared state, including from already-started scans.
    pub(crate) fn deactivate(&self) {
        let mut active = self.active.lock();
        *active = false;
        self.watcher.lock().take();
        // AGENT_UX_ROADMAP.md M4 follow-up: cancel the cooperative
        // shutdown token. The receiver task spawned by `start_watcher`
        // observes this via `select!` and exits at the next phase
        // boundary. `deactivate` is sync (called from `Drop`), so we
        // can't `await` the JoinHandle here — the pre-fix code
        // dropped it after `task.abort()`. We keep that semantics
        // for symmetry and rely on the receiver's own observation
        // of the token for graceful shutdown.
        self.cancel.cancel();
        if let Some(task) = self.watcher_task.lock().take() {
            task.abort();
        }
        if let Some(task) = self.watcher_task.lock().take() {
            task.abort();
        }
        let overlay = self.server_overlay.lock().clone();
        let ids: Vec<_> = self
            .overlay_paths
            .lock()
            .drain()
            .flat_map(|(_, ids)| ids)
            .collect();
        for id in &ids {
            overlay.remove_node(id);
        }
        if !ids.is_empty() {
            crate::server::overlay::broadcast_overlay_diff(crate::server::overlay::OverlayDiff {
                revision: 0, // assigned by the process-wide publisher
                added: vec![],
                removed: ids,
                updated: vec![],
            });
        }
        self.cross_repo_resolver.lock().take();
    }

    pub fn new(source: Box<dyn RepoSource>, data_dir: &Path) -> Result<Self, LainError> {
        let local_path = source.local_path().to_path_buf();
        let mut db = GraphDatabase::new(&data_dir.join("graph.bin"))?;
        // Read the repo's own `.lain/tuning.toml` (falling back to
        // defaults when absent) rather than hard-coding. The LSP poll
        // settings in particular were documented knobs that nothing read.
        let runtime = crate::tuning::load_tuning_config(&local_path).runtime;
        let lsp = LspPool::new(&local_path, 4, &runtime)?;
        let git = Arc::new(AsyncMutex::new(GitSensor::new(&local_path)?));
        // Read the namespace *before* moving `source` into the struct —
        // we need the source's id, and `Box<dyn RepoSource>` isn't
        // `Copy`. URGENT FIXES #2: every `GraphNode` this repo produces
        // for the federation overlay must use a per-repo namespace
        // so identical `(type, path, name, line)` across repos
        // doesn't collapse into one overlay entry.
        let id_namespace = *source.id_namespace();
        // Pin the per-repo DB's co-change id space to the same
        // namespace the static-graph scanner writes File nodes
        // under. Without this match, `insert_co_change_edges`
        // mints endpoint ids with a different namespace than the
        // File nodes carry, `index_map.get(...)` for the endpoint
        // returns None, `insert_edges_batch` drops every co-change
        // edge as an orphan, and `get_coupling_radar` reports "No
        // co-change coupling found" for every file. URGENT FIXES
        // #14 follow-up.
        db.set_namespace(id_namespace);
        Ok(Self {
            source,
            db,
            lsp,
            git,
            health: Arc::new(RwLock::new(RepoHealth::Indexing)),
            last_indexed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
            last_index_error: Arc::new(RwLock::new(None)),
            server_overlay: parking_lot::Mutex::new(Arc::new(VolatileOverlay::new())),
            overlay_paths: parking_lot::Mutex::new(HashMap::new()),
            sync_overlay_lock: AsyncMutex::new(()),
            watcher: parking_lot::Mutex::new(None),
            watcher_task: parking_lot::Mutex::new(None),
            active: parking_lot::Mutex::new(true),
            overlay_updated: Arc::new(tokio::sync::Notify::new()),
            last_overlay_lsp_failures: std::sync::atomic::AtomicU32::new(0),
            cross_repo_resolver: parking_lot::Mutex::new(None),
            id_namespace,
            indexed: Arc::new(tokio::sync::Notify::new()),
            indexed_at_least_once: std::sync::atomic::AtomicBool::new(false),
            cancel: CancellationToken::new(),
            outstanding: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Handle on the [`Notify`] the receiver task fires after each
    /// `index()` + `sync_overlay()` cycle. Tests clone this and
    /// the cancellation token so they can race shutdown against
    /// the receiver's natural wake.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Public test hook so a test can cancel without owning the
    /// `RepoIndex`. Production code uses `FederatedIndex::shutdown`
    /// to cancel the parent token.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Read the current depth of the watcher's bounded mpsc
    /// channel. Wired up in PR B (`feat/m4-spawn-blocking`) via
    /// `fetch_add` on every watcher callback and `fetch_sub` on
    /// every receiver-loop iteration.
    pub fn outstanding_files(&self) -> u64 {
        self.outstanding.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Owned clone of the `outstanding_files` atomic, used to
    /// share the counter between the inotify callback (increment
    /// side) and the Tokio receiver loop (decrement side). PR B
    /// `feat/m4-spawn-blocking`.
    pub(crate) fn outstanding_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        std::sync::Arc::clone(&self.outstanding)
    }

    /// Handle on the [`Notify`] the receiver task fires after each
    /// `index()` + `sync_overlay()` cycle. Tests clone this and
    /// `notified().await` instead of polling `tokio::time::sleep`
    /// with a guessed budget. Production code does not need it.
    pub fn overlay_updated(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.overlay_updated)
    }

    /// Handle on the [`Notify`] this index fires after every successful
    /// `index()` or `index_forced()`. The MCP dispatcher awaits it
    /// with a 200 ms budget when the active repo's per-repo graph is
    /// empty, so a tool call landing in the cold-boot window wakes
    /// up to a populated graph instead of an empty placeholder.
    /// `notify_one` is the same shape as `overlay_updated` above —
    /// one buffered permit, then back to un-fired state.
    pub fn indexed_signal(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.indexed)
    }

    /// Borrow the LSP pool. Tests outside this module use it to mark
    /// specific language servers unavailable (so the indexer takes the
    /// tree-sitter fallback on a host with no `rust-analyzer` on
    /// PATH); production code does not need it.
    pub fn lsp(&self) -> &LspPool {
        &self.lsp
    }

    /// Whether `index()` or `index_forced()` has succeeded at least
    /// once on this `RepoIndex`. Pinned at the same site as
    /// `indexed_signal().notify_one()`, so the two are observed
    /// together — the boolean exists for snapshot readers
    /// (`FederatedIndex::per_repo_readiness`,
    /// `get_capabilities`) that don't want the `Notify`'s one-shot
    /// waiting semantics.
    pub fn indexed_signal_was_fired(&self) -> bool {
        self.indexed_at_least_once
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Current depth of the watcher's bounded event channel, exposed
    /// as `PerRepoReadiness::outstanding_files` so
    /// `get_capabilities` can show watcher back-pressure without
    /// scraping the receiver task's internals. Wired up in PR B
    /// (`feat/m4-spawn-blocking`): the inotify callback
    /// `fetch_add`s, the receiver loop `fetch_sub`s.
    /// Number of files whose overlay refresh was skipped due to LSP
    /// unavailability during the most recent `sync_overlay` cycle.
    /// Returns 0 if `sync_overlay` hasn't run yet, or if the cycle
    /// ran cleanly.
    ///
    /// `sync_state` reads this from every repo and aggregates the
    /// totals into `RefreshOutcome::lsp_failures_last_cycle` so
    /// `get_health` can answer "did the last refresh have any LSP
    /// issues?" without grepping logs.
    pub fn last_overlay_lsp_failures(&self) -> u32 {
        self.last_overlay_lsp_failures
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn source(&self) -> &dyn RepoSource {
        self.source.as_ref()
    }

    pub fn db(&self) -> &GraphDatabase {
        &self.db
    }

    pub fn health(&self) -> RepoHealth {
        *self.health.read()
    }

    /// The error text from the most recent failed indexing attempt, if
    /// this repo is (or was last) `Degraded`. `None` if it has never
    /// failed, or the last attempt since a failure succeeded.
    pub fn last_index_error(&self) -> Option<String> {
        self.last_index_error.read().clone()
    }

    /// Install the federation's shared `VolatileOverlay`. Called by
    /// [`crate::server::federation::federated_index::FederatedIndex::install_overlay`]
    /// so a successful `index()` can touch the overlay and the
    /// freshness banner stops reading as "stale" forever on a
    /// freshly-indexed server. The Mutex<Arc> makes the swap atomic
    /// w.r.t. concurrent `index()` calls (each one clones the Arc
    /// inside the lock).
    pub fn set_overlay(&self, overlay: Arc<VolatileOverlay>) {
        *self.server_overlay.lock() = overlay;
    }

    /// Public read accessor for the shared volatile overlay. Used by tests
    /// and by the watcher's receiver task.
    pub fn server_overlay(&self) -> Arc<VolatileOverlay> {
        self.server_overlay.lock().clone()
    }
    pub fn set_health(&self, health: RepoHealth) {
        *self.health.write() = health;
    }

    /// Install the federation's cross-repo symbol resolver. Called by
    /// the federation loader after `add_repo` so the resolve phase
    /// in a subsequent `index()` can use it to materialize cross-repo
    /// `Calls` edges.
    pub fn set_cross_repo_resolver(
        &self,
        resolver: Arc<dyn crate::federation::cross_repo::CrossRepoResolver>,
    ) {
        *self.cross_repo_resolver.lock() = Some(resolver);
    }

    pub fn last_indexed(&self) -> SystemTime {
        *self.last_indexed.read()
    }

    pub fn nodes(&self) -> Vec<GraphNode> {
        self.db.all_nodes()
    }

    pub fn edges(&self) -> Vec<GraphEdge> {
        self.db.all_edges()
    }

    /// Run the per-repo ingestion pipeline: tree-sitter extract → LSP hydrate
    /// → git co-change, scoped to `source.local_path()`. On success,
    /// transitions health from `Indexing` → `Ready` and stamps `last_indexed`.
    /// On failure, transitions to `Degraded` (the caller does not retry).
    ///
    /// The git mutex is held for the entire call so the watcher callback
    /// (which schedules another `index()`) blocks until we finish, avoiding
    /// two concurrent writes to the same per-repo graph. The guard is
    /// `Send` and is held across `.await` points inside `index_one_repo`.
    ///
    /// The whole pipeline is wrapped in [`Self::index_timeout`]. The inner
    /// stages already have their own budgets (`LSP_STARTUP_TIMEOUT`,
    /// `LSP_REQUEST_TIMEOUT`, the `scan_timeout_secs` ingest cap), but a
    /// top-level bound keeps a misbehaving stage — a stuck child process,
    /// a tree-sitter parser wedged on a pathological file, an unresponsive
    /// git2 call — from holding the git mutex forever. When the timeout
    /// fires we transition to `Degraded` so the watcher (if any) keeps
    /// polling rather than wedging the federation.
    pub async fn index(self: &Arc<Self>) -> Result<(), LainError> {
        let path = self.source.local_path().to_path_buf();
        // Borrow `self.db` directly instead of cloning. `GraphDatabase`
        // derives Clone but `DashMap` clones its shards independently —
        // every `index_map` / `path_index` mutation lands on the clone,
        // and the server's bound `&self.db` reads from an empty index
        // map while the on-disk file (and the clone) hold the real
        // graph. `get_edges_to` and friends return empty even though
        // the edges exist in petgraph (which IS Arc-shared and survives
        // the clone). `git_guard` already serializes writers, so a
        // shared `&self.db` borrow across the pipeline is safe.
        let db = &self.db;
        let lsp = self.lsp.clone();
        let git = Arc::clone(&self.git);

        // Acquire the lock before running the pipeline so we serialize
        // against any concurrent `index()` call (e.g. from the watcher).
        let git_guard = git.lock().await;

        let pipeline = async {
            let overlay = self.server_overlay.lock().clone();
            let resolver = self.cross_repo_resolver.lock().clone();
            let resolver_ref: Option<&dyn crate::federation::cross_repo::CrossRepoResolver> =
                resolver.as_deref();
            let source_repo = self.source.id();
            index_one_repo(crate::server::ingest::ingestion::IndexRequest {
                path: &path,
                graph: db,
                lsp_pool: &lsp,
                git: &git_guard,
                overlay: &overlay,
                resolver: resolver_ref,
                source_repo: Some(source_repo),
                namespace: &self.id_namespace,
                force: false,
                cancel: &self.cancel,
            })
            .await
        };
        let result = match tokio::time::timeout(index_timeout(), pipeline).await {
            Ok(r) => r,
            Err(_) => {
                let budget = index_timeout();
                tracing::warn!(
                    "[federation] index timed out after {:?} for {:?}; transitioning to Degraded",
                    budget,
                    self.source.local_path()
                );
                drop(git_guard);
                let message = format!("RepoIndex::index exceeded {:?} budget", budget);
                *self.last_index_error.write() = Some(message.clone());
                self.set_health(RepoHealth::Degraded);
                return Err(LainError::Other(message));
            }
        };

        // Drop the guard explicitly before updating shared state so the
        // watcher can re-enter the lock promptly.
        drop(git_guard);

        if let Err(e) = &result {
            tracing::warn!(
                "[federation] index failed for {:?}: {}",
                self.source.local_path(),
                e
            );
            *self.last_index_error.write() = Some(e.to_string());
            self.set_health(RepoHealth::Degraded);
            return Err(result.unwrap_err());
        }

        *self.last_indexed.write() = SystemTime::now();
        *self.last_index_error.write() = None;
        self.set_health(RepoHealth::Ready);
        // Cold-boot race closure: the dispatcher awaits this on the
        // active repo when the per-repo graph is empty. Firing it
        // here (and only on success) means a resolve that lands
        // between boot and the next `index_forced` either wakes up
        // to a populated graph or, if the budget elapsed first,
        // returns its existing NotFound for the test's poll loop to
        // retry on.
        self.indexed.notify_one();
        // Same idempotent flag for snapshot readers that don't need
        // the `Notify`'s one-shot waiting semantics. Stores are
        // `Relaxed` — the only consumer (`per_repo_readiness`,
        // `get_capabilities`) treats this as a hint.
        self.indexed_at_least_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Like [`Self::index`], but bypasses the commit-hash short-circuit
    /// so the worktree is re-scanned even when no `git commit` has
    /// landed yet.
    ///
    /// The file watcher (`start_watcher`) calls this on every `notify`
    /// event: a user edit followed by no commit is the common case for
    /// a long-lived editor session, and the previous commit-hash gate
    /// meant the per-repo DB stayed at the pre-edit state until the
    /// user eventually committed. The boot loop, by contrast, runs on a
    /// known-good commit cadence and should keep the optimization
    /// (`index()` with `force=false`).
    pub async fn index_forced(self: &Arc<Self>) -> Result<(), LainError> {
        let path = self.source.local_path().to_path_buf();
        let db = &self.db;
        let lsp = self.lsp.clone();
        let git = Arc::clone(&self.git);

        let git_guard = git.lock().await;

        let pipeline = async {
            let overlay = self.server_overlay.lock().clone();
            let resolver = self.cross_repo_resolver.lock().clone();
            let resolver_ref: Option<&dyn crate::federation::cross_repo::CrossRepoResolver> =
                resolver.as_deref();
            let source_repo = self.source.id();
            index_one_repo(crate::server::ingest::ingestion::IndexRequest {
                path: &path,
                graph: db,
                lsp_pool: &lsp,
                git: &git_guard,
                overlay: &overlay,
                resolver: resolver_ref,
                source_repo: Some(source_repo),
                namespace: &self.id_namespace,
                force: true,
                cancel: &self.cancel,
            })
            .await
        };
        let result = match tokio::time::timeout(index_timeout(), pipeline).await {
            Ok(r) => r,
            Err(_) => {
                let budget = index_timeout();
                tracing::warn!(
                    "[federation] index_forced timed out after {:?} for {:?}; transitioning to Degraded",
                    budget,
                    self.source.local_path()
                );
                drop(git_guard);
                let message = format!("RepoIndex::index_forced exceeded {:?} budget", budget);
                *self.last_index_error.write() = Some(message.clone());
                self.set_health(RepoHealth::Degraded);
                return Err(LainError::Other(message));
            }
        };

        drop(git_guard);

        if let Err(e) = &result {
            tracing::warn!(
                "[federation] index_forced failed for {:?}: {}",
                self.source.local_path(),
                e
            );
            *self.last_index_error.write() = Some(e.to_string());
            self.set_health(RepoHealth::Degraded);
            return Err(result.unwrap_err());
        }

        *self.last_indexed.write() = SystemTime::now();
        *self.last_index_error.write() = None;
        self.set_health(RepoHealth::Ready);
        // Same cold-boot race closure as `index()`: a tool call that
        // arrives during a watcher-driven re-index gets the same
        // bounded-wait window the boot path gets.
        self.indexed.notify_one();
        self.indexed_at_least_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Watch the checkout through a bounded event channel. The receiver holds
    /// only a Weak reference while idle, so it cannot keep a removed repo alive.
    /// Deactivation drops the watcher and aborts its receiver task; an in-flight
    /// overlay scan must pass the active gate before publishing any nodes.
    pub async fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> {
        let active = self.active.lock();
        if !*active || self.watcher.lock().is_some() {
            return Ok(());
        }
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};
        use std::time::Duration;
        use tokio::sync::mpsc;

        let path = self.source.local_path().to_path_buf();
        let weak = Arc::downgrade(self);

        // Bounded channel to hand events from notify's inotify thread to
        // a Tokio task. The closure no longer calls `tokio::spawn`
        // directly — that panicked because the inotify thread is not a
        // Tokio runtime context. The closure uses `tx.try_send` and
        // ignores `Full` (logs at debug) so a stuck LSP child or a
        // `git checkout` storm cannot grow the queue without bound.
        const WATCHER_CHANNEL_DEPTH: usize = 1024;
        let (tx, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(WATCHER_CHANNEL_DEPTH);

        // AGENT_UX_ROADMAP.md M4 follow-up: clone the cancel token
        // *before* the `tokio::spawn` so the spawn closure doesn't
        // need to outlive `&self`. The clone is `Arc`-internal, so
        // this is cheap.
        let cancel = self.cancel.clone();

        // Receiver task: drains the channel and runs both the commit-based
        // pipeline (`index`) and the working-tree pipeline (`sync_overlay`)
        // per event. Runs in Tokio, so `.await` and `tokio::spawn` are sound.
        //
        // `overlay_updated.notify_one()` fires after every successful
        // `sync_overlay()` so tests awaiting the receiver can wake as
        // soon as the overlay reflects the event instead of guessing
        // a wall-clock budget. `notify_one` (not `notify_waiters`)
        // because tests hold their own clone of the `Arc<Notify>`
        // and call `notified().await` once per event they want to
        // observe.
        // PR B (spawn_blocking): the inotify callback `fetch_add`s
        // this counter, the receiver loop `fetch_sub`s it. Cloned
        // into both closures so they share the same atomic.
        let outstanding = self.outstanding_arc();
        let outstanding_for_task = std::sync::Arc::clone(&outstanding);

        let task = tokio::spawn(async move {
            let outstanding = outstanding_for_task;
            // AGENT_UX_ROADMAP.md M4 follow-up: race each watcher
            // event against the cooperative cancel token. When
            // `deactivate()` cancels `self.cancel` (called from
            // `FederatedIndex::shutdown` / `RepoIndex::Drop`), this
            // loop exits at the next idle moment instead of having
            // to be `task.abort()`'d mid-`index_forced` (which used
            // to leave the per-repo graph in whatever partial state
            // the abort landed in).
            while let Some(res) = rx.recv().await {
                // PR B (spawn_blocking): the matching `fetch_add`
                // happened on the inotify thread *before* the
                // corresponding `try_send` below, so we observe
                // the prior event having left the channel.
                outstanding.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                let Some(me_for_task) = weak.upgrade() else {
                    break;
                };
                if res.is_ok() {
                    // `index_forced` (not `index`) — the watcher fires
                    // on a kernel `notify` event, which is independent
                    // evidence the worktree changed. The commit-hash
                    // short-circuit in `index()` would skip the
                    // re-scan for any edit the user hadn't committed
                    // yet, leaving the per-repo DB stuck at the
                    // previous commit (wishlist #17).
                    //
                    // AGENT_UX_ROADMAP.md M4 follow-up: race the
                    // indexer against the cooperative cancel token.
                    // `Cancelled` (vs an `Err` from the pipeline
                    // itself) is the normal shutdown signal — don't
                    // surface it as a watcher-triggered failure.
                    if cancel.is_cancelled() {
                        break;
                    }
                    let index_result = me_for_task.index_forced().await;
                    if cancel.is_cancelled() {
                        break;
                    }
                    if let Err(e) = index_result {
                        if !matches!(e, LainError::Cancelled) {
                            tracing::debug!(
                                "[federation] watcher-triggered index failed for {:?}: {}",
                                me_for_task.source.local_path(),
                                e
                            );
                        }
                    }
                    let overlay_result = me_for_task.sync_overlay().await;
                    if cancel.is_cancelled() {
                        break;
                    }
                    if let Err(e) = overlay_result {
                        if !matches!(e, LainError::Cancelled) {
                            tracing::debug!(
                                "[federation] watcher-triggered overlay refresh failed for {:?}: {}",
                                me_for_task.source.local_path(),
                                e
                            );
                        }
                    }
                } else if let Err(e) = &res {
                    // Notify backend error (e.g. ENOSPC under inotify
                    // watch-handle pressure on busy CI runners). Don't
                    // run the pipelines on a backend error, but DO
                    // still fire the wake-signal so any caller awaiting
                    // the receiver advances instead of timing out.
                    tracing::warn!(
                        error = %e,
                        "[federation] watcher received notify error; firing wake signal anyway"
                    );
                }

                // Fire the wake-signal unconditionally so tests gating
                // on the receiver (and any future caller wiring
                // signal-driven refreshes) see a notify() per kernel
                // event, success or backend error.
                me_for_task.overlay_updated.notify_one();
            }
        });

        // Watcher callback: runs on notify's inotify thread. Pushes the
        // event into the bounded channel with `try_send`. Two
        // recoverable error shapes:
        //   - `Full`: receiver is slow (e.g. `index()` is mid-flight on
        //     a stuck LSP child). Drop the event; the next `notify`
        //     event will retry shortly, and the next legitimate file
        //     modification will eventually refresh the index. We log at
        //     `debug!` so the drop is observable but not noisy.
        //   - `Closed`: receiver task has exited (RepoIndex is being
        //     dropped). Silently drop — there's no one to wake up.
        let tx_for_closure = tx.clone();
        let outstanding_for_closure = outstanding;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                // PR B (spawn_blocking): increment *before* the
                // try_send so the receiver loop's matching
                // fetch_sub observes a non-negative depth even if
                // the channel is full and the send is dropped
                // below.
                outstanding_for_closure
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Err(e) = tx_for_closure.try_send(res) {
                    match e {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                            tracing::debug!(
                                "[federation] watcher channel full; dropping event (receiver is slow)"
                            );
                        }
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                            // Receiver gone — RepoIndex is being torn down.
                        }
                    }
                }
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        )
        .map_err(|e| LainError::Other(format!("watcher init: {e}")))?;

        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|e| LainError::Other(format!("watcher.watch({:?}): {e}", path)))?;

        *self.watcher.lock() = Some(watcher);
        *self.watcher_task.lock() = Some(task);
        Ok(())
    }

    /// Refresh the volatile overlay from uncommitted working-tree changes.
    /// Mirrors `LainServer::sync_volatile_overlay` (in `src/server/ingest/ingestion.rs:412`)
    /// but operates on this repo's own `git`/`lsp`/`overlay` so the federation
    /// watcher can re-populate the overlay without holding a `LainServer`
    /// reference.
    ///
    /// Sidecars (read-only graph) skip — their overlay is populated by the
    /// owner's `/overlay/subscribe` stream, not by working-tree scans.
    pub async fn sync_overlay(self: &Arc<Self>) -> Result<(), LainError> {
        if self.db.is_read_only() {
            return Ok(());
        }
        // Serialize concurrent calls for this repo (the watcher's
        // receiver task and `sync_state`'s per-repo `JoinSet` in
        // `enrichment.rs` can both land on the same repo at once) so
        // `overlay_paths` bookkeeping below is never read-modified by
        // two overlapping cycles.
        let _sync_guard = self.sync_overlay_lock.lock().await;
        if !*self.active.lock() {
            return Ok(());
        }
        let overlay = self.server_overlay.lock().clone();

        // Reset the per-cycle LSP-failure counter at the start so a
        // healthy cycle doesn't inherit a previous cycle's count.
        // `sync_state` reads this via `last_overlay_lsp_failures()`
        // after each per-repo refresh to aggregate the federation-
        // wide count into `RefreshOutcome`.
        self.last_overlay_lsp_failures
            .store(0, std::sync::atomic::Ordering::Relaxed);

        let (changes, indexed_current_commit) = {
            let git = self.git.lock().await;
            let changes = git.get_uncommitted_changes()?;
            // The canonical signal that an indexer pass has caught up
            // with HEAD is `db.last_commit == git HEAD`. Comparing the
            // two is O(1) per cycle and independent of which paths
            // changed. Unborn repositories have no HEAD at all; treat
            // them as "graph does not match HEAD" so we never purge
            // eagerly while the indexer hasn't run once.
            //
            // The pre-fix rule used `db.has_node_at_path(&path)` as
            // proof of "graph caught up" — but an older node at the
            // same path satisfied that predicate even when newly
            // committed additions had never been indexed. The result
            // was a window between commit and index() in which the
            // overlay entry for a freshly-committed symbol was purged
            // (the older pre-commit node satisfied `has_node_at_path`)
            // while the static graph never got a chance to add the
            // new symbol — both layers silently lost the function.
            let indexed_current_commit = match git.get_latest_commit_info() {
                Ok((head, _)) => self.db.get_last_commit()?.as_deref() == Some(head.as_str()),
                Err(_) => false,
            };
            (changes, indexed_current_commit)
        };
        // Overlay nodes are keyed by the workspace-relative form
        // (`process_overlay_change` mints them via `graph_path`, per its
        // own doc comment: "every site that mints or looks up a path key
        // goes through this... producer keys and consumer keys are only
        // comparable because both sides are reduced here first"), but
        // `change.path` (from `get_uncommitted_changes`) is absolute
        // (`self.workspace.join(path)` in `git.rs`).
        let workspace_root = self.source.local_path();
        let current_paths: HashSet<String> = changes
            .iter()
            .map(|c| crate::graph::graph_path(workspace_root, &c.path))
            .collect();

        // Staleness sweep: paths this repo owned as of the last cycle
        // that are no longer uncommitted (committed, or the uncommitted
        // change was discarded). Removed *by id*, not by
        // `remove_nodes_for_path` — the `VolatileOverlay` is shared
        // across every repo in the federation (one instance per
        // federation, not per repo), and two repos routinely share a
        // relative path (`src/lib.rs` is the common case). Removing
        // "whatever is at this path" would delete another repo's live
        // nodes at the same path too; removing the exact ids this repo
        // itself inserted there never touches anything it doesn't own.
        //
        // Purging waits until `indexed_current_commit` (graph's
        // `last_commit == HEAD`) so a freshly-committed addition whose
        // indexer pass hasn't run yet is preserved by the overlay
        // until the static graph catches up. The on-disk check is
        // retained for genuine deletions: a committed `git rm` makes
        // `index_one_repo`'s `prune_orphans` remove the path from the
        // static graph permanently (it's no longer in
        // `get_all_tracked_files()`), so `indexed_current_commit`
        // alone would leak the overlay entry for the life of the
        // process. `sync_state` (`enrichment.rs`) calls only
        // `sync_overlay` for each federation repo, by design, never a
        // paired `index()` — so a path can drop out of
        // `get_uncommitted_changes()` well before the static graph is
        // rebuilt for it, and purging eagerly in that window would
        // make a symbol that's still real disappear from *both* the
        // overlay and the graph. The on-disk check distinguishes
        // "commit landed but reindex hasn't run" (file still exists,
        // keep overlay) from "path is genuinely gone" (file gone,
        // purge eagerly).
        {
            let mut owned = self.overlay_paths.lock();
            let stale_paths: Vec<String> = owned
                .keys()
                .filter(|p| !current_paths.contains(*p))
                .cloned()
                .collect();
            for path in stale_paths {
                let deleted_from_disk = !workspace_root.join(&path).is_file();
                if indexed_current_commit || deleted_from_disk {
                    if let Some(ids) = owned.remove(&path) {
                        for id in ids {
                            overlay.remove_node(&id);
                        }
                    }
                }
            }
        }

        // Drop entries for THIS repo's changed paths BEFORE scanning —
        // again by id, for the same cross-repo-sharing reason above.
        for change in &changes {
            let key = crate::graph::graph_path(workspace_root, &change.path);
            let old_ids = self.overlay_paths.lock().remove(&key);
            if let Some(ids) = &old_ids {
                for id in ids {
                    overlay.remove_node(id);
                }
                tracing::debug!(
                    "[federation] sync_overlay: dropped {} stale overlay node(s) for {:?}",
                    ids.len(),
                    change.path
                );
            }

            // Skip LSP re-scan for files that were deleted — there's
            // nothing to scan, and the removal above already wiped them.
            if matches!(change.change_type, crate::git::ChangeType::Deleted) {
                continue;
            }
            match self
                .process_overlay_change(&change.path, &self.last_overlay_lsp_failures)
                .await
            {
                Ok(nodes) => {
                    let active = self.active.lock();
                    if !*active {
                        return Ok(());
                    }
                    let mut ids = Vec::with_capacity(nodes.len());
                    for node in nodes {
                        ids.push(node.id.clone());
                        overlay.insert_node(node);
                    }
                    self.overlay_paths.lock().insert(key.clone(), ids);
                }
                Err(e) => {
                    tracing::warn!(
                        "[federation] overlay refresh: failed for {:?}: {}",
                        change.path,
                        e
                    );
                }
            }
        }

        let failed = self.last_overlay_lsp_failures();
        if failed > 0 {
            tracing::warn!(
                "[federation] overlay refresh: {} file(s) skipped due to LSP unavailability; \
                 overlay coverage is partial this cycle",
                failed
            );
        }
        overlay.touch();
        Ok(())
    }

    /// LSP-then-overlay-insert flow for a single file. Mirrors
    /// `LainServer::process_change` (in `src/server/ingest/ingestion.rs:429`)
    /// and uses `self.source.local_path()` as the workspace root. Returns
    /// parsed nodes; the caller publishes them under the lifecycle gate.
    ///
    /// `lsp_failures` is incremented when the LSP lookup errors out (cold
    /// server, missing language server for this file type, etc.). The caller
    /// aggregates this for a per-cycle warning at the end of `sync_overlay`.
    async fn process_overlay_change(
        self: &Arc<Self>,
        path: &Path,
        lsp_failures: &std::sync::atomic::AtomicU32,
    ) -> Result<Vec<GraphNode>, LainError> {
        // 1. Try the LSP path first. A successful but empty response (cold
        // LSP that hasn't analyzed the file yet) falls through to
        // tree-sitter; only a true `Err` counts as an LSP failure for the
        // per-cycle aggregate.
        //
        // Mirrors the LSP->tree-sitter fallback at
        // `src/server/ingest/scan.rs:113-157` so the federation overlay
        // behaves the same way the main ingestion path already does on CI
        // (where rust-analyzer either times out cold-starting or returns
        // no symbols). Without this fallback the federation overlay stays
        // empty whenever LSP can't deliver symbols, which is the
        // root cause of the watcher_freshness CI failures.
        let lsp_symbols: Option<Vec<HierarchicalSymbol>> = {
            let lsp = self.lsp.next();
            let mut lsp = lsp.lock().await;
            match lsp
                .get_document_symbols_hierarchical(
                    path,
                    self.source.local_path(),
                    &self.id_namespace,
                )
                .await
            {
                Ok(syms) if !syms.is_empty() => Some(syms),
                Ok(_) => None, // cold LSP returned 0 symbols — fall through silently
                Err(e) => {
                    lsp_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        "[federation] no LSP symbols for {:?}: {}; falling back to tree-sitter",
                        path,
                        e
                    );
                    None
                }
            }
        };

        // 2. Tree-sitter fallback for the empty-LSP / LSP-error cases.
        // Same shape as `add_tree_sitter_definitions` in `scan.rs:341` —
        // read the file once, mint a `GraphNode` per `SymbolDef`, and let
        // the caller `insert_node` into the overlay. The id namespace
        // comes from `self.id_namespace` so this repo's nodes never
        // collide with another repo's identically-named symbol
        // (URGENT FIXES #2).
        let symbols = match lsp_symbols {
            Some(s) => s,
            None => {
                let Ok(content) = std::fs::read_to_string(path) else {
                    return Ok(Vec::new());
                };
                let graph_key = crate::graph::graph_path(self.source.local_path(), path);
                crate::treesitter::extract_definitions(path, &content)
                    .into_iter()
                    .map(|d| HierarchicalSymbol {
                        node: GraphNode::new_in(
                            d.kind,
                            d.name.clone(),
                            graph_key.clone(),
                            &self.id_namespace,
                        )
                        .with_location_in(
                            d.line_start,
                            d.line_end,
                            &self.id_namespace,
                        ),
                        children: vec![],
                    })
                    .collect()
            }
        };

        Ok(symbols.into_iter().map(|symbol| symbol.node).collect())
    }
}

// `RepoIndex::drop` shuts the LSP pool down synchronously. Without this hook,
// `LspMultiplexer` -> `LspBridge` -> `LspClient` -> `LspServer` -> `LspProcess`
// drops in field-declaration order on the thread that drops `RepoIndex`.
// `LspProcess::drop` then calls `futures::executor::block_on(self.kill())` to
// reap the spawned LSP child. On a `tokio::current_thread` runtime (which is
// what `#[tokio::test]` defaults to and which the MCP stdio transport uses)
// the worker is the only thread available to drive the kill — `block_on`
// parks it, the SIGCHLD that reaps the child is never delivered, and the
// runtime cannot shut down. From the test's perspective the future after
// `index()` is "still running" forever, even though `index()` itself
// returned successfully.
//
// By the time we get here, `RepoIndex::index` has finished — there are no
// in-flight LSP requests to lose. `LspPool::shutdown_all` calls
// `LspServer::stop` for every registered server, which moves the
// `LspProcess` out of the slot before returning (`self.process.write().await
// .take()`). After that the bridges hold no child handles, so the
// subsequent Drop chain has nothing to `block_on` reap.
//
// `Handle::block_on` panics inside a current_thread runtime, and
// `block_in_place` requires a multi_thread runtime, so we always offload
// the shutdown to a fresh OS thread with its own current_thread runtime
// and a bounded `mpsc` rendezvous. If the shutdown thread overruns its
// budget we drop the wait — the bridges still drop, the leftover
// `LspProcess::drop` reaper runs in the background (or at process exit
// when there is no runtime), and `tokio::process::Child::kill_on_drop`
// still SIGKILLs the child.
//
// `Handle::try_current` lets us skip the synchronous shutdown when no
// runtime is active (e.g. during process teardown). In that case the
// `LspProcess::drop` reaper runs on whatever thread happens to be dropping
// `RepoIndex` and is allowed to take as long as it likes — the runtime
// can't be stuck because there isn't one.
//
// The 10s budget is generous for a healthy LSP server (kill().await is
// synchronous inside `LspServer::stop`) but short enough that one stuck
// bridge can't hold a `#[tokio::test]` future forever. Past the budget
// we abandon the wait — the `LspMultiplexer` -> `LspBridge` chain still
// drops, `tokio::process::Child::kill_on_drop` SIGKILLs the children,
// and the runtime drop completes.
const LSP_SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

impl Drop for RepoIndex {
    fn drop(&mut self) {
        self.deactivate();
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let lsp = self.lsp.clone();
        // Bounded shutdown: see [`run_with_budget`] for the contract.
        // If `shutdown_all` hangs past `LSP_SHUTDOWN_BUDGET`, the
        // shutdown thread is detached and runs to completion in the
        // background; it holds its own `Arc<LspPool>` clone, which
        // drops the LSP child via `kill_on_drop` when the thread's
        // runtime exits. We return from `Drop` promptly either way.
        run_with_budget(
            "lain-repo-index-lsp-shutdown",
            move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build shutdown runtime");
                rt.block_on(async move {
                    lsp.shutdown_all().await;
                });
            },
            LSP_SHUTDOWN_BUDGET,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use std::path::PathBuf;

    #[test]
    fn new_creates_with_indexing_health() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("r").unwrap(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            )
            .unwrap(),
        );
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        assert_eq!(ri.health(), RepoHealth::Indexing);
    }

    #[test]
    fn set_health_updates_state() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("r").unwrap(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            )
            .unwrap(),
        );
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        ri.set_health(RepoHealth::Ready);
        assert_eq!(ri.health(), RepoHealth::Ready);
    }

    #[test]
    fn repo_index_is_send_and_sync() {
        // Compile-time Send/Sync check. Wrapping `GitSensor` in
        // `Arc<AsyncMutex<...>>` gives us `Send + Sync` for free, so no
        // `unsafe impl` is needed on `RepoIndex` itself — this assertion
        // double-checks the auto-traits.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RepoIndex>();
        assert_send_sync::<Arc<RepoIndex>>();
    }

    /// A fresh `RepoIndex` exposes the `indexed_signal` handle as a
    /// `Notify` with no buffered permits. The dispatcher's bounded
    /// wait therefore blocks until `index()` actually fires — the
    /// exact shape that closes the cold-boot race whose symptom was
    /// the "Node not found for handle" flake in
    /// `feat_negative_paths_end_to_end` (closed in commit `3436a51`
    /// together with the test-fixture tempdir-lifetime fix).
    #[tokio::test]
    async fn indexed_signal_starts_unfired_and_fires_after_successful_index() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(src_dir.path()).unwrap();
        // `git2::Repository::init` leaves HEAD pointing at an unborn
        // branch; the indexer reads `get_latest_commit_info()` which
        // errors with "reference 'refs/heads/master' not found" on an
        // unborn repo. Seed an empty initial commit so the head is
        // born (matches what `tests/common::mod.rs::git_init_committed`
        // does for the federation e2e tests).
        {
            let sig = git2::Signature::now("test", "test@lain").unwrap();
            let tree_oid = {
                let mut idx = repo.index().unwrap();
                idx.write_tree().unwrap()
            };
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        // `lib.rs` with one function so tree-sitter can extract a
        // symbol and the indexer succeeds without an LSP server.
        std::fs::write(
            src_dir.path().join("lib.rs"),
            "pub fn indexed_signal_marker() {}\n",
        )
        .unwrap();
        let src: Box<dyn RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new("idx").unwrap(), src_dir.path().to_path_buf())
                .unwrap(),
        );
        let ri = Arc::new(RepoIndex::new(src, tmp.path()).unwrap());
        // Mark the language server unavailable so the indexer falls
        // back to tree-sitter without trying to spawn rust-analyzer —
        // same pattern as the existing tests in this module. Without
        // this, a host with no rust-analyzer on PATH would block the
        // LSP request on its own startup timeout instead of taking
        // the fast fallback path. `LspPool::new` is constructed with
        // size=4 by `RepoIndex::new` (see the constructor above), so
        // four `next()` calls cover every multiplexer.
        for _ in 0..4 {
            ri.lsp.next().lock().await.mark_unavailable("rust-analyzer");
        }

        let signal = ri.indexed_signal();
        // `index_forced` skips the commit-hash short-circuit and
        // walks every tracked file — the right call for a fresh
        // fixture where the per-repo graph is empty.
        ri.index_forced()
            .await
            .expect("index_forced should succeed with tree-sitter fallback");

        // The signal is now ready: a wait bounded at 100 ms must
        // return Ok(_) because the permit is buffered (not consumed
        // before any waiter arrives).
        let notified = signal.notified();
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(100), notified).await;
        assert!(
            outcome.is_ok(),
            "indexed_signal should be ready immediately after index_forced returns Ok"
        );
    }
    #[tokio::test]
    async fn deactivation_clears_owned_overlay_and_stops_watcher() {
        let root = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn edited() {}\n").unwrap();
        let state = tempfile::tempdir().unwrap();
        let repo = Arc::new(
            RepoIndex::new(
                Box::new(
                    WorkspaceDirSource::new(
                        RepoId::new("removed").unwrap(),
                        root.path().to_owned(),
                    )
                    .unwrap(),
                ),
                state.path(),
            )
            .unwrap(),
        );
        for _ in 0..4 {
            repo.lsp
                .next()
                .lock()
                .await
                .mark_unavailable("rust-analyzer");
        }
        repo.sync_overlay().await.unwrap();
        let overlay = repo.server_overlay.lock().clone();
        assert_eq!(overlay.get_all_nodes().len(), 1);
        let unrelated = GraphNode::new(
            crate::schema::NodeType::Function,
            "other".into(),
            "lib.rs".into(),
        );
        overlay.insert_node(unrelated.clone());
        repo.start_watcher().await.unwrap();
        let mut lsp_guards = Vec::new();
        for _ in 0..4 {
            lsp_guards.push(repo.lsp.next().lock_owned().await);
        }
        let scanning = repo.clone();
        let mut in_flight = tokio::spawn(async move { scanning.sync_overlay().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut in_flight)
                .await
                .is_err()
        );
        let mut receiver = crate::server::overlay::subscribe_channel();
        // Seed an owned node because the blocked refresh has removed the old one.
        let owned = GraphNode::new(
            crate::schema::NodeType::Function,
            "owned_at_removal".into(),
            "lib.rs".into(),
        );
        overlay.insert_node(owned.clone());
        repo.overlay_paths
            .lock()
            .insert("lib.rs".into(), vec![owned.id.clone()]);
        let revision = overlay.current_revision();
        repo.deactivate();
        let mut saw_removal = false;
        while let Ok(diff) = receiver.try_recv() {
            saw_removal |= diff.removed.contains(&owned.id);
        }
        assert!(saw_removal);
        assert!(overlay
            .diffs_since(revision)
            .unwrap()
            .iter()
            .any(|d| d.removed.contains(&owned.id)));
        drop(lsp_guards);
        tokio::time::timeout(Duration::from_secs(2), in_flight)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(repo.watcher.lock().is_none());
        assert!(repo.watcher_task.lock().is_none());
        repo.sync_overlay().await.unwrap();
        repo.start_watcher().await.unwrap();
        assert!(repo.watcher.lock().is_none());
        assert_eq!(overlay.get_all_nodes().len(), 1);
        assert!(overlay.get_node(&unrelated.id).is_some());
    }
}
