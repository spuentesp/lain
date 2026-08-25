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
            // `ClaimRevoked`, not `ClaimReleased`: the agent did not ask
            // to give this up and may still be mid-edit. Subscribers
            // need to tell the two apart to know whether the file is
            // genuinely free.
            emit(PresenceEvent::ClaimRevoked {
                agent_id: id.clone(),
                path,
                reason: "session_expired".to_string(),
            });
        }
    }
    for (agent_id, path) in occupancy.expire_by_ttl() {
        emit(PresenceEvent::ClaimRevoked {
            agent_id,
            path,
            reason: "ttl_expired".to_string(),
        });
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

/// How often expired UI sessions are reaped.
const UI_SESSION_REAP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Periodically drop expired UI sessions.
///
/// `ToolContext::cleanup_expired_sessions` documents itself as "Call
/// periodically to prevent unbounded growth" and had no caller, so
/// nothing ever did. Every `/ui/blast-radius/...` session created by the
/// HTTP transport stayed in the map for the life of the process, past its
/// own `expires_at`, and a long-running server grew without bound.
pub fn spawn_ui_session_reaper(ctx: crate::server::tools::registry::ToolContext) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(UI_SESSION_REAP_INTERVAL).await;
            ctx.cleanup_expired_sessions().await;
        }
    });
}

/// Environment escape hatch for [`spawn_commit_sync`].
pub const DISABLE_COMMIT_SYNC_ENV: &str = "LAIN_DISABLE_COMMIT_SYNC";
/// Override for the commit-sync poll interval, in seconds.
pub const COMMIT_SYNC_INTERVAL_ENV: &str = "LAIN_COMMIT_SYNC_SECS";
/// Default gap between commit checks.
const COMMIT_SYNC_DEFAULT_SECS: u64 = 300;

/// Re-index when the checkout moves to a new commit.
///
/// `LainServer::run_background_sync` — the loop that compares the current
/// HEAD against the graph's recorded commit and re-runs
/// `build_core_memory` when they differ — had no caller, so nothing ever
/// re-indexed after startup. A server left running answered from whatever
/// commit it first indexed, indefinitely.
///
/// This is not theoretical: the graph in this repo's own `.lain/graph.bin`
/// was four days and 29 commits behind HEAD, and reading it produced two
/// confident but wrong conclusions about the code — a leaky
/// `find_dead_code` filter and a cross-language `Calls` edge, both of
/// which had already been fixed in commits the graph had never seen. The
/// tools do warn about staleness, but a warning the operator must act on
/// by hand is not the same as staying current.
///
/// The poll itself is cheap — one `git` commit lookup. The expensive
/// reindex only runs when the commit actually changed. Set
/// `LAIN_DISABLE_COMMIT_SYNC=1` to opt out, or
/// `LAIN_COMMIT_SYNC_SECS` to change the interval.
/// Whether an opt-out env value means "off". Unset, empty, and `0` all
/// mean "leave it on" so `LAIN_DISABLE_X=0` reads the way an operator
/// expects rather than tripping the mere-presence check.
pub(crate) fn env_disables(raw: Option<&str>) -> bool {
    matches!(raw, Some(v) if v != "0" && !v.is_empty())
}

/// Poll interval from the env override, falling back to the default.
/// A non-numeric or zero value falls back rather than disabling the
/// loop or spinning it at zero delay.
pub(crate) fn commit_sync_interval(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(COMMIT_SYNC_DEFAULT_SECS)
}

pub fn spawn_commit_sync(server: crate::server::LainServer) {
    if env_disables(std::env::var(DISABLE_COMMIT_SYNC_ENV).ok().as_deref()) {
        tracing::info!("commit sync disabled by {}", DISABLE_COMMIT_SYNC_ENV);
        return;
    }
    let secs = commit_sync_interval(std::env::var(COMMIT_SYNC_INTERVAL_ENV).ok().as_deref());
    tracing::info!("commit sync: re-index check every {secs}s");
    tokio::spawn(async move {
        server.run_background_sync(secs).await;
    });
}

/// Environment escape hatch for [`start_source_watcher`].
pub const DISABLE_SOURCE_WATCHER_ENV: &str = "LAIN_DISABLE_FILE_WATCHER";

