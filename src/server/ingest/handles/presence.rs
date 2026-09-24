//! Presence — registry, occupancy, broadcast sender, state-file timestamp.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<PresenceLayer>` in PR 3.7b. The state-path resolution is
//! precomputed at construction (single-workspace vs federation-mode
//! is fixed once the server is built) and stored in the handle so
//! presence methods can call `self.state_path()` directly without
//! reaching into the federation or ingest partitions.

use crate::config::state_path_for_workspace;
use crate::graph::GraphDatabase;
use crate::server::activity::ActivityTracker;
use crate::server::intent::IntentRegistry;
use crate::server::presence::{
    load_pair as load_presence_pair, save_pair as save_presence_pair, AgentId, OccupancyMap,
    PresenceEvent, PresenceRegistry,
};
use crate::server::state_lock as lain_state_lock;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Presence registry, occupancy map, broadcast sender, and the
/// state-file timestamp tracker. Owned by the LainServer but exposed
/// as a handle so the persistence + cross-process locking logic is
/// named after its concern rather than hiding in the god struct.
pub struct PresenceLayer {
    pub(crate) presence_state_seen: Arc<Mutex<Option<std::time::SystemTime>>>,
    pub(crate) presence: Arc<PresenceRegistry>,
    pub(crate) occupancy: Arc<OccupancyMap>,
    pub(crate) intent: Arc<IntentRegistry>,
    pub(crate) activity: Arc<ActivityTracker>,
    pub(crate) presence_event_tx: broadcast::Sender<(u64, PresenceEvent)>,
    pub(crate) state_path: PathBuf,
    /// Loaded tuning for the state-file lock. Snapshot at construction
    /// so a tuning.toml reload doesn't change mid-flight behaviour.
    lock_timeouts: LockTimeouts,
}

/// State-file lock timeouts. Snapshotted from `PresenceConfig` at
/// `PresenceLayer` construction; `with_shared_presence` passes these
/// to `state_lock::acquire_with` so the lock honours operator
/// overrides from `.lain/tuning.toml` instead of falling back to the
/// hard-coded defaults.
#[derive(Clone, Copy)]
struct LockTimeouts {
    acquire_timeout_ms: u64,
    retry_interval_ms: u64,
    stale_after_secs: u64,
}

