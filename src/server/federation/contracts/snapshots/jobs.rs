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
    match std::env::var("LAIN_SNAPSHOT_WORKERS") {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => DEFAULT_SNAPSHOT_WORKERS,
        },
        _ => DEFAULT_SNAPSHOT_WORKERS,
    }
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

    let wt_path = worktree_add(runner.data_dir(), &spec.repo, &sha)
        .map_err(|e| LainError::Io(format!("snapshot job: worktree_add failed: {e}")))?;

    // Indexing: open a fresh in-memory `GraphDatabase` (with a temp
    // payload path that gets cleaned up after we serialize) and run
    // `index_one_repo` in snapshot mode.
    let index_result = (|| -> Result<IndexedJobPayload, LainError> {
        // Per-job tempdir for the per-repo graph. Created in
        // `std::env::temp_dir()` so it does not collide with the
        // federation's own per-repo state directories. The path is
        // only used as the `GraphDatabase` `persistence_path`; we
        // serialize and rename the bytes into the cache entry's
        // `graph.bin` and never leave the temp file behind (the
        // `RunJobCleanup` block below removes any residue).
        let tmp = std::env::temp_dir().join(format!(
            "lain-snapshot-job-{}-{}-{}",
            spec.repo,
            sha,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&tmp);

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
        let _ = std::fs::remove_dir_all(&tmp);
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
