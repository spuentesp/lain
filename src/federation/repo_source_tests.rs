//! Contract tests for RepoSource. These run against LocalCloneSource in this
//! task; later tasks (WorkspaceDirSource, ShallowCloneSource) re-use the same
//! contract tests via parametrization.
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::*;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

fn dummy_id() -> RepoId {
    RepoId::new("test-repo").unwrap()
}

#[tokio::test]
async fn local_clone_source_id_returns_configured() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
}

#[tokio::test]
async fn local_clone_source_local_path_returns_configured() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert_eq!(src.local_path(), PathBuf::from("/tmp/repo").as_path());
}

#[tokio::test]
async fn local_clone_source_is_stale_when_never_refreshed() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert!(src.is_stale(Duration::from_secs(0)));
}

#[tokio::test]
async fn local_clone_source_is_not_stale_after_recent_refresh() {
    let mut src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    src.mark_refreshed(SystemTime::now());
    assert!(!src.is_stale(Duration::from_secs(60)));
}

#[tokio::test]
#[ignore]
async fn local_clone_source_real_fetch_against_public_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let src = LocalCloneSource::new(
        RepoId::new("hello-world").unwrap(),
        "https://github.com/octocat/Hello-World.git",
        "master",
        tmp.path().join("hello-world"),
    ).unwrap();
    src.fetch().await.expect("fetch should succeed");
    assert!(src.local_path().exists());
    assert!(!src.is_stale(Duration::from_secs(60)));
}
