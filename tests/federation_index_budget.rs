//! The repo index pass is bounded by `max_concurrent_indexers`, and the
//! federation records the peak number of passes that ran at once.
//!
//! The "peak reaches the limit" claim depends on scheduling, so these
//! tests assert only what is deterministic: the peak never exceeds the
//! limit, and every repo still gets indexed when the limit is one.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::config::SourceConfig;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::health::RepoHealth;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};

fn git_init_committed(dir: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

/// A federation of `names`, each a committed one-file repo.
async fn federation_of(root: &Path, names: &[&str]) -> Arc<FederatedIndex> {
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    for name in names {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/.keep"), b"").unwrap();
        git_init_committed(&dir);
        let id = RepoId::new(*name).unwrap();
        let source: Box<dyn RepoSource> = Box::new(
            WorkspaceDirSource::with_config(
                id,
                dir.clone(),
                SourceConfig::WorkspaceDir { path: dir.clone() },
            )
            .unwrap(),
        );
        source.fetch().await.unwrap();
        fed.add_repo(source, &data_dir).await.unwrap();
    }
    fed
}

#[tokio::test(flavor = "multi_thread")]
async fn index_all_never_runs_more_passes_than_the_limit() {
    let root = tempfile::tempdir().unwrap();
    let fed = federation_of(root.path(), &["a", "b", "c"]).await;
    fed.set_max_concurrent_indexers(1);
    fed.index_all().await;
    assert_eq!(
        fed.observed_index_concurrency(),
        1,
        "a limit of one must serialize the passes"
    );
    for (_, health) in fed.list_repos() {
        assert_ne!(health, RepoHealth::Degraded, "every repo should index");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn index_all_with_a_wider_limit_stays_within_it() {
    let root = tempfile::tempdir().unwrap();
    let fed = federation_of(root.path(), &["a", "b"]).await;
    fed.set_max_concurrent_indexers(2);
    fed.index_all().await;
    assert!(
        fed.observed_index_concurrency() <= 2,
        "observed {} passes against a limit of 2",
        fed.observed_index_concurrency()
    );
}

#[tokio::test]
async fn the_pass_guard_records_the_peak_and_releases_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let fed = federation_of(root.path(), &[]).await;
    let first = fed.enter_index_pass();
    let second = fed.enter_index_pass();
    assert_eq!(fed.observed_index_concurrency(), 2);
    drop(first);
    drop(second);
    // A later pass starts from zero, so the peak stays at two.
    let _third = fed.enter_index_pass();
    assert_eq!(fed.observed_index_concurrency(), 2);
}
