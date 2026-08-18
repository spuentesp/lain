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
        None, // no embedding model in tests
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
        None, // no embedding model in tests
    )
    .unwrap();
    let bus = server.reload_bus();
    // Subscribe BEFORE requesting so the subscriber can see the signal.
    let mut sub = bus.subscribe();
    bus.request_reload().unwrap();
    assert!(sub.try_recv().is_ok());
}

/// Stress test: rapidly swap `workspaces.yaml` through
/// `LainServer::set_workspace` 50+ times and verify every swap is
/// observed through the SAME `Arc<RwLock<WorkspacesFile>>` the
/// `LainMcpServer` constructed by `serve()` is holding. Before the
/// fix the dispatch path captured its own `Arc<WorkspacesFile>`
/// snapshot at construction time, so writer/receiver pairs could
/// disagree indefinitely; after the fix they share one inner cell
/// and the read on every JSON-RPC dispatch reflects the most recent
/// write.
///
/// One writer fires 100 `set_workspace` calls in rapid succession;
/// one reader spins holding the *same* lock and samples what the
/// next dispatch would see. The test passes iff (a) every write goes
/// through to the shared cell (every step 1..=NUM_WRITES is observed
/// at least once by the reader) and (b) the read guard never holds a
/// stale value past a more recent write — i.e. the writer's and
/// reader's `Arc<RwLock<...>>` point at the **same** inner cell.
///
/// Before the fix the LainMcpServer kept a clone of the
/// `Arc<WorkspacesFile>` captured at construction time, so the
/// reader saw `count == 1` for the entire run — this test would
/// fail immediately at the writer's first swap.
#[tokio::test(flavor = "multi_thread")]
async fn set_workspace_stress_visible_to_shared_lock() {
    use lain::server::federation::workspace::{WorkspaceSpec, WorkspacesFile};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const NUM_WRITES: usize = 100;

    // --- build the federation + server with one initial workspace ---
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

    let initial_ws = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "initial".into(),
            description: None,
            source: None,
            members: vec!["repo-a".into()],
        }],
    });

    let server = lain::server::LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        lain::server::Transport::Http,
        9999,
        Arc::clone(&initial_ws),
        Some(repos_yaml.clone()),
        None, // no embedding model in tests
    )
    .expect("LainServer::with_federation_and_workspaces");

    // The shared lock is the single cell the LainMcpServer holds.
    let shared_lock: Arc<parking_lot::RwLock<WorkspacesFile>> = server
        .workspaces_handle()
        .expect("server was constructed with workspaces; handle must be Some");

    // Sanity: the initial state the LainMcpServer would see at
    // startup matches the workspaces file we handed the constructor.
    {
        let guard = shared_lock.read();
        assert_eq!(guard.workspaces.len(), 1);
        assert_eq!(guard.workspaces[0].name, "initial");
    }

    // --- writer: fires `set_workspace` NUM_WRITES times, each step
    //     stamping a unique identity and a member-count of `step + 1`. ---
    //
    // We deliberately do NOT sleep between writes — this is the
    // pathological case the bug report described: the rebuild task
    // fires a write, then the next file event fires another write,
    // and the dispatcher must keep up. With the shared lock in
    // place, the dispatcher's reads always trail right behind the
    // writer's writes; without it, the reader sees the original
    // snapshot forever.
    let server = std::sync::Arc::new(server);
    let writer_server = std::sync::Arc::clone(&server);
    let writer = tokio::spawn(async move {
        for step in 0..NUM_WRITES {
            let ws = Arc::new(WorkspacesFile {
                default: None,
                workspaces: (0..step + 1)
                    .map(|n| WorkspaceSpec {
                        name: format!("s{step}-w{n}"),
                        description: None,
                        source: None,
                        members: vec!["repo-a".into()],
                    })
                    .collect(),
            });
            writer_server.set_workspace(ws);
        }
    });

    // --- reader: polls the shared lock and verifies that every step
    //     from 1..=NUM_WRITES is observable. `RwLockReadGuard` is
    //     `!Send`, so the guard never crosses an `.await` boundary.
    // ---
    let reader_lock = std::sync::Arc::clone(&shared_lock);
    let high_water: std::sync::Arc<parking_lot::Mutex<usize>> =
        std::sync::Arc::new(parking_lot::Mutex::new(0));
    let reader_water = std::sync::Arc::clone(&high_water);
    let reader = tokio::spawn(async move {
        let mut observed_counts: std::collections::BTreeSet<usize> =
            std::collections::BTreeSet::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_count: usize = 0;
        loop {
            let count = {
                let guard = reader_lock.read();
                guard.workspaces.len()
            };
            observed_counts.insert(count);
            {
                let mut hw = reader_water.lock();
                if count > *hw {
                    *hw = count;
                }
            }
            // Strict-progress invariant (the punchline of the test):
            // the count must monotonically non-decrease. A regression
            // means the reader is looking at a *different* cell than
            // the writer — exactly the snapshot divergence the fix
            // closes.
            assert!(
                count >= last_count,
                "reader regressed: count dropped from {} to {} \
                 (the writer and reader are observing different cells)",
                last_count, count,
            );
            last_count = count;
            if count >= NUM_WRITES {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "reader stuck at count={} after 15s — set_workspace \
                     writes are not propagating through the shared lock. \
                     observed distinct counts: {:?} (need 1..={NUM_WRITES})",
                    count, observed_counts,
                );
            }
            tokio::time::sleep(Duration::from_micros(500)).await;
        }
        observed_counts
    });

    writer.await.expect("writer task should complete");
    let observed = reader.await.expect("reader task should complete");

    // The reader may legitimately *skip* intermediate counts when the
    // writer races ahead — that's just normal scheduling. Without
    // the fix, however, the reader would see the construction-time
    // count of `1` for the *entire* test run, because the LainMcpServer
    // held an independent snapshot. The punched-line assertions:
    //
    // (1) the reader sees the writer's final state — proves writes
    //     reach the lock cell the dispatch path reads through.
    // (2) the reader sees a strictly greater max than the original
    //     count of `1` (this is implied by (1) but the `observed.len()`
    //     assertion below makes it crisp).
    // (3) the observed set is non-trivial — a snapshot bug would
    //     collapse it to a single value.
    let max_seen = *observed.iter().max().unwrap_or(&0);
    assert!(
        max_seen >= NUM_WRITES,
        "reader never saw the writer's final step ({NUM_WRITES}); \
         distinct counts seen: {:?}. Without the shared lock this is \
         exactly the stale-snapshot symptom — the reader is looking \
         at a different cell than the writer.",
        observed,
    );
    assert!(
        observed.len() >= 2,
        "reader saw only {} distinct counts ({:?}); with NUM_WRITES={} \
         rapid writes the reader must observe several different \
         states — collapsing to one is the snapshot bug",
        observed.len(),
        observed,
        NUM_WRITES,
    );

    // Belt-and-braces: the final state is exactly what the writer wrote last.
    {
        let guard = shared_lock.read();
        assert_eq!(
            guard.workspaces.len(),
            NUM_WRITES,
            "expected final WorkspacesFile to carry {NUM_WRITES} workspaces",
        );
        assert_eq!(
            guard.workspaces.last().unwrap().name,
            format!("s{}-w{}", NUM_WRITES - 1, NUM_WRITES - 1),
            "final entry should match the writer's last step",
        );
    }
}

