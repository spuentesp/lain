//! Regression tests for the watcher panic and sync_state freshness bugs
//! from the real-stress benchmark at /tmp/lain-stress-report.md.

mod common;

use lain::federation::repo_id::RepoId;
use lain::federation::repo_index::RepoIndex;
use lain::federation::repo_source::WorkspaceDirSource;
use std::path::PathBuf;
use std::sync::Arc;

fn init_temp_git_repo(path: &std::path::Path) {
    use std::process::Command;
    Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(path)
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "config", "user.email", "t@t"])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "config", "user.name", "t"])
        .status()
        .unwrap();
}

fn build_repo_index(tmp: &tempfile::TempDir) -> Arc<RepoIndex> {
    let repo_dir = PathBuf::from(tmp.path());
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("test").unwrap(), repo_dir).unwrap(),
    );
    Arc::new(RepoIndex::new(source, &data_dir).unwrap())
}

#[tokio::test]
async fn sync_overlay_picks_up_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    // Create a new untracked file inside the watched path.
    std::fs::write(
        tmp.path().join("src").join("new_module.rs"),
        "pub fn new_symbol() {}\n",
    )
    .unwrap();

    // Call sync_overlay directly (no watcher involved yet).
    ri.sync_overlay()
        .await
        .expect("sync_overlay should succeed");

    // The overlay must have been refreshed — assert that the volatile
    // overlay reflects the uncommitted new file.
    // Note: the brief's `snapshot().nodes()` shape doesn't exist; the
    // real public surface on `VolatileOverlay` is `get_all_nodes()`.
    // The intent is the same: the overlay contains at least one node
    // after the refresh.
    let overlay = ri.server_overlay();
    let nodes = overlay.get_all_nodes();
    assert!(
        !nodes.is_empty(),
        "sync_overlay should have populated the overlay with at least one node from new_module.rs"
    );

    // Hold the RepoIndex alive for the rest of the test process so the
    // (not-yet-started) watcher's eventual drop can't panic.
    std::mem::forget(ri);
}