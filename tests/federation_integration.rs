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
async fn repo_index_index_completes_within_timeout() {
    // Regression test for the hang observed in
    // `repo_index_indexes_files_via_index_one_repo` on systems where a
    // real language server (rust-analyzer) is on `PATH`.
    //
    // Before the fix, `RepoIndex::index()` itself completed quickly but
    // the *Drop* chain hung on the tokio current_thread worker: the
    // `lsp_bridge` crate's `LspProcess::drop` calls
    // `futures::executor::block_on(self.kill())` to reap the spawned
    // language server child, which deadlocks when the only thread in the
    // runtime is parked in `block_on`. From the test runner's
    // perspective the future after `index().await` was "still running"
    // indefinitely, even though `index()` had already returned Ok.
    //
    // After the fix, `RepoIndex::Drop` synchronously shuts down the LSP
    // pool on a fresh OS thread with its own runtime before the bridges
    // drop, so no `LspProcess` is left to be reaped on the test worker.
    // The `index()` call is also wrapped in a top-level
    // `tokio::time::timeout` as a belt-and-suspenders cap.
    //
    // This test wraps `index()` in its own `tokio::time::timeout` and
    // asserts that the inner call completed before the budget. If the
    // Drop chain re-introduces a hang, the test fails — but more
    // importantly, the test ends in bounded wall-clock time even when
    // the bug regresses, instead of stalling the whole test suite.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().to_path_buf();
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("hello.rs"), "pub fn greet() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("timeout").unwrap(), repo_dir.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    // 30s is well above the production `INDEX_TIMEOUT` (60s) but well
    // below cargo-test's 60s "still running" watchdog. If this fires,
    // the underlying Drop chain is hanging again and the production
    // shutdown fix has regressed.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ri.index(),
    )
    .await
    .expect("RepoIndex::index did not complete within 30s — Drop shutdown regressed");
    result.expect("RepoIndex::index returned an error");
    assert_eq!(ri.health(), RepoHealth::Ready);

    // Holding `ri` and `tmp` alive until here means the Drop chain runs
    // while the test future is still in flight. If the Drop hangs,
    // cargo-test reports "still running for over 60 seconds"; if it
    // completes, the test future resolves and we hit the assertions.
    drop(ri);
    drop(tmp);
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

// ---------------------------------------------------------------------------
// `LainServer::set_workspace` shares a lock with the MCP dispatcher.
//
// Regression test for the Task 6.9 hot-reload lock split. Before the fix,
// `LainServer::federation_workspaces` and `LainMcpServer::workspaces`
// were two separate `Arc<RwLock<WorkspacesFile>>` cells, so writing
// through the server slot wrote to dead space — the MCP dispatcher's
// read lock never saw the new contents. The fix makes `LainServer` and
// `LainMcpServer` share one lock; this test proves the live behavior
// by building a `LainServer`, calling `set_workspace` (the rebuild
// flow), and reading back through the same handle the MCP dispatcher
// would use.
// ---------------------------------------------------------------------------

use lain::federation::workspace::{WorkspaceSpec, WorkspacesFile};
use lain::server::{LainServer, Transport};

