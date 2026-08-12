use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_source::RepoSource;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{EdgeType, GraphEdge, GraphNode};
use crate::server::ingestion::index_one_repo;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex as AsyncMutex;

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
        // Match the existing default ingestion tuning until RepoIndex accepts configuration.
        let lsp = LspPool::new(&local_path, 4)?;
        let git = Arc::new(AsyncMutex::new(GitSensor::new(&local_path)?));
        Ok(Self {
            source,
            db,
            lsp,
            git,
            health: Arc::new(RwLock::new(RepoHealth::Indexing)),
            last_indexed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
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

    /// For every `Calls` edge in this repo's per-repo graph whose target is
    /// NOT a function defined in this repo (i.e., the target is an imported
    /// reference name), return `(source_local_id, target_name)`.
    ///
    /// Used by `FederatedIndex::project_repo` Pass B to resolve cross-repo
    /// `Calls` edges via the federation's `symbol_to_repos` index.
    pub fn external_calls(&self) -> Vec<(String, String)> {
        let local_node_names: std::collections::HashSet<String> = self
            .nodes()
            .into_iter()
            .map(|n| n.name)
            .collect();
        let mut out = Vec::new();
        for edge in self.edges() {
            if edge.edge_type != EdgeType::Calls {
                continue;
            }
            // The per-repo GraphDatabase's id for a node is the node's
            // `name` (see src/graph.rs all_nodes — it uses the node's name
            // as the local id). So if the target_id isn't in the local
            // node-name set, the target is an imported reference.
            let target_name = edge.target_id.clone();
            if !local_node_names.contains(&target_name) {
                out.push((edge.source_id, target_name));
            }
        }
        out
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
    pub async fn index(self: &Arc<Self>) -> Result<(), LainError> {
        let path = self.source.local_path().to_path_buf();
        let db = self.db.clone();
        let lsp = self.lsp.clone();
        let git = Arc::clone(&self.git);

        // Acquire the lock before running the pipeline so we serialize
        // against any concurrent `index()` call (e.g. from the watcher).
        let git_guard = git.lock().await;

        let result = index_one_repo(&path, &db, &lsp, &*git_guard).await;

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
