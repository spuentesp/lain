use lain::server::attribution::{
    AttributionBackend, AttributionWatcher, NoopBackend, ProcFsBackend,
};
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
    let events_log =
        Arc::new(lain::server::events_log::EventsLog::open(&tmp.path().join("events")).unwrap());
    let watcher = AttributionWatcher::new(
        presence.clone(),
        occupancy.clone(),
        tx,
        events_log,
        vec![tmp.path().to_path_buf()],
    );
    let _h = watcher.start();

    // A fixed pre-sleep guessing when the inotify watcher thread has
    // registered its watch (inotify drops writes that happen before the
    // watch is armed) plus a fixed post-sleep guessing when the event
    // has been processed both flaked under CI scheduling jitter
    // (confirmed live 2026-09-17). `AttributionWatcher` exposes no
    // readiness signal to poll instead, but re-writing the file is
    // itself observable once the watch *is* armed -- any write after
    // that point produces an event. Loop write+poll instead of a single
    // shot: each iteration both re-arms the "watch might not be ready
    // yet" case and gives more time for "event not processed yet".
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut claims = Vec::new();
    let mut attempt = 0u32;
    while std::time::Instant::now() < deadline {
        attempt += 1;
        std::fs::write(&file, format!("fn login() {{ changed {attempt} }}")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        claims = occupancy.list_for_agent(&s.id);
        if !claims.is_empty() {
            break;
        }
    }

    // The agent should now have an auto-claim on auth.rs.
    assert!(
        !claims.is_empty(),
        "expected auto-claim after {attempt} write attempt(s), got: {claims:?}"
    );
}

#[test]
fn attribution_backend_trait_returns_writer_pid() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let tmp = tempdir().unwrap();
    let file = tmp.path().join("auth.rs");
    std::fs::write(&file, "fn login() {}").unwrap();

    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            // Open the file on FD 3 (write-only) and *keep it open* for
            // a full second so the ProcFsBackend can find the writer
            // pid while the FD is still alive. A bare
            // `echo changed > file` would close the FD immediately
            // after the write and the procfs walk would race (or miss
            // it entirely).
            "exec 3>{} && echo changed >&3 && sleep 1",
            file.display()
        ))
        .spawn()
        .unwrap();

    let backend = ProcFsBackend;
    // Give the child a moment to open the FD and write.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let pid = backend.lookup_writer_pid(&file);
    let _ = child.wait();
    assert!(
        pid.is_some(),
        "ProcFsBackend must find the writer pid on Linux"
    );
}

#[test]
fn noop_backend_always_returns_none() {
    let backend = NoopBackend;
    let pid = backend.lookup_writer_pid(std::path::Path::new("/nonexistent"));
    assert_eq!(pid, None);
}
