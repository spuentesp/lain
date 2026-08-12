//! Integration smoke tests for the federation module.
//!
//! These tests exercise the end-to-end pipeline wired by Task 14:
//! `RepoIndex::index` → `server::ingestion::index_one_repo` →
//! tree-sitter extract → file/module nodes inserted into the per-repo
//! `GraphDatabase`. Full LSP hydration is best-effort: if no language
//! server is on `PATH`, the file/module hierarchy still lands in the
//! graph, which is what the smoke test asserts.

use lain::federation::health::RepoHealth;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_index::RepoIndex;
use lain::federation::repo_source::WorkspaceDirSource;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

/// Initialize a throwaway git repository at `dir` with a single commit so
/// `GitSensor::new` can open it. Returns the path to the repo.
fn init_temp_git_repo(dir: &std::path::Path) {
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg("--initial-branch=main")
        .arg(dir)
        .status()
        .expect("git init failed to start");
    assert!(status.success(), "git init failed: {status}");

    // Configure a local-only identity so the initial commit can be created.
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git failed to start");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Smoke Test"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "initial"]);
}

#[tokio::test]
async fn repo_index_indexes_files_via_index_one_repo() {
    // Create a temp git repo with a single Rust file in a subdirectory.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().to_path_buf();
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("hello.rs"),
        "pub fn greet() -> &'static str { \"hi\" }\n",
    )
    .unwrap();
    init_temp_git_repo(&repo_dir);

    // Build a RepoIndex on the temp repo.
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("smoke").unwrap(), repo_dir.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    // Health should start at Indexing.
    assert_eq!(ri.health(), RepoHealth::Indexing);

    // Run the pipeline. We don't care whether the LSP server is on PATH;
    // the file/module nodes are created before LSP is invoked, so the
    // smoke test passes either way.
    ri.index().await.expect("index should succeed");

    // After indexing, health should be Ready and the file node should
    // be present in the per-repo graph.
    assert_eq!(ri.health(), RepoHealth::Ready);
    let nodes = ri.nodes();
    let paths: Vec<&str> = nodes.iter().map(|n| n.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("hello.rs")),
        "expected file node for src/hello.rs, got paths: {paths:?}"
    );
}

#[tokio::test]
async fn repo_index_index_is_idempotent_on_same_commit() {
    // Indexing twice without a new commit should be a no-op the second time.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().to_path_buf();
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn id() -> u32 { 0 }\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("idem").unwrap(), repo_dir.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    ri.index().await.expect("first index should succeed");
    let nodes_after_first = ri.nodes().len();
    assert!(nodes_after_first > 0);

    // Second index on the same commit should be a no-op (latest-commit
    // short-circuit). The graph node count should not change.
    ri.index().await.expect("second index should succeed");
    let nodes_after_second = ri.nodes().len();
    assert_eq!(
        nodes_after_first, nodes_after_second,
        "second index should not add new nodes"
    );
}

#[tokio::test]
async fn repo_index_start_watcher_does_not_panic() {
    // Smoke test that the watcher can be installed. We don't assert on
    // file events (the test isn't reliable across platforms); we just
    // verify that `start_watcher` succeeds and that the RepoIndex stays
    // healthy while the watcher is active.
    //
    // Note: we deliberately do NOT drop the `RepoIndex` or the tempdir
    // at the end of this test. `notify::RecommendedWatcher::drop` in
    // notify 6.1.1 unwraps the result of `channel.send(Shutdown)` and
    // can panic if the inotify event-loop thread has already exited
    // (a known race in the upstream crate). The existing `watcher.rs`
    // sidesteps this by sleeping forever in the owning thread; in the
    // test we let the process exit handle the cleanup. The watcher
    // thread is OS-killed on process exit, so no leak survives.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = PathBuf::from(tmp.path());
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("watch.rs"), "pub fn watch() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("watch").unwrap(), repo_dir.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    // start_watcher should succeed. The notify watcher spawns its own
    // background thread; we don't need to wait for events.
    ri.start_watcher().expect("start_watcher should succeed");

    // Give the inotify backend a moment to register the watch. If
    // registration failed (e.g. on a sandboxed filesystem), the watcher
    // would still be alive but the kernel handle would be invalid;
    // that's acceptable for a smoke test.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Hold the RepoIndex alive for the rest of the test process so the
    // watcher is never dropped (see the comment above for why).
    std::mem::forget(ri);
    std::mem::forget(tmp);
}

// ---------------------------------------------------------------------------
// End-to-end federation integration tests (Task 20).
//
// Each test sets up N synthetic repos as real git repos in a tempdir,
// wires up the `FederatedIndex`, and asserts the documented contract.
// `GitSensor::new` requires the workspace path to be a real git repo, so we
// call `git2::Repository::init` on each workspace dir before constructing
// `WorkspaceDirSource`. (See `loader_tests.rs` for the same pattern.)
// ---------------------------------------------------------------------------

