//! Restart equivalence and conservative invalidation for completed federations.
use lain::federation::{federated_index::FederatedIndex, loader::load_federation};
use std::path::Path;
use std::sync::Arc;

fn commit(root: &Path) {
    let repo = git2::Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &parents)
        .unwrap();
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    for name in ["alpha", "beta"] {
        let root = tmp.path().join(name);
        std::fs::create_dir(&root).unwrap();
        git2::Repository::init(&root).unwrap();
        std::fs::write(
            root.join("main.py"),
            format!("def {name}():\n    return 1\n"),
        )
        .unwrap();
        commit(&root);
    }
    let config = tmp.path().join("repos.yaml");
    let mut cfg = lain::federation::config::FederationConfig {
        data_dir: tmp.path().join("data"),
        ..Default::default()
    };
    for name in ["alpha", "beta"] {
        cfg.repos.push(
            serde_yaml::from_str(&format!(
                "id: {name}\nsource: {{type: workspace_dir, path: {}}}",
                tmp.path().join(name).display()
            ))
            .unwrap(),
        );
    }
    std::fs::write(&config, serde_yaml::to_string(&cfg).unwrap()).unwrap();
    (tmp, config)
}

async fn complete(fed: &Arc<FederatedIndex>) {
    fed.index_all_for_startup().await;
    for (id, _) in fed.list_repos() {
        fed.relink_repo(&id).await.unwrap();
    }
    fed.mint_cross_repo_anchors(lain::server::sensors::patterns::AnchorShapes::bundled())
        .unwrap();
    fed.rejoin_contracts_if_dirty().unwrap();
    assert!(fed.checkpoint_startup().unwrap());
}

fn graph(fed: &FederatedIndex) -> (usize, usize) {
    (fed.backend().node_count(), fed.backend().edge_count())
}

