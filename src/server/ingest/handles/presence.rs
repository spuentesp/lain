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
    pub fn with_shared_presence<T>(&self, f: impl FnOnce() -> T) -> T {
        let path = self.state_path();
        let _lock = lain_state_lock::acquire(&path);
        self.refresh_shared_presence();
        // No explicit save here: `install_persist_callback` already
        // writes on every mutation, and only on an actual mutation.
        // Saving again wrote the whole state file a second time on
        // every presence call — including read-only ones that changed
        // nothing — which showed up as a ~300ms p99 on contended
        // claims.
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
}
