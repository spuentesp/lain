//! End-to-end integration test: adding a repo to `repos.yaml` while
//! the server is running should make it visible to the federation
//! tool surface within seconds — without a restart.
//!
//! Mirrors the spec's "what is hot-reloaded" promise in Task 6.6.

use lain::server::reload::{ReloadBus, ReloadState};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn add_repo_to_workspace_is_visible_to_list_repos() {
    let tmp = tempdir().unwrap();
    let repos_yaml: PathBuf = tmp.path().join("repos.yaml");
    let repo_a = tmp.path().join("repo-a");
    std::fs::create_dir_all(&repo_a).unwrap();
    git2::Repository::init(&repo_a).unwrap();

    // Initial repos.yaml with one workspace_dir repo.
    let yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo-a\n    source: {{ type: workspace_dir, path: {} }}\n",
        tmp.path().join("federation").display(),
        repo_a.display(),
    );
    std::fs::write(&repos_yaml, &yaml).unwrap();

    // Build the federation directly (bypassing load_federation's git
    // quirks) and a LainServer.
    let fed = build_federation(&repos_yaml).await;
    let server = lain::server::LainServer::with_federation(
        Arc::clone(&fed),
        lain::server::Transport::Http,
        9999,
        Some(repos_yaml.clone()),
    )
    .expect("LainServer::with_federation");
    assert_eq!(server.repo_count(), 1);

    let bus = server.reload_bus();

    // Spawn the rebuild loop, like `cli::server::spawn_hot_reload` does.
    let server_for_loop = server.clone_for_background();
    let bus_for_loop = Arc::clone(&bus);
    let _rebuild_task = tokio::spawn(async move {
        let mut sub = bus_for_loop.subscribe();
        loop {
            match sub.try_recv() {
                Ok(()) => {
                    let _ = lain::server::reload::run_rebuild(
                        &server_for_loop,
                        &bus_for_loop,
                    )
                    .await;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    // drain
                    continue;
                }
            }
        }
    });
    // Give the rebuild task a moment to subscribe before signalling.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Simulate `lain repos add` — append a new repo to repos.yaml and
    // ping the bus. We bypass the CLI's Unix socket for test isolation
    // and call `request_reload` directly.
    let repo_b = tmp.path().join("repo-b");
    std::fs::create_dir_all(&repo_b).unwrap();
    git2::Repository::init(&repo_b).unwrap();
    let yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo-a\n    source: {{ type: workspace_dir, path: {} }}\n  - id: repo-b\n    source: {{ type: workspace_dir, path: {} }}\n",
        tmp.path().join("federation").display(),
        repo_a.display(),
        repo_b.display(),
    );
    std::fs::write(&repos_yaml, &yaml).unwrap();

    bus.request_reload().expect("request_reload");

    // Wait up to 5s for the rebuild to converge.
    let mut converged = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if server.repo_count() == 2 {
            converged = true;
            break;
        }
    }
    assert!(converged, "expected repo_count == 2 after rebuild");
    assert_eq!(bus.status().state, ReloadState::Idle);
}

/// Build a `FederatedIndex` from a `repos.yaml` without going through
/// `load_federation`'s tempdir/git-source dance. Uses `workspace_dir`
/// sources only — no network, no clones.
async fn build_federation(
    repos_yaml: &std::path::Path,
) -> Arc<lain::server::federation::federated_index::FederatedIndex> {
    use lain::server::federation::config::FederationConfig;
    use lain::server::federation::federated_index::FederatedIndex;
    use lain::server::federation::graph_backend::PetgraphBackend;

    let cfg: FederationConfig = serde_yaml::from_str(
        &std::fs::read_to_string(repos_yaml).unwrap(),
    )
    .unwrap();
    let backend: Arc<dyn lain::server::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&cfg.data_dir).expect("PetgraphBackend::new"));
    let fed = Arc::new(FederatedIndex::new(backend));
    for repo in &cfg.repos {
        let source = cfg.build_source_for(repo).expect("build_source_for");
        source.fetch().await.expect("fetch");
        let rid = source.id().clone();
        fed.add_repo(source, &cfg.data_dir).await.expect("add_repo");
        fed.project_repo(&rid).await.expect("project_repo");
    }
    fed
}

/// Sanity check: `ReloadBus` is wired into the `LainServer` and the
/// underlying `Arc<ReloadBus>` is the same handle the bus exposes.
#[tokio::test(flavor = "current_thread")]
async fn lain_server_reload_bus_returns_same_handle() {
    let tmp = tempdir().unwrap();
    let repos_yaml: PathBuf = tmp.path().join("repos.yaml");
    let repo_a = tmp.path().join("repo-a");
    std::fs::create_dir_all(&repo_a).unwrap();
    git2::Repository::init(&repo_a).unwrap();
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}\nrepos:\n  - id: repo-a\n    source: {{ type: workspace_dir, path: {} }}\n",
            tmp.path().join("federation").display(),
            repo_a.display(),
        ),
    )
    .unwrap();
    let fed = build_federation(&repos_yaml).await;
    let server = lain::server::LainServer::with_federation(
        fed,
        lain::server::Transport::Http,
        9999,
        Some(repos_yaml.clone()),
    )
    .unwrap();
    let bus = server.reload_bus();
    // Subscribe BEFORE requesting so the subscriber can see the signal.
    let mut sub = bus.subscribe();
    bus.request_reload().unwrap();
    assert!(sub.try_recv().is_ok());
}