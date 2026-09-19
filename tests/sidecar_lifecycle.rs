//! Integration tests for SidecarGitSensor lifecycle management,
//! transparent auto-respawn, respawn budget enforcement, and health monitoring.

use lain::error::LainError;
use lain::sidecar::SidecarGitSensor;
use std::path::Path;
use std::process::Command;

#[test]
fn test_sidecar_normal_operations() {
    let repo_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sensor = SidecarGitSensor::new(repo_path).expect("failed to initialize SidecarGitSensor");

    assert!(sensor.is_valid(), "sensor should be valid for current repo");

    let (commit, timestamp) = sensor
        .get_latest_commit_info()
        .expect("failed to get latest commit info");
    assert!(!commit.is_empty(), "commit sha should not be empty");
    assert!(timestamp > 0, "timestamp should be positive");

    let tracked = sensor
        .get_all_tracked_files()
        .expect("failed to get tracked files");
    assert!(
        !tracked.is_empty(),
        "tracked files list should not be empty"
    );
    assert!(
        tracked.iter().any(|p| p.ends_with("Cargo.toml")),
        "tracked files should contain Cargo.toml"
    );

    let uncommitted = sensor
        .get_uncommitted_changes()
        .expect("failed to get uncommitted changes");
    let _ = uncommitted;

    let target_ignored = sensor
        .is_ignored(Path::new("target"))
        .expect("failed to check is_ignored");
    assert!(
        target_ignored,
        "target directory should be ignored by gitignore"
    );

    let health = sensor.health();
    assert!(health.alive, "sidecar should report alive");
    assert!(health.child_pid.is_some(), "child_pid should be present");
    assert_eq!(
        health.consecutive_failures, 0,
        "consecutive failures should be 0"
    );
}

#[test]
fn test_sidecar_auto_recovery_on_kill() {
    let repo_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sensor = SidecarGitSensor::new(repo_path).expect("failed to initialize SidecarGitSensor");

    let initial_health = sensor.health();
    let initial_pid = initial_health
        .child_pid
        .expect("initial child pid should be present");

    // Forcibly terminate the child process with SIGKILL (simulating a crash / OOM / stuck process)
    let status = Command::new("kill")
        .arg("-9")
        .arg(initial_pid.to_string())
        .status()
        .expect("failed to run kill command");
    assert!(status.success(), "kill command should succeed");

    // The next call must transparently detect the dead child, respawn a replacement, and succeed
    let (commit, timestamp) = sensor
        .get_latest_commit_info()
        .expect("call should succeed through automatic recovery");
    assert!(!commit.is_empty());
    assert!(timestamp > 0);

    let new_health = sensor.health();
    let new_pid = new_health
        .child_pid
        .expect("new child pid should be present");
    assert_ne!(
        initial_pid, new_pid,
        "sidecar should have spawned a new child process with a different PID"
    );
    assert!(new_health.alive, "sidecar should be alive after recovery");
}

#[test]
fn test_sidecar_respawn_budget_exhaustion() {
    let repo_path = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sensor = SidecarGitSensor::new(repo_path).expect("failed to initialize SidecarGitSensor");

    // Repeatedly kill the child process in rapid succession to exhaust the 3-in-30s budget
    for i in 0..4 {
        let health = sensor.health();
        if let Some(pid) = health.child_pid {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }

        let res = sensor.get_latest_commit_info();
        if i >= 3 {
            // By the 4th consecutive kill within the window, the budget must be exceeded
            match res {
                Err(LainError::Unavailable(msg)) => {
                    assert!(
                        msg.contains("respawn budget exceeded"),
                        "error message should indicate budget exhaustion: {msg}"
                    );
                    break;
                }
                other => {
                    panic!("expected LainError::Unavailable on budget exhaustion, got {other:?}")
                }
            }
        }
    }
}
