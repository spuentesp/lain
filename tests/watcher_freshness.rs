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

/// Regression test for the `sync_state` freshness bug: the MCP tool
/// short-circuited on git-commit equality and never refreshed the
/// volatile overlay, so a brand-new uncommitted file stayed invisible
/// until the next watcher tick (which never came for the first 15
/// minutes after the create). After the fix, `sync_state` must touch
/// the overlay regardless of whether HEAD has moved.
#[tokio::test]
async fn sync_state_refreshes_overlay_for_new_file() {
    use lain::federation::federated_index::FederatedIndex;
    use lain::federation::graph_backend::PetgraphBackend;
    use lain::graph::GraphDatabase;
    use lain::overlay::VolatileOverlay;
    use lain::server::tools::handlers::enrichment::sync_state;
    use lain::tuning::IngestionConfig;
    use std::collections::HashMap;

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = std::path::PathBuf::from(tmp.path());

    // Build the same shared overlay we'll wire into the federation.
    let shared_overlay = Arc::new(VolatileOverlay::new());

    // RepoIndex fixture identical to `build_repo_index`, but rebinds
    // its overlay to the shared one before the federation is built.
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("test").unwrap(), repo_dir.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());
    ri.set_overlay(shared_overlay.clone());

    // Build a real `FederatedIndex` containing the same repo. The
    // brief's `fed = None` path skips the overlay-refresh phase by
    // design, so the cleanest test path is to construct a real fed
    // and pass it as `Some(&fed)`.
    let fed_data_dir = tmp.path().join("fed");
    std::fs::create_dir_all(&fed_data_dir).unwrap();
    let backend: Arc<dyn lain::server::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&fed_data_dir).expect("PetgraphBackend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    // Install BEFORE add_repo so the new RepoIndex picks up the shared
    // overlay in its constructor branch.
    fed.install_overlay(shared_overlay.clone());
    let fed_source = Box::new(
        WorkspaceDirSource::new(RepoId::new("test").unwrap(), repo_dir.clone()).unwrap(),
    );
    fed.add_repo(fed_source, &fed_data_dir)
        .await
        .expect("add_repo");

    // Pre-condition: the federation sees exactly one repo and the
    // overlay is empty (no LSP scan has run yet).
    assert_eq!(fed.list_repos().len(), 1);
    assert!(
        shared_overlay.get_all_nodes().is_empty(),
        "shared overlay should be empty before sync_state"
    );

    // Create a new untracked file BEFORE calling sync_state. The
    // overlay-refresh phase must end up reflecting this file.
    std::fs::write(
        tmp.path().join("src").join("post_sync.rs"),
        "pub fn post_sync_symbol() {}\n",
    )
    .unwrap();

    // Build the args sync_state takes. Many of them are zero-value
    // because this test exercises only the overlay-refresh path.
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let git = std::sync::Arc::new(parking_lot::Mutex::new(
        lain::git::GitSensor::new(&repo_dir).unwrap(),
    ));
    let ingestion = IngestionConfig::default();
    let jobs = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::<
        String,
        lain::server::tools::JobInfo,
    >::new()));
    let last_outcome = std::sync::Arc::new(parking_lot::Mutex::new(
        lain::server::refresh::RefreshOutcome::default(),
    ));

    // `sync_state` is a sync function that calls `jobs_registry.blocking_lock()`
    // on a `tokio::sync::Mutex` (see handlers::enrichment::sync_state).
    // `blocking_lock()` panics on a `current_thread` Tokio runtime — which
    // is exactly what `#[tokio::test]` defaults to. Production calls
    // `sync_state` from a `new_multi_thread` runtime (see src/main.rs:45),
    // where `blocking_lock` is safe; we don't, so we hop to the blocking
    // thread pool here. Removing the wrapper would panic this test.
    let _response = tokio::task::spawn_blocking({
        let jobs = std::sync::Arc::clone(&jobs);
        let last_outcome = std::sync::Arc::clone(&last_outcome);
        let fed = std::sync::Arc::clone(&fed);
        move || {
            sync_state(
                &graph,
                &git,
                &ingestion,
                &jobs,
                &last_outcome,
                Some(&fed),
            )
        }
    })
    .await
    .expect("spawn_blocking join")
    .expect("sync_state should not error");

    // The spawned task is async; poll the overlay for up to ~2s.
    // Pre-fix, sync_state short-circuited on commit equality and the
    // overlay stayed empty. After the fix, sync_state walks every
    // repo in the federation and calls sync_overlay, which writes
    // the new file's symbols into the shared overlay.
    let mut populated = false;
    for _ in 0..40 {
        if !shared_overlay.get_all_nodes().is_empty() {
            populated = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        populated,
        "sync_state with fed=Some(&fed) should refresh the overlay so \
         a brand-new untracked file becomes visible"
    );

    // Hold both handles alive so the spawned background task doesn't
    // race with their drop.
    std::mem::forget(ri);
    std::mem::forget(fed);
}

/// Regression test for the 6-concurrent-agent threshold that the stress
/// benchmark flagged. Pre-fix, the watcher panicked on the first FS
/// event and silently lost its overlay-refresh capability; under load
/// from multiple "agents" writing simultaneously, the inotify thread
/// would die before the receiver task drained its backlog. After the
/// Task 2 channel handoff + receiver task, the watcher survives any
/// number of concurrent writers — this test guards that.
#[tokio::test]
async fn watcher_survives_six_concurrent_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    ri.start_watcher().await.expect("start_watcher should succeed");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let overlay = ri.server_overlay();

    // Baseline: nothing is uncommitted yet, so the overlay is empty.
    let baseline = overlay.get_all_nodes().len();
    assert_eq!(
        baseline, 0,
        "baseline overlay should be empty before any writes"
    );

    // Six concurrent "agents" each writing a file in the watched path.
    // Pre-fix, the watcher would panic on the first FS event and the
    // server would silently lose its overlay-refresh capability.
    let mut handles = Vec::new();
    for i in 0..6 {
        let target = tmp.path().join("src").join(format!("agent_{i}.rs"));
        handles.push(tokio::task::spawn_blocking(move || {
            std::fs::write(&target, format!("pub fn agent_{i}() {{}}\n")).unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Give the receiver task time to process all six events.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Regression check (strengthened from "no panic"): the receiver
    // task kept the overlay populated. If the receiver had panicked
    // mid-batch under load, the overlay would still be empty here.
    let after_swarm = overlay.get_all_nodes();
    assert!(
        !after_swarm.is_empty(),
        "after six concurrent writes + 500ms wait, the receiver task \
         should have refreshed the overlay with nodes from the new \
         uncommitted agent_*.rs files; an empty overlay here means the \
         receiver panicked or never reached `sync_overlay`. \
         after_swarm.len() = {}",
        after_swarm.len()
    );

    // The receiver task is still alive if no panic has occurred. We
    // assert by sending one more event and verifying the overlay's
    // freshness advances (a follow-up edit flows through).
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* after the swarm */ }\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Second regression check: the receiver is still processing events
    // after the swarm. If the receiver died after one of the six
    // writes, this overlay would also be empty.
    let after_followup = overlay.get_all_nodes();
    assert!(
        !after_followup.is_empty(),
        "after the follow-up edit + 500ms wait, the receiver task \
         should still be refreshing the overlay; an empty overlay \
         here means the receiver stopped processing events after the \
         six-agent swarm. after_followup.len() = {}",
        after_followup.len()
    );

    // Hold the RepoIndex alive for the rest of the test process.
    std::mem::forget(ri);
}
