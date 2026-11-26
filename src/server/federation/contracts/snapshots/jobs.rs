//! Snapshot job runner (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §8.4 "Jobs").
//!
//! One job per missing `(repo, sha, analyzer_version)` cache entry.
//! Jobs are deduplicated across snapshots — two snapshots that need
//! the same `(orders, abc..., 0.9.0+c1)` entry share a single job.
//! The runner owns `LAIN_SNAPSHOT_WORKERS` (default 2) worker
//! threads, polled by the manager. Queue cap is 64; beyond that
//! `prepare_snapshot` returns `busy` with a `retry_after_ms`.
//!
//! The actual indexing work runs through
//! `mirrors::resolve_ref` + `mirrors::worktree_add` + the cache's
//! `write_entry` + `mirrors::worktree_remove`, all under the per-repo
//! `RepoLock`. Indexing itself uses
//! `crate::server::ingest::ingestion::index_one_repo` in
//! `IndexMode::Snapshot` so the on-disk graph stays free of LSP /
//! overlay / resolver / co-change / NLP artefacts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::LainError;
use crate::federation::contracts::index_cache::{build_manifest, CacheKey, IndexCache};
use crate::federation::contracts::mirrors::{
    ensure_mirror, resolve_ref, worktree_add, worktree_prune, worktree_remove, MirrorError,
    RepoLock,
};

type IndexedJobPayload = (
    u64,
    Vec<String>,
    Vec<u8>,
    std::collections::BTreeMap<String, u64>,
    crate::federation::contracts::coverage::RepoCoverage,
);

/// Default worker count (`§8.4`): `LAIN_SNAPSHOT_WORKERS` or 2.
pub fn snapshot_workers() -> usize {
    crate::server::env_util::positive_env("LAIN_SNAPSHOT_WORKERS", DEFAULT_SNAPSHOT_WORKERS)
}

/// Hard queue cap (`§8.4`): 64. Beyond this, `prepare_snapshot`
/// returns `busy` with `retry_after_ms`. Sized so a misconfigured
/// `LAIN_SNAPSHOT_WORKERS` cannot silently pile up thousands of
/// worktrees.
pub const MAX_QUEUED_JOBS: usize = 64;

/// Default worker count when `LAIN_SNAPSHOT_WORKERS` is unset,
/// empty, or unparseable.
pub const DEFAULT_SNAPSHOT_WORKERS: usize = 2;

/// What a single indexing job needs to run.
///
/// `source` is the LAIN-configured source (workspace_dir path /
/// URL) for `ensure_mirror`. `analyzer_version` is the snapshot's
/// analyzer version (always the current build's, since `from`'s
/// analyzer version pins the base to a fixed build).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JobSpec {
    pub repo: String,
    pub sha: String,
    pub analyzer_version: String,
    pub source: String,
}

/// The state of a single job in the runner. The manager consults
/// this to render `repo_states` in the snapshot record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Indexing,
    Done { bytes: u64, files: Vec<String> },
    Failed { error: String },
}

/// One snapshot job. Multiple snapshots that share a `JobSpec` see
/// the same status (`Arc<JobState>`); a single run services them
/// all.
pub struct JobState {
    pub spec: JobSpec,
    pub status: Mutex<JobStatus>,
    /// Subscribers waiting on a status change. Today we use a
    /// simple polling loop in the manager; the `AtomicUsize` is
    /// the published status revision. The `Condvar` is reserved
    /// for a future `wait_for_ready` optimisation that needs to
    /// wake a parked thread without a Tokio runtime.
    pub revision: AtomicU64,
    pub bytes_written: AtomicU64,
    /// Resolved SHA after the worker has called `resolve_ref`.
    /// `None` until that step finishes (or fails). The manager
    /// uses this in `refresh_repo_states` to build the correct
    /// cache key — the cache is written under the resolved SHA,
    /// not the operator-supplied ref.
    pub resolved_sha: Mutex<Option<String>>,
}

impl JobState {
    pub fn new(spec: JobSpec) -> Self {
        Self {
            spec,
            status: Mutex::new(JobStatus::Queued),
            revision: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            resolved_sha: Mutex::new(None),
        }
    }
}

/// The runner's inner state. Lives behind a `Mutex` so the public
/// `JobRunner` API is `&self` for the manager's hot path.
#[derive(Default)]
pub(crate) struct RunnerInner {
    /// Map keyed on `(repo, sha, analyzer_version)`. The map is the
    /// dedup anchor: when a new snapshot needs the same entry, look
    /// up the existing `JobState` and subscribe rather than spawn a
    /// second worker.
    pub(crate) by_key: BTreeMap<JobSpec, Arc<JobState>>,
}

