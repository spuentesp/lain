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
