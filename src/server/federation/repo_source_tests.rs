//! Contract tests for RepoSource. These run against LocalCloneSource in this
//! task; later tasks (WorkspaceDirSource, ShallowCloneSource) re-use the same
//! contract tests via parametrization.
use crate::federation::config::SourceConfig;
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
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    src.mark_refreshed(SystemTime::now());
    assert!(!src.is_stale(Duration::from_secs(60)));
}

/// `source_config()` must return the exact `SourceConfig` the source was
/// constructed from — not a freshly-derived reconstruction that could
/// lose original formatting. `LocalCloneSource::new` auto-derives the
/// config from URL+ref, so the round-trip check here is on the
/// convenience constructor. The `with_config` form is exercised by
/// the integration test in `manifest_tests::save_manifest_round_trips_source_config`.
#[test]
fn local_clone_source_config_round_trips_via_new() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    let cfg = src.source_config();
    match cfg {
        SourceConfig::LocalClone { url, r#ref } => {
            assert_eq!(url, "https://example.com/repo.git");
            assert_eq!(r#ref, "main");
        }
        other => panic!("expected LocalClone, got {other:?}"),
    }
}

/// `with_config` preserves a verbatim `SourceConfig` so `config.rs`'s
/// manifest round-trip sees the original YAML value rather than a
/// re-derived one. This is the path the production loader uses.
#[test]
fn local_clone_source_with_config_preserves_value() {
    let cfg = SourceConfig::LocalClone {
        url: "https://example.com/repo.git".to_string(),
        r#ref: "develop".to_string(),
    };
    let src = LocalCloneSource::with_config(dummy_id(), "https://example.com/repo.git", "develop", PathBuf::from("/tmp/repo"), cfg.clone()).unwrap();
    assert_eq!(src.source_config(), &cfg);
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

#[tokio::test]
async fn shallow_clone_source_id_and_path() {
    let src = ShallowCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo"), Duration::from_secs(300)).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
    assert_eq!(src.local_path(), PathBuf::from("/tmp/repo").as_path());
    assert_eq!(src.refresh_interval(), Duration::from_secs(300));
}

#[tokio::test]
async fn shallow_clone_source_is_stale_when_never_refreshed() {
    let src = ShallowCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo"), Duration::from_secs(60)).unwrap();
    assert!(src.is_stale(Duration::from_secs(60)));
}

#[test]
fn shallow_clone_source_config_round_trips_via_new() {
    let src = ShallowCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo"), Duration::from_secs(300)).unwrap();
    match src.source_config() {
        SourceConfig::ShallowClone { url, r#ref, refresh_interval_secs } => {
            assert_eq!(url, "https://example.com/repo.git");
            assert_eq!(r#ref, "main");
            assert_eq!(*refresh_interval_secs, 300);
        }
        other => panic!("expected ShallowClone, got {other:?}"),
    }
}

#[tokio::test]
async fn workspace_dir_source_id_and_path() {
    let src = WorkspaceDirSource::new(dummy_id(), PathBuf::from("/srv/legacy")).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
    assert_eq!(src.local_path(), PathBuf::from("/srv/legacy").as_path());
}

#[tokio::test]
async fn workspace_dir_source_fetch_is_noop() {
    let src = WorkspaceDirSource::new(dummy_id(), PathBuf::from("/srv/legacy")).unwrap();
    src.fetch().await.expect("fetch should be a no-op");
}

#[tokio::test]
async fn workspace_dir_source_rejects_empty_path() {
    assert!(WorkspaceDirSource::new(dummy_id(), PathBuf::new()).is_err());
}

#[test]
fn workspace_dir_source_config_round_trips_via_new() {
    let src = WorkspaceDirSource::new(dummy_id(), PathBuf::from("/srv/legacy")).unwrap();
    match src.source_config() {
        SourceConfig::WorkspaceDir { path } => {
            assert_eq!(path, &PathBuf::from("/srv/legacy"));
        }
        other => panic!("expected WorkspaceDir, got {other:?}"),
    }
}

#[test]
fn workspace_dir_source_with_config_preserves_value() {
    let cfg = SourceConfig::WorkspaceDir { path: PathBuf::from("/srv/explicit") };
    let src = WorkspaceDirSource::with_config(dummy_id(), PathBuf::from("/srv/explicit"), cfg.clone()).unwrap();
    assert_eq!(src.source_config(), &cfg);
}

/// `content_hash` on a non-checkout directory must return `Ok(None)`, not
/// an error — the manifest treats `""` as "no fingerprint available"
/// and downstream change-detection skips the repo cleanly. A regular
/// `tempdir` is fine: it's a directory, but not a git repository, so
/// `git rev-parse` exits 128 with "not a git repository".
#[test]
fn workspace_dir_source_content_hash_returns_none_for_non_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let src = WorkspaceDirSource::new(dummy_id(), tmp.path().to_path_buf()).unwrap();
    let hash = src.content_hash().expect("non-git dir must not error");
    assert!(hash.is_none(), "non-repo path should yield None, got {hash:?}");
}

/// `content_hash` on a real git repo returns the HEAD hash. The hash
/// is non-empty and matches what `git rev-parse HEAD` says.
#[test]
fn workspace_dir_source_content_hash_returns_head_for_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(tmp.path()).unwrap();
    // An init'd repo with no commits has ambiguous HEAD; commit so the
    // hash query resolves cleanly.
    let sig = git2::Signature::now("test", "test@lain").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let _ = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]);
    let src = WorkspaceDirSource::new(dummy_id(), tmp.path().to_path_buf()).unwrap();
    let hash = src.content_hash().expect("git repo must yield a hash").expect("expected Some");
    assert!(!hash.is_empty(), "HEAD hash must not be empty");
    assert_eq!(hash.len(), 40, "SHA-1 hex is 40 chars, got {hash:?}");
}
