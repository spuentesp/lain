//! Comprehensive stress, hang recovery, high concurrency, payload size,
//! and leak-prevention test suite for Bug #2 libgit2 Sidecar architecture.

use lain::error::LainError;
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::sidecar::SidecarGitSensor;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn init_git_repo_base(path: &Path) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.name", "Sidecar Robustness Tester"]);
    run(&["config", "user.email", "robustness@example.com"]);
    run(&["config", "commit.gpgsign", "false"]);
}

fn init_simple_git_repo(path: &Path) {
    init_git_repo_base(path);

    std::fs::write(path.join(".gitignore"), "target/\n*.tmp\n").unwrap();
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("README.md"), "# Simple Repo\n").unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn hello() -> bool { true }\n").unwrap();

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(&["add", "."]);
    run(&["commit", "-q", "-m", "Initial simple commit"]);
}

fn init_rich_git_repo(path: &Path) {
    init_simple_git_repo(path);

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git {:?} failed", args);
    };

    // Second commit: modify lib.rs and add main.rs
    std::fs::write(
        path.join("src/lib.rs"),
        "pub fn hello() -> bool { true }\npub fn world() -> bool { true }\n",
    )
    .unwrap();
    std::fs::write(
        path.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "Second commit: co-changes"]);

    // Third commit: add extra documentation
    std::fs::write(path.join("CHANGELOG.md"), "# Changelog\n- initial\n").unwrap();
    run(&["add", "CHANGELOG.md"]);
    run(&["commit", "-q", "-m", "Third commit: documentation"]);

    // Add uncommitted changes
    std::fs::write(path.join("README.md"), "# Simple Repo (Modified)\n").unwrap();
    std::fs::write(path.join("staged_file.txt"), "staged\n").unwrap();
    run(&["add", "staged_file.txt"]);
    std::fs::write(path.join("untracked_file.txt"), "untracked\n").unwrap();
    std::fs::create_dir_all(path.join("target")).unwrap();
    std::fs::write(path.join("target/dummy.o"), "ignored build artifact\n").unwrap();
}

fn init_large_git_repo(path: &Path, commit_count: usize, file_count: usize) {
    init_git_repo_base(path);

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git {:?} failed", args);
    };

    // Create a base file
    std::fs::write(path.join("version.txt"), "0\n").unwrap();
    run(&["add", "version.txt"]);
    run(&["commit", "-q", "-m", "Initial commit"]);

    // Create rapid commits
    for i in 1..commit_count {
        std::fs::write(path.join("version.txt"), format!("version {i}\n")).unwrap();
        run(&[
            "commit",
            "-q",
            "-a",
            "-m",
            &format!("Commit #{i}: incremental update"),
        ]);
    }

    // Create many files across subdirectories
    let batch_size = 50;
    for i in 0..file_count {
        let dir_idx = i / batch_size;
        let sub_dir = path.join(format!("pkg_{dir_idx}"));
        if !sub_dir.exists() {
            std::fs::create_dir_all(&sub_dir).unwrap();
        }
        std::fs::write(
            sub_dir.join(format!("module_{i}.rs")),
            format!("pub fn fn_{i}() -> usize {{ {i} }}\n"),
        )
        .unwrap();
    }

    // Create a large file for diff testing (>120 KB)
    let large_file_path = path.join("large_payload.txt");
    let mut large_content = String::with_capacity(150_000);
    for line_idx in 0..2500 {
        large_content.push_str(&format!(
            "Line {:05}: The quick brown fox jumps over the lazy dog repeated text payload block.\n",
            line_idx
        ));
    }
    std::fs::write(&large_file_path, &large_content).unwrap();

    run(&["add", "."]);
    run(&[
        "commit",
        "-q",
        "-m",
        "Batch add bulk files and large payload",
    ]);

    // Introduce a large diff on large_payload.txt
    let mut modified_content = String::with_capacity(160_000);
    for line_idx in 0..2500 {
        if line_idx % 2 == 0 {
            modified_content.push_str(&format!(
                "MODIFIED Line {:05}: Changed content for payload diff test verification.\n",
                line_idx
            ));
        } else {
            modified_content.push_str(&format!(
                "Line {:05}: The quick brown fox jumps over the lazy dog repeated text payload block.\n",
                line_idx
            ));
        }
    }
    std::fs::write(&large_file_path, &modified_content).unwrap();
}