/// Start the source-file watcher that keeps the volatile overlay fresh
/// between reindexes.
///
/// This is the wiring that never existed. `FileWatcher` — the thread, the
/// debounce loop, `process_file`, and the `overlay.insert_node` call it
/// ends in — was fully written and never constructed anywhere outside its
/// own test module, so the overlay stayed empty for the life of every
/// server. The README's "stays fresh during editing via a file watcher
/// that updates a volatile overlay" described code that no process ran:
/// saving a file left `Volatile Nodes (Overlay): 0` and a new symbol
/// answered `Node not found` until the next commit and reindex.
///
/// Set `LAIN_DISABLE_FILE_WATCHER=1` to opt out — the watcher calls the
/// language server once per changed file, which is cheap for a save and
/// less so for a branch switch that rewrites thousands.
///
/// Caveat worth knowing: freshness is only as good as the language
/// server. `process_file` gets its symbols from LSP and returns `Ok(())`
/// silently when none come back, so a file whose language server is not
/// installed — or is installed but cannot load the project — updates
/// nothing and reports no error. Verified end to end: the same edit that
/// left the overlay at 0 with `cargo` off the PATH (rust-analyzer could
/// not run `cargo metadata`) took it to 2 with `cargo` present.
/// `get_health`'s language-support table is the place to check which
/// servers are actually resolvable.
pub fn start_source_watcher(workspace: PathBuf, server: crate::server::LainServer) {
    if env_disables(std::env::var(DISABLE_SOURCE_WATCHER_ENV).ok().as_deref()) {
        tracing::info!(
            "source file watcher disabled by {}",
            DISABLE_SOURCE_WATCHER_ENV
        );
        return;
    }
    tracing::info!("source file watcher: watching {:?}", workspace);
    crate::server::watcher::FileWatcher::new().start(workspace, server);
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

#[cfg(test)]
mod wiring_tests {
    /// `cleanup_expired_sessions` documents itself as "Call periodically
    /// to prevent unbounded growth" and had no caller anywhere, so UI
    /// sessions created by the HTTP transport accumulated for the life of
    /// the process regardless of their own `expires_at`.
    #[tokio::test]
    async fn expired_ui_sessions_are_actually_reaped() {
        use crate::tools::{UiSession, UiSessionData};
        use std::time::{Duration, SystemTime};

        let tmp = std::env::temp_dir().join("lain_ui_session_reap");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = crate::graph::GraphDatabase::new(&tmp).unwrap();
        let exec = crate::server::tools::create_test_executor_with_graph(graph);

        let mk = |id: &str, expires_at: SystemTime| UiSession {
            id: id.to_string(),
            session_type: "blast-radius".to_string(),
            created_at: SystemTime::now(),
            data: UiSessionData::BlastRadius {
                symbol: "foo".to_string(),
                nodes: Vec::new(),
            },
            expires_at,
        };

        {
            let mut g = exec.ui_sessions().lock().await;
            g.insert(
                "stale".into(),
                mk("stale", SystemTime::now() - Duration::from_secs(3600)),
            );
            g.insert(
                "live".into(),
                mk("live", SystemTime::now() + Duration::from_secs(3600)),
            );
            assert_eq!(g.len(), 2);
        }

        exec.ctx.cleanup_expired_sessions().await;

        let g = exec.ui_sessions().lock().await;
        assert!(!g.contains_key("stale"), "an expired session must be dropped");
        assert!(g.contains_key("live"), "a live session must survive");
        assert_eq!(g.len(), 1);
    }

    /// `run_background_sync` had no caller, so a running server answered
    /// from the commit it first indexed forever. Wired for `lain mcp`,
    /// where `build_core_memory` and `self.git` both refer to the one
    /// workspace; the federation path drives its own `repo.index()` per
    /// repo instead, so it deliberately does not use this loop.
    #[test]
    fn the_single_repo_bootstrap_starts_commit_sync() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = std::fs::read_to_string(root.join("src/cli/mcp.rs")).unwrap();
        assert!(
            src.contains("spawn_commit_sync"),
            "`lain mcp` must re-index when the checkout moves to a new commit"
        );
    }

    /// Federated repos must be watched for re-index too.
    /// `RepoIndex::start_watcher` had a test but no production caller, so
    /// each repo stayed frozen at the commit it was first indexed at —
    /// while `cli/server.rs` carried a comment claiming "the watcher would
    /// eventually pick up filesystem events".
    #[test]
    fn the_federation_bootstrap_watches_each_repo_for_reindex() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = std::fs::read_to_string(root.join("src/cli/server.rs")).unwrap();
        assert!(
            src.contains("start_watcher()"),
            "`lain server` must start each repo's re-index watcher"
        );
    }

    /// Every ingest pipeline must run the protocol sensors. They produce
    /// `HttpRoute`, `CallsHttp` and `Implements`; with no caller those
    /// types were advertised by `describe_schema` and unreachable in
    /// practice for the life of the project.
    #[test]
    fn both_ingest_pipelines_run_the_protocol_sensors() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = std::fs::read_to_string(root.join("src/server/ingest/ingestion.rs")).unwrap();
        let calls = src.matches("sensors::run_all").count();
        assert!(
            calls >= 2,
            "both the single-workspace and federation pipelines must call \
             sensors::run_all (found {calls} call site(s))"
        );
    }

    /// The sidecar half of the overlay-sharing feature must stay
    /// reachable. The owner already served `/overlay/subscribe` and
    /// `/overlay/get_snapshot`, and `overlay::subscribe`,
    /// `GraphDatabase::open_read_only`, `ToolExecutor::new_read_only`
    /// and `LainMcpServer::new_read_only` all existed and were tested —
    /// but no command could start one, so the ingest pipeline's
    /// `is_read_only()` guards defended a mode the binary could not
    /// enter. `lain mcp --owner-url URL` is that entry point.
    #[test]
    fn a_sidecar_can_actually_be_started() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cli = std::fs::read_to_string(root.join("src/cli/mcp.rs")).unwrap();
        assert!(
            cli.contains("pub async fn run_sidecar"),
            "the sidecar entry point must exist"
        );
        for piece in [
            "open_read_only",
            "overlay::subscribe",
            "new_read_only",
        ] {
            assert!(
                cli.contains(piece),
                "run_sidecar must use `{piece}`; without it the read-only \
                 path is dead code again"
            );
        }
        let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(
            main.contains("run_sidecar"),
            "`lain mcp --owner-url` must dispatch to run_sidecar"
        );
    }

    /// The opt-out must treat "0" and "" as *not* disabled. A bare
    /// presence check would turn `LAIN_DISABLE_COMMIT_SYNC=0` — which
    /// reads as "leave it on" — into an opt-out.
    #[test]
    fn zero_and_empty_do_not_count_as_opting_out() {
        assert!(!super::env_disables(None), "unset means enabled");
        assert!(!super::env_disables(Some("")), "empty means enabled");
        assert!(!super::env_disables(Some("0")), "explicit 0 means enabled");
        assert!(super::env_disables(Some("1")));
        assert!(super::env_disables(Some("true")));
    }

    /// A junk or zero interval must fall back to the default rather than
    /// disabling the loop or spinning it with no delay.
    #[test]
    fn a_bad_interval_falls_back_instead_of_spinning() {
        let default = super::commit_sync_interval(None);
        assert!(default > 0);
        assert_eq!(super::commit_sync_interval(Some("not-a-number")), default);
        assert_eq!(super::commit_sync_interval(Some("0")), default);
        assert_eq!(super::commit_sync_interval(Some("-5")), default);
        assert_eq!(super::commit_sync_interval(Some("30")), 30);
    }

    /// Both bootstraps must start the reaper, or the leak comes back
    /// silently — nothing about an unreaped map fails a test on its own.
    #[test]
    fn both_bootstraps_start_the_ui_session_reaper() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for f in ["src/cli/mcp.rs", "src/cli/server.rs"] {
            let src = std::fs::read_to_string(root.join(f)).unwrap();
            assert!(
                src.contains("spawn_ui_session_reaper"),
                "{f} must start the UI session reaper"
            );
        }
    }

    /// The startup half of freshness. `sync_volatile_overlay` seeds the
    /// overlay from uncommitted working-tree changes and had no caller,
    /// so a server started on a dirty checkout showed none of that work
    /// until the user happened to re-save one of those files.
    #[test]
    fn the_single_repo_bootstrap_seeds_the_overlay_at_startup() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = std::fs::read_to_string(root.join("src/cli/mcp.rs")).unwrap();
        assert!(
            src.contains("sync_volatile_overlay"),
            "`lain mcp` must seed the overlay from uncommitted changes at startup"
        );
        let sync_at = src.find("sync_volatile_overlay").unwrap();
        let watch_at = src.find("start_source_watcher").unwrap();
        assert!(
            sync_at < watch_at,
            "the seed must run before the watcher: it clears the overlay first, \
             so the reverse order discards whatever the watcher already inserted"
        );
    }
}
