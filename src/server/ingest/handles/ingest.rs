//! Ingest — config, graph, overlay, embedders, git, LSP, tool executor, namespace.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<IngestHandle>` in PR 3.7b and forward `readiness`,
//! `next_revision`, `broadcast_overlay_insert`, the four
//! `overlay_paths_*` helpers, and `shutdown` through.

use crate::git::AnyGitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::{CrossEncoder, NlpEmbedder};
use crate::overlay::{broadcast_overlay_diff, OverlayDiff, RevisionId, VolatileOverlay};
use crate::schema::{GraphNode, RepoNamespace};
use crate::server::ingest::config::LainConfig;
use crate::tools::ToolExecutor;
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// Configuration, persistent graph, volatile overlay, embedder + cross-encoder,
/// git sensor, LSP pool, tool executor, tuning, namespace, and the
/// overlay-path bookkeeping that ties watcher-side insertions to the
/// staleness sweep.
pub struct IngestHandle {
    pub(crate) config: LainConfig,
    pub(crate) graph: GraphDatabase,
    pub(crate) overlay: VolatileOverlay,
    pub(crate) embedder: NlpEmbedder,
    pub(crate) cross_encoder: CrossEncoder,
    pub(crate) git: Arc<AnyGitSensor>,
    pub(crate) lsp_pool: Arc<LspPool>,
    pub(crate) tool_executor: ToolExecutor,
    pub(crate) tuning: Arc<TuningConfig>,
    pub(crate) id_namespace: RepoNamespace,
    pub(crate) overlay_paths: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// B3 — per-path blake3 content hash cache for `process_change`.
    /// A no-op editor save (touch, metadata-only write) fires the
    /// watcher but the file's bytes are unchanged; comparing the
    /// new hash against the cached one short-circuits the LSP round
    /// trip without re-indexing. `parking_lot::Mutex` because every
    /// watcher event acquires it briefly; contention is bounded by
    /// the same lock the watcher itself takes (`process_change_lock`),
    /// so a separate coarse lock is fine.
    pub(crate) file_content_hashes: Arc<Mutex<HashMap<std::path::PathBuf, [u8; 32]>>>,
    pub(crate) process_change_lock: Arc<AsyncMutex<()>>,
    pub(crate) overlay_updated: Arc<Notify>,
    pub(crate) overlay_revision: Arc<AtomicU64>,
    /// Bug #2 from the 2026-09-18 Tauri postmortem: a wedged libgit2
    /// call inside an offthread closure holds the parking_lot
    /// `GitSensor` mutex indefinitely. The mitigation in
    /// `build_core_memory` already fails-fast on `try_lock`, but the
    /// stuck thread keeps running and we still want operators to see
    /// the hang *before* the per-pipeline `index_timeout()` budget
    /// (60 s default) fires. The watchdog task spawned by
    /// [`Self::start_git_sensor_watchdog`] probes this mutex with
    /// `try_lock`; when it's been continuously held for longer than
    /// the configured threshold, the watchdog emits a `tracing::warn!`
    /// with elapsed time and a `scripts/debug-hung-server.sh` pointer.
    ///
    /// `0` means "not currently held". Any other value is the
    /// wall-clock nanosecond timestamp at which the current hold
    /// began (the watchdog publishes it on the free→held transition
    /// via CAS so only the first observer wins, and clears it on the
    /// held→free transition). The offthread closures never touch
    /// it, so the fast path stays lock-free. `get_health` and
    /// `get_capabilities` surface the live timestamp in addition to
    /// the watchdog's `tracing::warn!` signal.
    pub(crate) git_busy_since_nanos: Arc<AtomicU64>,
}