fn init_custom_repo(path: &Path, name: &str, file_name: &str, content: &str) {
    init_git_repo_base(path);
    std::fs::write(path.join(file_name), content).unwrap();

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(&["add", file_name]);
    run(&["commit", "-q", "-m", &format!("Initial commit for {name}")]);
}

// ---------------------------------------------------------------------------
// TEST 1: True Bug #2 Deadlock / Hang Recovery (SIGSTOP Simulation)
// ---------------------------------------------------------------------------
#[test]
#[cfg(unix)]
fn test_sidecar_sigstop_hang_recovery() {
    let tmp = TempDir::new().expect("tempdir");
    init_simple_git_repo(tmp.path());

    // Configure with a short 250ms call timeout
    let call_timeout = Duration::from_millis(250);
    let sensor = SidecarGitSensor::new_with_options(tmp.path(), None, call_timeout)
        .expect("failed to init SidecarGitSensor");

    let initial_health = sensor.health();
    let initial_pid = initial_health
        .child_pid
        .expect("initial child PID should be present");
    assert!(is_pid_alive(initial_pid), "initial child should be running");

    // Freeze child with SIGSTOP (simulating libgit2 wedged in C syscall or deadlock)
    let stop_status = Command::new("kill")
        .arg("-STOP")
        .arg(initial_pid.to_string())
        .status()
        .expect("failed to send SIGSTOP");
    assert!(stop_status.success(), "SIGSTOP should succeed");

    // Call should wait ~250ms for socket read timeout, forcibly kill the wedged child,
    // respawn a fresh daemon, and transparently retry the request to success.
    let start = Instant::now();
    let (commit, ts) = sensor
        .get_latest_commit_info()
        .expect("call should succeed through automatic recovery from wedged child");
    let elapsed = start.elapsed();

    assert!(!commit.is_empty(), "commit sha must not be empty");
    assert!(ts > 0, "timestamp must be positive");
    assert!(
        elapsed >= Duration::from_millis(200),
        "should have waited for timeout; elapsed was {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "recovery took too long ({:?}); should not hang parent",
        elapsed
    );

    // Old frozen child must be killed and reaped
    assert!(
        !is_pid_alive(initial_pid),
        "initial wedged child PID {} must be dead and reaped",
        initial_pid
    );

    // New child must be active with a different PID
    let new_health = sensor.health();
    let new_pid = new_health.child_pid.expect("new child pid");
    assert_ne!(
        initial_pid, new_pid,
        "supervisor must spawn a new child process"
    );
    assert!(is_pid_alive(new_pid), "new child process must be alive");
    assert!(new_health.alive, "sensor must report alive after recovery");

    // Immediate follow-up call should be fast (<50ms)
    let fast_start = Instant::now();
    let tracked = sensor
        .get_all_tracked_files()
        .expect("follow-up call should succeed");
    assert!(!tracked.is_empty());
    assert!(
        fast_start.elapsed() < Duration::from_millis(50),
        "subsequent call should be fast, took {:?}",
        fast_start.elapsed()
    );
}