/// The runner. Workers are stateless; the manager schedules jobs by
/// pushing `JobSpec`s into `submit`.
pub struct JobRunner {
    /// Exposed so the worker pool (`snapshot_worker_loop`) can walk
    /// the spec→state map to pick the next queued job.
    pub(crate) inner: Mutex<RunnerInner>,
    /// Cache to write the resulting `graph.bin` payload to.
    cache: IndexCache,
    /// The `data_dir` to host worktrees + mirrors under.
    data_dir: PathBuf,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Counters {
    pub queued: usize,
    pub indexing: usize,
    pub done: usize,
    pub failed: usize,
}

impl JobRunner {
    pub fn new(cache: IndexCache, data_dir: PathBuf) -> Self {
        Self {
            inner: Mutex::new(RunnerInner::default()),
            cache,
            data_dir,
        }
    }

    pub fn cache(&self) -> &IndexCache {
        &self.cache
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn counters(&self) -> Counters {
        // Counters are derived from `JobStatus` of every job in the
        // inner map so the counts are always consistent with the
        // actual job state.
        let inner = self.inner.lock();
        let mut c = Counters::default();
        for state in inner.by_key.values() {
            // Use `try_lock` so a held lock in another thread does
            // not deadlock this read; the worst case is the count
            // under-reports by one for a transient window. Most
            // callers only care about the aggregate shape.
            if let Some(g) = state.status.try_lock() {
                match &*g {
                    JobStatus::Queued => c.queued += 1,
                    JobStatus::Indexing => c.indexing += 1,
                    JobStatus::Done { .. } => c.done += 1,
                    JobStatus::Failed { .. } => c.failed += 1,
                }
            }
        }
        c
    }

    /// Submit a `JobSpec`. Returns the (possibly pre-existing) job
    /// state and `true` when the call enqueued a new job (the caller
    /// should start a worker for it), `false` when the job was
    /// already queued / running / done.
    pub fn submit(&self, spec: JobSpec) -> (Arc<JobState>, bool) {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.by_key.get(&spec) {
            debug!(
                repo = %spec.repo,
                sha = %spec.sha,
                "JobRunner::submit: reusing existing job"
            );
            return (Arc::clone(existing), false);
        }
        let state = Arc::new(JobState::new(spec.clone()));
        inner.by_key.insert(spec.clone(), Arc::clone(&state));
        info!(
            repo = %spec.repo,
            sha = %spec.sha,
            total_jobs = inner.by_key.len(),
            "JobRunner::submit: enqueued new job"
        );
        (state, true)
    }

    /// Look up an existing job. Used by `get_snapshot`'s readiness
    /// check + by tests.
    pub fn lookup(&self, spec: &JobSpec) -> Option<Arc<JobState>> {
        self.inner.lock().by_key.get(spec).cloned()
    }

    /// All jobs whose spec matches a `repo`. Used by the manager to
    /// roll up per-repo state across snapshots.
    pub fn jobs_for_repo(&self, repo: &str) -> Vec<Arc<JobState>> {
        self.inner
            .lock()
            .by_key
            .iter()
            .filter(|(s, _)| s.repo == repo)
            .map(|(_, j)| Arc::clone(j))
            .collect()
    }

    /// Total jobs currently in the runner (queued + indexing + done).
    pub fn total_jobs(&self) -> usize {
        self.inner.lock().by_key.len()
    }

    /// Claim the next `Queued` job for a worker: find it and mark it
    /// `Indexing` under the runner lock, so two workers can never both
    /// pick the same job (finding it and marking it in two steps let
    /// both workers see `Queued` and index the same repo twice).
    pub fn claim_next(&self) -> Option<Arc<JobState>> {
        let inner = self.inner.lock();
        for state in inner.by_key.values() {
            let mut status = state.status.lock();
            if matches!(*status, JobStatus::Queued) {
                *status = JobStatus::Indexing;
                drop(status);
                state.revision.fetch_add(1, Ordering::Release);
                return Some(Arc::clone(state));
            }
        }
        None
    }

    /// Mark a job as `Indexing` (called by the worker before it
    /// starts work). The counters are derived on read; this is the
    /// only state mutation the worker performs.
    pub fn mark_indexing(&self, state: &JobState) {
        *state.status.lock() = JobStatus::Indexing;
        state.revision.fetch_add(1, Ordering::Release);
    }

    /// Mark a job as `Done` (success path). `bytes` is the size of
    /// the cache entry's `graph.bin` payload.
    pub fn mark_done(&self, state: &JobState, bytes: u64, files: Vec<String>) {
        state.bytes_written.store(bytes, Ordering::Relaxed);
        *state.status.lock() = JobStatus::Done { bytes, files };
        state.revision.fetch_add(1, Ordering::Release);
    }

    /// Mark a job as `Failed`. The snapshot record records the
    /// `error` text for the per-repo state.
    pub fn mark_failed(&self, state: &JobState, error: String) {
        *state.status.lock() = JobStatus::Failed { error };
        state.revision.fetch_add(1, Ordering::Release);
    }
}

/// Outcome of running one job.
pub struct JobOutcome {
    pub bytes: u64,
    pub files: Vec<String>,
}

/// Execute a single indexing job synchronously (called from a
/// worker thread). The path is:
///
/// 1. Acquire the per-repo `RepoLock` (the call site enforces the
///    type-level guard).
/// 2. `ensure_mirror` + `resolve_ref` (refreshes the mirror, then
///    resolves the ref — the §8.1 one-fetch-then-fail policy).
/// 3. `worktree_add` for `(data_dir, repo, sha)`.
/// 4. `index_one_repo` in `IndexMode::Snapshot` against a fresh
///    `GraphDatabase` opened in the per-job temp path.
/// 5. Serialize the graph with bincode (`GraphDatabase` writes a
///    `graph.bin`-shaped blob via `save_to_disk_sync`), wrap in a
///    cache entry, atomic rename-into-place.
/// 6. `worktree_remove` and release the lock.
///
/// On any failure the function still releases the lock + worktree
/// before returning; the `JobState` is updated through the runner's
/// `mark_*` helpers so the manager's `refresh_repo_states` sees the
/// failure immediately.
/// `<data_dir>/job-tmp/<repo>`: where a snapshot job keeps its scratch
/// graph while it indexes `repo`.
pub fn job_tmp_root(data_dir: &Path, repo: &str) -> PathBuf {
    data_dir.join("job-tmp").join(repo)
}

/// Removes a directory tree when dropped (best effort).
struct RemoveDirOnDrop(PathBuf);

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What [`sweep_orphaned_job_state`] removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// `<data_dir>/worktrees/<repo>/<sha>` directories.
    pub worktrees: usize,
    /// `<data_dir>/job-tmp/<repo>/*` scratch directories.
    pub job_tmp: usize,
    /// Pre-0.16.3 `$TMPDIR/lain-snapshot-job-*` directories whose owning
    /// process is gone (Linux only: liveness is read from `/proc`).
    pub legacy_tmp: usize,
    /// Repos skipped because a job holds their lock right now.
    pub busy_repos: usize,
}

/// Remove what crashed or killed snapshot jobs left behind.
///
/// A job adds a worktree and a scratch graph under the repo lock and
/// removes both before releasing it. A process that dies mid-job leaves
/// both on disk forever: `git worktree prune` only forgets worktrees
/// whose directory is already gone, and `worktree_add` reuses an
/// existing directory only if the same sha is indexed again. So for each
/// repo whose lock is free (no job is running, in this process or
/// another over the same data dir), everything under its `worktrees/`
/// and `job-tmp/` entries is an orphan and is removed. Repos whose lock
/// is held are left alone: their job cleans up after itself.
pub fn sweep_orphaned_job_state(data_dir: &Path) -> SweepReport {
    sweep_orphaned_job_state_in(data_dir, Some(&std::env::temp_dir()))
}

/// [`sweep_orphaned_job_state`] with the legacy scratch location as a
/// parameter (`None` skips it), so tests never touch the host's `/tmp`.
fn sweep_orphaned_job_state_in(data_dir: &Path, legacy_tmp: Option<&Path>) -> SweepReport {
    let mut report = SweepReport::default();
    let mut repos: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for root in [data_dir.join("worktrees"), data_dir.join("job-tmp")] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    repos.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    for repo in repos {
        let lock = match RepoLock::try_acquire(data_dir, &repo) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                report.busy_repos += 1;
                continue;
            }
            Err(e) => {
                tracing::warn!(repo = %repo, error = %e, "snapshot sweep: cannot lock repo; skipped");
                continue;
            }
        };
        let wt_root = data_dir.join("worktrees").join(&repo);
        if let Ok(entries) = std::fs::read_dir(&wt_root) {
            for entry in entries.flatten() {
                let sha = entry.file_name().to_string_lossy().into_owned();
                // Through git first so the mirror forgets the worktree;
                // a plain removal (then `prune`) when git cannot.
                if worktree_remove(data_dir, &repo, &sha).is_err() || entry.path().exists() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
                if !entry.path().exists() {
                    report.worktrees += 1;
                }
            }
            let _ = worktree_prune(data_dir, &repo);
        }
        let tmp_root = job_tmp_root(data_dir, &repo);
        if let Ok(entries) = std::fs::read_dir(&tmp_root) {
            for entry in entries.flatten() {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    report.job_tmp += 1;
                }
            }
        }
        drop(lock);
    }
    report.legacy_tmp = legacy_tmp.map(sweep_legacy_tmp).unwrap_or(0);
    if report != SweepReport::default() {
        info!(?report, "snapshot sweep: removed orphaned job state");
    }
    report
}

