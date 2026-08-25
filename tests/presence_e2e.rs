use lain::server::presence::{AgentKind, AgentMode, OccupancyMap, PresenceRegistry};
use lain::server::attribution::AttributionWatcher;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn e2e_attribution_via_real_child_pid() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let tmp = tempdir().unwrap();
    let target = tmp.path().join("auth.rs");
    std::fs::write(&target, "fn login() {}").unwrap();

    // Spawn a child that sleeps 500ms then writes to the file.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 0.3 && echo 'fn login() {{ changed }}' > {}", target.display()))
        .spawn()
        .expect("spawn");

    let pid = child.id();
    let presence = Arc::new(PresenceRegistry::new());
    let occupancy = Arc::new(OccupancyMap::new());
    let _ = presence.register("e2e-child".into(), AgentKind::Other("e2e".into()), AgentMode::Interactive, Some(pid), None);
    let (tx, _rx) = tokio::sync::broadcast::channel(8);
    let events_log = Arc::new(
        lain::server::events_log::EventsLog::open(&tmp.path().join("events")).unwrap(),
    );
    let watcher = AttributionWatcher::new(
        presence.clone(),
        occupancy.clone(),
        tx,
        events_log,
        vec![tmp.path().to_path_buf()],
    );
    let _h = watcher.start();

    // Wait for the child to write.
    std::thread::sleep(Duration::from_millis(800));
    let _ = child.wait();

    // The agent should now have an auto-claim on auth.rs.
    let sessions = presence.list_active(true);
    assert_eq!(sessions.len(), 1);
    let claims = occupancy.list_for_agent(&sessions[0].id);
    assert!(!claims.is_empty(), "expected auto-claim after child write; got: {claims:?}");
}