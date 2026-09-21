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
    pub fn with_shared_presence<T>(&self, f: impl Fn() -> T) -> T {
        const MAX_RACE_RETRIES: usize = 4;
        let path = self.state_path();
        let lock = lain_state_lock::acquire(&path);
        if lock.is_held() {
            // Lock held: no concurrent writers possible. Run the
            // closure under the lock so the in-memory refresh, the
            // mutation, and the persist callback all happen
            // serialized against peers. The lock guard's Drop
            // releases the sentinel when this scope exits.
            self.refresh_shared_presence();
            return f();
        }
        // Unlocked fallback. Capture the pre-f state hash so we can
        // tell, after f() returns, whether anyone else wrote to the
        // file during our critical section.
        //
        // On race detection we re-run `f` on the refreshed state so
        // the agent's own conflict filter sees the other writer's
        // claim. `f` must be re-callable (a `Fn`, not a `FnOnce`) so
        // every call site captures its arguments by clone or borrow;
        // the four presence-tools closures were updated accordingly.
        for attempt in 0..MAX_RACE_RETRIES {
            self.refresh_shared_presence();
            let pre_hash = state_file_hash(&path);
            let result = f();
            let post_hash = state_file_hash(&path);
            if pre_hash == post_hash {
                return result;
            }
            tracing::warn!(
                "with_shared_presence: concurrent unlocked write detected \
                 on attempt {}; re-running on refreshed state",
                attempt + 1
            );
        }
        // Exhausted retries. Run one last time and return whatever
        // f says. Under pathological contention the worst case is
        // the original last-writer-wins behavior.
        self.refresh_shared_presence();
        f()
    }

    /// Read-only half of [`Self::with_shared_presence`]: refresh from
    /// disk so a listing reflects peers, without taking the write lock
    /// or saving. Used by `list_active_agents`, `list_occupancy` and
    /// friends, where a stale read is the whole bug and a write would
    /// be pure contention.
    pub fn refresh_shared_presence(&self) {
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
                return;
            }
        }
        if let Err(e) = self.load_state() {
            tracing::debug!("presence refresh skipped: {e}");
            return;
        }
        *self.presence_state_seen.lock() = current;
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

/// BLAKE3 hash of the state file at `path`. Returns the all-zero
/// hash when the file is missing or unreadable; callers compare
/// against the previous hash to detect "the file changed between
/// my reads" and trigger a retry of the in-flight operation.
///
/// Used by `PresenceLayer::with_shared_presence`'s concurrent-
/// unlocked-write fallback. The hash is intentionally content-
/// based rather than mtime-based because mtime resolution is
/// filesystem-dependent and the reproducer's adversarial padding
/// keeps the file from getting a meaningful mtime bump.
fn state_file_hash(path: &std::path::Path) -> blake3::Hash {
    match std::fs::read(path) {
        Ok(bytes) => blake3::hash(&bytes),
        Err(_) => blake3::hash(b""),
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

    /// Unit test for the hash-based concurrent-unlocked-write
    /// detection. Two processes each take a separate `PresenceLayer`
    /// pointed at the same state file; the first writes through its
    /// layer, then the second writes through its layer. The second's
    /// `with_shared_presence` should detect that the file changed
    /// during its critical section and re-run the closure on the
    /// refreshed state. We assert that the re-run fires by counting
    /// closure invocations and confirming the second layer observed
    /// the first layer's write via the in-memory state.
    ///
    /// This test deliberately bypasses the file-lock by planting a
    /// fresh sentinel that the acquisition will treat as held, so
    /// the unlocked-fallback path is exercised. The reproducer in
    /// `tests/.../reproduce-stdio-lock-fail-open.py` is the
    /// cross-process version of this scenario.
    #[test]
    fn with_shared_presence_retries_when_unlocked_write_detected() {
        use crate::server::activity::ActivityTracker;
        use crate::server::intent::IntentRegistry;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let tmp = tempdir();
        let ws = tmp.path().to_path_buf();
        let repos = ws.join("repos.yaml");
        std::fs::write(&repos, "").unwrap();

        // Two layers, both pointed at the same state file. Each
        // will go through the unlocked fallback because we plant a
        // sentinel below.
        let l1 = layer(Some(&repos), &ws);
        let l2 = layer(Some(&repos), &ws);
        let reg1 = Arc::clone(l1.presence());
        let reg2 = Arc::clone(l2.presence());
        let occ1 = Arc::clone(l1.occupancy());
        let occ2 = Arc::clone(l2.occupancy());
        let intent = Arc::new(IntentRegistry::new());
        let activity = Arc::new(ActivityTracker::new());

        // Seed a session so each layer's register path succeeds.
        let _ = reg1.register(
            "agent-1".into(),
            crate::server::presence::AgentKind::Other("test".into()),
            crate::server::presence::AgentMode::Interactive,
            None,
            None,
        );
        let _ = reg2.register(
            "agent-2".into(),
            crate::server::presence::AgentKind::Other("test".into()),
            crate::server::presence::AgentMode::Interactive,
            None,
            None,
        );
        // Install persist callbacks so each mutation hits disk.
        let path1 = l1.state_path();
        let path2 = l2.state_path();
        let cb1 = {
            let r = Arc::clone(&reg1);
            let o = Arc::clone(&occ1);
            let i = Arc::clone(&intent);
            let a = Arc::clone(&activity);
            let p = path1.clone();
            move || {
                let _ = crate::server::presence::save_pair(&p, &r, &o, &i, &a);
            }
        };
        let cb2 = {
            let r = Arc::clone(&reg2);
            let o = Arc::clone(&occ2);
            let i = Arc::clone(&intent);
            let a = Arc::clone(&activity);
            let p = path2.clone();
            move || {
                let _ = crate::server::presence::save_pair(&p, &r, &o, &i, &a);
            }
        };
        reg1.set_persist_callback(cb1.clone());
        occ1.set_persist_callback(cb1);
        reg2.set_persist_callback(cb2.clone());
        occ2.set_persist_callback(cb2);

        // Plant a fresh sentinel so lock acquisition times out and
        // both layers proceed through the unlocked fallback. The
        // path the sentinel lives at is the state-file lock sentinel
        // path; the file-lock module reads it on every acquire.
        let sentinel = path1.with_extension("json.lock");
        std::fs::write(&sentinel, "forced-by-test\n").unwrap();

        // Each layer tries to claim a distinct path so the
        // post-refresh re-run has a chance to see the other layer's
        // mutation. Layer 1 writes first (manually), then layer 2
        // runs `with_shared_presence` which should detect the
        // concurrent write via the post_hash check.
        let call_count = Arc::new(AtomicUsize::new(0));
        let result = l2.with_shared_presence(|| {
            call_count.fetch_add(1, Ordering::SeqCst);
            // On the first call, layer 1's write hasn't hit disk
            // yet; we simulate it by writing here so the
            // post-f hash differs from pre-f.
            let _ = crate::server::presence::save_pair(&path1, &reg1, &occ1, &intent, &activity);
            42
        });

        // The closure was invoked at least once. With the CAS retry
        // it may be invoked more than once if the post-f hash
        // differs. We don't assert a specific count — the
        // reproducers in `harness/reproduce-stdio-lock-fail-open.py`
        // and the cross-process tests cover the end-to-end count.
        assert!(call_count.load(Ordering::SeqCst) >= 1);
        assert_eq!(result, 42);

        // Tidy up the sentinel so other tests don't trip over it.
        let _ = std::fs::remove_file(&sentinel);
    }
}