// ---------------------------------------------------------------------------
// TEST 2: High Concurrency Multi-Threaded Hammer
// ---------------------------------------------------------------------------
#[test]
fn test_sidecar_high_concurrency_hammer() {
    let tmp = TempDir::new().expect("tempdir");
    init_rich_git_repo(tmp.path());

    let sensor =
        Arc::new(SidecarGitSensor::new(tmp.path()).expect("failed to initialize SidecarGitSensor"));

    let num_threads = 24;
    let ops_per_thread = 20;
    let mut handles = Vec::new();

    for thread_idx in 0..num_threads {
        let s = Arc::clone(&sensor);
        handles.push(std::thread::spawn(move || {
            for i in 0..ops_per_thread {
                match (thread_idx + i) % 7 {
                    0 => {
                        let (commit, ts) = s.get_latest_commit_info().unwrap();
                        assert!(!commit.is_empty());
                        assert!(ts > 0);
                    }
                    1 => {
                        let files = s.get_all_tracked_files().unwrap();
                        assert!(files.len() >= 3);
                    }
                    2 => {
                        let uncommitted = s.get_uncommitted_changes().unwrap();
                        assert!(!uncommitted.is_empty());
                    }
                    3 => {
                        let ignored = s.is_ignored(Path::new("target/dummy.o")).unwrap();
                        assert!(ignored);
                    }
                    4 => {
                        let co = s.analyze_co_changes(10, 1, 50).unwrap();
                        let _ = co;
                    }
                    5 => {
                        let history = s.get_commit_history(5).unwrap();
                        assert_eq!(history.len(), 3);
                    }
                    6 => {
                        let branch = s.get_current_branch().unwrap();
                        assert_eq!(branch, "main");
                    }
                    _ => unreachable!(),
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread should not panic");
    }

    let health = sensor.health();
    assert!(health.alive, "sensor should remain alive after hammer");
    assert_eq!(
        health.consecutive_failures, 0,
        "consecutive failures should be 0"
    );
}

// ---------------------------------------------------------------------------
// TEST 3: Large Payload Streaming and Multi-Megabyte Buffers
// ---------------------------------------------------------------------------
#[test]
fn test_sidecar_large_payload_streaming() {
    let tmp = TempDir::new().expect("tempdir");
    // Create 100 commits and 250 tracked files with a large diff
    init_large_git_repo(tmp.path(), 100, 250);

    let sensor = SidecarGitSensor::new(tmp.path()).expect("failed to initialize SidecarGitSensor");

    // 1. Full 101-commit history (>50 KB bincode payload)
    let history = sensor
        .get_commit_history(101)
        .expect("should retrieve 101 commits");
    assert_eq!(history.len(), 101, "expected exactly 101 commits");
    assert!(
        history[0].message.contains("Batch add bulk files"),
        "most recent commit should be the batch add"
    );
    assert_eq!(
        history[100].message, "Initial commit",
        "oldest commit should be Initial commit"
    );

    // 2. Tracked files listing across 250+ files
    let tracked = sensor
        .get_all_tracked_files()
        .expect("should retrieve tracked files");
    assert!(
        tracked.len() >= 251,
        "expected >= 251 tracked files, got {}",
        tracked.len()
    );

    // 3. Diff of a large file (>60 KB diff)
    let diff = sensor
        .get_file_diff(Path::new("large_payload.txt"))
        .expect("should retrieve large file diff");
    assert!(
        diff.len() > 60_000,
        "diff should exceed 60,000 bytes, got {} bytes",
        diff.len()
    );

    // 4. Co-changes calculation over 100 commits
    let co_changes = sensor
        .analyze_co_changes(100, 1, 500)
        .expect("should analyze co changes");
    let _ = co_changes;

    let health = sensor.health();
    assert!(health.alive);
    assert_eq!(health.consecutive_failures, 0);
}

// ---------------------------------------------------------------------------
// TEST 4: Zero Daemon and Unix Socket Leaks
// ---------------------------------------------------------------------------
#[test]
#[cfg(unix)]
fn test_sidecar_zero_process_and_socket_leaks() {
    let tmp = TempDir::new().expect("tempdir");
    init_simple_git_repo(tmp.path());

    let iterations = 8;
    let mut tracked_pids = Vec::new();
    let mut tracked_sockets = Vec::new();

    for _ in 0..iterations {
        let sensor =
            SidecarGitSensor::new(tmp.path()).expect("failed to initialize SidecarGitSensor");
        let pid = sensor.health().child_pid.expect("child pid");
        let sock = sensor.socket_path();

        assert!(is_pid_alive(pid), "child PID {} must be running", pid);
        assert!(sock.exists(), "socket file {:?} must exist", sock);

        tracked_pids.push(pid);
        tracked_sockets.push(sock.clone());

        // Perform a quick query
        let (commit, _) = sensor.get_latest_commit_info().unwrap();
        assert!(!commit.is_empty());

        // Explicitly drop sensor
        drop(sensor);

        // Immediate assertion: child must be reaped and socket unlinked
        assert!(
            !is_pid_alive(pid),
            "child PID {} must be dead immediately after drop",
            pid
        );
        assert!(
            !sock.exists(),
            "socket {:?} must be unlinked immediately after drop",
            sock
        );
    }

    // Double check: none of the spawned PIDs or sockets remain
    for pid in tracked_pids {
        assert!(!is_pid_alive(pid), "PID {} should not be alive", pid);
    }
    for sock in tracked_sockets {
        assert!(!sock.exists(), "socket {:?} should not exist", sock);
    }
}

// ---------------------------------------------------------------------------
// TEST 5: Multi-Repo Federation Concurrency and Daemon Isolation
// ---------------------------------------------------------------------------
#[test]
fn test_sidecar_multi_repo_federation_isolation() {
    let tmp_a = TempDir::new().expect("repo a");
    let tmp_b = TempDir::new().expect("repo b");
    let tmp_c = TempDir::new().expect("repo c");

    init_custom_repo(
        tmp_a.path(),
        "repo_alpha",
        "alpha_source.txt",
        "content for repo alpha",
    );
    init_custom_repo(
        tmp_b.path(),
        "repo_beta",
        "beta_source.txt",
        "content for repo beta",
    );
    init_custom_repo(
        tmp_c.path(),
        "repo_gamma",
        "gamma_source.txt",
        "content for repo gamma",
    );

    let sensor_a = Arc::new(SidecarGitSensor::new(tmp_a.path()).unwrap());
    let sensor_b = Arc::new(SidecarGitSensor::new(tmp_b.path()).unwrap());
    let sensor_c = Arc::new(SidecarGitSensor::new(tmp_c.path()).unwrap());

    let (commit_a, _) = sensor_a.get_latest_commit_info().unwrap();
    let (commit_b, _) = sensor_b.get_latest_commit_info().unwrap();
    let (commit_c, _) = sensor_c.get_latest_commit_info().unwrap();

    // Verify all commit SHAs are distinct
    assert_ne!(commit_a, commit_b);
    assert_ne!(commit_b, commit_c);
    assert_ne!(commit_a, commit_c);

    // Concurrently hammer all 3 repos across 12 threads
    let mut handles = Vec::new();
    for thread_idx in 0..12 {
        let (sensor, expected_file, expected_commit) = match thread_idx % 3 {
            0 => (Arc::clone(&sensor_a), "alpha_source.txt", commit_a.clone()),
            1 => (Arc::clone(&sensor_b), "beta_source.txt", commit_b.clone()),
            2 => (Arc::clone(&sensor_c), "gamma_source.txt", commit_c.clone()),
            _ => unreachable!(),
        };

        handles.push(std::thread::spawn(move || {
            for _ in 0..25 {
                let (commit, _) = sensor.get_latest_commit_info().unwrap();
                assert_eq!(commit, expected_commit, "isolated commit mismatch");

                let tracked = sensor.get_all_tracked_files().unwrap();
                assert!(
                    tracked.iter().any(|p| p.ends_with(expected_file)),
                    "file {} not found in repo tracked files",
                    expected_file
                );
            }
        }));
    }

    for h in handles {
        h.join().expect("thread should finish without panic");
    }

    assert!(sensor_a.health().alive);
    assert!(sensor_b.health().alive);
    assert!(sensor_c.health().alive);
}

// ---------------------------------------------------------------------------
// TEST 6: Fast Preflight Failure on Invalid / Missing Repositories
// ---------------------------------------------------------------------------
#[test]
fn test_sidecar_preflight_fails_fast_on_invalid_repo() {
    let tmp = TempDir::new().expect("tempdir");
    // 1. Directory exists but is not a Git repository
    let res = SidecarGitSensor::new(tmp.path());
    assert!(
        res.is_err(),
        "SidecarGitSensor should reject non-git directories fast"
    );
    match res {
        Err(LainError::Git(msg)) => {
            assert!(
                msg.contains("repository") || msg.contains("could not find"),
                "expected git repo error: {msg}"
            );
        }
        other => panic!("expected LainError::Git, got {other:?}"),
    }

    // 2. Directory does not exist
    let non_existent = tmp.path().join("definitely_not_existing_dir");
    let res_non = SidecarGitSensor::new(&non_existent);
    assert!(
        res_non.is_err(),
        "SidecarGitSensor should reject non-existent paths fast"
    );
}

// ---------------------------------------------------------------------------
// TEST 7: Concurrency with Mid-Flight Child Crashes (Chaos Stress)
// ---------------------------------------------------------------------------
#[test]
#[cfg(unix)]
fn test_sidecar_concurrency_with_intermittent_child_crashes() {
    let tmp = TempDir::new().expect("tempdir");
    init_rich_git_repo(tmp.path());

    let sensor = Arc::new(SidecarGitSensor::new(tmp.path()).unwrap());

    // Spawn 8 worker threads continuously making queries
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let s = Arc::clone(&sensor);
            std::thread::spawn(move || {
                let mut completed = 0;
                for _ in 0..20 {
                    if let Ok((commit, _)) = s.get_latest_commit_info() {
                        if !commit.is_empty() {
                            completed += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                completed
            })
        })
        .collect();

    // Kill the child mid-run (within budget)
    std::thread::sleep(Duration::from_millis(40));
    if let Some(pid) = sensor.health().child_pid {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }

    let mut total_completed = 0;
    for w in workers {
        total_completed += w.join().unwrap();
    }

    // Almost all calls should succeed due to transparent single-call retry upon disconnect
    assert!(
        total_completed >= 120,
        "workers should transparently recover from child crash; completed: {}",
        total_completed
    );

    // A follow-up query after child crashes triggers transparent recovery
    let (commit, _) = sensor
        .get_latest_commit_info()
        .expect("should recover after child crash");
    assert!(!commit.is_empty());

    let health = sensor.health();
    assert!(
        health.alive,
        "sensor should be alive after recovery, got: {health:?}"
    );
}

// ---------------------------------------------------------------------------
// TEST 8: AnyGitSensor Polymorphic Wrapper Concurrency
// ---------------------------------------------------------------------------
#[test]
fn test_any_git_sensor_polymorphic_concurrency() {
    let tmp = TempDir::new().expect("tempdir");
    init_rich_git_repo(tmp.path());

    let sensor = Arc::new(
        AnyGitSensor::new(tmp.path(), GitSensorMode::Sidecar)
            .expect("failed to create AnyGitSensor in Sidecar mode"),
    );

    assert_eq!(sensor.mode(), GitSensorMode::Sidecar);

    let mut handles = Vec::new();
    for thread_idx in 0..12 {
        let s = Arc::clone(&sensor);
        handles.push(std::thread::spawn(move || {
            for i in 0..15 {
                if (thread_idx + i) % 2 == 0 {
                    let (commit, ts) = s.get_latest_commit_info().unwrap();
                    assert!(!commit.is_empty());
                    assert!(ts > 0);
                } else {
                    let files = s.get_all_tracked_files().unwrap();
                    assert!(files.len() >= 3);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread should not panic");
    }

    let health = sensor.sidecar_health().expect("health should be present");
    assert!(health.alive);
    assert_eq!(health.consecutive_failures, 0);
}