#[tokio::test]
async fn lain_server_set_workspace_is_visible_to_mcp_dispatcher() {
    // Build a tiny federation with three repos so the workspace
    // member-resolution path has something to consult.
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    for id in ["repo-a", "repo-b", "repo-c"] {
        let ws = tempfile::tempdir().unwrap();
        init_bare_git_repo(ws.path());
        let src: Box<dyn RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new(id).unwrap(), ws.path().to_path_buf())
                .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
    }

    // Start the server with workspace A (2 members).
    let ws_a = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "auth-ws".into(),
            description: Some("initial".into()),
            source: None,
            members: vec!["repo-a".into(), "repo-b".into()],
        }],
    });
    let server = LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        Transport::Stdio,
        0,
        Arc::clone(&ws_a),
        None,
    )
    .expect("with_federation_and_workspaces");

    // The server must expose the workspaces handle, and that handle
    // is the SAME `Arc<RwLock<WorkspacesFile>>` the MCP dispatcher
    // inside `serve()` would be given. Before the fix, the server
    // had no workspaces_handle() and the MCP server wrapped its own
    // private lock — the two were never comparable.
    let server_handle = server
        .workspaces_handle()
        .expect("server built with workspaces must expose a handle");

    // Initial state: list_workspaces through the shared lock shows 2 members.
    {
        let guard = server_handle.read();
        let infos = lain::mcp::federation_tools::list_workspaces(&guard, None);
        assert_eq!(infos.len(), 1, "expected exactly one workspace");
        assert_eq!(infos[0].name, "auth-ws");
        assert_eq!(infos[0].member_count, 2, "initial member_count");
    }

    // Mutate workspaces.yaml: add a 3rd member. The rebuild flow
    // calls `LainServer::set_workspace` after re-reading the file.
    let ws_a_prime = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "auth-ws".into(),
            description: Some("after rebuild".into()),
            source: None,
            members: vec!["repo-a".into(), "repo-b".into(), "repo-c".into()],
        }],
    });
    server.set_workspace(ws_a_prime);

    // The MCP dispatcher's view (read through the shared lock) must
    // observe the new member set on the very next dispatch. Before
    // the fix this assertion failed because the MCP server's lock
    // was a separate cell that nobody had written to.
    let detail = {
        let guard = server_handle.read();
        lain::mcp::federation_tools::get_workspace(&fed, &guard, "auth-ws")
            .expect("get_workspace should resolve the auth-ws workspace")
    };
    let member_ids: Vec<&str> = detail.members.iter().map(|m| m.repo_id.as_str()).collect();
    assert_eq!(
        member_ids,
        vec!["repo-a", "repo-b", "repo-c"],
        "get_workspace must observe the 3rd member after set_workspace, got {member_ids:?}",
    );

    // The list_workspaces view must also reflect the swap.
    let infos = {
        let guard = server_handle.read();
        lain::mcp::federation_tools::list_workspaces(&guard, None)
    };
    assert_eq!(
        infos[0].member_count, 3,
        "list_workspaces must reflect the swap — got {infos:?}",
    );
    assert_eq!(infos[0].description.as_deref(), Some("after rebuild"));
}


// ---------------------------------------------------------------------------
// `detect_overlap` — commit-time symbol overlap between two git refs.
//
// The tool answers "if I merge <head> into <base>, which symbols did both
// sides touch?" It resolves the named federation workspace to its member
// repos, runs `git diff --name-only <base> <head>` in each member's
// worktree, then extracts the symbol set of every touched file at both
// refs (via `git show <ref>:<path>` + tree-sitter) and intersects them.
// ---------------------------------------------------------------------------

/// Initialize a git repo at `dir` with a committable local identity. Unlike
/// `init_bare_git_repo` this configures `user.name` / `user.email` so the
/// test can create real commits, which `detect_overlap` needs in order to
/// diff two refs.
fn init_committable_git_repo(dir: &std::path::Path) {
    let repo = git2::Repository::init(dir).expect("git2::Repository::init");
    let mut cfg = repo.config().expect("repo config");
    cfg.set_str("user.email", "test@example.com").unwrap();
    cfg.set_str("user.name", "Overlap Test").unwrap();
}

