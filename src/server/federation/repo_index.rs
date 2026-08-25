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
        })
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
    pub fn set_health(&self, health: RepoHealth) {
        *self.health.write() = health;
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
    /// The whole pipeline is wrapped in [`Self::INDEX_TIMEOUT`]. The inner
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
            index_one_repo(&path, &db, &lsp, &*git_guard, &overlay).await
        };
        let result = match tokio::time::timeout(INDEX_TIMEOUT, pipeline).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(
                    "[federation] index timed out after {:?} for {:?}; transitioning to Degraded",
                    INDEX_TIMEOUT,
                    self.source.local_path()
                );
                drop(git_guard);
                self.set_health(RepoHealth::Degraded);
                return Err(LainError::Other(format!(
                    "RepoIndex::index exceeded {:?} budget",
                    INDEX_TIMEOUT
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

    /// Attach a `notify::RecommendedWatcher` to this repo's local path. On
    /// any filesystem event, the watcher re-runs `index()` on a spawned
    /// tokio task. Errors from the re-index are logged inside `index()` and
    /// do not propagate out of the watcher callback.
    ///
    /// The watcher is moved into `self.watcher` so it stays alive for the
    /// lifetime of the `RepoIndex`. The watcher holds a `Fn(Arc<RepoIndex>)`
    /// closure, which is `Send + 'static` because `Arc<RepoIndex>: Send +
    /// Sync + 'static`.
    pub fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> {
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};
        use std::time::Duration;

        let path = self.source.local_path().to_path_buf();
        let me = Arc::clone(self);

        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(_event) = res {
                    let me = Arc::clone(&me);
                    tokio::spawn(async move {
                        if let Err(e) = me.index().await {
                            tracing::debug!(
                                "[federation] watcher-triggered index failed for {:?}: {}",
                                me.source.local_path(),
                                e
                            );
                        }
                    });
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
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let shutdown_thread = std::thread::Builder::new()
            .name("lain-repo-index-lsp-shutdown".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build shutdown runtime");
                rt.block_on(async move {
                    lsp.shutdown_all().await;
                });
                let _ = tx.send(());
            })
            .expect("spawn shutdown thread");
        let _ = rx.recv_timeout(LSP_SHUTDOWN_BUDGET);
        let _ = shutdown_thread.join();
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
