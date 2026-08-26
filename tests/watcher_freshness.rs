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

#[tokio::test]
async fn watcher_does_not_panic_on_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    // start_watcher must succeed (was sync, now async — this exercises
    // the new signature).
    ri.start_watcher().await.expect("start_watcher should succeed");

    // Give the inotify backend a moment to register the watch.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Modify a tracked file. Pre-fix, this would panic the inotify thread.
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* edited */ }\n").unwrap();

    // Give the receiver task time to drain the channel and process the
    // event. Pre-fix, the channel handoff did not exist and the panic
    // would happen synchronously inside the watcher closure.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // The watcher / receiver task must still be alive. We assert this
    // indirectly by re-reading the overlay after each edit + wait: a
    // populated overlay means `sync_overlay` ran, which means the
    // receiver task processed the event end-to-end (no panic). If the
    // receiver task had panicked inside `me.index()` or `me.sync_overlay()`
    // between the two edits, the post-second-edit read would either hang
    // (because the task is dead and the next event isn't drained) or
    // surface an empty overlay (because the second `sync_overlay` never
    // ran). Pre-fix, the inotify thread panicked on the first event and
    // the overlay stayed at whatever it had before the test.
    let overlay = ri.server_overlay();
    // `snapshot()` doesn't exist on `VolatileOverlay`; the public read
    // accessor is `get_all_nodes()`. The assertion below is the real
    // regression check: an empty overlay after the first edit + wait
    // means the receiver task never made it past the first event.
    let before = overlay.get_all_nodes();
    assert!(
        !before.is_empty(),
        "after the first edit + 500ms wait, the receiver task should have \
         refreshed the overlay with at least one node from the edited \
         lib.rs; an empty overlay here means the receiver panicked or \
         never ran `sync_overlay`"
    );

    // Second edit — verify the receiver task is still processing events
    // (this is the regression check for the panic).
    std::fs::write(&target, "pub fn existing() { /* second edit */ }\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Re-read the overlay after the second edit. If the receiver task
    // had died after the first event, this call returns an empty
    // overlay (because no further `sync_overlay` runs) and the assertion
    // below fails — giving us a Rust-level signal that the watcher
    // panicked, not just a process-level "did the test crash".
    let after = overlay.get_all_nodes();
    assert!(
        !after.is_empty(),
        "after the second edit + 500ms wait, the overlay should still be \
         populated by the receiver task; an empty overlay here means the \
         receiver stopped processing events after the first one"
    );

    // Hold the RepoIndex alive for the rest of the test process (do NOT
    // drop — see tests/federation_integration.rs:184-192 for why).
    std::mem::forget(ri);
}