impl IngestHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: LainConfig,
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        cross_encoder: CrossEncoder,
        git: Arc<AnyGitSensor>,
        lsp_pool: Arc<LspPool>,
        tool_executor: ToolExecutor,
        tuning: Arc<TuningConfig>,
        id_namespace: RepoNamespace,
        overlay_paths: Arc<Mutex<HashMap<String, Vec<String>>>>,
        process_change_lock: Arc<AsyncMutex<()>>,
        overlay_updated: Arc<Notify>,
        overlay_revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            config,
            graph,
            overlay,
            embedder,
            cross_encoder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            id_namespace,
            overlay_paths,
            process_change_lock,
            overlay_updated,
            overlay_revision,
            git_busy_since_nanos: Arc::new(AtomicU64::new(0)),
            file_content_hashes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Wall-clock nanosecond timestamp at which the parking_lot
    /// `GitSensor` mutex became continuously held, or `0` if it's
    /// free. See [`Self::start_git_sensor_watchdog`] for the writer.
    /// Exposed so `get_health` can surface the hang to operator
    /// tooling (alertmanager, dashboards) without requiring log
    /// scraping.
    pub fn git_busy_since_unix_nanos(&self) -> u64 {
        self.git_busy_since_nanos
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Spawn a watchdog task that polls the parking_lot `GitSensor`
    /// mutex with `try_lock` and emits `tracing::warn!` when the
    /// mutex has been continuously held for longer than
    /// `threshold_secs`. Companion to the post-fix `build_core_memory`
    /// `try_lock` mitigation (PR #153) — the closure fails fast on a
    /// held mutex, but the stuck spawn_blocking thread keeps running
    /// and the federation transitions to `Degraded` only after the
    /// full `index_timeout()` budget exhausts. This watchdog surfaces
    /// the hang earlier, with a clear message and a pointer to
    /// `scripts/debug-hung-server.sh`.
    ///
    /// When the git sensor runs in `Sidecar` mode, there is no in-process
    /// mutex that could wedge; this spawns an idle task waiting for cancellation.
    ///
    /// Polls every 5 s; warns once per continuous hold past the
    /// threshold (clears `warned` when the mutex is observed free).
    /// Honors `cancel` for clean shutdown.
    pub fn start_git_sensor_watchdog(
        self: &Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
        threshold_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        let git = Arc::clone(&self.git);
        let busy_since = Arc::clone(&self.git_busy_since_nanos);
        if let Some(in_proc_mutex) = git.as_in_process() {
            let in_proc_mutex = Arc::clone(in_proc_mutex);
            tokio::spawn(run_git_sensor_watchdog(
                in_proc_mutex,
                busy_since,
                std::time::Duration::from_secs(threshold_secs),
                std::time::Duration::from_secs(5),
                cancel,
            ))
        } else {
            tokio::spawn(run_sidecar_watchdog(
                git,
                busy_since,
                std::time::Duration::from_secs(5),
                cancel,
            ))
        }
    }

    /// Shared index lifecycle handle. `build_core_memory` reports phase
    /// and progress through this same handle so there is exactly one
    /// place — never a second computation in `doctor`, a tool handler,
    /// or the MCP dispatch gate — that decides what "warming up" means.
    pub fn readiness(&self) -> &crate::server::readiness::ReadinessHandle {
        &self.tool_executor.ctx.readiness
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
    pub fn broadcast_overlay_insert(&self, node: GraphNode) {
        broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![node],
            removed: vec![],
            updated: vec![],
        });
    }

    /// Test-only helper: insert `node` into the overlay and record
    /// `node.id` under the workspace-relative `key` in `overlay_paths`
    /// — the same bookkeeping `process_change` does when LSP
    /// produces real symbols.
    pub fn overlay_paths_test_insert(&self, key: String, node: GraphNode) {
        self.overlay_paths
            .lock()
            .entry(key)
            .or_default()
            .push(node.id.clone());
        self.overlay.insert_node(node);
    }

    /// Test-only helper: read the current `overlay_paths` snapshot.
    pub fn overlay_paths_test_keys(&self) -> Vec<String> {
        self.overlay_paths.lock().keys().cloned().collect()
    }

    /// Record that this server's watcher (or any other overlay writer
    /// outside `process_change`) inserted a node at workspace-relative
    /// `key` with the given `node_id`.
    pub fn overlay_paths_record_insert(&self, key: String, node_id: String) {
        self.overlay_paths
            .lock()
            .entry(key)
            .or_default()
            .push(node_id);
    }

    /// Replace the bookkeeping entry for `key` with a fresh list of
    /// `node_ids`. Used when a re-saved file should drop the previous
    /// version's overlay entries from the same path before inserting
    /// the new ones.
    pub fn overlay_paths_replace(&self, key: String, node_ids: Vec<String>) {
        self.overlay_paths.lock().insert(key, node_ids);
    }

    pub async fn shutdown(&self) {
        tracing::info!("Shutting down Lain server...");
        self.lsp_pool.shutdown_all().await;
    }

    // =============== Field accessors for the LainServer façade (PR 3.7b) ===============

    pub fn config(&self) -> &LainConfig {
        &self.config
    }

    pub fn graph(&self) -> &GraphDatabase {
        &self.graph
    }

    pub fn overlay(&self) -> &VolatileOverlay {
        &self.overlay
    }

    pub fn embedder(&self) -> &NlpEmbedder {
        &self.embedder
    }

    pub fn cross_encoder(&self) -> &CrossEncoder {
        &self.cross_encoder
    }

    pub fn git(&self) -> &Arc<AnyGitSensor> {
        &self.git
    }

    pub fn lsp_pool(&self) -> &Arc<LspPool> {
        &self.lsp_pool
    }

    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }

    pub fn tuning(&self) -> &Arc<TuningConfig> {
        &self.tuning
    }

    pub fn id_namespace(&self) -> &RepoNamespace {
        &self.id_namespace
    }

    pub fn overlay_paths(&self) -> &Arc<Mutex<HashMap<String, Vec<String>>>> {
        &self.overlay_paths
    }

    pub fn process_change_lock(&self) -> &Arc<AsyncMutex<()>> {
        &self.process_change_lock
    }

    /// B3 — read+write access to the per-path blake3 hash cache.
    /// Exposed so `process_change` can compare the live file hash
    /// against the cached one and short-circuit on a no-op write.
    pub(crate) fn file_content_hashes(&self) -> &Arc<Mutex<HashMap<std::path::PathBuf, [u8; 32]>>> {
        &self.file_content_hashes
    }

    pub fn overlay_updated(&self) -> &Arc<Notify> {
        &self.overlay_updated
    }

    pub fn overlay_revision(&self) -> &Arc<AtomicU64> {
        &self.overlay_revision
    }
}