/// Run `git <args>` in `dir`, asserting success, and return stdout.
fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git failed to start");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[tokio::test]
async fn detect_overlap_reports_shared_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    // The member repo lives in its own tempdir so the PetgraphBackend's
    // graph bin (under `tmp`) is not swept into the repo's worktree.
    let ws_repo = tempfile::tempdir().unwrap();
    let repo_dir = ws_repo.path().to_path_buf();
    init_committable_git_repo(&repo_dir);

    // Base commit: `login` + `logout`.
    std::fs::write(
        repo_dir.join("auth.rs"),
        "pub fn login() -> &'static str { \"A\" }\npub fn logout() {}\n",
    )
    .unwrap();
    git_out(&repo_dir, &["add", "auth.rs"]);
    git_out(&repo_dir, &["commit", "--quiet", "-m", "base"]);
    let base_oid = git_out(&repo_dir, &["rev-parse", "HEAD"]).trim().to_string();

    // Head commit: `login` body changes (shared symbol → overlap), `logout`
    // is deleted, `refresh` is new, and a brand-new file `token.rs` appears
    // (no base-side symbols → no overlap).
    std::fs::write(
        repo_dir.join("auth.rs"),
        "pub fn login() -> &'static str { \"B\" }\npub fn refresh() {}\n",
    )
    .unwrap();
    std::fs::write(repo_dir.join("token.rs"), "pub fn mint() -> u32 { 7 }\n").unwrap();
    git_out(&repo_dir, &["add", "auth.rs", "token.rs"]);
    git_out(&repo_dir, &["commit", "--quiet", "-m", "head"]);

    // Register the repo with the federation and declare a workspace over it.
    let src: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("auth-svc").unwrap(), repo_dir.clone()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let workspaces = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "auth-ws".into(),
            description: None,
            source: None,
            members: vec!["auth-svc".into()],
        }],
    });
    let server = LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        Transport::Stdio,
        0,
        workspaces,
        None,
    )
    .expect("with_federation_and_workspaces");

    let out = lain::mcp::presence_tools::run_detect_overlap(
        &server,
        serde_json::json!({
            "base": base_oid,
            "head": "HEAD",
            "workspace": "auth-ws",
        }),
    )
    .expect("detect_overlap should succeed");

    assert_eq!(out["base"].as_str(), Some(base_oid.as_str()));
    assert_eq!(out["head"].as_str(), Some("HEAD"));
    assert_eq!(
        out["total_overlaps"].as_u64(),
        Some(1),
        "only `login` is touched on both sides — got {out}"
    );

    let files = out["files"].as_array().expect("files must be an array");
    assert_eq!(files.len(), 2, "auth.rs + token.rs — got {files:?}");

    let auth = files
        .iter()
        .find(|f| f["path"] == "auth.rs")
        .expect("auth.rs entry missing");
    assert_eq!(auth["repo"].as_str(), Some("auth-svc"));
    assert_eq!(
        auth["symbols_base"].as_array().unwrap(),
        &vec![
            serde_json::json!("login"),
            serde_json::json!("logout")
        ],
    );
    assert_eq!(
        auth["symbols_head"].as_array().unwrap(),
        &vec![
            serde_json::json!("login"),
            serde_json::json!("refresh")
        ],
    );
    assert_eq!(
        auth["overlap"].as_array().unwrap(),
        &vec![serde_json::json!("login")],
    );
    // One shared function weighs 4 → "medium" under the graduated scale
    // (>= 6 high, >= 3 medium, else low). Accept "high" too so the assertion
    // stays about "this is a real conflict signal" rather than the exact band.
    assert!(
        matches!(auth["severity"].as_str(), Some("high") | Some("medium")),
        "expected high or medium severity for shared fn, got: {}",
        auth["severity"]
    );

    let token = files
        .iter()
        .find(|f| f["path"] == "token.rs")
        .expect("token.rs entry missing");
    assert!(
        token["symbols_base"].as_array().unwrap().is_empty(),
        "token.rs did not exist at base — got {token}"
    );
    assert_eq!(
        token["symbols_head"].as_array().unwrap(),
        &vec![serde_json::json!("mint")],
    );
    assert!(token["overlap"].as_array().unwrap().is_empty());
    assert_eq!(token["severity"].as_str(), Some("none"));
}

#[tokio::test]
async fn detect_overlap_rejects_unknown_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    let ws_repo = tempfile::tempdir().unwrap();
    init_bare_git_repo(ws_repo.path());
    let src: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("solo").unwrap(), ws_repo.path().to_path_buf())
            .unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let workspaces = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "solo-ws".into(),
            description: None,
            source: None,
            members: vec!["solo".into()],
        }],
    });
    let server = LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        Transport::Stdio,
        0,
        workspaces,
        None,
    )
    .expect("with_federation_and_workspaces");

    let err = lain::mcp::presence_tools::run_detect_overlap(
        &server,
        serde_json::json!({ "base": "HEAD~1", "workspace": "nope" }),
    )
    .expect_err("unknown workspace must be an error");
    assert!(
        err.contains("nope"),
        "error should name the missing workspace — got {err}"
    );
}

