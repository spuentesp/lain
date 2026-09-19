//! Integration tests verifying identical behavior and outputs between
//! AnyGitSensor::InProcess and AnyGitSensor::Sidecar across all Git queries.

use lain::git::{AnyGitSensor, GitSensorMode};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn init_test_git_repo(path: &Path) {
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

    run(&["init"]);
    run(&["config", "user.name", "Test Runner"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "commit.gpgsign", "false"]);

    // Create .gitignore
    std::fs::write(path.join(".gitignore"), "target/\n*.tmp\nignored_dir/\n").unwrap();
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("README.md"), "# Test Repo\n").unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

    run(&["add", "."]);
    run(&["commit", "-m", "Initial commit"]);

    // Second commit: modify lib.rs and add main.rs
    std::fs::write(
        path.join("src/lib.rs"),
        "pub fn hello() {}\npub fn world() {}\n",
    )
    .unwrap();
    std::fs::write(path.join("src/main.rs"), "fn main() {}\n").unwrap();

    run(&["add", "."]);
    run(&["commit", "-m", "Second commit: co-changes"]);

    // Introduce uncommitted changes:
    // 1. Modified tracked file (unstaged)
    std::fs::write(path.join("README.md"), "# Test Repo\nModified content\n").unwrap();

    // 2. Added staged file
    std::fs::write(path.join("staged.txt"), "staged content\n").unwrap();
    run(&["add", "staged.txt"]);

    // 3. Untracked file
    std::fs::write(path.join("untracked.txt"), "untracked content\n").unwrap();

    // 4. Ignored files
    std::fs::create_dir_all(path.join("ignored_dir")).unwrap();
    std::fs::write(path.join("ignored_dir/secret.tmp"), "ignored\n").unwrap();
    std::fs::write(path.join("test.tmp"), "ignored temp\n").unwrap();
}

#[test]
fn test_git_sensor_mode_config_and_parsing() {
    assert_eq!(
        "in_process".parse::<GitSensorMode>().unwrap(),
        GitSensorMode::InProcess
    );
    assert_eq!(
        "inprocess".parse::<GitSensorMode>().unwrap(),
        GitSensorMode::InProcess
    );
    assert_eq!(
        "in-process".parse::<GitSensorMode>().unwrap(),
        GitSensorMode::InProcess
    );
    assert_eq!(
        "sidecar".parse::<GitSensorMode>().unwrap(),
        GitSensorMode::Sidecar
    );
    assert_eq!(
        "SIDECAR".parse::<GitSensorMode>().unwrap(),
        GitSensorMode::Sidecar
    );
    assert!("invalid".parse::<GitSensorMode>().is_err());

    assert_eq!(GitSensorMode::InProcess.to_string(), "in_process");
    assert_eq!(GitSensorMode::Sidecar.to_string(), "sidecar");

    // Serde roundtrip
    let json_in_proc = serde_json::to_string(&GitSensorMode::InProcess).unwrap();
    assert_eq!(json_in_proc, "\"in_process\"");
    let decoded_in_proc: GitSensorMode = serde_json::from_str(&json_in_proc).unwrap();
    assert_eq!(decoded_in_proc, GitSensorMode::InProcess);

    let json_sidecar = serde_json::to_string(&GitSensorMode::Sidecar).unwrap();
    assert_eq!(json_sidecar, "\"sidecar\"");
    let decoded_sidecar: GitSensorMode = serde_json::from_str(&json_sidecar).unwrap();
    assert_eq!(decoded_sidecar, GitSensorMode::Sidecar);
}

