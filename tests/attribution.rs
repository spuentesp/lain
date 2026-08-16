use lain::server::attribution::AttributionWatcher;
use lain::server::presence::{AgentKind, AgentMode, OccupancyMap, PresenceRegistry};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn attribution_auto_claims_via_pid_on_linux() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let tmp = tempdir().unwrap();
    let file = tmp.path().join("auth.rs");
    std::fs::write(&file, "fn login() {}").unwrap();

    let presence = Arc::new(PresenceRegistry::new());
    let occupancy = Arc::new(OccupancyMap::new());
    let s = presence.register(
        "test-agent".into(),
        AgentKind::ClaudeCode,
        AgentMode::Interactive,
        Some(std::process::id()),
        None,
    );

    let (tx, _rx) = tokio::sync::broadcast::channel(8);
    let watcher = AttributionWatcher::new(
        presence.clone(),
        occupancy.clone(),
        tx,
        tmp.path().to_path_buf(),
    );
    let _h = watcher.start();

    // Give the inotify watcher thread a moment to register its watch
    // before we touch the file. inotify only reports events that happen
    // after the watch is set up; a write before the watch is ready
    // is silently dropped by the kernel.
    std::thread::sleep(Duration::from_millis(100));

    // Touch the file from THIS process — the registered PID should match.
    std::fs::write(&file, "fn login() { changed }").unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // The agent should now have an auto-claim on auth.rs.
    let claims = occupancy.list_for_agent(&s.id);
    assert!(!claims.is_empty(), "expected auto-claim, got: {claims:?}");
}