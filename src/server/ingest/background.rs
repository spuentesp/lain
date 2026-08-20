//! Background tasks spawned by the LainServer constructors: the
//! presence expiry loop and the attribution watcher. Pulled out of
//! `mod.rs` so the per-second tick logic and the watcher wiring
//! live next to each other; the constructors (`constructors.rs`)
//! just call into here.

use crate::server::attribution::{
    AttributionBackend, AttributionWatcher, LsofBackend, NoopBackend, ProcFsBackend,
};
use crate::server::events_log::EventsLog;
use crate::server::presence::{OccupancyMap, PresenceEvent, PresenceRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Pick the platform-appropriate default [`AttributionBackend`] for
/// constructors that don't take an explicit backend (i.e. the
/// `LainServer::new` / `with_federation` / `with_federation_and_workspaces`
/// constructors — every test and embedder that isn't the `lain server`
/// CLI). The CLI uses the `_with_attribution` variants and picks its
/// own backend based on platform + `--no-process-attribution`.
pub fn default_attribution_backend() -> Arc<dyn AttributionBackend> {
    if cfg!(target_os = "linux") {
        Arc::new(ProcFsBackend)
    } else if cfg!(target_os = "macos") {
        Arc::new(LsofBackend)
    } else {
        Arc::new(NoopBackend)
    }
}

/// Spawn the background task that prunes expired sessions + claim
/// TTLs every 5 seconds and broadcasts `PresenceEvent` notifications.
/// The `JoinHandle` is intentionally dropped — the task lives for
/// the lifetime of the process. (For graceful shutdown we'd store
/// the handle and abort it; not needed for MVP.)
pub fn spawn_presence_expiry_loop(
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    tx: broadcast::Sender<(u64, PresenceEvent)>,
    events_log: Arc<EventsLog>,
) {
    let p = presence.clone();
    let o = occupancy.clone();
    let t = tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            expiry_tick(&p, &o, &t, &events_log);
        }
    });
}

/// One tick of the expiry loop. Expiring a session must also release
/// every claim it held: previously `expire_stale` removed only the
/// session, so a dead agent's claims (default: no TTL) persisted
/// forever — found live, where claims from days-old smoke tests kept
/// conflicting with new claims. Emits `HeartbeatExpired` plus one
/// `ClaimReleased` per released path, all with durable events-log ids.
pub fn expiry_tick(
    presence: &PresenceRegistry,
    occupancy: &OccupancyMap,
    tx: &broadcast::Sender<(u64, PresenceEvent)>,
    events_log: &EventsLog,
) {
    let emit = |ev: PresenceEvent| {
        let eid = events_log.append(&ev);
        let _ = tx.send((eid, ev));
    };
    for id in presence.expire_stale() {
        emit(PresenceEvent::HeartbeatExpired(id.clone()));
        for path in occupancy.release_all_for(&id) {
            emit(PresenceEvent::ClaimReleased {
                agent_id: id.clone(),
                path,
            });
        }
    }
    for (agent_id, path) in occupancy.expire_by_ttl() {
        emit(PresenceEvent::ClaimReleased { agent_id, path });
    }
}

/// Start the attribution watcher (inotify on each registered repo's
/// checkout for live edit attribution). The handle is dropped — the
/// thread lives until the channel closes. Previously this watched
/// `repos.yaml`'s parent dir, which auto-claimed unrelated files
/// (server logs, scratch files) under the single-agent heuristic;
/// now it watches exactly `FederatedIndex::repo_paths()`. Repos
/// added by a hot-reload after startup are not watched until the
/// next server restart.
pub fn start_attribution_watcher(
    attribution: Arc<dyn AttributionBackend>,
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    tx: broadcast::Sender<(u64, PresenceEvent)>,
    events_log: Arc<EventsLog>,
    repo_roots: Vec<PathBuf>,
) {
    let _ = AttributionWatcher::new_with_backend(
        attribution,
        presence,
        occupancy,
        tx,
        events_log,
        repo_roots,
    )
    .start();
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use crate::server::presence::{AgentKind, AgentMode, ClaimIntent, ClaimRequest};

    /// A session expiring must release its claims: the pre-fix behavior
    /// left dead agents' no-TTL claims in the occupancy map forever
    /// (observed live: days-old smoke-test claims kept conflicting).
    #[test]
    fn expiry_tick_releases_claims_of_expired_sessions() {
        let presence = PresenceRegistry::with_expiry(std::time::Duration::from_millis(20));
        let occupancy = OccupancyMap::new();
        let (tx, _rx) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let log = EventsLog::open(tmp.path()).unwrap();

        let sess = presence.register(
            "ghost".into(),
            AgentKind::ClaudeCode,
            AgentMode::Interactive,
            None,
            None,
        );
        let granted = occupancy.claim(
            &sess.id,
            vec![ClaimRequest {
                path: PathBuf::from("a.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None, // no TTL — the case that used to leak
                plan_revision: None,
            }],
        );
        assert_eq!(granted.granted.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(40));
        expiry_tick(&presence, &occupancy, &tx, &log);

        assert!(presence.list_active(true).is_empty(), "session expired");
        assert!(
            occupancy.list_all().is_empty(),
            "claims of an expired session must be released, got: {:?}",
            occupancy.list_all()
        );
    }

    /// Claims WITH a TTL still expire via their own path; sessions whose
    /// claims expire keep living (heartbeat-independent).
    #[test]
    fn expiry_tick_keeps_ttl_claim_path() {
        let presence = PresenceRegistry::new();
        let occupancy = OccupancyMap::new();
        let (tx, _rx) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let log = EventsLog::open(tmp.path()).unwrap();

        let sess = presence.register(
            "ttl-agent".into(),
            AgentKind::Kimi,
            AgentMode::Interactive,
            None,
            None,
        );
        occupancy.claim(
            &sess.id,
            vec![ClaimRequest {
                path: PathBuf::from("b.rs"),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: Some(0), // expires immediately
                plan_revision: None,
            }],
        );

        expiry_tick(&presence, &occupancy, &tx, &log);

        assert!(occupancy.list_all().is_empty(), "TTL claim expired");
        assert_eq!(presence.list_active(true).len(), 1, "session alive");
    }
}