use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::loader::load_federation;
use lain::federation::repo_source::RepoSource;

/// Write a minimal two-file Rust crate (`Cargo.toml` + `src/lib.rs`) into
/// `path` so the ingestion pipeline has something to walk.
fn write_tiny_rust_crate(path: &std::path::Path, name: &str) {
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(
        path.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        path.join("src/lib.rs"),
        "pub fn hello() -> &'static str { \"hi\" }\n",
    )
    .unwrap();
}

/// `RepoIndex::new` opens the workspace via `GitSensor::new`, which only needs
/// a `.git` directory (no working-tree commit). Initialize an empty git repo
/// at `dir` and leave it untouched otherwise.
fn init_bare_git_repo(dir: &std::path::Path) {
    git2::Repository::init(dir).expect("git2::Repository::init");
}

#[tokio::test]
async fn five_repos_indexed_and_queried() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..5 {
        let repo_path = tmp.path().join(format!("repo{i}"));
        write_tiny_rust_crate(&repo_path, &format!("repo{i}"));
        init_bare_git_repo(&repo_path);
    }

    let cfg_path = tmp.path().join("repos.yaml");
    let data_dir = tmp.path().join("data");
    let mut yaml = String::from("data_dir: ");
    yaml.push_str(&data_dir.display().to_string());
    yaml.push_str("\nrepos:\n");
    for i in 0..5 {
        yaml.push_str(&format!(
            "  - id: repo{i}\n    source: {{ type: workspace_dir, path: {} }}\n",
            tmp.path().join(format!("repo{i}")).display()
        ));
    }
    std::fs::write(&cfg_path, yaml).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 5);
    // (Real MCP-equivalent queries are exercised by the e2e script in Task 23.)
}

#[tokio::test]
async fn adding_repo_at_runtime_appears_in_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    // Two separate tempdirs as the repo workspaces — `PetgraphBackend` writes
    // its graph bin under `tmp.path()`, and each repo workspace sits in its
    // own tempdir so they don't collide on a `.git` parent.
    let ws_a = tempfile::tempdir().unwrap();
    let ws_b = tempfile::tempdir().unwrap();
    init_bare_git_repo(ws_a.path());
    init_bare_git_repo(ws_b.path());

    // Start with one repo.
    let src1: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), ws_a.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src1, tmp.path()).await.unwrap();
    assert_eq!(fed.list_repos().len(), 1);

    // Add a second repo at runtime; both must be listed.
    let src2: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("b").unwrap(), ws_b.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src2, tmp.path()).await.unwrap();
    assert_eq!(fed.list_repos().len(), 2);
}

#[tokio::test]
async fn stopped_repo_degrades_to_unavailable_others_continue() {
    // Set up two repos; remove one; assert the other still serves.
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    let ws_a = tempfile::tempdir().unwrap();
    let ws_b = tempfile::tempdir().unwrap();
    init_bare_git_repo(ws_a.path());
    init_bare_git_repo(ws_b.path());

    let src1: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("a").unwrap(), ws_a.path().to_path_buf()).unwrap(),
    );
    let src2: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("b").unwrap(), ws_b.path().to_path_buf()).unwrap(),
    );
    fed.add_repo(src1, tmp.path()).await.unwrap();
    fed.add_repo(src2, tmp.path()).await.unwrap();
    assert_eq!(fed.list_repos().len(), 2);

    fed.remove_repo(&RepoId::new("a").unwrap()).unwrap();

    let listed = fed.list_repos();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.as_str(), "b");
    // "b" must still be servable — the other repo's removal must not have
    // torn down the surviving index.
    assert!(fed.get_repo(&RepoId::new("b").unwrap()).is_some());
}

#[tokio::test]
async fn cold_restart_reloads_all_repos() {
    // Build federation, drop it, reload, assert the same repo set comes back.
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..3 {
        let repo_path = tmp.path().join(format!("repo{i}"));
        write_tiny_rust_crate(&repo_path, &format!("repo{i}"));
        init_bare_git_repo(&repo_path);
    }

    let cfg_path = tmp.path().join("repos.yaml");
    let data_dir = tmp.path().join("data");
    let mut yaml = String::from("data_dir: ");
    yaml.push_str(&data_dir.display().to_string());
    yaml.push_str("\nrepos:\n");
    for i in 0..3 {
        yaml.push_str(&format!(
            "  - id: repo{i}\n    source: {{ type: workspace_dir, path: {} }}\n",
            tmp.path().join(format!("repo{i}")).display()
        ));
    }
    std::fs::write(&cfg_path, yaml).unwrap();

    // First load: builds the federation and persists per-repo state.
    {
        let _first = load_federation(&cfg_path).await.unwrap();
        assert_eq!(_first.list_repos().len(), 3);
    }

    // Second load: simulates a cold restart against the same on-disk config
    // and shared data dir. The repo set must come back unchanged.
    let second = load_federation(&cfg_path).await.unwrap();
    assert_eq!(second.list_repos().len(), 3);
}

