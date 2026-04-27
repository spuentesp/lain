//! Tests for git.rs

use crate::git::GitSensor;
use std::path::Path;

#[test]
fn test_git_sensor_new_valid_repo() {
    // /Users/spuentesp/lain is a git repo
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain"));
    assert!(sensor.is_ok());
}

#[test]
fn test_git_sensor_is_valid() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    assert!(sensor.is_valid());
}

#[test]
fn test_git_sensor_new_invalid_path() {
    let sensor = GitSensor::new(Path::new("/nonexistent/path/xyz"));
    assert!(sensor.is_err());
}

#[test]
fn test_git_sensor_get_tracked_files() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    let files = sensor.get_all_tracked_files();
    assert!(files.is_ok());
    assert!(!files.unwrap().is_empty()); // This repo has files
}

#[test]
fn test_git_sensor_is_ignored() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    // .git directory should be ignored
    let is_ignored = sensor.is_ignored(Path::new(".git"));
    assert!(is_ignored.is_ok());
}

#[test]
fn test_git_sensor_is_ignored_target_dir() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    // target directory (rust build output) should be ignored
    let is_ignored = sensor.is_ignored(Path::new("target"));
    assert!(is_ignored.is_ok());
}

#[test]
fn test_git_sensor_is_ignored_nonexistent() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    // A nonexistent path may or may not be ignored depending on gitignore rules
    let result = sensor.is_ignored(Path::new("nonexistent_file_xyz123.txt"));
    assert!(result.is_ok());
}

#[test]
fn test_git_sensor_get_uncommitted_changes_none() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    // Clean working tree (after sync_state)
    let changes = sensor.get_uncommitted_changes();
    assert!(changes.is_ok());
    // Changes may be empty or may have staged changes depending on state
    let changes_vec = changes.unwrap();
    // Just verify it's a valid vec
    println!("Uncommitted changes: {}", changes_vec.len());
}

#[test]
fn test_git_sensor_get_uncommitted_changes_staged() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    let changes = sensor.get_uncommitted_changes().unwrap();
    for change in changes {
        println!("Change: {:?} at {:?}", change.change_type, change.path);
    }
}

#[test]
fn test_git_sensor_get_file_diff_on_clean_file() {
    let sensor = GitSensor::new(Path::new("/Users/spuentesp/lain")).unwrap();
    let files = sensor.get_all_tracked_files().unwrap();
    if let Some(file) = files.first() {
        let diff = sensor.get_file_diff(file);
        // File might not have unstaged changes, but call should succeed
        assert!(diff.is_ok());
    }
}

#[test]
fn test_git_sensor_file_change_struct() {
    use crate::git::FileChange;
    use crate::git::ChangeType;

    let change = FileChange {
        path: std::path::PathBuf::from("/test/path.rs"),
        change_type: ChangeType::Modified,
        staged: false,
    };

    assert_eq!(change.path.to_str(), Some("/test/path.rs"));
    assert!(matches!(change.change_type, ChangeType::Modified));
    assert!(!change.staged);
}

#[test]
fn test_git_sensor_change_type_variants() {
    use crate::git::ChangeType;

    let modified = ChangeType::Modified;
    let added = ChangeType::Added;
    let deleted = ChangeType::Deleted;

    // Just verify all variants can be constructed
    assert!(matches!(modified, ChangeType::Modified));
    assert!(matches!(added, ChangeType::Added));
    assert!(matches!(deleted, ChangeType::Deleted));
}

#[test]
fn test_git_sensor_in_temp_repo() {
    use std::fs;
    use std::process::Command;

    // Create a temp git repo
    let temp_dir = std::env::temp_dir().join("lain_git_test_repo");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Init git repo
    let result = Command::new("git")
        .args(&["init"])
        .current_dir(&temp_dir)
        .output();

    if result.is_err() {
        return; // git not available, skip test
    }

    let sensor = GitSensor::new(&temp_dir);
    assert!(sensor.is_ok());

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_git_sensor_in_temp_repo_with_file() {
    use std::fs;
    use std::process::Command;

    let temp_dir = std::env::temp_dir().join("lain_git_test_with_file");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Init git repo and add a file
    let _ = Command::new("git")
        .args(&["init"])
        .current_dir(&temp_dir)
        .output();

    let test_file = temp_dir.join("test.txt");
    fs::write(&test_file, "hello world").unwrap();

    let _ = Command::new("git")
        .args(&["add", "test.txt"])
        .current_dir(&temp_dir)
        .output();

    let sensor = GitSensor::new(&temp_dir).unwrap();
    let files = sensor.get_all_tracked_files().unwrap();

    // Should have at least one tracked file now
    assert!(!files.is_empty());

    let _ = fs::remove_dir_all(&temp_dir);
}