/// Two shared functions (4 + 4 = 8) must clear the `high` threshold, so the
/// top band stays reachable through the real git + tree-sitter pipeline and
/// not just in the weighting unit test below.
#[tokio::test]
async fn detect_overlap_two_shared_functions_is_high() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(tmp.path()).unwrap());
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(backend));

    let ws_repo = tempfile::tempdir().unwrap();
    let repo_dir = ws_repo.path().to_path_buf();
    init_committable_git_repo(&repo_dir);

    std::fs::write(
        repo_dir.join("auth.rs"),
        "pub fn login() -> &'static str { \"A\" }\npub fn logout() -> u32 { 1 }\n",
    )
    .unwrap();
    git_out(&repo_dir, &["add", "auth.rs"]);
    git_out(&repo_dir, &["commit", "--quiet", "-m", "base"]);
    let base_oid = git_out(&repo_dir, &["rev-parse", "HEAD"]).trim().to_string();

    // Both functions survive with changed bodies → both overlap.
    std::fs::write(
        repo_dir.join("auth.rs"),
        "pub fn login() -> &'static str { \"B\" }\npub fn logout() -> u32 { 2 }\n",
    )
    .unwrap();
    git_out(&repo_dir, &["add", "auth.rs"]);
    git_out(&repo_dir, &["commit", "--quiet", "-m", "head"]);

    let src: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("auth-svc").unwrap(), repo_dir.clone()).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();

    let workspaces = Arc::new(WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "auth-ws".into(),
            description: None,
            source: None,
            members: vec!["auth-svc".into()],
        }],
    });
    let server = LainServer::with_federation_and_workspaces(
        Arc::clone(&fed),
        Transport::Stdio,
        0,
        workspaces,
        None,
    )
    .expect("with_federation_and_workspaces");

    let out = lain::mcp::presence_tools::run_detect_overlap(
        &server,
        serde_json::json!({
            "base": base_oid,
            "head": "HEAD",
            "workspace": "auth-ws",
        }),
    )
    .expect("detect_overlap should succeed");

    assert_eq!(out["total_overlaps"].as_u64(), Some(2), "got {out}");
    let auth = out["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "auth.rs")
        .expect("auth.rs entry missing");
    assert_eq!(
        auth["severity"].as_str(),
        Some("high"),
        "two shared functions weigh 8 → high, got {auth}"
    );
}

// Note: graduated-band unit pinning (`none|low|medium|high`) lives in
// `src/server/mcp/presence_tools.rs`'s `#[cfg(test)] mod`, where
// `overlap_severity` is private. The end-to-end MCP path here exercises
// the `medium` and `high` bands via real git + tree-sitter.

// ---------------------------------------------------------------------------
// Single-repo federation: per-repo structural tools must see the real graph
// (not the placeholder staging dir).
//
// Before this fix, `with_federation` built the executor's
// `ToolContext::graph` from a fresh `GraphDatabase::new(<staging
// dir>/.lain/graph.bin)` — an empty sled DB that the executor's per-repo
// tool handlers (`find_anchors`, `explain_symbol`, `get_blast_radius`,
// `query_graph`, `get_function_callers`, `get_function_callees`) then
// queried. Result: in a real federation with thousands of nodes, the
// per-repo tools reported "0 anchors" / "node not found" while the
// federation-level tools (`list_repos`, `search_org`) reported a
// fully-populated graph. Confident false negatives.
//
// The fix: when the federation has exactly one repo, the executor's
// graph is that repo's indexed `GraphDatabase`. Multi-repo
// federations still bind to the placeholder and need the round-2
// federation-aware handler refactor; this test pins the single-repo
// case so the regression can't reappear silently.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_repo_federation_binds_per_repo_tools_to_real_graph() {
    // Repo with a tiny Rust crate — `find_anchors` should find at
    // least one node (`hello`) in the file/module hierarchy even
    // without a full LSP pass.
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().to_path_buf();
    write_tiny_rust_crate(&repo_path, "alpha");
    init_bare_git_repo(&repo_path);
    // Commit the tiny crate so the default branch (master) exists
    // AND `git.get_all_tracked_files()` returns the crate's files
    // (an empty `--allow-empty` commit would yield 0 files and the
    // indexer would short-circuit before populating the graph).
    {
        let status = std::process::Command::new("git")
            .args(["-C", repo_path.to_str().unwrap()])
            .args(["-c", "user.email=test@x"])
            .args(["-c", "user.name=t"])
            .args(["add", "-A"])
            .status()
            .expect("git add");
        assert!(status.success(), "git add failed: {status:?}");
        let status = std::process::Command::new("git")
            .args(["-C", repo_path.to_str().unwrap()])
            .args(["-c", "user.email=test@x"])
            .args(["-c", "user.name=t"])
            .args(["commit", "-m", "init alpha"])
            .status()
            .expect("git commit");
        assert!(status.success(), "git commit failed: {status:?}");
    }

    // Build a single-repo federation via a temp repos.yaml.
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path: PathBuf = cfg_dir.path().join("repos.yaml");
    let yaml = format!(
        "repos:\n- id: alpha\n  source:\n    type: workspace_dir\n    path: {}\n    ref: HEAD\ndata_dir: {}\nready_threshold: 0.5\n",
        repo_path.display(),
        cfg_dir.path().join("data").display(),
    );
    std::fs::write(&cfg_path, yaml).unwrap();
    let fed = load_federation(&cfg_path).await.unwrap();
    assert_eq!(fed.list_repos().len(), 1, "federation must have exactly one repo");

    // Index the repo: `load_federation` + `add_repo` only sets up
    // the sled DB; the actual indexed content is materialized by
    // `RepoIndex::index` (tree-sitter extract → LSP hydrate →
    // git co-change). `project_repo` then mirrors those nodes
    // into the federation's in-memory backend for cross-repo
    // queries. Without `index` the per-repo db is empty and the
    // test is meaningless.
    let alpha_id_proj = RepoId::new("alpha").unwrap();
    let alpha_repo = fed.get_repo(&alpha_id_proj).expect("alpha present");
    alpha_repo.index().await.expect("index alpha");
    fed.project_repo(&alpha_id_proj).await.expect("project_repo");

    // Build a LainServer in federation mode. The single-repo
    // fix should bind `tool_executor.graph` to the federation's
    // indexed repo graph. Clone the Arc so we can still read
    // `fed.get_repo(...).db()` below to compare against the
    // executor's view.
    let server = LainServer::with_federation(Arc::clone(&fed), Transport::Stdio, 0, None)
        .expect("with_federation");

    // The per-repo tool's view of the world. `find_anchors` calls
    // `graph.find_anchors(limit)` which is the very first read of
    // the executor's `ctx.graph`. Before the fix this returned 0
    // because the executor was bound to the empty staging DB.
    let raw = lain::server::tools::handlers::metrics::find_anchors(
        server.tool_executor.graph(),
        server.tool_executor.overlay(),
        10,
    )
    .expect("find_anchors should not error");
    // The exact text varies (anchors vs "No anchors"), but it must
    // NOT be the "0 anchors in empty staging" failure that the
    // pre-fix server returned. The test asserts the graph is
    // reachable from the executor by counting nodes.
    let alpha_id = RepoId::new("alpha").unwrap();
    let alpha_repo = fed
        .get_repo(&alpha_id)
        .expect("alpha repo present");
    let repo_graph = alpha_repo.db();
    let executor_count = server.tool_executor.graph().node_count();
    let real_count = repo_graph.node_count();
    assert_eq!(
        executor_count, real_count,
        "single-repo executor must share the repo's indexed graph (executor={executor_count}, real={real_count})"
    );
    // Also: the placeholder is NOT empty (we wrote one Rust file
    // with one function), so the equality test above would already
    // pass. The additional sanity check is that we don't get
    // the historical "0 anchors in empty staging" output.
    assert!(
        real_count > 0,
        "indexed repo should have at least one node; got {real_count}"
    );
    // Suppress the unused-warning for the `raw` capture while
    // keeping the find_anchors call as a smoke test of the
    // dispatch path. (The real assertion is on graph identity.)
    let _ = raw;
}