/// Pre-0.16.3 jobs kept their scratch graph in
/// `$TMPDIR/lain-snapshot-job-<repo>-<sha12>-<pid>-<seq>-<uuid>`. Remove
/// the ones whose `<pid>` is no longer running. Only where `/proc` tells
/// liveness (Linux); elsewhere nothing is removed.
fn sweep_legacy_tmp(tmp: &Path) -> usize {
    if !Path::new("/proc/self").exists() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("lain-snapshot-job-") else {
            continue;
        };
        // `<repo>-<sha12>-<pid>-<seq>-<uuid>`; the repo may contain '-'.
        let mut parts = rest.rsplitn(4, '-');
        let (_uuid, _seq, pid) = (parts.next(), parts.next(), parts.next());
        let Some(pid) = pid.and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn run_job(runner: &JobRunner, state: Arc<JobState>) -> Result<JobOutcome, LainError> {
    let spec = state.spec.clone();
    runner.mark_indexing(&state);

    let result = run_job_inner(runner, &state, &spec);
    match &result {
        Ok(outcome) => {
            runner.mark_done(&state, outcome.bytes, outcome.files.clone());
        }
        Err(e) => {
            runner.mark_failed(&state, e.to_string());
        }
    }
    result
}

fn run_job_inner(
    runner: &JobRunner,
    _state: &Arc<JobState>,
    spec: &JobSpec,
) -> Result<JobOutcome, LainError> {
    // Acquire the per-repo lock. The T10 API forces this at the type
    // level — `resolve_ref` and the worktree operations take
    // `&RepoLock`.
    let lock = match RepoLock::acquire(runner.data_dir(), &spec.repo) {
        Ok(l) => l,
        Err(e) => {
            return Err(LainError::Io(format!(
                "snapshot job: failed to acquire repo lock for {}: {e}",
                spec.repo
            )))
        }
    };

    // Ensure the mirror exists. workspace_dir sources pass the
    // local path; clone sources pass the URL.
    ensure_mirror(runner.data_dir(), &spec.repo, &spec.source)
        .map_err(|e| LainError::Io(format!("snapshot job: ensure_mirror failed: {e}")))?;

    // `worktree prune` at the start of every job (the T10 startup
    // prune pattern). Cheap when there's nothing to remove.
    let _ = worktree_prune(runner.data_dir(), &spec.repo);

    // Resolve the ref to a sha (idempotent if the mirror is up to
    // date). Lock is held throughout.
    let sha = resolve_ref(
        &lock,
        runner.data_dir(),
        &spec.repo,
        &spec.sha,
        &spec.source,
    )
    .map_err(|e| match e {
        MirrorError::RefNotFound { repo, ref_str } => LainError::NotFound(format!(
            "snapshot job: ref {ref_str:?} not found in repo {repo}"
        )),
        MirrorError::FetchFailed {
            repo,
            source,
            stderr,
        } => LainError::Io(format!(
            "snapshot job: fetch_failed for {repo} ({source}): {stderr}"
        )),
        other => LainError::Io(format!("snapshot job: mirror error: {other}")),
    })?;

    // Publish the resolved SHA so the manager's `refresh_repo_states`
    // can build the right cache key. The cache is keyed on the
    // resolved SHA, not the operator-supplied ref (`spec.sha`).
    *_state.resolved_sha.lock() = Some(sha.clone());

    // The entry may already be on disk: written by an earlier process
    // (a restart, or a cache baked into an image) or by another
    // snapshot manager over the same data_dir. Indexing it again costs
    // minutes and GBs on a large repo and produces the same bytes.
    let cached_key = CacheKey::new(&spec.repo, &sha, &spec.analyzer_version);
    if runner.cache().has_entry(&cached_key) {
        if let Ok(Some(manifest)) = runner.cache().read_manifest(&cached_key) {
            info!(repo = %spec.repo, sha = %sha, "snapshot job: cache entry present, not re-indexing");
            drop(lock);
            return Ok(JobOutcome {
                bytes: manifest.bytes,
                files: manifest.files,
            });
        }
    }

    let wt_path = worktree_add(runner.data_dir(), &spec.repo, &sha)
        .map_err(|e| LainError::Io(format!("snapshot job: worktree_add failed: {e}")))?;

    // Indexing: open a fresh in-memory `GraphDatabase` (with a temp
    // payload path that gets cleaned up after we serialize) and run
    // `index_one_repo` in snapshot mode.
    let index_result = (|| -> Result<IndexedJobPayload, LainError> {
        // Per-job scratch dir for the per-repo graph, under
        // `<data_dir>/job-tmp/<repo>/` (not `std::env::temp_dir()`: on
        // hosts where /tmp is tmpfs that is RAM, and a crashed job's
        // graph stayed there until reboot). The guard removes it on
        // every exit path, error or success; the repo lock (held for
        // the whole job) lets the startup sweep remove what a crash
        // leaves behind ([`sweep_orphaned_job_state`]).
        static JOB_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = JOB_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let short_sha = &sha[..sha.len().min(12)];
        let tmp = job_tmp_root(runner.data_dir(), &spec.repo).join(format!(
            "{}-{}-{}-{}",
            short_sha,
            std::process::id(),
            seq,
            uuid::Uuid::new_v4().simple(),
        ));
        std::fs::create_dir_all(&tmp)
            .map_err(|e| LainError::Io(format!("snapshot job: create {}: {e}", tmp.display())))?;
        let _tmp_guard = RemoveDirOnDrop(tmp.clone());

        // Run a synchronous indexing pass against the per-job graph.
        // We use the `run_job_index` helper which avoids the async
        // runtime because the job runner is on its own OS thread.
        let (files, sensor_counts, repo_coverage) =
            crate::server::federation::contracts::snapshots::jobs::sync::run_job_index(
                &wt_path,
                &tmp.join("graph.bin"),
                &spec.repo,
                &sha,
                &spec.analyzer_version,
            )?;

        let bytes = std::fs::read(tmp.join("graph.bin"))
            .map_err(|e| LainError::Io(format!("snapshot job: read indexed graph: {e}")))?;
        Ok((
            bytes.len() as u64,
            files,
            bytes,
            sensor_counts,
            repo_coverage,
        ))
    })();

    let (bytes, files, graph_bytes, sensor_counts, repo_coverage) = match index_result {
        Ok(t) => t,
        Err(e) => {
            // Best-effort worktree cleanup so the lock release
            // path is short.
            let _ = worktree_remove(runner.data_dir(), &spec.repo, &sha);
            return Err(e);
        }
    };

    // Write the cache entry. The cache's atomic write protects
    // against a torn entry — see `index_cache::write_entry`.
    let manifest = build_manifest(
        &spec.repo,
        &sha,
        &spec.analyzer_version,
        files.clone(),
        sensor_counts,
        bytes,
    );
    let key = CacheKey::new(&spec.repo, &sha, &spec.analyzer_version);
    if let Err(e) = runner.cache().write_entry(&key, &graph_bytes, &manifest) {
        let _ = worktree_remove(runner.data_dir(), &spec.repo, &sha);
        return Err(e);
    }
    // §8.4 budget: the cache may not grow past LAIN_INDEX_CACHE_MB. Evict the
    // least recently used entries that no analysis holds; entries are
    // regenerable, so this only costs a rebuild later.
    let budget = crate::federation::contracts::index_cache::cache_budget_mb();
    if let Err(e) = runner.cache().evict_lru(budget) {
        tracing::warn!("index cache could not be bounded to {budget} MiB: {e}");
    }
    // Write the coverage ledger alongside the manifest
    let mut ledger = crate::federation::contracts::coverage::CoverageLedger::default();
    ledger.insert(spec.repo.clone(), repo_coverage);
    let ledger_path =
        crate::federation::contracts::index_cache::entry_dir(runner.cache().data_dir(), &key)
            .join(crate::federation::contracts::coverage::LEDGER_FILE);
    if let Err(e) = crate::federation::contracts::coverage::write_ledger(&ledger_path, &ledger) {
        tracing::warn!(
            "could not write coverage ledger to {}: {e}",
            ledger_path.display()
        );
    }
    let _ = worktree_remove(runner.data_dir(), &spec.repo, &sha);
    drop(lock);
    Ok(JobOutcome { bytes, files })
}

/// Synchronous indexing helper. Lives in its own submodule to keep
/// the larger `jobs.rs` file readable.
pub mod sync {
    use std::path::Path;

    use crate::error::LainError;
    use crate::git::{AnyGitSensor, GitSensorMode};
    use crate::graph::GraphDatabase;
    use crate::schema::RepoNamespace;
    use crate::server::ingest::ingestion::{index_one_repo, IndexMode, IndexRequest};

    pub type SnapshotIndexResult = (
        Vec<String>,
        std::collections::BTreeMap<String, u64>,
        crate::federation::contracts::coverage::RepoCoverage,
    );

    /// Run `index_one_repo` in `IndexMode::Snapshot` against a
    /// fresh `GraphDatabase` opened at `graph_path`. Returns the
    /// list of files the indexer walked, ready to record in the
    /// cache manifest.
    pub fn run_job_index(
        workspace: &Path,
        graph_path: &Path,
        repo_id: &str,
        sha: &str,
        analyzer_version: &str,
    ) -> Result<SnapshotIndexResult, LainError> {
        let db = GraphDatabase::new(graph_path)?;
        let git = AnyGitSensor::new(workspace, GitSensorMode::InProcess)
            .map_err(|e| LainError::Git(format!("AnyGitSensor: {e}")))?;
        // The repo id is known here — do not leave `source_repo` as
        // `None`. Sensors whose output is keyed by repo (CODEOWNERS)
        // use it to key their index; without it they fall back to the
        // worktree directory name, which on a snapshot index is a
        // commit SHA and never matches the `GlobalId` repo that
        // `get_service` looks up with.
        let source_repo = crate::federation::repo_id::RepoId::new(repo_id)
            .map_err(|e| LainError::InvalidRepoId(e.to_string()))?;
        let namespace = RepoNamespace::from_repo_id(&source_repo);
        let cancel = tokio_util::sync::CancellationToken::new();

        // Block on the async pass via a single-threaded runtime.
        // The indexer pipeline is mostly CPU-bound (tree-sitter
        // parses + sensor walks), so spinning a fresh runtime per
        // job keeps the per-repo lock window short without
        // borrowing from a shared runtime.
        let workspace_box: std::path::PathBuf = workspace.to_path_buf();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| LainError::Other(format!("tokio runtime: {e}")))?;
        let (files, outcome) = rt.block_on(async {
            let request = IndexRequest {
                path: &workspace_box,
                graph: &db,
                lsp_pool: None,
                git: &git,
                overlay: None,
                resolver: None,
                source_repo: Some(&source_repo),
                namespace: &namespace,
                force: true,
                cancel: &cancel,
                mode: IndexMode::Snapshot,
            };
            let outcome = index_one_repo(request).await?;
            let all = db.get_all_nodes();
            let mut paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for n in all {
                if !n.path.is_empty() {
                    paths.insert(n.path);
                }
            }
            Ok::<_, LainError>((paths.into_iter().collect(), outcome))
        })?;

        let sensor_counts = outcome.sensor_counts.as_map();
        let key = crate::federation::contracts::index_cache::CacheKey::new(
            repo_id,
            sha,
            analyzer_version,
        );
        let repo_coverage = crate::federation::contracts::coverage::coverage_from_reports(
            workspace,
            &key,
            &outcome.sensor_counts,
            &outcome.sensor_reports,
        );
        Ok((files, sensor_counts, repo_coverage))
    }
}