#[tokio::test]
async fn unchanged_restart_reuses_completed_graph() {
    let (_tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    assert!(!fed.startup_restored());
    complete(&fed).await;
    let counts = graph(&fed);
    let restored = load_federation(&config).await.unwrap();
    assert!(
        restored.startup_restored(),
        "unchanged restart must reuse the checkpoint"
    );
    assert_eq!(counts, graph(&restored));
    let again = load_federation(&config).await.unwrap();
    assert!(
        again.startup_restored(),
        "a read must not consume the completion record"
    );
    assert_eq!(canonical(&restored), canonical(&again));
}

#[tokio::test]
async fn dirty_edit_and_new_commit_invalidate_checkpoint() {
    let (tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    std::fs::write(
        tmp.path().join("alpha/main.py"),
        "def changed():\n    return 2\n",
    )
    .unwrap();
    let changed = load_federation(&config).await.unwrap();
    assert!(!changed.startup_restored());
    changed.index_all_for_startup().await;
    assert!(changed
        .get_repo(&lain::federation::repo_id::RepoId::new("alpha").unwrap())
        .unwrap()
        .nodes()
        .iter()
        .any(|n| n.name == "changed"));
    assert!(
        !fed.checkpoint_startup().unwrap(),
        "dirty inputs cannot publish a cache"
    );
    commit(&tmp.path().join("alpha"));
    assert!(!load_federation(&config).await.unwrap().startup_restored());
}

#[tokio::test]
async fn config_changes_and_corrupt_completion_records_miss() {
    let (tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let mut cfg = lain::federation::config::FederationConfig::load(&config).unwrap();
    cfg.params.insert("CACHE_TEST".into(), "different".into());
    std::fs::write(&config, serde_yaml::to_string(&cfg).unwrap()).unwrap();
    assert!(!load_federation(&config).await.unwrap().startup_restored());
    std::fs::write(tmp.path().join("data/startup-complete.json"), b"truncated").unwrap();
    assert!(!load_federation(&config).await.unwrap().startup_restored());
}

fn canonical(fed: &FederatedIndex) -> Vec<Vec<u8>> {
    let mut rows = Vec::new();
    fed.backend()
        .visit_nodes(&mut |node| {
            let mut node = node.clone();
            node.last_git_sync = None;
            node.last_lsp_sync = None;
            node.is_hydrated = false;
            node.embedding = None;
            rows.push(serde_json::to_vec(&node).unwrap());
        })
        .unwrap();
    fed.backend()
        .visit_edges(&mut |edge| rows.push(serde_json::to_vec(edge).unwrap()))
        .unwrap();
    rows.sort();
    rows
}

#[tokio::test]
async fn cache_hit_restores_environment_and_ownership_side_channels() {
    use lain::server::sensors::{codeowners_sensor, env_sensor};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(tmp.path().file_name().unwrap());
    std::fs::create_dir(&root).unwrap();
    git2::Repository::init(&root).unwrap();
    std::fs::write(root.join("main.py"), "def main():\n    return 1\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(
        root.join(".env"),
        "API_URL=https://restore.test\nTOPIC=orders\n",
    )
    .unwrap();
    std::fs::write(root.join("CODEOWNERS"), "* @restore-team\n").unwrap();
    commit(&root);
    let config = tmp.path().join("repos.yaml");
    std::fs::write(
        &config,
        format!(
        "data_dir: {}\nrepos:\n  - id: metadata\n    source: {{type: workspace_dir, path: {}}}\n",
        tmp.path().join("data").display(), root.display(),
    ),
    )
    .unwrap();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let id = lain::federation::repo_id::RepoId::new("metadata").unwrap();
    let repo = fed.get_repo(&id).unwrap();
    // Emulate fresh process tables without modifying any certified input.
    let empty = tmp.path().join("empty").join(root.file_name().unwrap());
    std::fs::create_dir_all(&empty).unwrap();
    env_sensor::scan_workspace_env(repo.db(), &empty, repo.db().namespace()).unwrap();
    codeowners_sensor::scan_workspace_codeowners_for_repo(
        repo.db(),
        &empty,
        repo.db().namespace(),
        "metadata",
    )
    .unwrap();
    assert!(env_sensor::env_bindings_for(&root).is_empty());
    assert!(codeowners_sensor::codeowners_for("metadata", "main.py").is_empty());
    let restored = load_federation(&config).await.unwrap();
    assert!(restored.startup_restored());
    assert_eq!(
        env_sensor::env_bindings_for(&root).distinct_hosts("API_URL"),
        vec!["restore.test"]
    );
    assert_eq!(
        env_sensor::scalar_bindings_for(&root).unique_value("TOPIC"),
        Some("orders")
    );
    assert_eq!(
        codeowners_sensor::codeowners_for("metadata", "main.py"),
        vec!["@restore-team"]
    );
    assert_eq!(canonical(&fed), canonical(&restored));
    std::fs::write(root.join(".env"), "API_URL=https://changed.test\n").unwrap();
    assert!(
        !load_federation(&config).await.unwrap().startup_restored(),
        "ignored environment inputs still invalidate the completed graph"
    );
}

#[tokio::test]
async fn cold_runs_and_cache_miss_restart_have_identical_graphs() {
    let (tmp, config) = fixture();
    let first = load_federation(&config).await.unwrap();
    complete(&first).await;
    let expected = canonical(&first);
    let digest =
        lain::federation::contracts::digest::federation_content_digest(first.backend().as_ref())
            .unwrap();
    std::fs::remove_file(tmp.path().join("data/startup-complete.json")).unwrap();
    let resumed = load_federation(&config).await.unwrap();
    assert!(!resumed.startup_restored());
    complete(&resumed).await;
    assert_eq!(
        expected,
        canonical(&resumed),
        "cache miss must not add edges on restart"
    );
    let mut cfg = lain::federation::config::FederationConfig::load(&config).unwrap();
    cfg.data_dir = tmp.path().join("second-cold");
    cfg.max_concurrent_indexers = 2;
    let second_config = tmp.path().join("second.yaml");
    std::fs::write(&second_config, serde_yaml::to_string(&cfg).unwrap()).unwrap();
    let second = load_federation(&second_config).await.unwrap();
    complete(&second).await;
    assert_eq!(
        digest,
        lain::federation::contracts::digest::federation_content_digest(second.backend().as_ref())
            .unwrap()
    );
    assert_eq!(
        expected,
        canonical(&second),
        "concurrent cold run must have the same output"
    );
}

#[tokio::test]
async fn identical_sources_share_context_independent_reference_cache() {
    let (tmp, config) = fixture();
    for name in ["alpha", "beta"] {
        std::fs::write(
            tmp.path().join(name).join("main.py"),
            "def shared():\n    return helper()\ndef helper():\n    return 1\n",
        )
        .unwrap();
        commit(&tmp.path().join(name));
    }
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let entries: Vec<_> = std::fs::read_dir(tmp.path().join("data/static-ref-cache"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "equal relative source trees share one extraction artifact"
    );
    assert!(fed
        .get_repo(&lain::federation::repo_id::RepoId::new("alpha").unwrap())
        .unwrap()
        .nodes()
        .iter()
        .any(|n| n.name == "shared"));
    let ids: Vec<_> = fed.list_repos().iter().map(|(id, _)| id.clone()).collect();
    let left = fed.get_repo(&ids[0]).unwrap().nodes();
    let right = fed.get_repo(&ids[1]).unwrap().nodes();
    assert_ne!(
        left.iter().find(|n| n.name == "shared").unwrap().id,
        right.iter().find(|n| n.name == "shared").unwrap().id,
        "graph namespaces stay isolated"
    );
}

#[tokio::test]
async fn ignored_overrides_and_changed_graph_outputs_invalidate_checkpoint() {
    let (tmp, config) = fixture();
    std::fs::write(tmp.path().join("alpha/.gitignore"), ".lain/\n").unwrap();
    commit(&tmp.path().join("alpha"));
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let id = lain::federation::repo_id::GlobalId::new(
        &lain::federation::repo_id::RepoId::new("alpha").unwrap(),
        lain::schema::NodeType::Function,
        "extra.py",
        "extra",
        None,
    );
    fed.backend()
        .upsert_node_global(
            id.as_str(),
            lain::schema::NodeType::Function,
            "extra.py",
            "extra",
        )
        .unwrap();
    assert!(!load_federation(&config).await.unwrap().startup_restored());
    // Seed a new completion and then change an ignored per-repo override.
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let patterns = tmp.path().join("alpha/.lain/patterns");
    std::fs::create_dir_all(&patterns).unwrap();
    std::fs::write(patterns.join("custom.yaml"), "# operator change\n").unwrap();
    assert!(!load_federation(&config).await.unwrap().startup_restored());
}

#[test]
fn required_cache_refuses_an_unprepared_startup() {
    let (_tmp, config) = fixture();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["server", "--config"])
        .arg(&config)
        .args(["--workspace", ""])
        .env("LAIN_REQUIRE_STARTUP_CACHE", "1")
        .env("LAIN_DISABLE_LSP", "1")
        .env("LAIN_GIT_SENSOR", "in_process")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cache is required"));
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn launch(config: &Path, port: u16, log: &Path, required: bool) -> Server {
    let output = std::fs::File::create(log).unwrap();
    Server(
        std::process::Command::new(env!("CARGO_BIN_EXE_lain"))
            .args(["server", "--config"])
            .arg(config)
            .args([
                "--workspace",
                "",
                "--transport",
                "http",
                "--port",
                &port.to_string(),
            ])
            .env("LAIN_DISABLE_LSP", "1")
            .env("LAIN_TEST_NO_LSP", "")
            .env("LAIN_GIT_SENSOR", "in_process")
            .env(
                "LAIN_REQUIRE_STARTUP_CACHE",
                if required { "1" } else { "0" },
            )
            .stdin(std::process::Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    )
}

async fn ready(
    client: &reqwest::Client,
    url: &str,
    server: &mut Server,
    log: &Path,
) -> serde_json::Value {
    for _ in 0..200 {
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!(
                "server exited {status}: {}",
                std::fs::read_to_string(log).unwrap()
            );
        }
        if let Ok(response) = client
            .get(format!("{url}/health/index-status"))
            .send()
            .await
        {
            if response.status() == reqwest::StatusCode::OK {
                return client
                    .get(format!("{url}/health"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!(
        "server did not become ready: {}",
        std::fs::read_to_string(log).unwrap()
    );
}

#[tokio::test]
async fn live_cached_startup_keeps_watchers_and_invalidates_after_an_edit() {
    let (tmp, config) = fixture();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let cold_log = tmp.path().join("cold.log");
    let mut cold = launch(&config, port, &cold_log, false);
    let first = ready(&client, &url, &mut cold, &cold_log).await;
    assert!(
        tmp.path().join("data/startup-complete.json").is_file(),
        "checkpoint precedes readiness: {}",
        std::fs::read_to_string(&cold_log).unwrap()
    );
    drop(cold);
    let warm_log = tmp.path().join("warm.log");
    let mut warm = launch(&config, port, &warm_log, true);
    let second = ready(&client, &url, &mut warm, &warm_log).await;
    assert_eq!(
        first["federation"]["total_nodes"],
        second["federation"]["total_nodes"]
    );
    assert_eq!(
        first["federation"]["total_edges"],
        second["federation"]["total_edges"]
    );
    assert!(std::fs::read_to_string(&warm_log)
        .unwrap()
        .contains("startup cache hit"));
    let before = second["federation"]["total_nodes"].as_u64().unwrap();
    std::fs::write(
        tmp.path().join("alpha/main.py"),
        "def alpha():\n    return 1\ndef changed_live():\n    return alpha()\n",
    )
    .unwrap();
    let mut updated = false;
    for _ in 0..100 {
        let health: serde_json::Value = client
            .get(format!("{url}/health"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if health["federation"]["total_nodes"].as_u64().unwrap() > before {
            updated = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        updated,
        "cached startup must retain live indexing and projection"
    );
    drop(warm);
    let refused_log = tmp.path().join("refused.log");
    let mut refused = launch(&config, port, &refused_log, true);
    for _ in 0..100 {
        if let Some(status) = refused.0.try_wait().unwrap() {
            assert!(!status.success());
            assert!(std::fs::read_to_string(refused_log)
                .unwrap()
                .contains("cache is required"));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("strict cached startup must refuse dirty inputs");
}

#[tokio::test]
async fn dirty_then_reverted_worktree_cannot_reuse_the_dirty_analysis() {
    let (tmp, config) = fixture();
    let first = load_federation(&config).await.unwrap();
    complete(&first).await;
    let expected = canonical(&first);
    let source = tmp.path().join("alpha/main.py");
    let original = std::fs::read(&source).unwrap();
    std::fs::write(&source, "def temporary_dirty():\n    return 42\n").unwrap();
    let dirty = load_federation(&config).await.unwrap();
    dirty.index_all_for_startup().await;
    assert!(!dirty.checkpoint_startup().unwrap());
    std::fs::write(source, original).unwrap();
    let reverted = load_federation(&config).await.unwrap();
    complete(&reverted).await;
    assert_eq!(expected, canonical(&reverted));
}

#[tokio::test]
async fn invalid_analysis_context_rebuilds_local_output_even_at_the_same_head() {
    let (tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let expected = canonical(&fed);
    let repo = fed
        .get_repo(&lain::federation::repo_id::RepoId::new("alpha").unwrap())
        .unwrap();
    let mut node = repo
        .nodes()
        .into_iter()
        .find(|n| n.name == "alpha")
        .unwrap();
    node.signature = Some("obsolete context".into());
    repo.db().upsert_node(node).unwrap();
    repo.db().save_to_disk_sync().unwrap();
    std::fs::write(
        tmp.path().join("data/repos/alpha/analysis-inputs.txt"),
        "old-context",
    )
    .unwrap();
    let rebuilt = load_federation(&config).await.unwrap();
    assert!(!rebuilt.startup_restored());
    complete(&rebuilt).await;
    assert_eq!(expected, canonical(&rebuilt));
}

#[tokio::test]
async fn cache_miss_retracts_obsolete_derived_cross_repo_calls() {
    let (_tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let mut source = None;
    let mut target = None;
    fed.backend()
        .visit_nodes(&mut |node| {
            if node.name == "alpha" && node.node_type == lain::schema::NodeType::Function {
                source = Some(node.id.clone());
            }
            if node.name == "beta" && node.node_type == lain::schema::NodeType::Function {
                target = Some(node.id.clone());
            }
        })
        .unwrap();
    let edge = lain::schema::GraphEdge::new(
        lain::schema::EdgeType::Calls,
        source.unwrap(),
        target.unwrap(),
    );
    fed.backend().upsert_edge(edge.clone()).unwrap();
    let rebuilt = load_federation(&config).await.unwrap();
    assert!(!rebuilt.startup_restored());
    complete(&rebuilt).await;
    let mut stale = false;
    rebuilt
        .backend()
        .visit_edges(&mut |current| {
            stale |= current.edge_type == edge.edge_type
                && current.source_id == edge.source_id
                && current.target_id == edge.target_id;
        })
        .unwrap();
    assert!(
        !stale,
        "completed rebuild must remove a derived call absent from current source references"
    );
}

#[tokio::test]
async fn relinking_preserves_confirmed_cross_repo_calls() {
    let (_tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let mut source = None;
    let mut target = None;
    fed.backend()
        .visit_nodes(&mut |node| {
            if node.name == "alpha" && node.node_type == lain::schema::NodeType::Function {
                source = Some(node.id.clone());
            }
            if node.name == "beta" && node.node_type == lain::schema::NodeType::Function {
                target = Some(node.id.clone());
            }
        })
        .unwrap();
    let mut edge = lain::schema::GraphEdge::new(
        lain::schema::EdgeType::Calls,
        source.unwrap(),
        target.unwrap(),
    );
    edge.provenance = Some(lain::schema::EdgeProvenance::Confirmed {
        source: "fixture".into(),
    });
    fed.backend().upsert_edge(edge.clone()).unwrap();
    fed.relink_repo(&lain::federation::repo_id::RepoId::new("alpha").unwrap())
        .await
        .unwrap();
    let preserved = fed
        .backend()
        .edges_to(&edge.target_id, edge.edge_type)
        .unwrap();
    assert!(preserved.iter().any(|e| e.source_id == edge.source_id
        && matches!(
            e.provenance,
            Some(lain::schema::EdgeProvenance::Confirmed { .. })
        )));
}

#[tokio::test]
async fn changed_membership_removes_unselected_repo_nodes_before_caching() {
    let (_tmp, config) = fixture();
    let fed = load_federation(&config).await.unwrap();
    complete(&fed).await;
    let mut cfg = lain::federation::config::FederationConfig::load(&config).unwrap();
    cfg.repos.retain(|repo| repo.id == "alpha");
    std::fs::write(&config, serde_yaml::to_string(&cfg).unwrap()).unwrap();
    let selected = load_federation(&config).await.unwrap();
    assert!(!selected.startup_restored());
    let mut leaked = false;
    selected
        .backend()
        .visit_nodes(&mut |node| leaked |= node.id.starts_with("beta:"))
        .unwrap();
    assert!(
        !leaked,
        "old selected membership must not leak through the persisted graph"
    );
    complete(&selected).await;
    assert!(load_federation(&config).await.unwrap().startup_restored());
}