#[tokio::test]
async fn multi_repo_federation_falls_back_to_placeholder() {
    // Two repos → multi-repo federation → the placeholder graph
    // still binds the executor. Per-repo tools still won't work,
    // but the executor must construct cleanly and the placeholder
    // must be the SAME empty DB regardless of repo count. This
    // pins the "only the single-repo path got the fix" contract.
    let ws_a = tempfile::tempdir().unwrap();
    let ws_b = tempfile::tempdir().unwrap();
    init_bare_git_repo(ws_a.path());
    init_bare_git_repo(ws_b.path());

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path: PathBuf = cfg_dir.path().join("repos.yaml");
    let yaml = format!(
        "repos:\n- id: a\n  source:\n    type: workspace_dir\n    path: {}\n    ref: HEAD\n- id: b\n  source:\n    type: workspace_dir\n    path: {}\n    ref: HEAD\ndata_dir: {}\nready_threshold: 0.5\n",
        ws_a.path().display(),
        ws_b.path().display(),
        cfg_dir.path().join("data").display(),
    );
    std::fs::write(&cfg_path, yaml).unwrap();
    let fed = load_federation(&cfg_path).await.unwrap();
    assert_eq!(fed.list_repos().len(), 2);

    let server = LainServer::with_federation(Arc::clone(&fed), Transport::Stdio, 0, None)
        .expect("with_federation");

    // The placeholder path: the executor's graph is fresh and
    // empty (0 nodes), not bound to either repo. This is the
    // known limitation the next round-2 refactor will address.
    assert_eq!(
        server.tool_executor.graph().node_count(),
        0,
        "multi-repo federation still binds the placeholder; round-2 will fix this"
    );
}
