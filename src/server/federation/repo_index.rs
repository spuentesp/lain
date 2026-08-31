use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_source::RepoSource;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{GraphEdge, GraphNode};
use crate::server::ingest::ingestion::index_one_repo;
use crate::server::overlay::VolatileOverlay;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex as AsyncMutex;

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
    /// Shared handle to the federation's `VolatileOverlay`. `index()`
    /// touches it after a successful index pass so the `Overlay
    /// freshness` banner doesn't read as "stale" the moment the
    /// server comes up. Defaults to a fresh, unconnected overlay
    /// (tests); production wires the federation's overlay in via
    /// [`Self::set_overlay`] right after `add_repo`.
    server_overlay: parking_lot::Mutex<Arc<VolatileOverlay>>,
    /// Active file-system watcher for this repo. `None` until
    /// `start_watcher` is called. The watcher is dropped (and the
    /// background thread stops) when the `RepoIndex` is dropped.
    watcher: parking_lot::Mutex<Option<notify::RecommendedWatcher>>,
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
    cross_repo_resolver: parking_lot::Mutex<Option<Arc<dyn crate::federation::cross_repo::CrossRepoResolver>>>,
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
    pub fn new(source: Box<dyn RepoSource>, data_dir: &Path) -> Result<Self, LainError> {
        let local_path = source.local_path().to_path_buf();
        let db = GraphDatabase::new(&data_dir.join("graph.bin"))?;
        // Read the repo's own `.lain/tuning.toml` (falling back to
        // defaults when absent) rather than hard-coding. The LSP poll
        // settings in particular were documented knobs that nothing read.
        let runtime = crate::tuning::load_tuning_config(&local_path).runtime;
        let lsp = LspPool::new(&local_path, 4, &runtime)?;
        let git = Arc::new(AsyncMutex::new(GitSensor::new(&local_path)?));
        Ok(Self {
            source,
            db,
            lsp,
            git,
            health: Arc::new(RwLock::new(RepoHealth::Indexing)),
            last_indexed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
            server_overlay: parking_lot::Mutex::new(Arc::new(VolatileOverlay::new())),
            watcher: parking_lot::Mutex::new(None),
            overlay_updated: Arc::new(tokio::sync::Notify::new()),
            last_overlay_lsp_failures: std::sync::atomic::AtomicU32::new(0),
            cross_repo_resolver: parking_lot::Mutex::new(None),
        })
    }

    /// Handle on the [`Notify`] the receiver task fires after each
    /// `index()` + `sync_overlay()` cycle. Tests clone this and
    /// `notified().await` instead of polling `tokio::time::sleep`
    /// with a guessed budget. Production code does not need it.
    pub fn overlay_updated(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.overlay_updated)
    }

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
            index_one_repo(
                &path,
                &db,
                &lsp,
                &*git_guard,
                &overlay,
                resolver_ref,
                Some(source_repo),
                false,
            )
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
                self.set_health(RepoHealth::Degraded);
                return Err(LainError::Other(format!(
                    "RepoIndex::index exceeded {:?} budget",
                    budget
                )));
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
            self.set_health(RepoHealth::Degraded);
            return Err(result.unwrap_err());
        }

        *self.last_indexed.write() = SystemTime::now();
        self.set_health(RepoHealth::Ready);
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
            index_one_repo(
                &path,
                &db,
                &lsp,
                &*git_guard,
                &overlay,
                resolver_ref,
                Some(source_repo),
                true,
            )
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
                self.set_health(RepoHealth::Degraded);
                return Err(LainError::Other(format!(
                    "RepoIndex::index_forced exceeded {:?} budget",
                    budget
                )));
            }
        };

        drop(git_guard);

        if let Err(e) = &result {
            tracing::warn!(
                "[federation] index_forced failed for {:?}: {}",
                self.source.local_path(),
                e
            );
            self.set_health(RepoHealth::Degraded);
            return Err(result.unwrap_err());
        }

        *self.last_indexed.write() = SystemTime::now();
        self.set_health(RepoHealth::Ready);
        Ok(())
    }

    /// Attach a `notify::RecommendedWatcher` to this repo's local path.
    ///
    /// The watcher callback runs on notify's inotify thread and pushes the
    /// raw `notify::Result<notify::Event>` into a bounded
    /// (`WATCHER_CHANNEL_DEPTH = 1024`) `tokio::sync::mpsc::Sender` via
    /// `try_send`. A Tokio-spawned receiver task owns the matching
    /// `Receiver` and an `Arc<RepoIndex>`; per event it runs
    /// `me.index().await` (commit-based pipeline) followed by
    /// `me.sync_overlay().await` (working-tree refresh). Failures from
    /// either call are logged via `tracing::debug!` and do not propagate.
    ///
    /// The channel handoff keeps `tokio::spawn` and `.await` off the
    /// inotify thread, which is not a Tokio runtime context — calling
    /// either from the watcher closure directly would panic.
    ///
    /// The watcher is moved into `self.watcher` so it stays alive for
    /// the lifetime of the `RepoIndex`. When the `RepoIndex` is dropped
    /// the receiver task exits on the next `rx.recv()` returning `None`,
    /// the inotify watch is released by `RecommendedWatcher::drop`, and
    /// any events that arrive on a now-dropped channel are silently
    /// dropped (the closure uses `tx.try_send` and ignores `Full`).
    ///
    /// The mpsc channel is **bounded at 1024 events**. `index()` is
    /// itself bounded at [`Self::index_timeout`] (60s by default,
    /// overridable via `LAIN_REINDEX_TIMEOUT`); a stuck LSP child process
    /// or a slow-to-warm LSP could let a `git checkout` storm (hundreds
    /// of files in seconds) **block the receiver task** while it
    /// processes one event at a time. An unbounded queue would let the
    /// inotify-side sender grow without bound until the receiver
    /// unblocked — a memory bomb under pathological workloads. The
    /// bounded queue caps worst-case memory at ~1024 serialized events;
    /// when full, the sender drops the event at `tracing::debug!` level
    /// and the next event from `notify` retries shortly (cooperative
    /// backpressure).
    pub async fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> {
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};
        use std::time::Duration;
        use tokio::sync::mpsc;

        let path = self.source.local_path().to_path_buf();
        let me_for_task = Arc::clone(self);

        // Bounded channel to hand events from notify's inotify thread to
        // a Tokio task. The closure no longer calls `tokio::spawn`
        // directly — that panicked because the inotify thread is not a
        // Tokio runtime context. The closure uses `tx.try_send` and
        // ignores `Full` (logs at debug) so a stuck LSP child or a
        // `git checkout` storm cannot grow the queue without bound.
        const WATCHER_CHANNEL_DEPTH: usize = 1024;
        let (tx, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(WATCHER_CHANNEL_DEPTH);

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
        tokio::spawn(async move {
            while let Some(res) = rx.recv().await {
                if res.is_ok() {
                    // `index_forced` (not `index`) — the watcher fires
                    // on a kernel `notify` event, which is independent
                    // evidence the worktree changed. The commit-hash
                    // short-circuit in `index()` would skip the
                    // re-scan for any edit the user hadn't committed
                    // yet, leaving the per-repo DB stuck at the
                    // previous commit (wishlist #17).
                    if let Err(e) = me_for_task.index_forced().await {
                        tracing::debug!(
                            "[federation] watcher-triggered index failed for {:?}: {}",
                            me_for_task.source.local_path(),
                            e
                        );
                    }
                    if let Err(e) = me_for_task.sync_overlay().await {
                        tracing::debug!(
                            "[federation] watcher-triggered overlay refresh failed for {:?}: {}",
                            me_for_task.source.local_path(),
                            e
                        );
                    }
                    me_for_task.overlay_updated.notify_one();
                }
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
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
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
        let overlay = self.server_overlay.lock().clone();

        // Reset the per-cycle LSP-failure counter at the start so a
        // healthy cycle doesn't inherit a previous cycle's count.
        // `sync_state` reads this via `last_overlay_lsp_failures()`
        // after each per-repo refresh to aggregate the federation-
        // wide count into `RefreshOutcome`.
        self.last_overlay_lsp_failures
            .store(0, std::sync::atomic::Ordering::Relaxed);

        let changes = self.git.lock().await.get_uncommitted_changes()?;

        // Drop entries for THIS repo's changed paths BEFORE scanning.
        // The federation's overlay is shared across repos (one
        // `VolatileOverlay` per federation, not per repo), so a
        // blanket `overlay.clear()` would wipe every other repo's
        // entries — the single-repo behavior was correct only because
        // there was nothing else to clobber. Per-path removal
        // preserves the rest of the federation's working tree.
        for change in &changes {
            let removed = overlay.remove_nodes_for_path(&change.path.to_string_lossy());
            if removed > 0 {
                tracing::debug!(
                    "[federation] sync_overlay: dropped {} stale overlay node(s) for {:?}",
                    removed,
                    change.path
                );
            }
        }

        for change in &changes {
            // Skip LSP re-scan for files that were deleted — there's
            // nothing to scan, and the entry removal above already
            // wiped them.
            if matches!(
                change.change_type,
                crate::git::ChangeType::Deleted
            ) {
                continue;
            }
            if let Err(e) = self
                .process_overlay_change(&change.path, &overlay, &self.last_overlay_lsp_failures)
                .await
            {
                tracing::warn!(
                    "[federation] overlay refresh: failed for {:?}: {}",
                    change.path,
                    e
                );
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
        Ok(())
    }

    /// LSP-then-overlay-insert flow for a single file. Mirrors
    /// `LainServer::process_change` (in `src/server/ingest/ingestion.rs:429`)
    /// but takes the overlay as a parameter and uses `self.source.local_path()`
    /// as the workspace root. The federation has no `LainServer` reference,
    /// so we re-implement the flow here.
    ///
    /// `lsp_failures` is incremented when the LSP lookup errors out (cold
    /// server, missing language server for this file type, etc.). The caller
    /// aggregates this for a per-cycle warning at the end of `sync_overlay`.
    async fn process_overlay_change(
        self: &Arc<Self>,
        path: &Path,
        overlay: &Arc<crate::server::overlay::VolatileOverlay>,
        lsp_failures: &std::sync::atomic::AtomicU32,
    ) -> Result<(), LainError> {
        let symbols = {
            let lsp = self.lsp.next();
            let mut lsp = lsp.lock().await;
            match lsp
                .get_document_symbols_hierarchical(path, self.source.local_path())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    // Promoted from debug! so operators have signal when
                    // overlay coverage is incomplete. The aggregate count
                    // is logged at the end of `sync_overlay`.
                    lsp_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        "[federation] no LSP symbols for {:?}: {} (overlay entry skipped)",
                        path,
                        e
                    );
                    return Ok(());
                }
            }
        };
        for symbol in symbols {
            overlay.insert_node(symbol.node.clone());
        }
        Ok(())
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
}