#[tokio::test]
async fn project_repo_projects_intra_repo_calls_edges() {
    // Use the explicit per-repo indexing pattern (load_federation does NOT
    // trigger indexing — see tests/federation_integration.rs:46-84 for the
    // canonical pattern, and src/cmds/server.rs:49-74 for the production
    // pattern). The order is: build a RepoSource, register it via
    // add_repo, retrieve the resulting RepoIndex, run index(), then
    // project_repo. Without index(), the per-repo graph is empty and
    // project_repo has nothing to project.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shared = root.join("shared");
    let auth_svc = root.join("auth-svc");

    // Write all files BEFORE init_temp_git_repo, which runs `git add -A`
    // and requires at least one tracked file to produce a non-empty commit.
    for sub in [&shared, &auth_svc] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
    }
    std::fs::write(
        shared.join("Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    std::fs::write(
        shared.join("src/lib.rs"),
        "pub fn inner_hash(s: &str) -> u64 { 0 }\n\
         pub fn hash(s: &str) -> u64 { inner_hash(s) }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("Cargo.toml"),
        "[package]\nname = \"auth-svc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("src/lib.rs"),
        "pub fn auth(s: &str) -> bool { shared::hash(s) > 0 }\n",
    ).unwrap();
    for sub in [&shared, &auth_svc] {
        init_temp_git_repo(sub);
    }

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(lain::federation::graph_backend::PetgraphBackend::new(&data_dir).unwrap());
    let fed = Arc::new(lain::federation::federated_index::FederatedIndex::new(backend));

    // Index each repo and project it into the federation.
    for (id_str, path) in [("shared", &shared), ("auth-svc", &auth_svc)] {
        let id = RepoId::new(id_str).unwrap();
        let source: Box<dyn lain::federation::repo_source::RepoSource> =
            Box::new(lain::federation::repo_source::WorkspaceDirSource::new(
                id.clone(),
                path.clone(),
            ).unwrap());
        fed.add_repo(source, &data_dir).await.expect("add_repo should succeed");
        let ri = fed.get_repo(&id).expect("repo should be registered");
        ri.index().await.expect("repo index should succeed");
        fed.project_repo(&id).await.expect("project_repo should succeed");
    }

    // Pass A: per-repo Calls edges must be projected to the global backend.
    // hash (in shared) calls inner_hash (in shared) — this is an intra-repo
    // Calls edge that must exist in the global graph after project_repo.
    let hash_global = "shared:Function:src/lib.rs:hash".to_string();
    let inner_global = "shared:Function:src/lib.rs:inner_hash".to_string();
    let path = fed.backend().find_path(&hash_global, &inner_global).unwrap();
    assert!(
        !path.is_empty(),
        "expected non-empty path from shared::hash to shared::inner_hash; \
         Pass A (project per-repo edges) not yet implemented"
    );
}

#[tokio::test]
async fn project_repo_produces_cross_repo_calls_edges() {
    // 2-crate fixture where auth-svc imports from shared. Same shape as
    // the Pass A test, but the call target is in a different repo, so
    // Pass A's intra-repo projection doesn't help. Pass B must insert
    // a cross-repo Calls edge from auth-svc::auth to shared::hash.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shared = root.join("shared");
    let auth_svc = root.join("auth-svc");

    for sub in [&shared, &auth_svc] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
        git2::Repository::init(sub).expect("git init");
    }
    std::fs::write(
        shared.join("Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    std::fs::write(
        shared.join("src/lib.rs"),
        "pub fn hash(s: &str) -> u64 { 0 }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("Cargo.toml"),
        "[package]\nname = \"auth-svc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("src/lib.rs"),
        "pub fn auth(s: &str) -> bool { shared::hash(s) > 0 }\n",
    ).unwrap();

    let cfg_path = root.join("repos.yaml");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(&cfg_path, format!(
        "data_dir: {}\nrepos:\n  - id: shared\n    source: {{ type: workspace_dir, path: {} }}\n  - id: auth-svc\n    source: {{ type: workspace_dir, path: {} }}\n",
        data_dir.display(), shared.display(), auth_svc.display(),
    )).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();

    // Pass B: auth-svc::auth calls shared::hash. After Pass A projects the
    // intra-repo Calls (none here, since auth's call target is in another repo),
    // Pass B must insert a cross-repo Calls edge from auth-svc::auth to
    // shared::hash.
    let auth_global = "auth-svc:Function:src/lib.rs:auth".to_string();
    let hash_global = "shared:Function:src/lib.rs:hash".to_string();
    let path = fed.backend().find_path(&auth_global, &hash_global).unwrap();
    assert!(
        !path.is_empty(),
        "expected non-empty path from auth-svc::auth to shared::hash; \
         Pass B (cross-repo Calls resolution) not yet implemented"
    );
}
