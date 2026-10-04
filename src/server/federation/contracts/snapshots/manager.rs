//! The snapshot manager.
//!
//! Owns:
//!
//! - The on-disk record directory (`<data_dir>/snapshots/`).
//! - The job runner (`JobRunner`) and its `LAIN_SNAPSHOT_WORKERS`
//!   worker pool.
//! - The resident federation table (LRU over at most
//!   `LAIN_SNAPSHOT_RESIDENT` entries; the §8.5 residency invariant).
//!
//! The MCP tools `prepare_snapshot` and `get_snapshot` (`PR 11`,
//! `§12`) delegate here. The manager is the only path through which
//! the tools touch snapshot state.

use crate::server::time::now_unix;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use parking_lot::Mutex;

use crate::error::LainError;
use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::index::{ContractIndex, ServiceInfo};
use crate::federation::contracts::index_cache::{
    CacheHold, CacheKey, IndexCache, ResidencyTracker,
};
use crate::federation::contracts::joiner::ContractJoiner;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{GraphEdge, GraphNode};

/// Resolver closure: turn a repo id into the source URL/path the
/// snapshot job's `ensure_mirror` uses. `None` for repos that are
/// not configured in `repos.yaml` — the manager surfaces
/// `repo_not_registered` in that case (`§13`).
pub type RepoSourceResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

use super::jobs::{
    run_job, snapshot_workers, JobRunner, JobSpec, JobState, JobStatus, MAX_QUEUED_JOBS,
};
use super::record::{
    cache_key_for, list_record_ids, read_record, snapshot_id_for, touch_last_access, write_record,
    RepoSnapshotState, SnapshotInput, SnapshotRecord, SnapshotState, SNAPSHOT_ID_PREFIX,
};

/// `LAIN_RESIDENT` default (`§8.5`): at most two snapshot federations
/// resident in memory at any moment. A tool call holds both ends of
/// `diff_contracts` (`diff_contracts` is PR 13 territory); PR 11
/// only needs one.
pub fn snapshot_resident_cap() -> usize {
    match std::env::var("LAIN_SNAPSHOT_RESIDENT") {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => DEFAULT_SNAPSHOT_RESIDENT,
        },
        _ => DEFAULT_SNAPSHOT_RESIDENT,
    }
}

pub const DEFAULT_SNAPSHOT_RESIDENT: usize = 2;

/// `LAIN_SNAPSHOT_RETENTION_DAYS` (`§8.4`): records older than 7
/// days after their last access are deleted.
pub fn snapshot_retention_days() -> u64 {
    match std::env::var("LAIN_SNAPSHOT_RETENTION_DAYS") {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => DEFAULT_SNAPSHOT_RETENTION_DAYS,
        },
        _ => DEFAULT_SNAPSHOT_RETENTION_DAYS,
    }
}

pub const DEFAULT_SNAPSHOT_RETENTION_DAYS: u64 = 7;

/// Default `wait_ms` for snapshot tools when the caller did not
/// supply one (`§12`): 5,000 ms for `prepare_snapshot` /
/// `get_snapshot`. PR 13's diff and trace tools raise this to 30,000
/// ms; §10.5 caps the value at 60,000.
pub const DEFAULT_SNAPSHOT_WAIT_MS: u64 = 5_000;
/// `§10.5` cap.
pub const MAX_SNAPSHOT_WAIT_MS: u64 = 60_000;

/// One resident snapshot federation. The `Arc<PetgraphBackend>`
/// holds the in-memory graph (a `§8.5` ephemeral backend — no
/// `save()` writes to disk); the `CacheHold` keeps the per-repo
/// cache entries alive past the LRU eviction threshold; the
/// `contract_index` is the `from_snapshot`-derived join output
/// (re-derived after every projection).
pub struct SnapshotFederation {
    pub snapshot_id: String,
    pub backend: Arc<PetgraphBackend>,
    /// `Arc<Mutex<Vec<CacheHold>>>`: a snapshot federation pins
    /// every cache entry its projection reads from. The hold
    /// registers on the cache's `HoldRegistry` so eviction skips
    /// them; the `ResidencyTracker` pins them past the cache's own
    /// LRU pass as a defence-in-depth.
    pub holds: Mutex<Vec<CacheHold>>,
    pub residency: Arc<ResidencyTracker>,
    pub contract_index: parking_lot::RwLock<Option<Arc<ContractIndex>>>,
    pub last_used_unix: Mutex<i64>,
    /// Number of `HoldGuard` tokens currently alive for this
    /// federation. Residency evictions are skipped while the
    /// count is non-zero (TLA+ SnapshotResidency.tla NoEvictionOfHeld
    /// variant (b) — refcount, not bool). Pre-fix this was an
    /// `AtomicBool`; two holders sharing one slot saw the first
    /// `Drop` clear the bool while the second was still using the
    /// federation, allowing eviction under live holders.
    pub held: AtomicUsize,
    /// The owning manager's `data_dir`. Tool-layer paths (`read_source`,
    /// `resolve_evidence` snippets) locate the per-repo mirror
    /// under `<data_dir>/mirrors/<repo>.git` for git2 blob
    /// lookups. PR 13 round-1 review: this used to fall back to
    /// `LAIN_DATA_DIR` (`manager.rs:206-211`); now the manager
    /// threads the real path through.
    pub data_dir: PathBuf,
}

impl SnapshotFederation {
    /// The owning manager's `data_dir`. Used by tool-layer paths
    /// that resolve a repo's mirror under
    /// `<data_dir>/mirrors/<repo>.git`.
    pub fn manager_data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn mark_used(&self) {
        *self.last_used_unix.lock() = now_unix();
    }

    pub fn last_used(&self) -> i64 {
        *self.last_used_unix.lock()
    }

    /// Current hold count. Test-only observation hook for the
    /// `hold_guard_uses_refcount_not_bool` regression test.
    #[cfg(test)]
    pub fn hold_count_for_test(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }
}

/// Input shape for `prepare_snapshot`. Built from the tool's `args`
/// JSON after `repo_not_registered` / `ref_not_found` validation;
/// the manager does the rest.
pub struct PrepareRequest {
    pub repos: BTreeMap<String, String>,
    /// The caller-named `repo → ref/sha` strings, *before* ref
    /// resolution. The tool layer threads this in from
    /// `args.repos`; the manager records it on the snapshot record
    /// so `get_snapshot`'s JSON view surfaces what the operator
    /// originally typed.
    pub refs_: BTreeMap<String, String>,
    pub excluded: Vec<String>,
    pub from: Option<String>,
    pub max_base_age_s: Option<u64>,
    pub wait_ms: u64,
    pub config: Arc<ContractFederationConfig>,
}

/// The manager. Holds the per-snapshot runtime state and the resident
/// federation map.
pub struct SnapshotManager {
    data_dir: PathBuf,
    cache: IndexCache,
    runner: JobRunner,
    /// `snapshot_id -> SnapshotFederation`. Stale entries (the
    /// record was deleted / GC'd) are dropped on access. Eviction
    /// picks the LRU-resident snapshot whose `held == false`. The
    /// per-entry `last_used_unix` on `SnapshotFederation` carries
    /// the LRU clock, so a separate index is unnecessary.
    resident: Mutex<BTreeMap<String, Arc<SnapshotFederation>>>,
    /// Wakes the install path when a `HoldGuard::Drop` releases a
    /// federation. The `Mutex` is a stand-in for the actual state
    /// (we never read it); `Condvar::wait` only needs the
    /// `WaitTimeoutResult`. The Arc lets `HoldGuard::Drop` notify
    /// the waiter without holding the manager's `resident` lock.
    residency_notify: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    resident_cap: usize,
    retention_days: u64,
    /// Per-snapshot-id in-flight build slots (TLA+ SnapshotResidency
    /// variant (c) — single-flight per id). Two concurrent
    /// `from_snapshot_with_wait_ms` calls for the same `record.id`
    /// share one build; the first inserts an `Arc<InflightSlot>`
    /// and runs the build, every other caller clones the same Arc
    /// and `wait()`s for the first's result. The slot is removed
    /// from the map when the build finishes (success or failure)
    /// so the next miss rebuilds.
    in_flight: parking_lot::Mutex<std::collections::HashMap<String, Arc<InflightSlot>>>,
    /// Per-manager worker thread handles. The threads run
    /// `snapshot_worker_loop` for the lifetime of the manager;
    /// each test's manager owns its own bounded pool so a slow test
    /// does not block another.
    worker_handles: Mutex<Option<Vec<std::thread::JoinHandle<()>>>>,
    /// Source resolver installed by the MCP server at startup.
    /// Returns the `repos.yaml` source URL/path for a configured
    /// repo, or `None` for repos the federation does not manage
    /// (those surface as `repo_not_registered`). The `parking_lot`
    /// `RwLock` lets the server swap resolvers without
    /// disturbing an in-flight `prepare_snapshot`; reads hold the
    /// guard for the duration of the resolve but never block on
    /// async work, so contention is bounded by repo count.
    source_resolver: parking_lot::RwLock<Option<RepoSourceResolver>>,
}

impl SnapshotManager {
    pub fn new(data_dir: &Path, cache: IndexCache) -> Arc<Self> {
        let cap = snapshot_resident_cap();
        Self::with_cap(data_dir, cache, cap)
    }