#[test]
fn test_any_git_sensor_parity_on_repo() {
    if let Ok(bin) = std::env::var("CARGO_BIN_EXE_lain-git-sidecar") {
        std::env::set_var("LAIN_GIT_SIDECAR_BIN", bin);
    }

    let temp_dir = TempDir::new().expect("tempdir");
    let repo_path = temp_dir.path();
    init_test_git_repo(repo_path);

    let in_proc = AnyGitSensor::new(repo_path, GitSensorMode::InProcess)
        .expect("failed to create InProcess AnyGitSensor");
    let sidecar = AnyGitSensor::new(repo_path, GitSensorMode::Sidecar)
        .expect("failed to create Sidecar AnyGitSensor");

    // 1. Validity
    assert!(in_proc.is_valid());
    assert!(sidecar.is_valid());

    // 2. Mode and health inspection
    assert_eq!(in_proc.mode(), GitSensorMode::InProcess);
    assert!(in_proc.is_in_process());
    assert!(!in_proc.is_sidecar());
    assert!(in_proc.sidecar_health().is_none());

    assert_eq!(sidecar.mode(), GitSensorMode::Sidecar);
    assert!(sidecar.is_sidecar());
    assert!(!sidecar.is_in_process());
    let health = sidecar.sidecar_health().expect("sidecar health present");
    assert!(health.alive);
    assert!(health.child_pid.is_some());

    // 3. Commit info parity
    let (in_commit, in_time) = in_proc.get_latest_commit_info().unwrap();
    let (sc_commit, sc_time) = sidecar.get_latest_commit_info().unwrap();
    assert_eq!(in_commit, sc_commit);
    assert_eq!(in_time, sc_time);

    let in_latest = in_proc.get_latest_commit().unwrap();
    let sc_latest = sidecar.get_latest_commit().unwrap();
    assert_eq!(in_latest, sc_latest);

    // 4. Branch parity
    let in_branch = in_proc.get_current_branch().unwrap();
    let sc_branch = sidecar.get_current_branch().unwrap();
    assert_eq!(in_branch, sc_branch);

    // 5. Tracked files parity (sorted for strict comparison)
    let mut in_tracked = in_proc.get_all_tracked_files().unwrap();
    let mut sc_tracked = sidecar.get_all_tracked_files().unwrap();
    in_tracked.sort();
    sc_tracked.sort();
    assert_eq!(in_tracked, sc_tracked);
    assert!(in_tracked.iter().any(|p| p.ends_with("README.md")));
    assert!(in_tracked.iter().any(|p| p.ends_with("src/lib.rs")));
    assert!(in_tracked.iter().any(|p| p.ends_with("src/main.rs")));
    assert!(!in_tracked
        .iter()
        .any(|p| p.ends_with("ignored_dir/secret.tmp")));

    // 6. Gitignore query parity
    assert_eq!(
        in_proc
            .is_ignored(Path::new("ignored_dir/secret.tmp"))
            .unwrap(),
        sidecar
            .is_ignored(Path::new("ignored_dir/secret.tmp"))
            .unwrap()
    );
    assert!(sidecar
        .is_ignored(Path::new("ignored_dir/secret.tmp"))
        .unwrap());

    assert_eq!(
        in_proc.is_ignored(Path::new("test.tmp")).unwrap(),
        sidecar.is_ignored(Path::new("test.tmp")).unwrap()
    );
    assert!(sidecar.is_ignored(Path::new("test.tmp")).unwrap());

    assert_eq!(
        in_proc.is_ignored(Path::new("src/lib.rs")).unwrap(),
        sidecar.is_ignored(Path::new("src/lib.rs")).unwrap()
    );
    assert!(!sidecar.is_ignored(Path::new("src/lib.rs")).unwrap());

    // 7. Uncommitted changes parity
    let mut in_uncommitted = in_proc.get_uncommitted_changes().unwrap();
    let mut sc_uncommitted = sidecar.get_uncommitted_changes().unwrap();
    in_uncommitted.sort_by(|a, b| a.path.cmp(&b.path));
    sc_uncommitted.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(in_uncommitted.len(), sc_uncommitted.len());
    for (i, (in_ch, sc_ch)) in in_uncommitted.iter().zip(sc_uncommitted.iter()).enumerate() {
        assert_eq!(in_ch.path, sc_ch.path, "mismatch on path at index {i}");
        assert_eq!(
            in_ch.change_type, sc_ch.change_type,
            "mismatch on type at index {i}"
        );
        assert_eq!(
            in_ch.staged, sc_ch.staged,
            "mismatch on staged at index {i}"
        );
    }

    // Verify expected uncommitted files are present
    assert!(sc_uncommitted.iter().any(|c| c.path.ends_with("README.md")));
    assert!(sc_uncommitted
        .iter()
        .any(|c| c.path.ends_with("staged.txt")));
    assert!(sc_uncommitted
        .iter()
        .any(|c| c.path.ends_with("untracked.txt")));

    // 8. Co-changes analysis parity
    let mut in_co = in_proc.analyze_co_changes(10, 1, 10).unwrap();
    let mut sc_co = sidecar.analyze_co_changes(10, 1, 10).unwrap();
    in_co.sort_by(|a, b| {
        b.co_change_count
            .cmp(&a.co_change_count)
            .then_with(|| (&a.file1, &a.file2).cmp(&(&b.file1, &b.file2)))
    });
    sc_co.sort_by(|a, b| {
        b.co_change_count
            .cmp(&a.co_change_count)
            .then_with(|| (&a.file1, &a.file2).cmp(&(&b.file1, &b.file2)))
    });
    assert_eq!(in_co.len(), sc_co.len());
    for (in_pair, sc_pair) in in_co.iter().zip(sc_co.iter()) {
        assert_eq!(in_pair.file1, sc_pair.file1);
        assert_eq!(in_pair.file2, sc_pair.file2);
        assert_eq!(in_pair.co_change_count, sc_pair.co_change_count);
    }

    // 9. File diff parity
    let in_diff = in_proc.get_file_diff(Path::new("README.md")).unwrap();
    let sc_diff = sidecar.get_file_diff(Path::new("README.md")).unwrap();
    assert_eq!(in_diff, sc_diff);
    assert!(sc_diff.contains("+Modified content"));

    // 10. Commit history parity
    let in_history = in_proc.get_commit_history(5).unwrap();
    let sc_history = sidecar.get_commit_history(5).unwrap();
    assert_eq!(in_history.len(), sc_history.len());
    for (in_c, sc_c) in in_history.iter().zip(sc_history.iter()) {
        assert_eq!(in_c.id, sc_c.id);
        assert_eq!(in_c.message, sc_c.message);
        assert_eq!(in_c.time, sc_c.time);
    }

    // 11. Changed files since initial commit
    let initial_commit_id = &in_history.last().unwrap().id;
    let mut in_changed = in_proc.get_changed_files_since(initial_commit_id).unwrap();
    let mut sc_changed = sidecar.get_changed_files_since(initial_commit_id).unwrap();
    in_changed.sort();
    sc_changed.sort();
    assert_eq!(in_changed, sc_changed);
    assert!(sc_changed.iter().any(|p| p.ends_with("src/main.rs")));
}
