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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// `true` when at least one tool call currently holds a
    /// reference. Residency evictions are skipped while held.
    pub held: AtomicBool,
}

impl SnapshotFederation {
    pub fn mark_used(&self) {
        *self.last_used_unix.lock() = now_unix();
    }

    pub fn last_used(&self) -> i64 {
        *self.last_used_unix.lock()
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
        });
        mgr.recover_from_disk();
        mgr
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Copy the `data_dir` for callers that hold only the snapshot
    /// record (e.g. the `read_source` tool's blob path lookup).
    /// Returns `Some(path)` for any non-empty record.
    pub fn data_dir_accessor(_record: &SnapshotRecord) -> Option<PathBuf> {
        // The record doesn't carry the data_dir; the tool layer
        // resolves it from the manager. For the pure-record path
        // we approximate with the env var `LAIN_DATA_DIR` or
        // relative to the record's on-disk directory.
        std::env::var("LAIN_DATA_DIR").ok().map(PathBuf::from)
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
    fn recover_from_disk(&self) {
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
            let spec = JobSpec {
                repo: repo.clone(),
                sha: key.sha.clone(),
                analyzer_version: key.analyzer_version.clone(),
                source: String::new(),
            };
            // Cache hit short-circuits to `cached`. No need to
            // re-queue; the indexer ran once and the bytes are
            // already on disk.
            if self.cache.has_entry(&key) {
                record.repo_states.insert(
                    repo.clone(),
                    RepoSnapshotState::Cached {
                        commit: key.sha.clone(),
                    },
                );
                continue;
            }
            let Some(state) = self.runner.lookup(&spec) else {
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
                    record.repo_states.insert(
                        repo.clone(),
                        RepoSnapshotState::Cached {
                            commit: key.sha.clone(),
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
    fn ensure_workers_running(self: &Arc<Self>) {
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
        let mut handles = Vec::new();
        for n in 0..snapshot_workers() {
            let mgr = Arc::clone(self);
            let handle = std::thread::Builder::new()
                .name(format!("lain-snapshot-worker-{n}"))
                .spawn(move || snapshot_worker_loop(mgr))
                .expect("spawn snapshot worker");
            handles.push(handle);
        }
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
    pub fn from_snapshot(
        self: &Arc<Self>,
        record: &SnapshotRecord,
    ) -> Result<(Arc<SnapshotFederation>, HoldGuard), LainError> {
        // 1. Try the resident cache first.
        if let Some(fed) = self.resident.lock().get(&record.id).cloned() {
            // A resident federation has its cache holds still
            // attached; the caller takes a *new* `HoldGuard` that
            // bumps the held counter for residency-eviction
            // purposes only.
            return Ok((
                Arc::clone(&fed),
                HoldGuard::new(fed, Arc::clone(&self.residency_notify)),
            ));
        }
        // 2. Build a new ephemeral federation.
        let fed = self.build_snapshot_federation(record)?;
        let hold_guard = HoldGuard::new(Arc::clone(&fed), Arc::clone(&self.residency_notify));
        if let Err(busy) = self.install_resident(Arc::clone(&fed), 0) {
            // The freshly-built federation has no caller holding a
            // guard yet, so the only way this branch fires is a
            // racing install filling the cap between the cache
            // check above and here. Drop the federation and surface
            // the busy error so the tool layer retries.
            return Err(LainError::Other(format!(
                "snapshot residency busy (retry after {}ms)",
                busy.retry_after_ms
            )));
        }
        Ok((fed, hold_guard))
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
            held: AtomicBool::new(false),
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
            // Try to evict one unheld LRU entry, then install. If
            // the cap is not full, install directly.
            let evicted = self.try_evict_one_lru_unheld();
            if self.resident.lock().len() < cap || evicted {
                self.resident
                    .lock()
                    .insert(fed.snapshot_id.clone(), fed.clone());
                return Ok(());
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
        let mut evict_id: Option<String> = None;
        {
            let resident = self.resident.lock();
            for (id, f) in resident.iter() {
                if !f.held.load(Ordering::Acquire)
                    && (evict_id.is_none()
                        || f.last_used() < resident[evict_id.as_ref().unwrap()].last_used())
                {
                    evict_id = Some(id.clone());
                }
            }
        }
        match evict_id {
            Some(id) => {
                self.resident.lock().remove(&id);
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
        fed.held.store(true, Ordering::Release);
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
        self.fed.held.store(false, Ordering::Release);
        self.fed.mark_used();
        self.residency_notify.1.notify_all();
    }
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
            let _ = run_job(&mgr.runner, state);
        } else {
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
            held: AtomicBool::new(false),
        });
        fed.held.store(true, Ordering::Release);
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        let _g = HoldGuard::new(Arc::clone(&fed), Arc::clone(&notify));
        assert!(fed.held.load(Ordering::Acquire));
        drop(_g);
        assert!(!fed.held.load(Ordering::Acquire));
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
            held: AtomicBool::new(false),
        });
        held_fed.held.store(true, Ordering::Release);
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        // Keep the guard alive for the whole test so `held` stays true.
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
            held: AtomicBool::new(false),
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
            held: AtomicBool::new(false),
        });
        held_fed.held.store(true, Ordering::Release);
        let notify = Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
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
            held: AtomicBool::new(false),
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

    #[test]
    fn parse_ref_not_found_marker_recognises_known_shape() {
        let err = "snapshot job: ref \"main\" not found in repo orders";
        assert_eq!(parse_ref_not_found("orders", err).as_deref(), Some("main"));
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
}