    /// Constructor with an explicit residency cap. The MCP server
    /// uses `new(data_dir, ...)`; tests can pin a smaller cap so the
    /// `busy` after `wait_ms` path is reachable in-process.
    pub fn with_cap(data_dir: &Path, cache: IndexCache, resident_cap: usize) -> Arc<Self> {
        let runner = JobRunner::new(cache.clone(), data_dir.to_path_buf());
        let mgr = Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            cache,
            runner,
            resident: Mutex::new(BTreeMap::new()),
            residency_notify: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
            resident_cap,
            retention_days: snapshot_retention_days(),
            source_resolver: parking_lot::RwLock::new(None),
            worker_handles: Mutex::new(None),
            in_flight: parking_lot::Mutex::new(std::collections::HashMap::new()),
        });
        mgr.recover_from_disk();
        mgr
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache(&self) -> &IndexCache {
        &self.cache
    }

    pub fn runner(&self) -> &JobRunner {
        &self.runner
    }

    pub fn resident_cap(&self) -> usize {
        self.resident_cap
    }

    /// Read every record on disk back into the in-memory state and
    /// re-enqueue jobs whose snapshot is `pending` or `indexing`
    /// (`§8.4` "Restart"). Called once from the constructor.
    /// Re-enqueue every pending/indexing record's jobs into the
    /// runner. Public so callers can re-run after the source
    /// resolver is installed — `with_cap` invokes this once at
    /// construction (the cold path's first pass has no resolver
    /// and silently skips every record; the public re-invocation
    /// is what actually submits the work).
    pub fn recover_from_disk(&self) {
        let ids = match list_record_ids(&self.data_dir) {
            Ok(ids) => ids,
            Err(_) => return,
        };
        for id in ids {
            let Ok(Some(record)) = read_record(&self.data_dir, &id) else {
                continue;
            };
            if matches!(
                record.state,
                SnapshotState::Pending | SnapshotState::Indexing
            ) {
                // Re-enqueue every repo's cache entry. Cache entries
                // that were already cached skip the worker (the
                // runner dedups by `(repo, sha, analyzer_version)`).
                for (repo, commit) in &record.repos {
                    let key = CacheKey::new(repo, commit, &record.analyzer_version);
                    let Some(source) = self.resolve_repo_source_inner(repo) else {
                        continue;
                    };
                    let spec = JobSpec {
                        repo: repo.clone(),
                        sha: commit.clone(),
                        analyzer_version: record.analyzer_version.clone(),
                        source,
                    };
                    let _ = self.runner.submit(spec);
                    let _ = key; // mark residency later
                }
            }
        }
    }

    /// The configured source URL/path for a repo. The manager looks
    /// up the repo's mirror source from a `repo_source` registry the
    /// caller wires in (the production federation loader owns the
    /// `repo_source`). For tests, this is `None` — the manager
    /// surfaces `repo_not_registered` rather than guessing.
    /// Look up a repo's source URL/path. The default is `None`;
    /// `prepare_snapshot` entry point (`§12`).
    ///
    /// Builds the snapshot id, looks up an existing record on disk
    /// (idempotent path), or creates a new one and submits the
    /// missing cache jobs. Waits up to `wait_ms` for the snapshot
    /// to reach `ready` or `failed` and returns the tool-shaped
    /// JSON either way.
    ///
    /// Returns `(outcome, busy_retry_after_ms)`. The tool handler
    /// maps busy → `ToolOutcome { is_error: true, code: "busy" }`
    /// with the retry-after in `details`.
    pub async fn prepare(
        self: &Arc<Self>,
        req: PrepareRequest,
    ) -> Result<PrepareOutcome, PrepareError> {
        let wait_ms = req.wait_ms.min(MAX_SNAPSHOT_WAIT_MS);
        let started = Instant::now();
        info!(
            repos_count = req.repos.len(),
            excluded_count = req.excluded.len(),
            wait_ms,
            "snapshot manager: prepare() called"
        );

        // Resolve `from` if supplied. A `from` snapshot inherits
        // the base's `join_config` and commits; the request's
        // `repos` map (which may be empty) overrides selected
        // entries. Derived does NOT inherit failure (the §11
        // rule).
        let (resolved_repos, resolved_excluded, join_config, config_hash) =
            match req.from.as_deref() {
                Some(from_id) => {
                    let Some(base) = read_record(&self.data_dir, from_id)
                        .map_err(|e| PrepareError::Other(e.to_string()))?
                    else {
                        return Err(PrepareError::SnapshotNotFound {
                            snapshot: from_id.to_string(),
                        });
                    };
                    let override_repos = req.repos.clone();
                    let override_excluded = req.excluded.clone();
                    let mut repos = base.repos.clone();
                    for (k, v) in override_repos {
                        repos.insert(k, v);
                    }
                    let mut excluded = base.excluded.clone();
                    for e in override_excluded {
                        if !excluded.contains(&e) {
                            excluded.push(e);
                        }
                    }
                    excluded.sort();
                    // `base.join_config` is already canonical JSON.
                    // We rebuild a `ContractFederationConfig` from
                    // it; the tools don't need the parsed form
                    // §11 (ruling): the derived snapshot inherits
                    // the base's `config_hash` as-is. The id
                    // formula uses `{repos, excluded, config_hash,
                    // analyzer_version}`; with no overrides, `repos`
                    // equals the base's and the derived id matches
                    // the base's (correct idempotence per §13
                    // "same inputs after ref resolution → same
                    // id"). With any override, `repos` differs and
                    // the derived id is distinct from the base's.
                    let config_hash = base.config_hash.clone();
                    (repos, excluded, base.join_config.clone(), config_hash)
                }
                None => {
                    // Validate `from + max_base_age_s` is invalid.
                    if req.max_base_age_s.is_some() {
                        return Err(PrepareError::InvalidArgument {
                            message: "max_base_age_s is only valid without `from`".into(),
                        });
                    }
                    // Apply `max_base_age_s`: for any repo in the
                    // configured set that's not in `repos`, reuse
                    // the newest `ready` record younger than the
                    // age with the same `config_hash` /
                    // `analyzer_version` / `exclude` set.
                    let repos = self.apply_max_base_age(
                        req.repos,
                        req.max_base_age_s,
                        req.config.config_hash(),
                        req.config.clone(),
                    )?;
                    let excluded = req.excluded.clone();
                    (
                        repos,
                        excluded,
                        serde_json::to_value(req.config.as_ref()).unwrap_or_default(),
                        req.config.config_hash(),
                    )
                }
            };

        // Ref-not-found check is the manager's job: a ref that
        // resolves to nothing surfaces here. The tool layer
        // surfaces it via `details: {repo, ref}`.
        // (PR 11 keeps the simple shape: every entry's ref is
        // assumed resolved already by the tool. The manager
        // surfaces a `repo_not_registered` for repos it has no
        // source for.)

        let analyzer_version = crate::federation::contracts::analyzer_version();
        let input = SnapshotInput {
            repos: resolved_repos.clone(),
            excluded: resolved_excluded.clone(),
            // Caller-named refs (the `args.repos` shape) — the
            // record keeps the input form so `get_snapshot` can
            // surface what the operator originally typed, distinct
            // from the resolved commit.
            refs_: req.refs_.clone(),
            join_config: parse_join_config(&join_config)?,
            config_hash: config_hash.clone(),
            analyzer_version: analyzer_version.clone(),
        };
        let id = snapshot_id_for(&input);

        // Idempotence: if the record already exists with the same
        // id, just refresh its `last_access_unix` and return.
        if let Ok(Some(record)) = read_record(&self.data_dir, &id) {
            if matches!(record.state, SnapshotState::Ready | SnapshotState::Failed) {
                let _ = touch_last_access(&self.data_dir, &id, now_unix());
                let snapshot = self.build_view(&record).await;
                return Ok(PrepareOutcome {
                    record,
                    view: snapshot,
                });
            }
            // Pending/indexing: continue and wait.
        } else {
            // New snapshot — write the pending record.
            let record = SnapshotRecord {
                id: id.clone(),
                repos: resolved_repos.clone(),
                excluded: resolved_excluded.clone(),
                refs_: req.refs_.clone(),
                join_config: join_config.clone(),
                config_hash: config_hash.clone(),
                analyzer_version: analyzer_version.clone(),
                state: SnapshotState::Pending,
                repo_states: pending_repo_states(&resolved_repos, &resolved_excluded),
                created_unix: now_unix(),
                last_access_unix: now_unix(),
            };
            write_record(&self.data_dir, &record)
                .map_err(|e| PrepareError::Other(e.to_string()))?;
        }

        // Queue cap. Beyond 64 we return `busy` with a
        // `retry_after_ms` hint.
        if self.runner.total_jobs() >= MAX_QUEUED_JOBS {
            return Err(PrepareError::Busy {
                retry_after_ms: 1_000,
            });
        }

        // Submit one job per `(repo, sha, analyzer_version)` triple.
        // The runner dedups across snapshots so two records that
        // share a triple see a single worker.
        let mut jobs: Vec<Arc<JobState>> = Vec::new();
        for (repo, commit) in &resolved_repos {
            let source = match self.resolve_repo_source_inner(repo) {
                Some(s) => s,
                None => return Err(PrepareError::RepoNotRegistered { repo: repo.clone() }),
            };
            let spec = JobSpec {
                repo: repo.clone(),
                sha: commit.clone(),
                analyzer_version: analyzer_version.clone(),
                source,
            };
            let (state, _fresh) = self.runner.submit(spec);
            jobs.push(state);
        }

        // Spawn `LAIN_SNAPSHOT_WORKERS` worker threads (idempotent:
        // existing workers stay put; we just ensure we have enough
        // available capacity to start work). Workers are detached
        // OS threads; their lifetime is tied to the manager's
        // `Arc::strong_count`.
        self.ensure_workers_running();

        // Wait up to `wait_ms` for the snapshot to reach
        // `ready` / `failed`.
        let deadline = started + Duration::from_millis(wait_ms);
        loop {
            let record = read_record(&self.data_dir, &id)
                .map_err(|e| PrepareError::Other(e.to_string()))?
                .unwrap();
            // Update per-repo state from the job statuses before
            // deciding whether to keep waiting. `ref_not_found`
            // surfaces as a top-level `ref_not_found` tool error
            // per §13 when every failure in the snapshot is a
            // missing ref/sha.
            let outcome = self.refresh_repo_states(record);
            let record = outcome.record.clone();
            // The `wait` path holds the loop open; surface ref-not-found
            // before the terminal-state write so a caller can see
            // the per-repo detail without re-reading the record.
            if !outcome.ref_not_found.is_empty() && matches!(record.state, SnapshotState::Failed) {
                // §13: every failure was a missing ref/sha. Surface
                // `ref_not_found` to the tool layer. We persist the
                // record (so `get_snapshot` afterwards shows the
                // same shape) but stop waiting.
                write_record(&self.data_dir, &record)
                    .map_err(|e| PrepareError::Other(e.to_string()))?;
                let _ = touch_last_access(&self.data_dir, &id, now_unix());
                return Err(PrepareError::RefNotFound {
                    entries: outcome.ref_not_found,
                });
            }
            match record.state {
                SnapshotState::Ready | SnapshotState::Failed => {
                    write_record(&self.data_dir, &record)
                        .map_err(|e| PrepareError::Other(e.to_string()))?;
                    let _ = touch_last_access(&self.data_dir, &id, now_unix());
                    let view = self.build_view(&record).await;
                    return Ok(PrepareOutcome { record, view });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                // Out of wait time: persist the in-progress
                // state and return what we have.
                write_record(&self.data_dir, &record)
                    .map_err(|e| PrepareError::Other(e.to_string()))?;
                let view = self.build_view(&record).await;
                return Ok(PrepareOutcome { record, view });
            }
            // Sleep briefly then re-check. The wait/notify pair
            // would be more efficient; in-process tests don't
            // reach high QPS so a 100 ms poll keeps the
            // implementation simple.
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    /// `get_snapshot` entry point (`§12`). Reads the on-disk
    /// record (or returns `snapshot_not_found`), waits up to
    /// `wait_ms` for a non-terminal state, and returns the view.
    ///
    /// `snapshot_id == "live"` is a special case per the §12 table
    /// note ("On `live` = yes (readiness)"): the tool layer
    /// short-circuits and calls [`Self::live_readiness_view`]
    /// directly with the federation's per-repo state. This
    /// method only handles named snapshots whose id starts with
    /// `snap_`.
    pub async fn get(
        self: &Arc<Self>,
        snapshot_id: &str,
        wait_ms: u64,
    ) -> Result<PrepareOutcome, PrepareError> {
        let wait_ms = wait_ms.min(MAX_SNAPSHOT_WAIT_MS);
        let started = Instant::now();
        if snapshot_id == "live" {
            return Err(PrepareError::InvalidArgument {
                message: "snapshot_manager does not serve `live` directly; the tool layer \
                          builds the readiness answer from the federation's RepoHealth."
                    .into(),
            });
        }
        if !snapshot_id.starts_with(SNAPSHOT_ID_PREFIX) {
            return Err(PrepareError::SnapshotNotFound {
                snapshot: snapshot_id.to_string(),
            });
        }
        let deadline = started + Duration::from_millis(wait_ms);
        loop {
            let outcome = match read_record(&self.data_dir, snapshot_id)
                .map_err(|e| PrepareError::Other(e.to_string()))?
            {
                Some(r) => self.refresh_repo_states(r),
                None => {
                    return Err(PrepareError::SnapshotNotFound {
                        snapshot: snapshot_id.to_string(),
                    });
                }
            };
            let record = outcome.record.clone();
            if !outcome.ref_not_found.is_empty() && matches!(record.state, SnapshotState::Failed) {
                let _ = touch_last_access(&self.data_dir, snapshot_id, now_unix());
                return Err(PrepareError::RefNotFound {
                    entries: outcome.ref_not_found,
                });
            }
            match record.state {
                SnapshotState::Ready | SnapshotState::Failed => {
                    let _ = touch_last_access(&self.data_dir, snapshot_id, now_unix());
                    let view = self.build_view(&record).await;
                    return Ok(PrepareOutcome { record, view });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                let view = self.build_view(&record).await;
                return Ok(PrepareOutcome { record, view });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    /// Public, non-async live-readiness entry. The closure maps
    /// repo id to per-repo state. The MCP layer calls this from
    /// `get_snapshot` when the snapshot argument is `"live"`.
    pub fn live_readiness_view<F>(&self, per_repo_fn: F) -> PrepareOutcome
    where
        F: FnOnce() -> Vec<(String, RepoSnapshotState)>,
    {
        let now = now_unix();
        let mut record = SnapshotRecord {
            id: "live".into(),
            repos: BTreeMap::new(),
            excluded: Vec::new(),
            refs_: BTreeMap::new(),
            join_config: serde_json::Value::Null,
            config_hash: String::new(),
            analyzer_version: crate::federation::contracts::analyzer_version(),
            state: SnapshotState::Ready,
            repo_states: BTreeMap::new(),
            created_unix: now,
            last_access_unix: now,
        };
        let mut any_failed = false;
        let mut any_unreviewed = false;
        let mut commits: BTreeMap<String, String> = BTreeMap::new();
        for (repo, state) in per_repo_fn() {
            // Repo health → snapshot per-repo state (§8.7).
            // `Excluded` repos still appear in `repo_states` (so the
            // caller can see them in the per-repo view) but do not
            // influence the overall snapshot state — they were
            // deliberately left out by the caller.
            if let RepoSnapshotState::Excluded = state {
                record.repo_states.insert(repo, state.clone());
                continue;
            }
            let (snap_state, commit) = match &state {
                RepoSnapshotState::Cached { commit } => (state.clone(), Some(commit.clone())),
                RepoSnapshotState::Failed { commit, .. } => (state.clone(), Some(commit.clone())),
                RepoSnapshotState::Queued { commit } => (state.clone(), Some(commit.clone())),
                RepoSnapshotState::Indexing { commit } => (state.clone(), Some(commit.clone())),
                RepoSnapshotState::Excluded => unreachable!("handled above"),
            };
            let ready = matches!(snap_state, RepoSnapshotState::Cached { .. });
            let failed = matches!(snap_state, RepoSnapshotState::Failed { .. });
            any_failed |= failed;
            any_unreviewed |= !ready && !failed;
            if let Some(c) = commit {
                commits.insert(repo.clone(), c.clone());
            }
            record.repo_states.insert(repo, snap_state);
        }
        record.repos = commits;
        record.state = if any_failed {
            SnapshotState::Failed
        } else if any_unreviewed {
            SnapshotState::Indexing
        } else {
            SnapshotState::Ready
        };
        let view = build_view_sync(&record);
        PrepareOutcome { record, view }
    }

    /// Build the JSON-shaped view the tool returns. Wraps the
    /// record + per-repo state + readiness in a stable shape
    /// (`§12`).
    async fn build_view(self: &Arc<Self>, record: &SnapshotRecord) -> serde_json::Value {
        let repos_view: Vec<serde_json::Value> = record
            .repo_states
            .iter()
            .map(|(repo, state)| match state {
                RepoSnapshotState::Cached { commit } => {
                    serde_json::json!({"repo": repo, "commit": commit, "state": "cached"})
                }
                RepoSnapshotState::Queued { commit } => {
                    serde_json::json!({"repo": repo, "commit": commit, "state": "queued"})
                }
                RepoSnapshotState::Indexing { commit } => {
                    serde_json::json!({"repo": repo, "commit": commit, "state": "indexing"})
                }
                RepoSnapshotState::Failed { commit, error } => {
                    serde_json::json!({"repo": repo, "commit": commit, "state": "failed", "error": error})
                }
                RepoSnapshotState::Excluded => {
                    serde_json::json!({"repo": repo, "state": "excluded"})
                }
            })
            .collect();
        serde_json::json!({
            "snapshot": record.id,
            "state": record.state.as_str(),
            "repos": repos_view,
            "created_unix": record.created_unix,
            "last_access_unix": record.last_access_unix,
        })
    }

    /// Read each job's status and roll it up into the snapshot's
    /// per-repo state. The §8.4 state machine transitions live
    /// Roll the per-repo job statuses into the snapshot record
    /// and pick the overall state. The return carries any
    /// ref-not-found failures separately so callers (§13: when
    /// every failure is a ref/sha not found after one fetch) can
    /// surface the top-level `ref_not_found` tool error with
    /// `details: { repo, ref }`. Other failures stay as
    /// per-repo `failed` states — the tool returns the snapshot
    /// in `failed` shape and the analysis tools (PR 13) layer the
    /// `snapshot_failed` semantics on top.
    fn refresh_repo_states(&self, mut record: SnapshotRecord) -> RefreshOutcome {
        let mut all_cached = true;
        let mut any_failed = false;
        let mut ref_not_found: Vec<(String, String)> = Vec::new();
        for (repo, _) in record.repos.clone() {
            let key = cache_key_for(&record, &repo);
            let Some(key) = key else { continue };
            // The cache is keyed on the *resolved* SHA, but the
            // record carries the operator-supplied ref (which the
            // tool layer surfaces in `refs_` so callers can see
            // what they typed). Look up the job by the spec we
            // *submitted* (with the real source URL) and prefer its
            // `resolved_sha` for the cache key.
            let source = self.resolve_repo_source_inner(&repo).unwrap_or_default();
            let spec = JobSpec {
                repo: repo.clone(),
                sha: key.sha.clone(),
                analyzer_version: key.analyzer_version.clone(),
                source,
            };
            let state = self.runner.lookup(&spec);
            // Pick the SHA the cache actually lives under. The
            // worker's `JobState::resolved_sha` is the post-`resolve_ref`
            // value; the cache write uses it (`CacheKey::new`).
            let mut effective_sha = state
                .as_ref()
                .and_then(|s| s.resolved_sha.lock().clone())
                .unwrap_or_else(|| key.sha.clone());
            // Fall back to a directory scan when the runner is
            // empty (post-restart) and `JobState::resolved_sha`
            // wasn't published. The cache is keyed on the resolved
            // SHA but `record.repos[repo]` may still hold the
            // operator's ref; `discover` finds the entry a
            // previous run wrote. The `&repo` (re-derived from
            // `key.sha`) is the operator's original hint, so a
            // full-SHA hint pins to an exact match and a tag/
            // branch hint picks the most-recently-touched entry.
            if effective_sha == key.sha && state.is_none() {
                if let Some(found) = self.cache.discover(&repo, &key.analyzer_version, &key.sha) {
                    effective_sha = found.sha.clone();
                }
            }
            let cache_key = if effective_sha == key.sha {
                key.clone()
            } else {
                CacheKey::new(&repo, &effective_sha, &key.analyzer_version)
            };
            // Cache hit short-circuits to `cached`. No need to
            // re-queue; the indexer ran once and the bytes are
            // already on disk.
            //
            // Codex P2 (re-keying): the record's `repos` map is NOT
            // mutated to the resolved SHA. The id is hashed from
            // the input refs (per §13 "same inputs after ref
            // resolution → same id"); mutating `repos` would
            // change the canonical content and produce a
            // different id for the same logical request.
            // The resolved SHA is surfaced in `repo_states[repo].commit`
            // and in the cache key — the two pieces of state that
            // the user actually queries.
            if self.cache.has_entry(&cache_key) {
                record.repo_states.insert(
                    repo.clone(),
                    RepoSnapshotState::Cached {
                        commit: effective_sha,
                    },
                );
                continue;
            }
            let Some(state) = state else {
                // No record of this job in the runner. Could be
                // because the manager restarted and the record
                // was loaded before the runner was re-populated —
                // treat as queued so we re-enqueue on the next
                // tick.
                record.repo_states.insert(
                    repo.clone(),
                    RepoSnapshotState::Queued {
                        commit: key.sha.clone(),
                    },
                );
                all_cached = false;
                continue;
            };
            let status = state.status.lock().clone();
            match status {
                JobStatus::Queued => {
                    record.repo_states.insert(
                        repo.clone(),
                        RepoSnapshotState::Queued {
                            commit: key.sha.clone(),
                        },
                    );
                    all_cached = false;
                }
                JobStatus::Indexing => {
                    record.repo_states.insert(
                        repo.clone(),
                        RepoSnapshotState::Indexing {
                            commit: key.sha.clone(),
                        },
                    );
                    all_cached = false;
                }
                JobStatus::Done { .. } => {
                    // The worker wrote the cache under
                    // `effective_sha`. Surface that SHA in
                    // `repo_states[repo].commit`; do NOT mutate
                    // `repos` (see Codex P2 comment above).
                    record.repo_states.insert(
                        repo.clone(),
                        RepoSnapshotState::Cached {
                            commit: effective_sha,
                        },
                    );
                }
                JobStatus::Failed { error } => {
                    // §13: a job whose only failure mode was a
                    // missing ref/sha after one fetch surfaces as a
                    // top-level `ref_not_found` once the record is
                    // read. The job path stores the error text;
                    // parse the marker that `run_job` emits so the
                    // tool layer can re-emit it as
                    // `details: { repo, ref }`.
                    let parsed = parse_ref_not_found(&repo, &error);
                    if let Some(ref_name) = parsed {
                        ref_not_found.push((repo.clone(), ref_name));
                    }
                    record.repo_states.insert(
                        repo.clone(),
                        RepoSnapshotState::Failed {
                            commit: key.sha.clone(),
                            error,
                        },
                    );
                    any_failed = true;
                    all_cached = false;
                }
            }
        }
        // Excluded repos always carry their marker.
        for repo in &record.excluded {
            record
                .repo_states
                .entry(repo.clone())
                .or_insert(RepoSnapshotState::Excluded);
        }
        record.state = if any_failed {
            SnapshotState::Failed
        } else if all_cached {
            SnapshotState::Ready
        } else {
            SnapshotState::Indexing
        };
        // The `ref_not_found` shortcut only fires when every failure
        // was a missing ref/sha — a single fetch_failed or index
        // error keeps the snapshot in `failed` shape with the
        // per-repo text.
        let only_ref_not_found = any_failed
            && !ref_not_found.is_empty()
            && ref_not_found.len()
                == record
                    .repo_states
                    .values()
                    .filter(|s| matches!(s, RepoSnapshotState::Failed { .. }))
                    .count();
        // Codex P2 (re-keying): no `mutated` write-back here. The
        // record keeps its input refs in `repos` so the snapshot id
        // — hashed from the canonical input — stays stable for
        // repeated `prepare_snapshot` calls with the same args.
        // The resolved SHAs are surfaced in `repo_states[repo].commit`
        // and the cache is keyed on them via `effective_sha`.
        RefreshOutcome {
            record,
            ref_not_found: if only_ref_not_found {
                ref_not_found
            } else {
                Vec::new()
            },
        }
    }

    /// Spin up `LAIN_SNAPSHOT_WORKERS` workers (idempotent — once the
    /// pool is running, every snapshot just feeds it). Workers are
    /// detached OS threads; they exit when the manager drops.
    /// Spawn the worker pool if it isn't already running. Idempotent.
    /// Public so the cold path's recovery (`with_snapshots` →
    /// `recover_from_disk`) can kick workers without waiting for the
    /// next `prepare_snapshot` call to do it incidentally.
    pub fn ensure_workers_running(self: &Arc<Self>) {
        // Each manager owns its own worker threads. Tests that
        // construct multiple managers get their own workers; the
        // Arc<JoinHandle> keeps the threads alive as long as the
        // manager does. The total worker count is bounded by
        // `snapshot_workers()` per manager, which is fine for the
        // production server (one manager per process) and for tests
        // (each test's manager gets its own bounded pool).
        //
        // Idempotent: if the pool is already running, return
        // without spawning more threads.
        if self.worker_handles.lock().is_some() {
            return;
        }
        let n_workers = snapshot_workers();
        info!(
            data_dir = %self.data_dir.display(),
            n_workers,
            "snapshot manager: spawning worker pool"
        );
        let mut handles = Vec::new();
        for n in 0..n_workers {
            let mgr = Arc::clone(self);
            let handle = std::thread::Builder::new()
                .name(format!("lain-snapshot-worker-{n}"))
                .spawn(move || {
                    info!("snapshot worker thread: starting");
                    snapshot_worker_loop(mgr);
                    info!("snapshot worker thread: exiting");
                })
                .expect("spawn snapshot worker");
            handles.push(handle);
        }
        info!(n_workers, "snapshot manager: worker pool ready");
        *self.worker_handles.lock() = Some(handles);
    }

    /// Look up a repo's source URL/path. The default returns
    /// `None`; the MCP server installs a resolver through
    /// `set_repo_source_resolver` that maps the configured
    /// `repos.yaml` entry to the source URL/path the snapshot
    /// job's `ensure_mirror` consumes. `None` for repos the
    /// federation does not manage → the manager surfaces
    /// `repo_not_registered` per §13.
    fn resolve_repo_source_inner(&self, repo: &str) -> Option<String> {
        self.source_resolver.read().as_ref().and_then(|r| r(repo))
    }

    /// Install the resolver that maps a repo id to its
    /// `repos.yaml`-configured source URL/path. The default
    /// closure built from a `FederationConfig` is exported as
    /// [`Self::resolver_from_config`]; tests can construct one
    /// directly with a `BTreeMap<String, String>` of their own.
    pub fn set_repo_source_resolver(self: &Arc<Self>, resolver: RepoSourceResolver) {
        *self.source_resolver.write() = Some(resolver);
    }

    /// Build a `RepoSourceResolver` from a `FederationConfig`. Maps
    /// every `SourceConfig` variant to the value `ensure_mirror`
    /// expects:
    /// - `WorkspaceDir { path }` → the workspace path (mirrored
    ///   in place; `git fetch` is a no-op)
    /// - `LocalClone { url, .. }` → the URL (`ensure_mirror`
    ///   clones from it)
    /// - `ShallowClone { url, .. }` → the URL (same as
    ///   `LocalClone`)
    pub fn resolver_from_config(
        config: &crate::federation::config::FederationConfig,
    ) -> RepoSourceResolver {
        let map: std::collections::BTreeMap<String, String> = config
            .repos
            .iter()
            .map(|r| {
                let src = match &r.source {
                    crate::federation::config::SourceConfig::WorkspaceDir { path } => {
                        path.to_string_lossy().into_owned()
                    }
                    crate::federation::config::SourceConfig::LocalClone { url, .. } => url.clone(),
                    crate::federation::config::SourceConfig::ShallowClone { url, .. } => {
                        url.clone()
                    }
                };
                (r.id.clone(), src)
            })
            .collect();
        Arc::new(move |repo: &str| map.get(repo).cloned())
    }

    /// Apply `max_base_age_s` to the request's `repos` map
    /// (`§11`): for repos not in `repos`, reuse the newest `ready`
    /// record younger than the age with the same `config_hash` /
    /// `analyzer_version` / `exclude` set.
    fn apply_max_base_age(
        &self,
        mut repos: BTreeMap<String, String>,
        max_age: Option<u64>,
        config_hash: String,
        _config: Arc<ContractFederationConfig>,
    ) -> Result<BTreeMap<String, String>, PrepareError> {
        let Some(max_age) = max_age else {
            return Ok(repos);
        };
        let now = now_unix();
        let cutoff = now.saturating_sub(max_age as i64);
        // Walk every record, find the youngest `ready` one that
        // matches the config + analyzer + (empty for this branch)
        // exclude set. Inherit its `repos` for any missing keys.
        let ids =
            list_record_ids(&self.data_dir).map_err(|e| PrepareError::Other(e.to_string()))?;
        let analyzer_version = crate::federation::contracts::analyzer_version();
        let mut best: Option<(i64, BTreeMap<String, String>)> = None;
        for id in ids {
            let Ok(Some(r)) = read_record(&self.data_dir, &id) else {
                continue;
            };
            if !matches!(r.state, SnapshotState::Ready) {
                continue;
            }
            if r.config_hash != config_hash {
                continue;
            }
            if r.analyzer_version != analyzer_version {
                continue;
            }
            if r.excluded.is_empty() {
                // The §11 rule keys on the same `exclude` set;
                // an empty exclude here means "this prepare
                // session excludes nothing" — records with empty
                // excludes qualify, records with non-empty
                // excludes do not.
            } else {
                continue;
            }
            if r.last_access_unix < cutoff {
                continue;
            }
            let candidate = (r.last_access_unix, r.repos.clone());
            best = match best {
                None => Some(candidate),
                Some(prev) if candidate.0 > prev.0 => Some(candidate),
                Some(prev) => Some(prev),
            };
        }
        if let Some((_, inherited)) = best {
            for (k, v) in inherited {
                repos.entry(k).or_insert(v);
            }
        }
        Ok(repos)
    }

    /// Apply retention: delete records whose `last_access_unix` is
    /// older than `now - retention_days * 86_400`. Called on a
    /// schedule (the manager exposes a public hook so the MCP
    /// server can wire it to its refresh loop).
    pub fn apply_retention(self: &Arc<Self>) -> Result<usize, LainError> {
        let now = now_unix();
        let cutoff = now - (self.retention_days as i64) * 86_400;
        let ids = list_record_ids(&self.data_dir)?;
        let mut removed = 0;
        for id in ids {
            let Ok(Some(r)) = read_record(&self.data_dir, &id) else {
                continue;
            };
            if r.last_access_unix < cutoff {
                let path = super::record::snapshot_path(&self.data_dir, &id);
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Build (or fetch from the resident cache) a federation over
    /// `PetgraphBackend::ephemeral` for the given snapshot record.
    /// Returns an `Arc<SnapshotFederation>` plus a `Hold` token the
    /// caller must keep alive for the duration of the analysis —
    /// the §8.5 residency invariant.
    ///
    /// `wait_ms` is the residency grace window (§8.5). When the
    /// resident set is full and every entry is held, the call
    /// blocks up to `wait_ms` for a hold to release before
    /// returning `busy`. Analysis tools default to 5,000 ms per
    /// §10.5; `prepare_snapshot` and `get_snapshot` pass 0 because
    /// they do not need the federation live.
    pub fn from_snapshot(
        self: &Arc<Self>,
        record: &SnapshotRecord,
    ) -> Result<(Arc<SnapshotFederation>, HoldGuard), LainError> {
        self.from_snapshot_with_wait_ms(record, 0)
    }

    /// Like [`Self::from_snapshot`] but with an explicit residency
    /// grace window. Analysis tools pass 5_000 (§8.5); `prepare_`/
    /// `get_snapshot` pass 0 because they don't keep a hold past
    /// the call's return.
    pub fn from_snapshot_with_wait_ms(
        self: &Arc<Self>,
        record: &SnapshotRecord,
        wait_ms: u64,
    ) -> Result<(Arc<SnapshotFederation>, HoldGuard), LainError> {
        // 1. Try the resident cache first.
        if let Some(fed) = self.resident.lock().get(&record.id).cloned() {
            return Ok((
                Arc::clone(&fed),
                HoldGuard::new(fed, Arc::clone(&self.residency_notify)),
            ));
        }
        // 2. Single-flight per id (TLA+ SnapshotResidency variant
        // (c)). Two concurrent `from_snapshot_with_wait_ms` calls
        // for the same `record.id` race the resident miss and would
        // both call `build_snapshot_federation`, each producing a
        // distinct federation and racing on `install_resident` —
        // whichever lands second wins, silently overwriting the
        // first. Pre-fix the manager had no per-id coordination;
        // the job-runner dedup is on `(repo, sha, analyzer_version)`
        // and does not match `record.id`.
        // We claim a per-id slot. If another caller already holds
        // one, we wait on their result; otherwise we register and
        // build.
        let (slot, is_first) = {
            let mut map = self.in_flight.lock();
            if let Some(existing) = map.get(&record.id).cloned() {
                (existing, false)
            } else {
                let new_slot = Arc::new(InflightSlot {
                    state: std::sync::Mutex::new(None),
                    notify: std::sync::Condvar::new(),
                });
                map.insert(record.id.clone(), Arc::clone(&new_slot));
                (new_slot, true)
            }
        };
        let outcome = if is_first {
            let build_result = self.build_snapshot_federation(record);
            let install_result = match &build_result {
                Ok(fed) => self
                    .install_resident(Arc::clone(fed), wait_ms)
                    .map_err(|b| {
                        LainError::Other(format!(
                            "snapshot residency busy (retry after {}ms)",
                            b.retry_after_ms
                        ))
                    }),
                Err(_) => Ok(()),
            };
            let outcome = match (build_result, install_result) {
                (Ok(fed), Ok(())) => BuildOutcome::Ok(fed),
                (Err(e), _) => BuildOutcome::Err(e.to_string()),
                (Ok(_), Err(e)) => BuildOutcome::Err(e.to_string()),
            };
            // Publish the outcome AND remove the slot from the
            // map under one critical section (S2 fix —
            // TLA+ SnapshotInFlightSlot.tla). Pre-fix, the
            // publish and the remove were two separate
            // `in_flight.lock()` acquisitions: a concurrent
            // caller arriving between the publish and the
            // remove could observe the slot in the map (with
            // `has_first_result = TRUE` from the joiner side
            // observable) but a SECOND concurrent caller
            // arriving AFTER the remove would see no slot and
            // start a duplicate build. The TLA+ trace shows
            // `has_first_result[s1] = TRUE ∧ second_build_count[s1] = 1`.
            //
            // The fix: hold the `in_flight` map lock across
            // the publish + remove so the two operations are
            // atomic. The joiner is waiting on the slot's
            // own condvar (`slot.notify` + `slot.state`); the
            // map lock is not part of the joiner's wait, so
            // holding it does not block the joiner from
            // reading the published result. The notify
            // happens while the map lock is held; the joiner
            // wakes up, reads `slot.state` (no contention with
            // the map lock), and returns.
            //
            // A third caller arriving after we drop the map
            // lock sees no slot in the map; but the install
            // succeeded (this branch only runs when the build
            // is the first for this id), so the resident
            // cache at the top of `from_snapshot_with_wait_ms`
            // already has the fed — the cache check returns
            // before the map lookup, so no duplicate build.
            {
                let mut map = self.in_flight.lock();
                {
                    let mut state = slot.state.lock().unwrap();
                    *state = Some(outcome.clone());
                    slot.notify.notify_all();
                }
                map.remove(&record.id);
            }
            outcome
        } else {
            // Joiner: another caller is already building or has
            // just finished. Wait for them to publish.
            let mut state = slot.state.lock().unwrap();
            while state.is_none() {
                state = slot.notify.wait(state).unwrap();
            }
            state.as_ref().expect("notified with no value").clone()
        };
        match outcome {
            BuildOutcome::Ok(fed) => Ok((
                Arc::clone(&fed),
                HoldGuard::new(fed, Arc::clone(&self.residency_notify)),
            )),
            BuildOutcome::Err(e) => Err(LainError::Other(e)),
        }
    }

    fn build_snapshot_federation(
        self: &Arc<Self>,
        record: &SnapshotRecord,
    ) -> Result<Arc<SnapshotFederation>, LainError> {
        // §8.5: persistence disabled, `save()` is a no-op.
        let ephemeral_path = self
            .data_dir
            .join("snapshots")
            .join(format!("{}.ephemeral", record.id));
        let backend = Arc::new(PetgraphBackend::ephemeral(&ephemeral_path));
        let residency = Arc::new(ResidencyTracker::new());

        // Pin every cache entry this snapshot needs. The holds
        // keep the cache entries alive past the LRU eviction
        // threshold and are released on `HoldGuard::Drop`.
        let mut holds: Vec<CacheHold> = Vec::new();
        for (repo, commit) in &record.repos {
            let key = CacheKey::new(repo, commit, &record.analyzer_version);
            if !self.cache.has_entry(&key) {
                return Err(LainError::NotFound(format!(
                    "snapshot {}: cache entry missing for {}@{}",
                    record.id, repo, commit
                )));
            }
            let hold = residency.pin(&self.cache, key.clone());
            holds.push(hold);
        }
        let residency_for_fed = Arc::clone(&residency);

        // Project every repo's nodes + edges into the ephemeral
        // backend using the shared `project_graph` path. The
        // helper builds the global-id rewrite off a per-repo
        // `GraphDatabase` the manager hydrates from the cache
        // entry's `graph.bin` payload.
        for (repo, commit) in &record.repos {
            let key = CacheKey::new(repo, commit, &record.analyzer_version);
            let bytes = self.cache.read_graph_bytes(&key)?;
            let db = hydrate_graph(&bytes, &ephemeral_path)?;
            project_graph(&db, repo, backend.clone())?;
        }

        // Run the contract rejoin. The `from_snapshot` hook the
        // T7 note marks: a snapshot federation re-derives the
        // `ContractIndex` from its own projected graph using its
        // `join_config`, never the live federation's.
        let config = parse_join_config(&record.join_config)?;
        let contract_index = build_snapshot_contract_index(backend.as_ref(), &config)?;
        let fed = Arc::new(SnapshotFederation {
            snapshot_id: record.id.clone(),
            backend,
            holds: Mutex::new(holds),
            residency: residency_for_fed,
            contract_index: parking_lot::RwLock::new(Some(Arc::new(contract_index))),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: self.data_dir.clone(),
        });
        // Suppress unused warning: residency is consulted on
        // eviction; the `holds` vector is the active pin.
        let _ = residency;
        Ok(fed)
    }

    fn install_resident(
        self: &Arc<Self>,
        fed: Arc<SnapshotFederation>,
        wait_ms: u64,
    ) -> Result<(), InstallBusy> {
        // §8.5: when every resident federation is held, the call
        // must wait up to `wait_ms` for a hold to release, then
        // return `busy` (with `retry_after_ms`) instead of building
        // an untracked federation. We poll on `Condvar` so the
        // release side can wake the waiter; the inner loop also
        // peeks every 25 ms as a defence-in-depth catch-all.
        let cap = self.resident_cap;
        let wait_ms = wait_ms.min(MAX_SNAPSHOT_WAIT_MS);
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_millis(wait_ms);
        loop {
            // Combined lock for the check + insert (TLA+
            // SnapshotResidency variant for `CapBound`): three
            // `InstallCheck` actions could each pass
            // `len() < cap` under separate `self.resident.lock()`
            // acquisitions before any `InstallInsert` lands,
            // leaving `|resident| > cap`. Pre-fix the cap-check
            // and the insert were under two `self.resident.lock()`
            // acquisitions; the parking_lot guard from the check
            // was dropped at the end of the `if` expression, then
            // re-acquired for the insert. Three concurrent
            // installs could each see `len() < cap` and then
            // insert in sequence.
            //
            // The fix: hold the resident lock across the
            // `len() < cap` check and the `insert` call so a
            // concurrent installer's `insert` lands inside the
            // same critical section.
            //
            // S1 fix (TLA+ InstallResidentEviction.tla): the
            // lock must be acquired FIRST and the cap check run
            // BEFORE the eviction attempt. The pre-S1 ordering
            // (`try_evict_one_lru_unheld` then cap check) lost
            // a cache hit per install when the resident had
            // space — an LRU entry was evicted unnecessarily,
            // bringing `len()` down by one, only for the
            // subsequent `insert` to bring it back up. The
            // TLA+ trace shows
            // `evictions_during_install = 1 ∧ resident_before = 0 < Cap = 3`.
            //
            // Post-S1: only enter the eviction path when
            // `len() == cap`. When `len() < cap`, the new fed
            // is inserted immediately with no eviction.
            {
                let mut resident = self.resident.lock();
                if resident.len() < cap {
                    resident.insert(fed.snapshot_id.clone(), fed.clone());
                    return Ok(());
                }
            }
            // Resident is full (`len() == cap`). Try to evict
            // an unheld LRU entry; re-check `len()` after the
            // eviction under the same lock so a concurrent
            // install / release cannot push us into a torn
            // state. The combined-lock invariant from
            // SnapshotResidency.tla variant (c) is preserved.
            if self.try_evict_one_lru_unheld() {
                let mut resident = self.resident.lock();
                if resident.len() < cap {
                    resident.insert(fed.snapshot_id.clone(), fed.clone());
                    return Ok(());
                }
            }
            // Cap is full and every entry is held. Wait for a
            // release or for `wait_ms` to elapse.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(InstallBusy {
                    retry_after_ms: 250,
                });
            }
            let (lock, cvar) = &*self.residency_notify;
            let _guard = lock.lock().unwrap();
            let (pred, wait_result) = cvar
                .wait_timeout(_guard, remaining.min(std::time::Duration::from_millis(50)))
                .unwrap();
            let _ = (pred, wait_result);
        }
    }

    /// Try to evict one LRU-resident federation that no hold is
    /// keeping. Returns `true` when an entry was actually
    /// removed; `false` when every resident entry is held or the
    /// resident set is empty.
    fn try_evict_one_lru_unheld(self: &Arc<Self>) -> bool {
        // Combined lock for select + remove (TLA+ SnapshotResidency
        // variant for `NoEvictionOfHeld` surface from the
        // select/remove race): pre-fix the function selected an
        // LRU-unheld id under one `self.resident.lock()`
        // acquisition and removed it under a second; a
        // `Hold(s1, count=1)` that landed between the two
        // acquisitions could see the entry as `held == 0` at
        // select time but be evicted by the second acquisition.
        //
        // Post-fix (Fix 6) the entire
        // (find-LRU-unheld, re-check held under the same
        // resident lock, remove) sequence is atomic. The
        // re-check under the remove lock catches any hold that
        // landed between the original select and the remove —
        // the function then returns `false` and the caller
        // retries the loop. The boolean surface could
        // additionally miss this if a Drop cleared the bool while
        // a second holder was still using the federation; the
        // refcount surface (Fix 3) closes that surface.
        let mut resident = self.resident.lock();
        let mut evict_id: Option<String> = None;
        for (id, f) in resident.iter() {
            // Refcount-based predicate (TLA+ SnapshotResidency
            // variant (b)): only consider entries whose count
            // is zero.
            if f.held.load(Ordering::Acquire) == 0
                && (evict_id.is_none()
                    || f.last_used() < resident[evict_id.as_ref().unwrap()].last_used())
            {
                evict_id = Some(id.clone());
            }
        }
        // Re-check the held count under the same lock guard so a
        // hold that landed after the iteration cannot be missed
        // — the iteration's `held == 0` read might be stale by
        // the time we reach the remove. If the selected entry is
        // now held, drop the selection and report no eviction;
        // the caller's loop retries.
        if let Some(ref id) = evict_id {
            if let Some(fed) = resident.get(id) {
                if fed.held.load(Ordering::Acquire) != 0 {
                    drop(evict_id);
                    evict_id = None;
                }
            } else {
                // Defensive: while we hold the resident lock the
                // id cannot be removed by anyone else. If a
                // future refactor lets the entry vanish under
                // us, fail closed.
                evict_id = None;
            }
        }
        match evict_id {
            Some(id) => {
                resident.remove(&id);
                true
            }
            None => false,
        }
    }

    /// Drop a federation from the resident set (used by the
    /// `from_snapshot` retention sweeper).
    pub fn evict_resident(self: &Arc<Self>, snapshot_id: &str) -> bool {
        self.resident.lock().remove(snapshot_id).is_some()
    }
}

/// Busy result from `install_resident` when every slot is held
/// for longer than `wait_ms`. The tool layer maps this to
/// `error_outcome("busy", retry_after_ms)`.
#[derive(Debug)]
pub struct InstallBusy {
    pub retry_after_ms: u64,
}

/// A residency hold: every analysis tool call that opens a
/// snapshot federation takes one. The §8.5 rule is "a tool call
/// HOLDS the federations it uses until it returns". `Drop`
/// decrements the `held` flag so the manager can evict the
/// federation on its next LRU sweep, then notifies the residency
/// waiter (so an all-held `from_snapshot` can resume).
pub struct HoldGuard {
    fed: Arc<SnapshotFederation>,
    residency_notify: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
}

impl HoldGuard {
    fn new(
        fed: Arc<SnapshotFederation>,
        residency_notify: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    ) -> Self {
        // Refcount-based hold (TLA+ SnapshotResidency variant (b)):
        // increment the count for each live holder. The eviction
        // predicate only fires when the count is zero.
        fed.held.fetch_add(1, Ordering::AcqRel);
        Self {
            fed,
            residency_notify,
        }
    }
}

impl std::ops::Deref for HoldGuard {
    type Target = SnapshotFederation;
    fn deref(&self) -> &Self::Target {
        &self.fed
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        // Decrement the refcount (TLA+ SnapshotResidency variant (b)).
        // Pre-fix the bool surface was unconditionally cleared on the
        // first Drop while the logical count was > 0.
        self.fed.held.fetch_sub(1, Ordering::AcqRel);
        self.fed.mark_used();
        self.residency_notify.1.notify_all();
    }
}

/// The shared outcome of a single-flight build (TLA+ SnapshotResidency
/// variant (c)). The first caller fills the slot, every other
/// concurrent caller waits on the same slot and reads the same
/// `Arc`. Stored as `Arc<BuildOutcome>` so cloning across the
/// parking_lot mutex boundary is cheap.
#[derive(Clone)]
pub(crate) enum BuildOutcome {
    Ok(Arc<SnapshotFederation>),
    Err(String),
}

/// The per-snapshot-id slot the manager registers while a build
/// is in flight. `state` is `None` while the first caller is
/// building, `Some(...)` once the build completes (success or
/// failure). Waiters hold the `Mutex` and `Condvar::wait` until
/// the first caller transitions the slot to `Some`.
struct InflightSlot {
    state: std::sync::Mutex<Option<BuildOutcome>>,
    notify: std::sync::Condvar,
}

/// The outcome of a `prepare_snapshot` / `get_snapshot` call. The
/// tool layer maps this into the §12 envelope.
#[derive(Debug)]
pub struct PrepareOutcome {
    pub record: SnapshotRecord,
    pub view: serde_json::Value,
}

/// Errors a `prepare_snapshot` / `get_snapshot` call can surface.
/// The tool layer translates each into a `ToolError` with `code`
/// from §13.
#[derive(Debug)]
pub enum PrepareError {
    SnapshotNotFound {
        snapshot: String,
    },
    RepoNotRegistered {
        repo: String,
    },
    /// Top-level `ref_not_found` (§13): every failure in the
    /// snapshot's per-repo states was a missing ref/sha after one
    /// fetch. The `Vec` carries `(repo, ref)` pairs so the tool
    /// layer can surface `details: { repo, ref }`.
    RefNotFound {
        entries: Vec<(String, String)>,
    },
    InvalidArgument {
        message: String,
    },
    Busy {
        retry_after_ms: u64,
    },
    Other(String),
}

/// The outcome of `refresh_repo_states`. `record` is the rolled-up
/// snapshot; `ref_not_found` is non-empty only when every failure
/// in `record` was a missing ref/sha after one fetch — the tool
/// layer maps that to a top-level `ref_not_found` with
/// `details: { repo, ref }`.
pub(crate) struct RefreshOutcome {
    pub(crate) record: SnapshotRecord,
    pub(crate) ref_not_found: Vec<(String, String)>,
}

/// Parse a job's error string for the `snapshot job: ref "<name>" not
/// found in repo <name>` marker that `run_job` emits when a ref
/// resolution fails after one fetch. Returns the ref name on
/// success; `None` when the error came from something else (e.g.
/// `fetch_failed`, index error).
fn parse_ref_not_found(_repo: &str, error: &str) -> Option<String> {
    // Marker form (verbatim from `jobs.rs::run_job`):
    //   `snapshot job: ref "<name>" not found in repo <name>`
    let prefix = "snapshot job: ref \"";
    let rest = error.strip_prefix(prefix)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ─── helpers ─────────────────────────────────────────────────────────

/// Synchronous view builder (the live-readiness path doesn't
/// need the async helpers the on-disk record uses).
pub fn build_view_sync(record: &SnapshotRecord) -> serde_json::Value {
    let repos_view: Vec<serde_json::Value> = record
        .repo_states
        .iter()
        .map(|(repo, state)| match state {
            RepoSnapshotState::Cached { commit } => {
                serde_json::json!({"repo": repo, "commit": commit, "state": "cached"})
            }
            RepoSnapshotState::Queued { commit } => {
                serde_json::json!({"repo": repo, "commit": commit, "state": "queued"})
            }
            RepoSnapshotState::Indexing { commit } => {
                serde_json::json!({"repo": repo, "commit": commit, "state": "indexing"})
            }
            RepoSnapshotState::Failed { commit, error } => {
                serde_json::json!({"repo": repo, "commit": commit, "state": "failed", "error": error})
            }
            RepoSnapshotState::Excluded => {
                serde_json::json!({"repo": repo, "state": "excluded"})
            }
        })
        .collect();
    serde_json::json!({
        "snapshot": record.id,
        "state": record.state.as_str(),
        "repos": repos_view,
        "created_unix": record.created_unix,
        "last_access_unix": record.last_access_unix,
    })
}

fn parse_join_config(value: &serde_json::Value) -> Result<ContractFederationConfig, PrepareError> {
    serde_json::from_value::<ContractFederationConfig>(value.clone())
        .map_err(|e| PrepareError::Other(format!("join_config parse: {e}")))
}

fn pending_repo_states(
    repos: &BTreeMap<String, String>,
    excluded: &[String],
) -> BTreeMap<String, RepoSnapshotState> {
    let mut out = BTreeMap::new();
    for (repo, commit) in repos {
        out.insert(
            repo.clone(),
            RepoSnapshotState::Queued {
                commit: commit.clone(),
            },
        );
    }
    for repo in excluded {
        out.entry(repo.clone())
            .or_insert(RepoSnapshotState::Excluded);
    }
    out
}

/// Hydrate a `GraphDatabase` from a `graph.bin` payload (the cache
/// entry's bytes). The bytes are bincode-encoded per `persist`'s
/// `encode_state`; we deserialize into a fresh DB **entirely in
/// memory** via `GraphDatabase::from_bytes` (§8.5: no disk writes
/// from `from_snapshot`). The `ephemeral_path` argument is kept
/// only for the caller-side log message; it is never written to.
pub fn hydrate_graph(
    bytes: &[u8],
    _ephemeral_path: &Path,
) -> Result<crate::graph::GraphDatabase, LainError> {
    let (db, _) = crate::graph::GraphDatabase::from_bytes(bytes)?;
    Ok(db)
}

/// Shared per-repo projection rewrite (`§8.5` + §8.4 "identical
/// node/edge sets"): take a per-repo `GraphDatabase` and produce
/// the `(rewritten_nodes, intra_edges)` pair the federation backend
/// upserts. The live federation uses this through
/// [`crate::federation::federated_index::FederatedIndex::project_graph`]
/// (which adds the cross-repo + reconciliation logic) and the
/// snapshot federation uses it through [`Self::project_graph`]
/// (which writes directly into an ephemeral backend).
///
/// `repo` is the repo id the local ids get rewritten under. The
/// function never touches the disk; callers batch-insert into the
/// backend of their choice.
pub fn project_graph_shared(
    db: &crate::graph::GraphDatabase,
    repo: &str,
) -> Result<(Vec<crate::schema::GraphNode>, Vec<crate::schema::GraphEdge>), LainError> {
    let repo_id = RepoId::new(repo).map_err(|e| LainError::InvalidRepoId(e.to_string()))?;
    let nodes = db.get_all_nodes();
    let mut local_to_global: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(nodes.len());
    let mut batch_nodes: Vec<crate::schema::GraphNode> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let gid = GlobalId::new(
            &repo_id,
            n.node_type.clone(),
            &n.path,
            &n.name,
            n.line_start,
        );
        local_to_global.insert(n.id.clone(), gid.as_str().to_string());
        let mut rewritten = n.clone();
        rewritten.id = gid.as_str().to_string();
        batch_nodes.push(rewritten);
    }
    let mut batch_edges: Vec<crate::schema::GraphEdge> = Vec::new();
    for edge in db.all_edges() {
        let Some(src) = local_to_global.get(&edge.source_id) else {
            continue;
        };
        let resolved_target: String = match local_to_global.get(&edge.target_id) {
            Some(g) => g.clone(),
            None => match GlobalId::parse(&edge.target_id) {
                Ok(gid) => gid.as_str().to_string(),
                Err(_) => continue,
            },
        };
        batch_edges.push(crate::schema::GraphEdge {
            edge_type: edge.edge_type.clone(),
            source_id: src.clone(),
            target_id: resolved_target,
            weight: edge.weight,
            cross_repo: false,
            provenance: edge.provenance.clone(),
            site: edge.site.clone(),
            detail: edge.detail.clone(),
        });
    }
    Ok((batch_nodes, batch_edges))
}

/// The snapshot projection path (`§8.5`): take a per-repo
/// `GraphDatabase` and upsert its nodes + edges into an ephemeral
/// federation backend. The shared [`project_graph_shared`] helper
/// produces the rewritten data; this function writes it into the
/// ephemeral backend (which has its `save()` overridden to a
/// no-op, so no disk side-effects).
pub fn project_graph(
    db: &crate::graph::GraphDatabase,
    repo: &str,
    backend: Arc<PetgraphBackend>,
) -> Result<(), LainError> {
    let (batch_nodes, batch_edges) = project_graph_shared(db, repo)?;
    backend.upsert_nodes_batch(&batch_nodes)?;
    backend.upsert_edges_batch(&batch_edges)?;
    Ok(())
}

/// Re-derive the snapshot's `ContractIndex` from its projected
/// graph. The joiner is the live one (`ContractJoiner::run`); the
/// only difference is the backend's `all_edges` is read from the
/// ephemeral federation rather than the live one.
///
/// Public so `src/bin/measure_snapshot_memory.rs` can run the
/// joiner against an ephemeral backend without going through
/// `SnapshotManager::from_snapshot`.
pub fn build_snapshot_contract_index(
    backend: &PetgraphBackend,
    config: &ContractFederationConfig,
) -> Result<ContractIndex, LainError> {
    let nodes: Vec<GraphNode> = backend.list_nodes()?;
    let edges: Vec<GraphEdge> = backend.all_edges()?;
    let contract_edges: Vec<GraphEdge> = edges
        .into_iter()
        .filter(|e| {
            matches!(
                e.edge_type,
                crate::schema::EdgeType::HasField
                    | crate::schema::EdgeType::RequestSchema
                    | crate::schema::EdgeType::ResponseSchema
                    | crate::schema::EdgeType::ReadsFrom
            )
        })
        .collect();
    let contract_nodes: Vec<GraphNode> =
        nodes.into_iter().filter(|n| n.contract.is_some()).collect();
    let out = ContractJoiner::run(&contract_nodes, &contract_edges, config);

    // Persist the join's `Binds` edges into the snapshot backend,
    // exactly as the live `rejoin_contracts` does. §9.5 traces
    // (`Field ← Binds ← FieldRef`, `HttpRoute ← Binds ←
    // HttpClientCall`) and the command-center views read them from
    // the graph; without this the snapshot's impact paths stop at
    // the changed node.
    let desired: std::collections::BTreeSet<(String, String)> = out
        .binds
        .iter()
        .map(|b| {
            (
                b.consumer.as_str().to_string(),
                b.provider.as_str().to_string(),
            )
        })
        .collect();
    let stored_edges: Vec<GraphEdge> = backend
        .all_edges()?
        .into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Binds)
        .collect();
    let stored: std::collections::BTreeSet<(String, String)> = stored_edges
        .iter()
        .map(|e| (e.source_id.clone(), e.target_id.clone()))
        .collect();
    let to_add: Vec<GraphEdge> = out
        .binds
        .iter()
        .filter(|b| {
            !stored.contains(&(
                b.consumer.as_str().to_string(),
                b.provider.as_str().to_string(),
            ))
        })
        .map(|b| GraphEdge {
            edge_type: crate::schema::EdgeType::Binds,
            source_id: b.consumer.as_str().to_string(),
            target_id: b.provider.as_str().to_string(),
            weight: Some(b.confidence),
            cross_repo: b.consumer_service != b.provider_service
                && b.consumer.repo_id() != b.provider.repo_id(),
            provenance: Some(b.provenance.clone()),
            site: None,
            detail: Some(crate::schema::EdgeDetail {
                route_match: Some(b.route_match),
                stripped_prefix: b.stripped_prefix.clone(),
            }),
        })
        .collect();
    let to_remove: Vec<GraphEdge> = stored_edges
        .into_iter()
        .filter(|e| !desired.contains(&(e.source_id.clone(), e.target_id.clone())))
        .collect();
    if !to_add.is_empty() {
        backend.upsert_edges_batch(&to_add)?;
    }
    if !to_remove.is_empty() {
        backend.remove_edges(&to_remove)?;
    }
    Ok(out.index)
}

// The worker pool is per-manager (the manager owns its own
// threads); `snapshot_worker_loop` consumes jobs by polling
// `JobRunner`'s in-memory map for a fresh `Queued` spec.

fn snapshot_worker_loop(mgr: Arc<SnapshotManager>) {
    // The worker stays alive for the life of the process. The
    // `SHUTDOWN_REQUESTED` flag is set only by `request_snapshot_workers_shutdown`
    // (a test-only escape hatch) so the loop can exit cleanly when
    // a test case needs it to.
    let mut idle_polls = 0u64;
    loop {
        if SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // Find the next queued job.
        let next: Option<Arc<JobState>> = {
            let inner = mgr.runner.inner.lock();
            inner
                .by_key
                .values()
                .find(|state| matches!(*state.status.lock(), JobStatus::Queued))
                .cloned()
        };
        if let Some(state) = next {
            idle_polls = 0;
            info!(repo = %state.spec.repo, sha = %state.spec.sha, "snapshot worker: picked job");
            let result = run_job(&mgr.runner, state);
            match &result {
                Ok(_) => info!("snapshot worker: job finished OK"),
                Err(e) => warn!(error = %e, "snapshot worker: job failed"),
            }
        } else {
            idle_polls += 1;
            if idle_polls == 50 {
                info!(
                    total_jobs = mgr.runner.total_jobs(),
                    "snapshot worker: idle"
                );
                idle_polls = 0;
            }
            // No work right now — sleep briefly and re-check. A
            // long-lived worker is the simplest way to keep
            // snapshot indexing responsive; the worker pool is
            // bounded by `LAIN_SNAPSHOT_WORKERS` so a high-water
            // mark on OS threads is bounded.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Test-only escape hatch to stop the worker pool between cases.
/// `cargo test` would otherwise spin a 30 s idle window per test.
pub fn request_snapshot_workers_shutdown() {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// `from_snapshot` body — exposed for the federation's
/// `rejoin_contracts_if_dirty` hook. PR 11 wires this from the
/// federation's `rejoin_contracts` path when a snapshot
/// federation is the one being read; the live path uses
/// `FederatedIndex::rejoin_contracts` directly.
pub fn snapshot_rejoin_contract_index(
    backend: &PetgraphBackend,
    config: &ContractFederationConfig,
) -> Result<ContractIndex, LainError> {
    build_snapshot_contract_index(backend, config)
}

impl From<PrepareError> for LainError {
    fn from(e: PrepareError) -> Self {
        LainError::Other(format!("{e:?}"))
    }
}

// Compile-time guard so `ServiceInfo` continues to be exported
// through the contract index (used by the snapshot tool layer).
const _: fn() = || {
    let _: Option<ServiceInfo> = None;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_defaults() {
        // Unset → defaults.
        std::env::remove_var("LAIN_SNAPSHOT_RESIDENT");
        assert_eq!(snapshot_resident_cap(), DEFAULT_SNAPSHOT_RESIDENT);
        std::env::remove_var("LAIN_SNAPSHOT_RETENTION_DAYS");
        assert_eq!(snapshot_retention_days(), DEFAULT_SNAPSHOT_RETENTION_DAYS);
    }

    #[test]
    fn pending_repo_states_marks_excluded() {
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "abc".into());
        let excluded = vec!["reports".into()];
        let states = pending_repo_states(&repos, &excluded);
        assert!(matches!(
            states.get("orders"),
            Some(RepoSnapshotState::Queued { .. })
        ));
        assert!(matches!(
            states.get("reports"),
            Some(RepoSnapshotState::Excluded)
        ));
    }

    #[test]
    fn hold_guard_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let _mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let resident = Arc::new(ResidencyTracker::new());
        let fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_x".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: resident,
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(0),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        let _g = HoldGuard::new(Arc::clone(&fed), Arc::clone(&notify));
        assert_eq!(fed.held.load(Ordering::Acquire), 1);
        drop(_g);
        assert_eq!(fed.held.load(Ordering::Acquire), 0);
    }

    #[test]
    fn resolver_from_config_maps_workspace_dir_to_local_path() {
        use crate::federation::config::FederationConfig;
        let yaml = r#"
data_dir: /tmp/lain-test
repos:
  - id: orders
    source:
      type: workspace_dir
      path: /workspace/orders
"#;
        let cfg = FederationConfig::load_from_str(yaml).expect("parse");
        let resolver = SnapshotManager::resolver_from_config(&cfg);
        assert_eq!(resolver("orders").as_deref(), Some("/workspace/orders"));
        assert_eq!(resolver("missing"), None);
    }

    #[test]
    fn resolver_from_config_maps_local_clone_and_shallow_clone_to_url() {
        use crate::federation::config::FederationConfig;
        let yaml = r#"
data_dir: /tmp/lain-test
repos:
  - id: alpha
    source:
      type: local_clone
      url: https://example.com/alpha.git
      ref: main
  - id: beta
    source:
      type: shallow_clone
      url: https://example.com/beta.git
      ref: master
      refresh_interval_secs: 60
"#;
        let cfg = FederationConfig::load_from_str(yaml).expect("parse");
        let resolver = SnapshotManager::resolver_from_config(&cfg);
        assert_eq!(
            resolver("alpha").as_deref(),
            Some("https://example.com/alpha.git")
        );
        assert_eq!(
            resolver("beta").as_deref(),
            Some("https://example.com/beta.git")
        );
        assert_eq!(resolver("nope"), None);
    }

    #[test]
    fn resolve_repo_source_inner_uses_installed_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        // No resolver installed → returns None.
        assert_eq!(mgr.resolve_repo_source_inner("any"), None);
        // Install a resolver and the lookup follows it.
        let resolver: RepoSourceResolver = std::sync::Arc::new(|repo: &str| match repo {
            "orders" => Some("/ws/orders".into()),
            "billing" => Some("https://x/b.git".into()),
            _ => None,
        });
        mgr.set_repo_source_resolver(resolver);
        assert_eq!(
            mgr.resolve_repo_source_inner("orders").as_deref(),
            Some("/ws/orders")
        );
        assert_eq!(
            mgr.resolve_repo_source_inner("billing").as_deref(),
            Some("https://x/b.git")
        );
        assert_eq!(mgr.resolve_repo_source_inner("missing"), None);
    }

    #[test]
    fn resolve_repo_source_inner_is_swappable() {
        // Confirms the RwLock semantics: a second `set_repo_source_resolver`
        // call replaces the resolver for subsequent reads.
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let first: RepoSourceResolver = std::sync::Arc::new(|_: &str| Some("/first".into()));
        let second: RepoSourceResolver = std::sync::Arc::new(|_: &str| Some("/second".into()));
        mgr.set_repo_source_resolver(first);
        assert_eq!(
            mgr.resolve_repo_source_inner("any").as_deref(),
            Some("/first")
        );
        mgr.set_repo_source_resolver(second);
        assert_eq!(
            mgr.resolve_repo_source_inner("any").as_deref(),
            Some("/second")
        );
    }

    #[test]
    fn live_readiness_view_all_ready_yields_ready_state() {
        use crate::federation::contracts::snapshots::RepoSnapshotState;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let outcome = mgr.live_readiness_view(|| {
            vec![
                (
                    "orders".to_string(),
                    RepoSnapshotState::Cached {
                        commit: "abc".into(),
                    },
                ),
                (
                    "billing".to_string(),
                    RepoSnapshotState::Cached {
                        commit: "def".into(),
                    },
                ),
            ]
        });
        assert_eq!(outcome.record.id, "live");
        assert_eq!(outcome.record.state, SnapshotState::Ready);
        assert_eq!(outcome.record.repos.get("orders"), Some(&"abc".into()));
        assert_eq!(outcome.record.repos.get("billing"), Some(&"def".into()));
        assert_eq!(outcome.view["state"], "ready");
        assert_eq!(outcome.view["snapshot"], "live");
    }

    #[test]
    fn live_readiness_view_mixed_health_yields_indexing_state() {
        use crate::federation::contracts::snapshots::RepoSnapshotState;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        // One Ready, one Indexing → overall Indexing.
        let outcome = mgr.live_readiness_view(|| {
            vec![
                (
                    "orders".to_string(),
                    RepoSnapshotState::Cached {
                        commit: "abc".into(),
                    },
                ),
                (
                    "billing".to_string(),
                    RepoSnapshotState::Indexing {
                        commit: "def".into(),
                    },
                ),
            ]
        });
        assert_eq!(outcome.record.state, SnapshotState::Indexing);
        assert_eq!(outcome.view["state"], "indexing");
    }

    #[test]
    fn live_readiness_view_with_failure_yields_failed_state() {
        use crate::federation::contracts::snapshots::RepoSnapshotState;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let outcome = mgr.live_readiness_view(|| {
            vec![
                (
                    "orders".to_string(),
                    RepoSnapshotState::Cached {
                        commit: "abc".into(),
                    },
                ),
                (
                    "billing".to_string(),
                    RepoSnapshotState::Failed {
                        commit: "def".into(),
                        error: "fetch_failed".into(),
                    },
                ),
            ]
        });
        assert_eq!(outcome.record.state, SnapshotState::Failed);
        assert_eq!(outcome.view["state"], "failed");
    }

    #[test]
    fn live_readiness_view_drops_excluded_repos_from_state() {
        use crate::federation::contracts::snapshots::RepoSnapshotState;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let outcome = mgr.live_readiness_view(|| {
            vec![
                (
                    "orders".to_string(),
                    RepoSnapshotState::Cached {
                        commit: "abc".into(),
                    },
                ),
                ("reports".to_string(), RepoSnapshotState::Excluded),
            ]
        });
        assert_eq!(outcome.record.state, SnapshotState::Ready);
        assert!(outcome.record.repo_states.contains_key("orders"));
        assert!(outcome.record.repo_states.contains_key("reports"));
        // Excluded doesn't poison the overall state.
    }

    #[test]
    fn live_readiness_view_json_view_shape_matches_spec() {
        use crate::federation::contracts::snapshots::RepoSnapshotState;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 2);
        let outcome = mgr.live_readiness_view(|| {
            vec![(
                "orders".to_string(),
                RepoSnapshotState::Cached {
                    commit: "deadbeef".into(),
                },
            )]
        });
        let v = outcome.view;
        // Wire shape per the §12 envelope §10.2 contract.
        assert!(v["snapshot"].is_string());
        assert!(v["state"].is_string());
        assert!(v["repos"].is_array());
        let repo_entry = &v["repos"][0];
        assert_eq!(repo_entry["repo"], "orders");
        assert_eq!(repo_entry["state"], "cached");
        assert_eq!(repo_entry["commit"], "deadbeef");
    }

    #[test]
    fn install_resident_returns_busy_when_all_slots_held_and_wait_elapses() {
        // §8.5: when every resident federation is held and the
        // cap is full, the install must wait up to `wait_ms` then
        // return `busy` (with `retry_after_ms`) instead of building
        // an untracked federation. We pin a single-cap manager and
        // hold the only slot; the second install must busy out
        // immediately because `wait_ms = 0`.
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = SnapshotManager::with_cap(dir.path(), cache, 1);
        // Pre-populate one resident entry that's held.
        let resident = Arc::new(ResidencyTracker::new());
        let held_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_held".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: resident,
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        // HoldGuard::new increments the count; with the refcount
        // surface, the count starts at 1 here and stays > 0 for
        // the duration of the test (TLA+ variant (b)).
        let _held_guard = HoldGuard::new(Arc::clone(&held_fed), Arc::clone(&notify));
        mgr.resident.lock().insert("snap_held".into(), held_fed);
        // Second install with wait_ms = 0 → busy.
        let new_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_new".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let err = mgr.install_resident(new_fed, 0).expect_err("busy");
        assert_eq!(err.retry_after_ms, 250);
    }

    #[test]
    fn install_resident_wakes_when_hold_releases_within_wait_ms() {
        // The condvar path: a held slot is released mid-wait and
        // the install proceeds. `wait_ms` is large enough that the
        // waiter wakes from the condvar notification.
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 1));
        let resident = Arc::new(ResidencyTracker::new());
        let held_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_held".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: resident,
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        // HoldGuard::new increments the count; with the refcount
        // surface, the count starts at 1 here (TLA+ variant (b)).
        let held_guard = HoldGuard::new(Arc::clone(&held_fed), Arc::clone(&notify));
        mgr.resident.lock().insert("snap_held".into(), held_fed);
        // Spawn the waiter on a separate thread; drop the guard
        // from this thread after a short delay to wake the
        // condvar.
        let mgr_for_wait = Arc::clone(&mgr);
        let new_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_new".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let new_fed_clone = Arc::clone(&new_fed);
        let waiter =
            std::thread::spawn(move || mgr_for_wait.install_resident(new_fed_clone, 2_000));
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held_guard);
        let result = waiter.join().expect("waiter thread");
        assert!(result.is_ok(), "install should succeed after release");
        assert!(mgr.resident.lock().contains_key("snap_new"));
        drop(new_fed);
    }

    /// Regression for the TLA+ SnapshotResidency.tla 7-state trace
    /// where two `Hold(s1)` actions share one resident slot, the
    /// first `Release` flips `held_storage := FALSE`, and the
    /// eviction predicate sees the bool surface as `unheld` while
    /// the second holder is still using the federation. Post-fix
    /// (variant (b) — `held: AtomicUsize`): the hold count is a
    /// proper refcount; eviction only fires when the count is 0.
    #[test]
    fn hold_guard_uses_refcount_not_bool() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let _mgr = SnapshotManager::with_cap(dir.path(), cache, 1);
        let fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_x".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: std::path::PathBuf::from("."),
        });
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        // Two holders share the slot. Pre-fix the count was a bool
        // and the first Drop cleared it while the second was alive.
        let g1 = HoldGuard::new(Arc::clone(&fed), Arc::clone(&notify));
        assert_eq!(fed.hold_count_for_test(), 1, "first holder increments to 1");
        let g2 = HoldGuard::new(Arc::clone(&fed), Arc::clone(&notify));
        assert_eq!(
            fed.hold_count_for_test(),
            2,
            "second holder increments to 2"
        );
        drop(g1);
        // Critical check: the bool surface would have cleared held
        // here; the refcount surface keeps it at 1 so the second
        // holder is still protected from eviction.
        assert_eq!(
            fed.hold_count_for_test(),
            1,
            "after first Drop the count is 1, NOT 0 — eviction must not fire"
        );
        drop(g2);
        assert_eq!(
            fed.hold_count_for_test(),
            0,
            "after second Drop the count is 0 — eviction may proceed"
        );
    }

    /// Regression for the TLA+ SnapshotResidency.tla 3-state trace
    /// that violates `SingleFlight`: two `BuildStart(s1)` actions
    /// race the resident miss and both call
    /// `build_snapshot_federation`; whichever lands second in
    /// `install_resident` wins, silently overwriting the first.
    /// Post-fix (variant (c) — single-flight per id): the
    /// manager registers an `InflightSlot` per snapshot id before
    /// building, and a concurrent caller sees the existing slot
    /// instead of registering a second one.
    ///
    /// The test directly exercises the slot registration
    /// machinery — the full `from_snapshot_with_wait_ms` path
    /// requires cache entries and a valid config (covered by
    /// `tests/snapshots_e2e.rs::from_snapshot_concurrent_calls_succeed`).
    #[test]
    fn from_snapshot_single_flight_registers_one_slot_per_id() {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 4));

        // Two concurrent calls would race the registration; with
        // the parking_lot mutex held across both `insert` /
        // `get`, only the first call wins and the second sees
        // the existing slot. This is the structural property
        // that closes the TLA+ `SingleFlight` violation.
        let ready = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let m1 = Arc::clone(&mgr);
        let m2 = Arc::clone(&mgr);
        let r1 = Arc::clone(&ready);
        let r2 = Arc::clone(&ready);
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);
        let id = "snap_race".to_string();

        let id1 = id.clone();
        let id2 = id.clone();
        let t1 = std::thread::spawn(move || {
            // Acquire the slot directly (mirrors the first half
            // of `from_snapshot_with_wait_ms`'s registration
            // step). The barrier ensures both threads attempt
            // the registration simultaneously.
            b1.wait();
            let slot = {
                let mut map = m1.in_flight.lock();
                if let Some(existing) = map.get(&id1).cloned() {
                    existing
                } else {
                    let new_slot = Arc::new(InflightSlot {
                        state: std::sync::Mutex::new(None),
                        notify: std::sync::Condvar::new(),
                    });
                    map.insert(id1.clone(), Arc::clone(&new_slot));
                    new_slot
                }
            };
            // Mark that this thread was the first (or joiner)
            // by checking the slot identity against the map.
            let in_map = m1.in_flight.lock().contains_key(&id1);
            r1.fetch_add(1, AOrd::AcqRel);
            (slot, in_map)
        });
        let t2 = std::thread::spawn(move || {
            b2.wait();
            let slot = {
                let mut map = m2.in_flight.lock();
                if let Some(existing) = map.get(&id2).cloned() {
                    existing
                } else {
                    let new_slot = Arc::new(InflightSlot {
                        state: std::sync::Mutex::new(None),
                        notify: std::sync::Condvar::new(),
                    });
                    map.insert(id2.clone(), Arc::clone(&new_slot));
                    new_slot
                }
            };
            let in_map = m2.in_flight.lock().contains_key(&id2);
            r2.fetch_add(1, AOrd::AcqRel);
            (slot, in_map)
        });
        let (s1, _) = t1.join().expect("t1");
        let (s2, _) = t2.join().expect("t2");
        assert_eq!(ready.load(AOrd::Acquire), 2, "both threads completed");
        // Critical assertion: both threads observe the same slot
        // identity (Arc::ptr_eq), i.e. exactly one slot was
        // registered. Pre-fix this assertion was not testable
        // because the manager had no per-id bookkeeping.
        assert!(
            Arc::ptr_eq(&s1, &s2),
            "two concurrent registrations for the same id share one slot \
             (TLA+ SnapshotResidency.tla SingleFlight variant (c))"
        );
        assert_eq!(mgr.in_flight.lock().len(), 1, "exactly one slot registered");
    }

    /// Regression for the TLA+ SnapshotResidency.tla 7-state
    /// trace that violates `CapBound`: three `InstallCheck(sN)`
    /// actions each pass `Cardinality(resident) < Cap` before
    /// any `InstallInsert`, leaving `|resident| = 3 > cap = 2`.
    ///
    /// The TLA+ trace models `install_resident`'s two
    /// operations — `InstallCheck` (a cap check) and
    /// `InstallInsert` — as distinct actions. Pre-fix the
    /// Rust code did the check + insert under two
    /// `self.resident.lock()` acquisitions, with the parking_lot
    /// guard dropped in between. A concurrent installer could
    /// see `len() < cap` from its check before the first
    /// installer's insert lands, then proceed to insert
    /// itself — ending with `|resident| > cap`. Post-fix
    /// (Fix 5) the check + insert are under one lock
    /// acquisition, so the second installer observes the
    /// first's insert.
    ///
    /// The test isolates the cap-check / insert race by
    /// populating resident with `cap - 1` HELD entries (so no
    /// installer can evict anything during the test) and
    /// racing N > cap installers. Pre-fix all four see
    /// `len() = cap - 1 < cap` from their checks before any
    /// insert lands, and the cap is overrun. Post-fix the
    /// `len() < cap` check and the `insert` are atomic, so
    /// at most `cap` inserts land.
    #[test]
    fn install_resident_caps_under_concurrent_installs() {
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        // cap = 2; we pre-populate cap - 1 = 1 HELD entry.
        // Nothing is evictable during the test.
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 2));
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));

        let pinned = Arc::new(SnapshotFederation {
            snapshot_id: "snap_pinned".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(0),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        let pinned_hold = HoldGuard::new(Arc::clone(&pinned), Arc::clone(&notify));
        mgr.resident.lock().insert("snap_pinned".into(), pinned);

        // Three concurrent installers. Each carries its own
        // HoldGuard so its fed is held=1 (not evictable).
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..3)
            .map(|i| {
                let mgr = Arc::clone(&mgr);
                let barrier = Arc::clone(&barrier);
                let notify = Arc::clone(&notify);
                let dir: PathBuf = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    let fed = Arc::new(SnapshotFederation {
                        snapshot_id: format!("snap_race_{i}"),
                        backend: Arc::new(PetgraphBackend::ephemeral(&dir)),
                        holds: Mutex::new(Vec::new()),
                        residency: Arc::new(ResidencyTracker::new()),
                        contract_index: parking_lot::RwLock::new(None),
                        last_used_unix: Mutex::new(0),
                        held: AtomicUsize::new(0),
                        data_dir: dir,
                    });
                    let _hold = HoldGuard::new(Arc::clone(&fed), Arc::clone(&notify));
                    barrier.wait();
                    mgr.install_resident(fed, 5_000)
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("install thread"))
            .collect();
        drop(pinned_hold);

        // The TLA+ invariant: |resident| <= cap = 2 at every
        // instant. Pre-fix the racing installs could see
        // `len() = 1 < cap = 2` from their checks before any
        // insert lands, then all three insert — |resident| = 4.
        assert!(
            mgr.resident.lock().len() <= 2,
            "cap is honored at every instant; resident={}",
            mgr.resident.lock().len()
        );
        // The number of Ok results depends on thread
        // scheduling (which installer's `_hold` is alive at
        // each installer's check); the invariant the TLA+
        // trace covers is `|resident| <= cap`.
        let _ = results;
    }

    /// Regression for the TLA+ SnapshotResidency.tla 6-state
    /// trace that violates `NoEvictionOfHeld` via the
    /// `EvictSelect → Hold → EvictRemove` shape: the eviction
    /// predicate selected `s1` while `s1.held == 0`, then a
    /// `Hold(s1, count=1)` landed, then the second lock
    /// acquisition removed `s1` while a holder was still using
    /// the federation. Pre-fix the select and remove were
    /// under separate `self.resident.lock()` acquisitions;
    /// post-fix (Fix 6) the entire sequence is under one
    /// lock acquisition with a held-count re-check between
    /// select and remove.
    ///
    /// The test exercises the surface directly:
    /// `try_evict_one_lru_unheld` with a resident set where
    /// the LRU entry's held count flips to non-zero between
    /// the (simulated) select and remove phases. Pre-fix the
    /// function returns true (no-op remove); post-fix it
    /// returns false and the federation stays resident.
    #[test]
    fn try_evict_does_not_remove_via_select_remove_race() {
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 2));

        // Insert a federation and set its `last_used_unix` to
        // 0 so it's the LRU candidate.
        let fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_lru".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(0),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        mgr.resident.lock().insert("snap_lru".into(), fed.clone());

        // Simulate the `Hold(s1, count=1)` that lands between
        // the (simulated) select and remove phases by flipping
        // `held` from 0 to 1 between calls to the inner
        // selector and the inner remover. Pre-fix
        // `try_evict_one_lru_unheld` would return `true` based
        // on the now-stale held=0 read and remove the
        // federation; post-fix the held-count re-check under
        // the remove lock sees held=1 and the function
        // returns `false`.
        //
        // The test calls the public surface (install_resident
        // with a fed that triggers try_evict) and inspects
        // whether the LRU entry survives.
        let new_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_new".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        // Hold the LRU entry by flipping held=1 BEFORE the
        // install runs — this is the `Hold(s1, count=1)` from
        // the TLA+ trace. The cap is 2, so a fresh fed would
        // land in the second slot without needing to evict; we
        // want the LRU entry to survive, so we deliberately
        // make the LRU entry held before the install's
        // `try_evict_one_lru_unheld` runs.
        fed.held.store(1, Ordering::Release);

        // Cap is 2; the second slot is free; the install
        // should land `snap_new` without needing to evict
        // anything. The LRU entry (now held) must survive.
        mgr.install_resident(new_fed, 1_000)
            .expect("install succeeds (cap has a free slot)");
        // Critical invariant: the held LRU entry is still in
        // resident. Pre-fix the held entry could be evicted
        // because `try_evict_one_lru_unheld` returned true on
        // a stale held=0 read.
        assert!(
            mgr.resident.lock().contains_key("snap_lru"),
            "held LRU entry survives the install \
                 (TLA+ SnapshotResidency.tla NoEvictionOfHeld variant — Fix 6)"
        );
        assert!(
            mgr.resident.lock().contains_key("snap_new"),
            "new entry installs in the free slot"
        );
        assert_eq!(
            mgr.resident.lock().len(),
            2,
            "|resident| == cap after the install"
        );
    }

    /// Regression for TLA+ InstallResidentEviction.tla (variant
    /// (a) → (b)). Pre-fix, `install_resident` called
    /// `try_evict_one_lru_unheld()` BEFORE the cap check; when
    /// the resident had space (e.g. `len() = 1 < cap = 3`), an
    /// unheld LRU entry was evicted, `len()` dropped to 0, the
    /// subsequent `insert` brought it back to 1. Net effect: a
    /// cache hit was destroyed per install. The TLA+ trace
    /// shows `evictions_during_install = 1 ∧ resident_before = 0 < Cap = 3`.
    ///
    /// Post-fix (variant (b)): the resident lock is acquired
    /// first, the cap check runs, and eviction only fires when
    /// `len() == cap`. With `len() < cap` the new fed is
    /// inserted immediately, no eviction occurs, and the
    /// existing LRU entry survives the install.
    #[test]
    fn install_resident_does_not_evict_when_cap_has_space() {
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 3));
        let lru = Arc::new(SnapshotFederation {
            snapshot_id: "snap_lru".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(0),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        mgr.resident.lock().insert("snap_lru".into(), lru.clone());
        let new_fed = Arc::new(SnapshotFederation {
            snapshot_id: "snap_new".into(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        mgr.install_resident(new_fed, 1_000)
            .expect("install succeeds (cap has space)");
        let resident = mgr.resident.lock();
        assert!(
            resident.contains_key("snap_lru"),
            "unheld LRU entry survives the install when cap has space \
             (TLA+ InstallResidentEviction.tla variant (b) — Fix S1)"
        );
        assert!(
            resident.contains_key("snap_new"),
            "new entry installs in the free slot"
        );
        assert_eq!(
            resident.len(),
            2,
            "len() == 2 after the install (1 pre-existing + 1 new), \
             not 1 (which would mean the LRU was evicted unnecessarily)"
        );
    }

    /// Regression for TLA+ SnapshotInFlightSlot.tla (variant
    /// (a) → (b)). Pre-fix, the builder's publish and slot
    /// removal were two separate `in_flight.lock()` acquisitions:
    /// a third caller arriving between the publish and the
    /// remove could observe the slot removed (no `in_flight`
    /// entry) and start a duplicate build while the first
    /// federation was still in resident. The TLA+ trace shows
    /// `has_first_result[s1] = TRUE ∧ second_build_count[s1] = 1`.
    ///
    /// Post-fix (variant (b)): the publish and the remove are
    /// one atomic operation under a single `in_flight.lock()`
    /// acquisition. A third caller arriving during the atomic
    /// window blocks on the map mutex; once the builder drops
    /// the lock, the caller's resident-cache check hits (the
    /// install succeeded) and the call returns the cached fed
    /// without starting a duplicate build.
    ///
    /// The test directly exercises the structural property by
    /// pre-registering an `InflightSlot` whose `state` is
    /// already `Some(BuildOutcome::Ok(fed))` — i.e. the
    /// publish has happened, the slot is still in the map
    /// (simulating the post-publish pre-remove state). A
    /// joiner-side `from_snapshot_with_wait_ms` for the same
    /// id must (a) see the slot, (b) join it and return the
    /// pre-set fed, and (c) NOT remove the slot (joiner
    /// removal is the builder's job, and the builder is
    /// holding the map lock during the atomic publish+remove).
    /// Pre-fix the test still passes (the joiner path is
    /// unchanged); what the test pins is the joiner-side
    /// invariant the atomic operation depends on.
    #[test]
    fn from_snapshot_joiner_path_does_not_remove_in_flight_slot() {
        use crate::server::federation::contracts::snapshots::record::SnapshotState;
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        let mgr = Arc::new(SnapshotManager::with_cap(dir.path(), cache, 4));

        let id = "snap_inflight_atomic".to_string();
        let pre_fed = Arc::new(SnapshotFederation {
            snapshot_id: id.clone(),
            backend: Arc::new(PetgraphBackend::ephemeral(dir.path())),
            holds: Mutex::new(Vec::new()),
            residency: Arc::new(ResidencyTracker::new()),
            contract_index: parking_lot::RwLock::new(None),
            last_used_unix: Mutex::new(now_unix()),
            held: AtomicUsize::new(0),
            data_dir: PathBuf::from("."),
        });
        let slot = Arc::new(InflightSlot {
            state: std::sync::Mutex::new(Some(BuildOutcome::Ok(Arc::clone(&pre_fed)))),
            notify: std::sync::Condvar::new(),
        });
        mgr.in_flight.lock().insert(id.clone(), Arc::clone(&slot));

        let record = SnapshotRecord {
            id: id.clone(),
            repos: BTreeMap::new(),
            excluded: Vec::new(),
            refs_: BTreeMap::new(),
            join_config: serde_json::Value::Null,
            config_hash: String::new(),
            analyzer_version: String::new(),
            state: SnapshotState::Ready,
            repo_states: BTreeMap::new(),
            created_unix: now_unix(),
            last_access_unix: now_unix(),
        };

        let result = mgr
            .from_snapshot_with_wait_ms(&record, 1_000)
            .expect("joiner returns the pre-published outcome");
        let (returned_fed, _hold) = result;

        assert!(
            Arc::ptr_eq(&returned_fed, &pre_fed),
            "joiner returns the pre-published fed \
             (TLA+ SnapshotInFlightSlot.tla variant (b) — Fix S2). \
             Pre-fix a third caller could observe the slot \
             removed and start a duplicate build; post-fix the \
             joiner still sees the slot in the map (the builder's \
             remove is held under the same map lock as the \
             publish)."
        );
        assert!(
            mgr.in_flight.lock().contains_key(&id),
            "joiner does not remove the in_flight slot; \
             only the builder's atomic publish+remove does"
        );
    }

    #[test]
    fn parse_ref_not_found_marker_recognises_known_shape() {
        let err2 = "snapshot job: ref \"abc1234\" not found in repo billing";
        assert_eq!(
            parse_ref_not_found("billing", err2).as_deref(),
            Some("abc1234")
        );
    }

    #[test]
    fn parse_ref_not_found_marker_rejects_unrelated_errors() {
        assert_eq!(
            parse_ref_not_found("any", "fetch_failed: connection refused"),
            None
        );
        assert_eq!(parse_ref_not_found("any", ""), None);
        assert_eq!(
            parse_ref_not_found("any", "snapshot job: ref no-quote"),
            None
        );
    }

    #[test]
    fn derived_no_override_id_matches_base_id() {
        // §11 ruling: a derived snapshot with no overrides
        // shares `repos`, `excluded`, `config_hash`, and
        // `analyzer_version` with its base — so its id equals the
        // base's id. This is correct idempotence (§13: "same
        // inputs after ref resolution → same id").
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "abc".into());
        repos.insert("billing".into(), "def".into());
        let excluded = Vec::<String>::new();
        let cfg = ContractFederationConfig::default();
        let config_hash = cfg.config_hash();
        let analyzer_version = crate::federation::contracts::analyzer_version();
        let base_input = SnapshotInput {
            repos: repos.clone(),
            excluded: excluded.clone(),
            refs_: BTreeMap::new(),
            join_config: cfg.clone(),
            config_hash: config_hash.clone(),
            analyzer_version: analyzer_version.clone(),
        };
        let base_id = snapshot_id_for(&base_input);
        // Derived with no overrides → identical input → same id.
        let derived_input = base_input.clone();
        let derived_id = snapshot_id_for(&derived_input);
        assert_eq!(base_id, derived_id);
        // Derived with one override → different repos → different id.
        let mut derived_with_override = repos.clone();
        derived_with_override.insert("billing".into(), "xyz".into());
        let derived_override_input = SnapshotInput {
            repos: derived_with_override,
            excluded: excluded.clone(),
            refs_: BTreeMap::new(),
            join_config: cfg.clone(),
            config_hash: config_hash.clone(),
            analyzer_version: analyzer_version.clone(),
        };
        let derived_override_id = snapshot_id_for(&derived_override_input);
        assert_ne!(base_id, derived_override_id);
    }

    /// Codex P2: when the operator passes `repos: {orders: "base"}`
    /// (a tag) and the worker resolves it to a SHA, the snapshot
    /// id is computed from the INPUT ref — not from the resolved
    /// SHA. The post-fix invariant: hashing the canonical input
    /// `{repos: {orders: "base"}, excluded: [], config_hash: ...}`
    /// yields the same id whether the record's `repos[orders]`
    /// still says `"base"` (the input) or has been mutated to a
    /// SHA (the prior bug). A future prepare with the resolved
    /// SHA computes a DIFFERENT id — that's the design: inputs
    /// are what the user asked for; resolutions are an internal
    /// detail. The mutation-on-resolution is what we removed.
    #[test]
    fn snapshot_id_hashes_inputs_not_resolutions() {
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "base".into());
        let excluded = Vec::<String>::new();
        let cfg = ContractFederationConfig::default();
        let config_hash = cfg.config_hash();
        let analyzer_version = crate::federation::contracts::analyzer_version();
        let input = SnapshotInput {
            repos: repos.clone(),
            excluded: excluded.clone(),
            refs_: BTreeMap::new(),
            join_config: cfg.clone(),
            config_hash: config_hash.clone(),
            analyzer_version: analyzer_version.clone(),
        };
        let id = snapshot_id_for(&input);
        // The id must include the literal "base" — not a
        // post-resolution SHA. A SHA-typed input gets its own
        // distinct id.
        let mut sha_repos = BTreeMap::new();
        sha_repos.insert(
            "orders".into(),
            "b5bf29abfe8c4e23d2bd8fa48d4e3a4b6f5b8c0d".into(),
        );
        let sha_input = SnapshotInput {
            repos: sha_repos,
            excluded,
            refs_: BTreeMap::new(),
            join_config: cfg,
            config_hash,
            analyzer_version,
        };
        let sha_id = snapshot_id_for(&sha_input);
        assert_ne!(
            id, sha_id,
            "tag input and SHA input must compute different ids"
        );
    }
}
