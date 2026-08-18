//! End-to-end integration test: removing a repo from `repos.yaml`
//! while the server is running should make it disappear from the
//! federation tool surface within seconds — without a restart.

use lain::server::reload::ReloadState;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn remove_repo_from_workspace_makes_it_invisible_to_list_repos() {
    let tmp = tempdir().unwrap();
    let repos_yaml: PathBuf = tmp.path().join("repos.yaml");
    let repo_a = tmp.path().join("repo-a");
    std::fs::create_dir_all(&repo_a).unwrap();
    git2::Repository::init(&repo_a).unwrap();
    let repo_b = tmp.path().join("repo-b");
    std::fs::create_dir_all(&repo_b).unwrap();
    git2::Repository::init(&repo_b).unwrap();

    // Initial: two workspace_dir repos.
    let yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo-a\n    source: {{ type: workspace_dir, path: {} }}\n  - id: repo-b\n    source: {{ type: workspace_dir, path: {} }}\n",
        tmp.path().join("federation").display(),
        repo_a.display(),
        repo_b.display(),
    );
    std::fs::write(&repos_yaml, &yaml).unwrap();

    let fed = build_federation(&repos_yaml).await;
    // `LainServer::with_federation` builds a placeholder git repo at
    // `/tmp/lain-federation-{pid}`. A prior test in the same process
    // may have left it in an inconsistent state — clear it so this
    // test starts fresh.
    let staging = std::env::temp_dir()
        .join(format!("lain-federation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let server = lain::server::LainServer::with_federation(
        Arc::clone(&fed),
        lain::server::Transport::Http,
        9999,
        Some(repos_yaml.clone()),
        None, // no embedding model in tests
    )
    .expect("LainServer::with_federation");
    assert_eq!(server.repo_count(), 2);

    let bus = server.reload_bus();

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
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }
    });
    // Give the rebuild task a moment to subscribe before signalling.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Remove repo-b from repos.yaml and ping the bus.
    let yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo-a\n    source: {{ type: workspace_dir, path: {} }}\n",
        tmp.path().join("federation").display(),
        repo_a.display(),
    );
    std::fs::write(&repos_yaml, &yaml).unwrap();
    bus.request_reload().expect("request_reload");

    let mut converged = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if server.repo_count() == 1 {
            converged = true;
            break;
        }
    }
    assert!(converged, "expected repo_count == 1 after rebuild");
    assert_eq!(bus.status().state, ReloadState::Idle);
}

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