/// Bug #2 watchdog loop. Extracted from
/// [`IngestHandle::start_git_sensor_watchdog`] so the behavior can be
/// unit-tested without spinning up a full `LainServer`. Probes
/// `git.try_lock()`; if the parking_lot mutex has been continuously
/// held for longer than `threshold`, emits `tracing::warn!` and
/// exits the warn-once path (clears when the mutex is observed
/// free). Honors `cancel` for clean shutdown and clears the
/// shared `busy_since` atomic on exit so a server restart doesn't
/// carry a stale timestamp.
///
/// The shared `busy_since` atomic is the *observable* signal —
/// callers reading `IngestHandle::git_busy_since_nanos` get the
/// wall-clock nanosecond timestamp at which the current hold began,
/// or `0` if the mutex is free. `get_health` surfaces this so the
/// watchdog's signal is visible to operator tooling (alertmanager,
/// dashboards) without needing to grep logs. We use `SystemTime`
/// rather than `Instant` so the value is comparable to the host's
/// wall clock — Instant has no global epoch, which would make
/// "how long has this been held?" unanswerable from outside this
/// loop.
///
/// Generic over `T` so tests can exercise the loop with a
/// `Mutex<()>` instead of constructing a real `GitSensor` (which
/// requires an on-disk repo).
async fn run_git_sensor_watchdog<T>(
    git: Arc<parking_lot::Mutex<T>>,
    busy_since: Arc<AtomicU64>,
    threshold: std::time::Duration,
    poll_interval: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut warned = false;
    loop {
        // Use `try_lock` to probe — parking_lot's API is infallible,
        // so `None` means the mutex is currently held by someone else
        // (i.e. a libgit2 call in flight). We don't care who, just that
        // the hold duration exceeds the threshold.
        if git.try_lock().is_none() {
            // CAS-loop: only the first observer of the held
            // transition writes the timestamp. Any later observer
            // sees the existing value and leaves it.
            let now_nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let observed_start_nanos = match busy_since.compare_exchange(
                0,
                now_nanos,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => now_nanos,
                Err(existing) => existing,
            };
            if !warned && observed_start_nanos > 0 {
                let elapsed_nanos = now_nanos.saturating_sub(observed_start_nanos);
                if std::time::Duration::from_nanos(elapsed_nanos) >= threshold {
                    tracing::warn!(
                        threshold_secs = threshold.as_secs(),
                        elapsed_secs = std::time::Duration::from_nanos(elapsed_nanos).as_secs(),
                        "GitSensor parking_lot mutex has been continuously held for \
                         >{}s. A prior call is likely wedged in libgit2 \
                         (Bug #2, 2026-09-18 postmortem). The offthread closures \
                         already fail-fast on try_lock, so subsequent \
                         build_core_memory calls return LainError::Other, but \
                         the stuck spawn_blocking thread keeps running. \
                         Inspect /proc/<pid>/wchan or run \
                         scripts/debug-hung-server.sh for the user's postmortem \
                         recipe.",
                        threshold.as_secs()
                    );
                    warned = true;
                }
            }
        } else {
            // Mutex is free. Clear both the local warn latch and the
            // shared timestamp so the next hold starts fresh.
            busy_since.store(0, std::sync::atomic::Ordering::Relaxed);
            warned = false;
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
    // Clear on exit so a server restart doesn't carry a stale timestamp.
    busy_since.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Bug #2 sidecar watchdog loop. Extracted so behavior can be unit-tested.
/// When running in `Sidecar` mode, there is no parent in-process mutex to wedge.
/// Instead, this loop periodically polls `git.sidecar_health()`:
/// - If `alive` is false, it writes the timestamp to `busy_since` (just like the
///   mutex watchdog) and logs a warning.
/// - When `alive` recovers to true, it clears `busy_since`.
/// Honors `cancel` and clears `busy_since` on exit.
async fn run_sidecar_watchdog(
    git: Arc<AnyGitSensor>,
    busy_since: Arc<AtomicU64>,
    poll_interval: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut warned = false;
    loop {
        if let Some(health) = git.sidecar_health() {
            if !health.alive {
                let now_nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let _ = busy_since.compare_exchange(
                    0,
                    now_nanos,
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                );
                if !warned {
                    tracing::warn!(
                        child_pid = ?health.child_pid,
                        failures = health.consecutive_failures,
                        respawns = health.respawns_in_window,
                        "GitSensor sidecar process is not alive. Calls will attempt auto-recovery or fail-fast."
                    );
                    warned = true;
                }
            } else {
                busy_since.store(0, std::sync::atomic::Ordering::Relaxed);
                warned = false;
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
    busy_since.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_revision_starts_at_one_and_increments() {
        // The actual construction requires a non-trivial graph /
        // tool-executor stack, so we test the revision-counter math in
        // isolation here by spinning up a tiny handle-like wrapper.
        let arc = Arc::new(AtomicU64::new(0));
        let r1 = arc.fetch_add(1, Ordering::Relaxed) + 1;
        let r2 = arc.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(r1, 1);
        assert_eq!(r2, 2);
    }

    /// Companion test to the post-fix Bug #2 mitigation (PR #153):
    /// `run_git_sensor_watchdog` must warn when a parking_lot mutex
    /// has been continuously held for longer than the threshold.
    /// We exercise the loop with a `Mutex<()>` so we don't need a
    /// real `GitSensor` (which requires an on-disk repo). The
    /// `tracing_subscriber::fmt::TestWriter` captures emitted events
    /// into a shared buffer we then grep for the warning text.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn git_sensor_watchdog_warns_when_mutex_held_past_threshold() {
        use std::sync::Mutex as StdMutex;
        use tracing_subscriber::fmt::MakeWriter;

        let buffer = Arc::new(StdMutex::new(Vec::<u8>::new()));

        // `tracing_subscriber` requires a `MakeWriter`; a tiny shim
        // that clones the buffer Arc and writes into it.
        struct BufWriter(Arc<StdMutex<Vec<u8>>>);
        impl<'a> MakeWriter<'a> for BufWriter {
            type Writer = BufWriterHandle;
            fn make_writer(&'a self) -> Self::Writer {
                BufWriterHandle(Arc::clone(&self.0))
            }
        }
        struct BufWriterHandle(Arc<StdMutex<Vec<u8>>>);
        impl std::io::Write for BufWriterHandle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let make_writer = BufWriter(Arc::clone(&buffer));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(make_writer)
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let git: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
        let busy_since: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let cancel = tokio_util::sync::CancellationToken::new();
        let threshold = std::time::Duration::from_millis(150);
        let poll_interval = std::time::Duration::from_millis(20);

        let handle = tokio::spawn(run_git_sensor_watchdog(
            Arc::clone(&git),
            Arc::clone(&busy_since),
            threshold,
            poll_interval,
            cancel.clone(),
        ));

        // Hold the mutex for > threshold + a few poll intervals.
        let _held = git.lock();
        tokio::time::sleep(threshold * 4).await;

        // While the mutex is held past the threshold, the watchdog
        // must publish a non-zero `busy_since` so `get_health` can
        // surface the hang to operator tooling (alertmanager,
        // dashboards) without needing to grep logs.
        assert!(
            busy_since.load(Ordering::Relaxed) > 0,
            "watchdog must publish busy_since while mutex is held past threshold; \
             got {}",
            busy_since.load(Ordering::Relaxed)
        );

        drop(_held);

        // Give the watchdog one more cycle to observe the free mutex
        // and clear `held_since`/`warned` (covers the cleanup branch).
        tokio::time::sleep(poll_interval * 2).await;

        // Once the mutex is free, the watchdog must clear
        // `busy_since` so a subsequent hold starts fresh.
        assert_eq!(
            busy_since.load(Ordering::Relaxed),
            0,
            "watchdog must clear busy_since once the mutex is free"
        );

        cancel.cancel();
        handle.await.expect("watchdog task panicked");

        let captured =
            String::from_utf8(buffer.lock().unwrap().clone()).expect("non-utf8 in tracing buffer");
        assert!(
            captured.contains("GitSensor parking_lot mutex has been continuously held"),
            "expected Bug #2 watchdog warning in tracing output, got:\n{captured}"
        );

        // On clean exit the watchdog must clear the shared atomic so a
        // server restart doesn't carry a stale timestamp.
        assert_eq!(
            busy_since.load(Ordering::Relaxed),
            0,
            "watchdog must clear busy_since atomic on exit"
        );
    }

    /// When the mutex is never held past the threshold, the watchdog
    /// must stay silent. Pins the warn-once reset path: a brief
    /// hold that doesn't cross the threshold must not log.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn git_sensor_watchdog_stays_silent_when_threshold_not_crossed() {
        use std::sync::Mutex as StdMutex;
        use tracing_subscriber::fmt::MakeWriter;

        let buffer = Arc::new(StdMutex::new(Vec::<u8>::new()));
        struct BufWriter(Arc<StdMutex<Vec<u8>>>);
        impl<'a> MakeWriter<'a> for BufWriter {
            type Writer = BufWriterHandle;
            fn make_writer(&'a self) -> Self::Writer {
                BufWriterHandle(Arc::clone(&self.0))
            }
        }
        struct BufWriterHandle(Arc<StdMutex<Vec<u8>>>);
        impl std::io::Write for BufWriterHandle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let subscriber = tracing_subscriber::fmt()
            .with_writer(BufWriter(Arc::clone(&buffer)))
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let git: Arc<parking_lot::Mutex<()>> = Arc::new(parking_lot::Mutex::new(()));
        let busy_since: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let cancel = tokio_util::sync::CancellationToken::new();
        // High threshold; we never hold the mutex long enough to cross it.
        let threshold = std::time::Duration::from_secs(60);
        let poll_interval = std::time::Duration::from_millis(20);

        let handle = tokio::spawn(run_git_sensor_watchdog(
            Arc::clone(&git),
            Arc::clone(&busy_since),
            threshold,
            poll_interval,
            cancel.clone(),
        ));

        // Brief hold well below the threshold.
        {
            let _held = git.lock();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Let the watchdog observe the cleared state.
        tokio::time::sleep(poll_interval * 2).await;

        cancel.cancel();
        handle.await.expect("watchdog task panicked");

        let captured =
            String::from_utf8(buffer.lock().unwrap().clone()).expect("non-utf8 in tracing buffer");
        assert!(
            !captured.contains("GitSensor parking_lot mutex has been continuously held"),
            "watchdog must NOT warn for a brief hold, got:\n{captured}"
        );
    }

    #[tokio::test]
    async fn sidecar_watchdog_clears_on_cancel() {
        let repo_root = std::env::current_dir().unwrap();
        let git = Arc::new(AnyGitSensor::from_env(&repo_root).unwrap());
        let busy_since = Arc::new(AtomicU64::new(0));
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = tokio::spawn(run_sidecar_watchdog(
            git,
            Arc::clone(&busy_since),
            std::time::Duration::from_millis(20),
            cancel.clone(),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
        handle.await.expect("watchdog task panicked");
        assert_eq!(busy_since.load(Ordering::Relaxed), 0);
    }
}
