//! Tests for tools/handlers/gitops.rs

use crate::server::git::AnyGitSensor;
use crate::server::tools::handlers::gitops::{
    get_branch_status, get_commit_history, get_file_diff,
};
use std::path::Path;
use std::sync::Arc;

/// A tempdir git repo with one committed file. Every test here needs a
/// git-backed workspace and cargo-mutants runs tests in a copied scratch
/// tree with no `.git`, so each test builds its own fixture.
fn seeded_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = git2::Repository::init(&root).unwrap();
    std::fs::write(root.join("keep.txt"), "a\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("keep.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    (dir, root)
}

#[test]
fn test_get_file_diff_no_changes() {
    let (_tmp, root) = seeded_repo();
    let git = Arc::new(AnyGitSensor::from_env(&root).unwrap());

    let result = get_file_diff(&git, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    // May be empty or say no changes
    assert!(text.contains("No uncommitted") || text.contains("changed"));
}

#[test]
fn test_get_file_diff_with_filter() {
    let (_tmp, root) = seeded_repo();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("wip.rs"), "fn wip() {}\n").unwrap();
    let git = Arc::new(AnyGitSensor::from_env(&root).unwrap());

    let result = get_file_diff(&git, Some("src"));
    assert!(result.is_ok());
    // Result should either show changes filtered by "src" or say no changes
    let text = result.unwrap();
    assert!(!text.contains("✨ Added") || text.contains("src") || text.contains("No uncommitted"));
}

#[test]
fn test_get_commit_history_basic() {
    let (_tmp, root) = seeded_repo();
    let git = Arc::new(AnyGitSensor::from_env(&root).unwrap());

    let result = get_commit_history(&git, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Commit History"));
}

#[test]
fn test_get_commit_history_with_limit() {
    let (_tmp, root) = seeded_repo();
    let git = Arc::new(AnyGitSensor::from_env(&root).unwrap());

    let result = get_commit_history(&git, Some(5));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Commit History"));
    // Should have commit hashes
    assert!(text.contains("**") && text.contains(" ago"));
}

#[test]
fn test_get_branch_status() {
    let (_tmp, root) = seeded_repo();
    let git = Arc::new(AnyGitSensor::from_env(&root).unwrap());

    let result = get_branch_status(&git);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("## Git Branch Status"));
    assert!(text.contains("Branch:"));
    assert!(text.contains("Status:"));
}
