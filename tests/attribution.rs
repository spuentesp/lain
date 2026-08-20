use lain::server::attribution::{AttributionBackend, AttributionWatcher, NoopBackend, ProcFsBackend};
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
    let events_log = Arc::new(
        lain::server::events_log::EventsLog::open(&tmp.path().join("events")).unwrap(),
    );
    let watcher = AttributionWatcher::new(
        presence.clone(),
        occupancy.clone(),
        tx,
        events_log,
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
    assert!(pid.is_some(), "ProcFsBackend must find the writer pid on Linux");
}

#[test]
fn noop_backend_always_returns_none() {
    let backend = NoopBackend;
    let pid = backend.lookup_writer_pid(std::path::Path::new("/nonexistent"));
    assert_eq!(pid, None);
}