impl PresenceLayer {
    /// Construct from the registry, occupancy map, broadcast sender,
    /// and a precomputed state file path. The state path is derived
    /// from `repos_yaml` if set, otherwise from
    /// `config.workspace`; the caller (PR 3.7b's `LainServer::serve`)
    /// computes it via the same `state_path_for_workspace` helper the
    /// LainServer uses today.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        presence_state_seen: Arc<Mutex<Option<std::time::SystemTime>>>,
        presence: Arc<PresenceRegistry>,
        occupancy: Arc<OccupancyMap>,
        intent: Arc<IntentRegistry>,
        activity: Arc<ActivityTracker>,
        presence_event_tx: broadcast::Sender<(u64, PresenceEvent)>,
        repos_yaml: Option<&std::path::Path>,
        workspace: &std::path::Path,
        presence_config: &crate::server::tuning::PresenceConfig,
    ) -> Self {
        let state_path = match repos_yaml {
            Some(repos) => state_path_for_workspace(repos),
            None => state_path_for_workspace(workspace),
        };
        Self {
            presence_state_seen,
            presence,
            occupancy,
            intent,
            activity,
            presence_event_tx,
            state_path,
            lock_timeouts: LockTimeouts {
                acquire_timeout_ms: presence_config.state_lock_acquire_timeout_ms,
                retry_interval_ms: presence_config.state_lock_retry_interval_ms,
                stale_after_secs: presence_config.state_lock_stale_after_secs,
            },
        }
    }

    /// Path on disk where this server's `PresenceRegistry` +
    /// `OccupancyMap` JSON snapshot is persisted. Derived from a
    /// stable identifier at construction; see [`Self::new`].
    pub fn state_path(&self) -> PathBuf {
        self.state_path.clone()
    }

    /// Borrowed handle to the presence registry. The field itself is
    /// already `pub`, but this accessor keeps the contract consistent
    /// with the other `Arc`-sharing accessors.
    pub fn presence(&self) -> &Arc<PresenceRegistry> {
        &self.presence
    }

    /// Borrowed handle to the occupancy map. Same rationale as
    /// `presence()`.
    pub fn occupancy(&self) -> &Arc<OccupancyMap> {
        &self.occupancy
    }

    /// Borrowed handle to the intent registry (PR 1 of
    /// `docs/INTENT_AND_OBSERVABILITY_PLAN.md`). Same rationale as
    /// `presence()`.
    pub fn intent(&self) -> &Arc<crate::server::intent::IntentRegistry> {
        &self.intent
    }

    /// Borrowed handle to the activity tracker (PR 1). Same
    /// rationale as `presence()`.
    pub fn activity(&self) -> &Arc<crate::server::activity::ActivityTracker> {
        &self.activity
    }

    /// Borrowed handle to the `PresenceEvent` broadcast sender.
    pub fn presence_event_tx(&self) -> &broadcast::Sender<(u64, PresenceEvent)> {
        &self.presence_event_tx
    }

    /// Modification time of the presence state file as of our last load.
    pub fn presence_state_seen(&self) -> &Arc<Mutex<Option<std::time::SystemTime>>> {
        &self.presence_state_seen
    }

    /// Emit a presence event: append it to the durable events log
    /// (assigning its monotonic `event_id`) and broadcast the
    /// `(event_id, event)` pair to all subscribers. This is the only
    /// path live events should take — sending on `presence_event_tx`
    /// directly would skip durability and break SSE `Last-Event-ID`
    /// resume.
    ///
    /// The events log is held by the sibling `AuditState` handle in
    /// PR 3.7b. In this PR the method takes it as a parameter so the
    /// handle compiles standalone.
    pub fn emit_presence_event(
        &self,
        events_log: &crate::server::events_log::EventsLog,
        event: PresenceEvent,
    ) {
        let id = events_log.append(&event);
        let _ = self.presence_event_tx.send((id, event));
    }

    /// Unregister an agent session by its ID, removing it from the presence
    /// registry and releasing all its occupancy claims and advisory lock leases.
    /// Emits `ClaimRevoked` presence events for any released claims.
    /// Returns the paths that were released.
    ///
    /// The events log is passed in for the same reason as
    /// `emit_presence_event`.
    pub fn unregister_agent(
        &self,
        events_log: &crate::server::events_log::EventsLog,
        agent_id: &AgentId,
    ) -> Vec<PathBuf> {
        self.with_shared_presence(|| {
            let released = self.occupancy.release_all_for(agent_id);
            for path in &released {
                self.emit_presence_event(
                    events_log,
                    PresenceEvent::ClaimRevoked {
                        agent_id: agent_id.clone(),
                        path: path.clone(),
                        reason: "unregistered".to_string(),
                    },
                );
            }
            // Drop the agent's intent and activity too so the
            // activity feed doesn't show a ghost entry after the
            // session ends. `presence.remove` triggers the
            // `on_remove_callback` (which only releases
            // occupancy); intent + activity are independent and
            // need explicit cleanup here.
            self.intent.retire(agent_id);
            self.activity.drop_agent(agent_id);
            self.presence.remove(agent_id);
            released
        })
        .unwrap_or_default()
    }

    /// Alias for [`Self::unregister_agent`].
    pub fn unregister_session(
        &self,
        events_log: &crate::server::events_log::EventsLog,
        agent_id: &AgentId,
    ) -> Vec<PathBuf> {
        self.unregister_agent(events_log, agent_id)
    }

    /// Persist the live `PresenceRegistry` + `OccupancyMap` +
    /// `IntentRegistry` + `ActivityTracker` to the server's state
    /// file. The intent and activity registries are additive
    /// (`#[serde(default)]` on the JSON shape), so older state files
    /// hydrate cleanly with empty registries.
    pub fn save_state(&self) -> Result<(), crate::server::error::LainError> {
        let path = self.state_path();
        save_presence_pair(
            &path,
            &self.presence,
            &self.occupancy,
            &self.intent,
            &self.activity,
        )
        .map_err(|e| {
            crate::server::error::LainError::Other(format!("save_state({}): {e}", path.display()))
        })
    }

    /// Hydrate the live `PresenceRegistry` + `OccupancyMap` +
    /// `IntentRegistry` + `ActivityTracker` from the server's state
    /// file, if any. Idempotent: missing file is a no-op.
    ///
    /// Side effect: `load_presence_pair` returns the list of
    /// `ClaimRevoked { reason: "stale_owner" }` events the load
    /// itself produced (a fresh server reclaiming claims whose
    /// owner is no longer in `PresenceRegistry::sessions`). Those
    /// events are forwarded to the SSE broadcast channel here so
    /// peers learn the linearizability gap has been closed.
    /// `events_log::append` is intentionally skipped — the audit
    /// log is append-only across the *current* process's lifetime
    /// and a load happens before any agent is registered, so the
    /// reclamation never needs an audit trail.
    pub fn load_state(&self) -> Result<(), crate::server::error::LainError> {
        let path = self.state_path();
        let revoked = load_presence_pair(
            &path,
            &self.presence,
            &self.occupancy,
            &self.intent,
            &self.activity,
        )
        .map_err(|e| {
            crate::server::error::LainError::Other(format!("load_state({}): {e}", path.display()))
        })?;
        // Use a monotonic counter so each load-time event gets a
        // unique SSE id even though we skipped the audit log.
        let mut counter: u64 = 0;
        for event in revoked {
            counter = counter.saturating_add(1);
            let _ = self.presence_event_tx.send((counter, event));
        }
        Ok(())
    }

    /// Run `f` inside the cross-process presence critical section:
    /// take the state-file lock, refresh the in-memory registries from
    /// disk, run `f`, then write the result back.
    ///
    /// Advisory throughout — a lock timeout proceeds unlocked, and a
    /// failed load or save is logged rather than surfaced. Presence is
    /// a coordination hint; it must never be the thing that breaks a
    /// tool call.
    ///
    /// Unlocked-fallback concurrency: when the lock acquisition
    /// times out, two or more processes can end up holding nothing and
    /// reading the same pre-claim snapshot. Each then mutates its
    /// in-memory copy and the `install_persist_callback` writes its
    /// own state to disk in some order, so the last writer's claims
    /// are the only ones a peer ever observes. To keep the
    /// linearizability invariant from the archived coordination plan
    /// ("at most one live exclusive lease per scope") under that
    /// concurrent-unlocked-write scenario, we hash the state file
    /// before `f` and again after `f` returns; if the hash changed,
    /// another process wrote during our critical section, our
    /// in-memory decision may have missed its claim, and we re-run
    /// `f` on the refreshed state. The retry is bounded so a
    /// permanently-contended workload doesn't livelock.
    ///
    /// Re-running is safe for the claim/release paths because the
    /// in-memory conflict filter excludes the agent's own id
    /// (`OccupancyMap::claim_in_memory` filters `entry.agents` with
    /// `a != agent_id`) and re-claiming your own scope is a no-op.
    /// `register_agent` is NOT idempotent — it mints a fresh
    /// `agent_id` per call — but the linearizability invariant
    /// applies only to exclusive leases; a duplicate session under
    /// the rare race where two agents register at the same instant
    /// is benign and the duplicate expires on its TTL.
    ///
    /// Refresh / persist failures: if `refresh_shared_presence`
    /// can't read the on-disk state (e.g. the state path was
    /// replaced by a directory, or the file's permissions revoke
    /// read access mid-run), or the persist callback can't write
    /// the on-disk state after `f` returns, this function surfaces
    /// the error as `Err(CoordinationError::RefreshFailed)` or
    /// `Err(CoordinationError::PersistFailed)`. Without this
    /// propagation both processes see stale in-memory decisions and
    /// return `granted` based on no peer state — the
    /// `runs/20260921-pr199-persist-failure/` artefact under
    /// `anemone/runs/` reproduces that exact scenario.
    /// Run `f` inside the cross-process presence critical section.
    ///
    /// The state-file lock is acquired with the loaded tuning
    /// (`tuning.toml`'s `state_lock_*` keys) and held through
    /// `refresh_shared_presence` + `f` + the persist callback. If
    /// the lock cannot be acquired within the configured deadline,
    /// this returns `Err(CoordinationError::Unavailable)` — the
    /// caller surfaces the failure to the agent so it can retry
    /// instead of silently writing a stale view.
    ///
    /// Why fail closed (instead of the historical "proceed unlocked"
    /// advisory): the linearizability invariant from
    /// `docs/archive/COORDINATION_CONSISTENCY_PLAN.md` says at most
    /// one live exclusive lease per scope. Holding the lock through
    /// persist is the only way to enforce that invariant under
    /// contention. Proceeding unlocked is the failure mode that
    /// the reproducer in `harness/reproduce-stdio-lock-fail-open.py`
    /// surfaces — six agents reading the same pre-claim snapshot,
    /// each firing its persist callback, and the last writer's
    /// state being the only one peers ever observe. Returning
    /// `Unavailable` puts the retry decision on the agent, which
    /// can do exponential backoff, surface the error to its user,
    /// or escalate to a hard failure.
    pub fn with_shared_presence<T>(&self, f: impl FnOnce() -> T) -> Result<T, CoordinationError> {
        let path = self.state_path();
        let lock = lain_state_lock::acquire_with(
            &path,
            self.lock_timeouts.acquire_timeout_ms,
            self.lock_timeouts.retry_interval_ms,
            self.lock_timeouts.stale_after_secs,
        );
        if !lock.is_held() {
            tracing::warn!(
                "with_shared_presence: state-file lock not acquired within \
                 {}ms; refusing to proceed unlocked",
                self.lock_timeouts.acquire_timeout_ms
            );
            return Err(CoordinationError::Unavailable);
        }
        // Refresh: must succeed before we mutate. A failed read
        // would leave the agent's decision based on a stale view
        // that no longer reflects peer activity on disk; surfacing
        // the error is the only way to enforce the linearizability
        // invariant in that case.
        if let Err(e) = self.refresh_shared_presence() {
            return Err(CoordinationError::RefreshFailed(e));
        }
        // Capture the persist callback's result so we can surface
        // it after `f` returns. The callback signature is `Fn()`
        // (installed once at LainServer construction), so we wrap
        // it for the duration of this critical section by stashing
        // the previous callback and installing a fresh one that
        // records its result in the cell.
        let persist_result =
            std::sync::Arc::new(parking_lot::Mutex::new(None::<Result<(), String>>));
        let prev_presence_cb = self.presence.swap_persist_capture(
            std::sync::Arc::clone(&persist_result),
            path.clone(),
            self.presence.clone(),
            self.occupancy.clone(),
            self.intent.clone(),
            self.activity.clone(),
        );
        let prev_occupancy_cb = self.occupancy.swap_persist_capture(
            std::sync::Arc::clone(&persist_result),
            path.clone(),
            self.presence.clone(),
            self.occupancy.clone(),
            self.intent.clone(),
            self.activity.clone(),
        );
        let result = f();
        if let Some(prev) = prev_presence_cb {
            self.presence.restore_persist_callback(prev);
        }
        if let Some(prev) = prev_occupancy_cb {
            self.occupancy.restore_persist_callback(prev);
        }
        if let Some(Err(e)) = persist_result.lock().clone() {
            return Err(CoordinationError::PersistFailed(e));
        }
        Ok(result)
    }

    /// Read-only half of [`Self::with_shared_presence`]: refresh from
    /// disk so a listing reflects peers, without taking the write lock
    /// or saving. Used by `list_active_agents`, `list_occupancy` and
    /// friends, where a stale read is the whole bug and a write would
    /// be pure contention.
    pub fn refresh_shared_presence(&self) -> Result<(), String> {
        let path = self.state_path();
        // The reload exists to observe *other processes'* writes. If the
        // file has not changed since we last read it, there is nothing
        // to observe and parsing it again is wasted work on a path
        // every presence call goes through.
        let current = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());
        {
            let seen = self.presence_state_seen.lock();
            if current.is_some() && *seen == current {
                return Ok(());
            }
        }
        self.load_state().map_err(|e| e.to_string())?;
        *self.presence_state_seen.lock() = current;
        Ok(())
    }

    // `install_persist_callback` stays on LainServer in PR 3.7b — it
    // wires `Presence` + `Occupancy` together with the workspace root
    // and federation claim roots, which cross handle boundaries.

    // Reference held for the doc comment on the LainServer fields
    // originally borrowed these; the import is no longer needed at
    // this layer but kept here so the docstring stays accurate.
    #[allow(dead_code)]
    fn _graph_db_marker(_g: &GraphDatabase) {}
}