/// Map a `JobStatus` to the per-repo state string for the snapshot
/// record (`§12`).
pub fn repo_state_label(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Indexing => "indexing",
        JobStatus::Done { .. } => "cached",
        JobStatus::Failed { .. } => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(repo: &str, sha: &str) -> JobSpec {
        JobSpec {
            repo: repo.into(),
            sha: sha.into(),
            analyzer_version: "0.9.0+c1".into(),
            source: "/tmp/source".into(),
        }
    }

    #[test]
    fn claim_next_hands_each_queued_job_to_exactly_one_worker() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Arc::new(JobRunner::new(
            IndexCache::new(dir.path()),
            dir.path().to_path_buf(),
        ));
        runner.submit(spec("orders", "abc"));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let runner = Arc::clone(&runner);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    runner.claim_next().is_some()
                })
            })
            .collect();
        let claimed = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|c| *c)
            .count();
        assert_eq!(claimed, 1, "one queued job must be claimed once");
        assert!(runner.claim_next().is_none());
        assert_eq!(runner.counters().indexing, 1);
    }

    /// A one-commit git repo; returns `(dir, sha)`.
    fn one_commit_repo() -> (tempfile::TempDir, String) {
        let src = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(src.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(src.path().join("main.rs"), "fn main() {}\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        let sha = git(&["rev-parse", "HEAD"]);
        (src, sha)
    }

    /// Leave behind exactly what a job killed mid-index leaves: a
    /// worktree registered in the mirror and a scratch graph.
    fn crashed_job_residue(data: &Path, src: &Path, sha: &str) -> (PathBuf, PathBuf) {
        ensure_mirror(data, "orders", &src.display().to_string()).unwrap();
        let lock = RepoLock::acquire(data, "orders").unwrap();
        let wt = worktree_add(data, "orders", sha).unwrap();
        let scratch = job_tmp_root(data, "orders").join("deadbeef-1-0-x");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("graph.bin"), b"partial").unwrap();
        drop(lock);
        assert!(wt.exists() && scratch.exists());
        (wt, scratch)
    }

    fn mirror_worktrees(data: &Path) -> String {
        let out = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(crate::federation::contracts::mirrors::mirror_path(
                data, "orders",
            ))
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn sweep_removes_what_a_crashed_job_left_behind() {
        let (src, sha) = one_commit_repo();
        let data = tempfile::tempdir().unwrap();
        let (wt, scratch) = crashed_job_residue(data.path(), src.path(), &sha);
        assert!(mirror_worktrees(data.path()).contains(&sha));

        let report = sweep_orphaned_job_state_in(data.path(), None);

        assert_eq!(report.worktrees, 1, "{report:?}");
        assert_eq!(report.job_tmp, 1, "{report:?}");
        assert_eq!(report.busy_repos, 0);
        assert!(!wt.exists(), "orphaned worktree still on disk");
        assert!(!scratch.exists(), "orphaned scratch graph still on disk");
        assert!(
            !mirror_worktrees(data.path()).contains(&sha),
            "the mirror still lists the removed worktree"
        );
        // A second sweep finds nothing.
        let again = sweep_orphaned_job_state_in(data.path(), None);
        assert_eq!((again.worktrees, again.job_tmp), (0, 0));
    }

    #[test]
    fn sweep_leaves_a_repo_alone_while_a_job_holds_its_lock() {
        let (src, sha) = one_commit_repo();
        let data = tempfile::tempdir().unwrap();
        let (wt, scratch) = crashed_job_residue(data.path(), src.path(), &sha);
        let running_job = RepoLock::acquire(data.path(), "orders").unwrap();

        let report = sweep_orphaned_job_state_in(data.path(), None);

        assert_eq!(report.busy_repos, 1, "{report:?}");
        assert_eq!((report.worktrees, report.job_tmp), (0, 0));
        assert!(wt.exists() && scratch.exists(), "swept a live job's state");
        drop(running_job);
        let report = sweep_orphaned_job_state_in(data.path(), None);
        assert_eq!((report.worktrees, report.job_tmp), (1, 1), "{report:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_tmp_sweep_removes_only_dead_owners() {
        let tmp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // pid 4_000_000_000 is above any kernel pid_max.
        let dead = tmp
            .path()
            .join("lain-snapshot-job-NeMo-Fabric-4fdf65a4b4c9-4000000000-3-abc");
        let alive = tmp
            .path()
            .join(format!("lain-snapshot-job-orders-0123456789ab-{me}-0-def"));
        let unrelated = tmp.path().join("lain-something-else-4000000000-1-x");
        for d in [&dead, &alive, &unrelated] {
            std::fs::create_dir_all(d).unwrap();
        }
        assert_eq!(sweep_legacy_tmp(tmp.path()), 1);
        assert!(!dead.exists());
        assert!(alive.exists(), "removed the scratch dir of a live process");
        assert!(unrelated.exists(), "removed a dir that is not a job's");
    }

    #[test]
    fn a_finished_job_leaves_no_worktree_or_scratch_dir() {
        let (src, sha) = one_commit_repo();
        let data = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(data.path()), data.path().to_path_buf());
        let (state, _) = runner.submit(JobSpec {
            repo: "orders".into(),
            sha: sha.clone(),
            analyzer_version: "0.9.0+c1".into(),
            source: src.path().display().to_string(),
        });
        run_job(&runner, Arc::clone(&state)).unwrap();
        assert!(matches!(*state.status.lock(), JobStatus::Done { .. }));
        let leftovers = |p: PathBuf| {
            std::fs::read_dir(p)
                .map(|d| d.flatten().count())
                .unwrap_or(0)
        };
        assert_eq!(leftovers(data.path().join("worktrees").join("orders")), 0);
        assert_eq!(leftovers(job_tmp_root(data.path(), "orders")), 0);
    }

    #[test]
    fn run_job_reuses_a_cached_entry_instead_of_reindexing() {
        let src = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(src.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(src.path().join("main.rs"), "fn main() {}\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        let sha = git(&["rev-parse", "HEAD"]);

        let data = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(data.path());
        let key = CacheKey::new("orders", &sha, "0.9.0+c1");
        let manifest = build_manifest(
            "orders",
            &sha,
            "0.9.0+c1",
            vec!["main.rs".into()],
            BTreeMap::new(),
            7,
        );
        cache.write_entry(&key, b"baked", &manifest).unwrap();

        let runner = JobRunner::new(cache.clone(), data.path().to_path_buf());
        let (state, _) = runner.submit(JobSpec {
            repo: "orders".into(),
            sha: sha.clone(),
            analyzer_version: "0.9.0+c1".into(),
            source: src.path().display().to_string(),
        });
        let outcome = run_job(&runner, Arc::clone(&state)).unwrap();
        assert_eq!(outcome.bytes, 7);
        assert_eq!(outcome.files, vec!["main.rs".to_string()]);
        assert!(matches!(*state.status.lock(), JobStatus::Done { .. }));
        // Re-indexing would have replaced the entry with a real graph.
        let graph = crate::federation::contracts::index_cache::graph_path(data.path(), &key);
        assert_eq!(std::fs::read(graph).unwrap(), b"baked");
    }

    #[test]
    fn submit_dedups_identical_specs() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let s = spec("orders", "abc");
        let (a, fresh_a) = runner.submit(s.clone());
        let (b, fresh_b) = runner.submit(s);
        assert!(fresh_a);
        assert!(!fresh_b);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(runner.total_jobs(), 1);
    }

    #[test]
    fn submit_returns_separate_jobs_for_different_specs() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let a = runner.submit(spec("orders", "abc"));
        let b = runner.submit(spec("orders", "def"));
        let c = runner.submit(spec("billing", "abc"));
        assert_eq!(runner.total_jobs(), 3);
        assert!(a.1);
        assert!(b.1);
        assert!(c.1);
    }

    #[test]
    fn job_state_transitions_through_indexing_to_done() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let (state, fresh) = runner.submit(spec("orders", "abc"));
        assert!(fresh);
        assert!(matches!(*state.status.lock(), JobStatus::Queued));
        runner.mark_indexing(&state);
        assert!(matches!(*state.status.lock(), JobStatus::Indexing));
        runner.mark_done(&state, 42, vec!["a.rs".into()]);
        assert!(matches!(*state.status.lock(), JobStatus::Done { .. }));
        let counters = runner.counters();
        assert_eq!(counters.queued, 0);
        assert_eq!(counters.indexing, 0);
        assert_eq!(counters.done, 1);
        assert_eq!(counters.failed, 0);
    }

    #[test]
    fn job_state_transitions_through_indexing_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let (state, fresh) = runner.submit(spec("orders", "abc"));
        assert!(fresh);
        runner.mark_indexing(&state);
        runner.mark_failed(&state, "boom".into());
        {
            let status = state.status.lock();
            match &*status {
                JobStatus::Failed { error } => assert_eq!(error, "boom"),
                _ => panic!("expected Failed"),
            }
        }
        // Drop the status lock before calling `counters()` — the
        // counter derivation uses `try_lock` so a held lock in
        // another scope would under-report.
        let counters = runner.counters();
        assert_eq!(counters.failed, 1);
    }

    #[test]
    fn jobs_for_repo_filters() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        runner.submit(spec("orders", "abc"));
        runner.submit(spec("orders", "def"));
        runner.submit(spec("billing", "abc"));
        let orders_jobs = runner.jobs_for_repo("orders");
        assert_eq!(orders_jobs.len(), 2);
        let billing_jobs = runner.jobs_for_repo("billing");
        assert_eq!(billing_jobs.len(), 1);
    }

    #[test]
    fn repo_state_label_matches_wire_names() {
        assert_eq!(repo_state_label(&JobStatus::Queued), "queued");
        assert_eq!(repo_state_label(&JobStatus::Indexing), "indexing");
        assert_eq!(
            repo_state_label(&JobStatus::Done {
                bytes: 0,
                files: vec![]
            }),
            "cached"
        );
        assert_eq!(
            repo_state_label(&JobStatus::Failed { error: "x".into() }),
            "failed"
        );
    }

    #[test]
    fn default_workers_is_two() {
        // `LAIN_SNAPSHOT_WORKERS` unset → default 2.
        std::env::remove_var("LAIN_SNAPSHOT_WORKERS");
        assert_eq!(snapshot_workers(), DEFAULT_SNAPSHOT_WORKERS);
        assert_eq!(snapshot_workers(), 2);
    }

    #[test]
    fn empty_runner_counters() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let c = runner.counters();
        assert_eq!(c.queued, 0);
        assert_eq!(c.indexing, 0);
        assert_eq!(c.done, 0);
        assert_eq!(c.failed, 0);
    }

    #[test]
    fn submit_does_not_exceed_queue_cap_during_admission() {
        // Admission checks happen in the manager, but the runner
        // must still be usable under a saturated queue: submit
        // returns a job state without panic.
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        for i in 0..MAX_QUEUED_JOBS + 5 {
            let s = spec("repo", &format!("{i:040x}"));
            let _ = runner.submit(s);
        }
        assert_eq!(runner.total_jobs(), MAX_QUEUED_JOBS + 5);
    }

    #[test]
    fn hashmap_lookup_works_after_submit() {
        let dir = tempfile::tempdir().unwrap();
        let runner = JobRunner::new(IndexCache::new(dir.path()), dir.path().to_path_buf());
        let s = spec("orders", "abc");
        runner.submit(s.clone());
        let looked = runner.lookup(&s);
        assert!(looked.is_some());
        let other = runner.lookup(&spec("orders", "def"));
        assert!(other.is_none());
    }
}