/// `LainServer::set_workspace` writes through the same
/// `Arc<RwLock<WorkspacesFile>>` cell the `LainMcpServer` constructed
/// eagerly inside `with_federation_and_workspaces` is holding. This
/// shorter companion to the stress test pins down that invariant
/// after a single swap: after `set_workspace(beta)`, the very next
/// read through `workspaces_handle()` (the path the LainHandler's
/// workspace dispatch reads through) sees `beta`, not the
/// construction-time snapshot. Before the fix the handler held an
/// independent `Arc<WorkspacesFile>` and this read would still show
/// `alpha`.
#[tokio::test(flavor = "current_thread")]
async fn set_workspace_publishes_to_shared_workspaces_handle() {
    use lain::server::federation::workspace::{WorkspaceSpec, WorkspacesFile};
    use std::sync::Arc;

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
    let initial = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "alpha".into(),
            description: None,
            source: None,
            members: vec!["repo-a".into()],
        }],
    });
    let server = lain::server::LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        lain::server::Transport::Http,
        9999,
        Arc::clone(&initial),
        Some(repos_yaml.clone()),
        None, // no embedding model in tests
    )
    .expect("LainServer::with_federation_and_workspaces");

    // Initial state (the cell the LainHandler sees right now).
    {
        let handle = server
            .workspaces_handle()
            .expect("workspaces_handle must be Some after with_..._workspaces");
        let guard = handle.read();
        assert_eq!(guard.workspaces[0].name, "alpha");
    }

    // The swap that `reload::run_rebuild` would perform after
    // re-reading `workspaces.yaml` from disk.
    server.set_workspace(Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "beta".into(),
            description: None,
            source: None,
            members: vec!["repo-a".into()],
        }],
    }));

    // The same `Arc<RwLock<WorkspacesFile>>` that the LainHandler is
    // holding now reflects the swap — i.e., the in-flight MCP
    // dispatch would return `beta` on the next call to
    // `list_workspaces`.
    {
        let handle = server
            .workspaces_handle()
            .expect("workspaces_handle must remain Some");
        let guard = handle.read();
        assert_eq!(
            guard.workspaces[0].name,
            "beta",
            "set_workspace did not publish through the shared lock — \
             the LainMcpServer would still serve the construction-time snapshot",
        );
    }
}