/// Coordination failure surfaced by `PresenceLayer::with_shared_presence`.
/// Every variant indicates that the call did NOT complete its
/// in-memory mutation safely: the agent sees the error and retries,
/// rather than silently operating on stale state. The
/// `Unavailable` variant covers lock-acquisition timeout;
/// `RefreshFailed` covers the case where the on-disk state file
/// can't be read after the lock is held (e.g. the path was
/// replaced by a directory between lock acquisition and read);
/// `PersistFailed` covers the case where the in-memory mutation
/// ran but the on-disk write failed. All three together enforce
/// the linearizability invariant from
/// `docs/archive/COORDINATION_CONSISTENCY_PLAN.md` ("at most one
/// live exclusive lease per scope").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinationError {
    /// State-file lock acquisition timed out. A peer process is
    /// holding the critical section; retry after a brief delay.
    Unavailable,
    /// `refresh_shared_presence` couldn't read the on-disk state.
    /// The in-memory state is now stale; the agent must retry so
    /// the next call sees fresh peer state.
    RefreshFailed(String),
    /// The persist callback failed to write the on-disk state
    /// after the closure ran. The in-memory mutation succeeded but
    /// peers won't see it; the agent must retry so the lock is
    /// re-acquired and the write is re-attempted.
    PersistFailed(String),
}

impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoordinationError::Unavailable => f.write_str(
                "presence coordination unavailable: state-file lock not acquired within the configured deadline",
            ),
            CoordinationError::RefreshFailed(e) => write!(
                f,
                "presence coordination failed: state-file refresh failed ({e})"
            ),
            CoordinationError::PersistFailed(e) => write!(
                f,
                "presence coordination failed: persist callback failed ({e})"
            ),
        }
    }
}

impl std::error::Error for CoordinationError {}

impl From<CoordinationError> for String {
    fn from(e: CoordinationError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::presence::PresenceRegistry;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn layer(repos_yaml: Option<&std::path::Path>, workspace: &std::path::Path) -> PresenceLayer {
        let dir = tempdir();
        let workspace = if workspace.as_os_str().is_empty() {
            dir.path()
        } else {
            workspace
        };
        let (presence_tx, _rx) = broadcast::channel(8);
        // Intent + activity trackers ride alongside presence /
        // occupancy (PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
        // These tests only assert on presence state paths, so empty
        // trackers are correct.
        PresenceLayer::new(
            Arc::new(Mutex::new(None)),
            Arc::new(PresenceRegistry::new()),
            Arc::new(OccupancyMap::default()),
            Arc::new(crate::server::intent::IntentRegistry::new()),
            Arc::new(crate::server::activity::ActivityTracker::new()),
            presence_tx,
            repos_yaml,
            workspace,
            &crate::server::tuning::PresenceConfig::default(),
        )
    }

    #[test]
    fn state_path_prefers_repos_yaml_when_set() {
        let repos = std::path::PathBuf::from("/tmp/repos.yaml");
        let workspace = std::path::PathBuf::from("/tmp/ws");
        let l = layer(Some(&repos), &workspace);
        let expected = state_path_for_workspace(&repos);
        assert_eq!(l.state_path(), expected);
    }

    #[test]
    fn state_path_falls_back_to_workspace() {
        let workspace = std::path::PathBuf::from("/tmp/ws");
        let l = layer(None, &workspace);
        let expected = state_path_for_workspace(&workspace);
        assert_eq!(l.state_path(), expected);
    }

    #[test]
    fn accessors_return_the_stored_arcs() {
        let l = layer(None, std::path::Path::new("/tmp/ws"));
        assert!(Arc::strong_count(l.presence()) > 0);
        assert!(Arc::strong_count(l.occupancy()) > 0);
        assert_eq!(l.presence_event_tx().receiver_count(), 0);
    }

    /// Unit test for fail-closed: `with_shared_presence` must
    /// return `Err(CoordinationError::Unavailable)` when the
    /// state-file lock can't be acquired, so the agent sees a
    /// coordination failure instead of silently writing a stale
    /// view. We plant a fresh sentinel that the acquisition will
    /// treat as held for the full timeout.
    ///
    /// Cross-process version of this scenario is the reproducer in
    /// `harness/reproduce-stdio-lock-fail-open.py`.
    #[test]
    fn with_shared_presence_fails_closed_when_lock_unavailable() {
        let tmp = tempdir();
        let ws = tmp.path().to_path_buf();
        let repos = ws.join("repos.yaml");
        std::fs::write(&repos, "").unwrap();

        // Build a `PresenceLayer` with a tiny acquire timeout so the
        // test doesn't spend the production default 2000 ms waiting.
        let short_timeouts = crate::server::tuning::PresenceConfig {
            state_lock_acquire_timeout_ms: 50,
            state_lock_retry_interval_ms: 5,
            state_lock_stale_after_secs: 60,
            ..crate::server::tuning::PresenceConfig::default()
        };
        let presence_state_seen = Arc::new(Mutex::new(None));
        let presence = Arc::new(crate::server::presence::PresenceRegistry::new());
        let occupancy = Arc::new(crate::server::presence::OccupancyMap::new());
        let intent = Arc::new(crate::server::intent::IntentRegistry::new());
        let activity = Arc::new(crate::server::activity::ActivityTracker::new());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let state_path = crate::config::state_path_for_workspace(&repos);
        let l = PresenceLayer {
            presence_state_seen,
            presence,
            occupancy,
            intent,
            activity,
            presence_event_tx: tx,
            state_path,
            lock_timeouts: LockTimeouts {
                acquire_timeout_ms: short_timeouts.state_lock_acquire_timeout_ms,
                retry_interval_ms: short_timeouts.state_lock_retry_interval_ms,
                stale_after_secs: short_timeouts.state_lock_stale_after_secs,
            },
        };

        // Hold the lock from another handle (as a peer process would) so
        // the next `state_lock::acquire` times out. `with_shared_presence`
        // must observe `!lock.is_held()` and surface `Unavailable`.
        if let Some(parent) = l.state_path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let lock_path = l.state_path().with_extension("json.lock");
        let peer = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap();
        peer.try_lock().expect("peer takes the lock");

        let result: Result<(), CoordinationError> = l.with_shared_presence(|| ());
        assert!(
            matches!(result, Err(CoordinationError::Unavailable)),
            "expected CoordinationError::Unavailable when lock is held; got {result:?}"
        );

        // Cleanup: remove the sentinel so other tests don't trip
        // over it.
        drop(peer);
    }

    /// Unit test for the I/O-failure path the user identified: the
    /// state-file lock acquisition succeeds (sibling lock path is
    /// writable), but the state-file *content* path is unwritable
    /// (replaced by a directory between two agents registering).
    /// Both `refresh_shared_presence` (which tries to read the
    /// file) and the persist callback (which tries to write it)
    /// would silently fail under the historical advisory design;
    /// both agents would then return "granted" based on a stale
    /// in-memory view that never observed peer activity. After the
    /// PR #199 fix that surfaces those failures, both calls must
    /// return `Err(CoordinationError::RefreshFailed | PersistFailed)`.
    #[test]
    fn with_shared_presence_fails_closed_when_state_path_is_a_directory() {
        let tmp = tempdir();
        let ws = tmp.path().to_path_buf();
        let repos = ws.join("repos.yaml");
        std::fs::write(&repos, "").unwrap();

        // Build a `PresenceLayer` with the production defaults —
        // the lock acquisition is fast so the test runs without
        // sitting on a 2-second timeout.
        let cfg = crate::server::tuning::PresenceConfig::default();
        let presence_state_seen = Arc::new(Mutex::new(None));
        let presence = Arc::new(crate::server::presence::PresenceRegistry::new());
        let occupancy = Arc::new(crate::server::presence::OccupancyMap::new());
        let intent = Arc::new(crate::server::intent::IntentRegistry::new());
        let activity = Arc::new(crate::server::activity::ActivityTracker::new());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let state_path = crate::config::state_path_for_workspace(&repos);
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let l = PresenceLayer {
            presence_state_seen,
            presence,
            occupancy,
            intent,
            activity,
            presence_event_tx: tx,
            state_path: state_path.clone(),
            lock_timeouts: LockTimeouts {
                acquire_timeout_ms: cfg.state_lock_acquire_timeout_ms,
                retry_interval_ms: cfg.state_lock_retry_interval_ms,
                stale_after_secs: cfg.state_lock_stale_after_secs,
            },
        };

        // Replace the state-file *content* path with a directory.
        // The sibling lock path (`<path>.json.lock`) stays a regular
        // file (or doesn't exist yet), so lock acquisition still
        // succeeds. The state-file read in `refresh_shared_presence`
        // and the state-file write in the persist callback both
        // fail.
        if state_path.exists() {
            std::fs::remove_file(&state_path).unwrap();
        }
        std::fs::create_dir(&state_path).unwrap();

        let result: Result<(), CoordinationError> = l.with_shared_presence(|| ());
        let err_msg = match result {
            Ok(()) => panic!(
                "with_shared_presence must fail closed when the state path \
                 is a directory; got Ok(())"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            err_msg.contains("refresh failed") || err_msg.contains("persist failed"),
            "expected RefreshFailed or PersistFailed; got {err_msg:?}"
        );

        // Cleanup: remove the directory we created.
        let _ = std::fs::remove_dir(&state_path);
    }
}
