//! Tests for tools/handlers/gitops.rs

use crate::tools::handlers::gitops::{get_file_diff, get_commit_history, get_branch_status};
use crate::git::GitSensor;
use std::sync::Arc;
use parking_lot::Mutex;

#[test]
fn test_get_file_diff_no_changes() {
    let repo_root = std::env::current_dir().unwrap();
    let git = Arc::new(Mutex::new(GitSensor::new(&repo_root).unwrap()));

    let result = get_file_diff(&git, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // May be empty or say no changes
    assert!(text.contains("No uncommitted") || text.contains("changed"));
}

#[test]
fn test_get_file_diff_with_filter() {
    let repo_root = std::env::current_dir().unwrap();
    let git = Arc::new(Mutex::new(GitSensor::new(&repo_root).unwrap()));

    let result = get_file_diff(&git, Some("src"));
    assert!(result.is_ok());
    // Result should either show changes filtered by "src" or say no changes
    let text = result.unwrap();
    assert!(!text.contains("✨ Added") || text.contains("src") || text.contains("No uncommitted"));
}

#[test]
fn test_get_commit_history_basic() {
    let repo_root = std::env::current_dir().unwrap();
    let git = Arc::new(Mutex::new(GitSensor::new(&repo_root).unwrap()));

    let result = get_commit_history(&git, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Commit History"));
}

#[test]
fn test_get_commit_history_with_limit() {
    let repo_root = std::env::current_dir().unwrap();
    let git = Arc::new(Mutex::new(GitSensor::new(&repo_root).unwrap()));

    let result = get_commit_history(&git, Some(5));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Commit History"));
    // Should have commit hashes
    assert!(text.contains("**") && text.contains(" ago"));
}

#[test]
fn test_get_branch_status() {
    let repo_root = std::env::current_dir().unwrap();
    let git = Arc::new(Mutex::new(GitSensor::new(&repo_root).unwrap()));

    let result = get_branch_status(&git);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Git Branch Status"));
    assert!(text.contains("Branch:"));
    assert!(text.contains("Status:"));
}