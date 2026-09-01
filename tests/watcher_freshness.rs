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
    // Content check: assert the LSP-derived symbol from new_module.rs
    // actually landed in the overlay. A bare `!is_empty()` would also
    // pass if the overlay picked up an unrelated symbol (e.g., from the
    // repo's pre-existing lib.rs before the new file was added), so
    // this matches the regression we actually want to catch — the
    // watcher receiver not picking up the new file.
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("new_symbol")),
        "sync_overlay should have populated the overlay with the new \
         symbol from new_module.rs; got nodes: {names:?}"
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

    // Subscribe to the receiver's "I finished an index + sync_overlay
    // cycle" signal so we wake as soon as the receiver has processed
    // each edit, instead of guessing a wall-clock sleep budget. The
    // 5 s ceiling catches a wedged receiver without making the test
    // slow on healthy runs.
    let overlay_notify = ri.overlay_updated();
    let wait_for_refresh = |label: String| {
        let overlay_notify = Arc::clone(&overlay_notify);
        async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                overlay_notify.notified(),
            )
            .await
            .unwrap_or_else(|_| panic!("{label}: receiver did not refresh overlay within 15s"))
        }
    };

    // Modify a tracked file. Pre-fix, this would panic the inotify thread.
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* edited */ }\n").unwrap();

    wait_for_refresh("first edit".to_string()).await;

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
    let before = overlay.get_all_nodes();
    assert!(
        !before.is_empty(),
        "after the first edit, the receiver task should have refreshed \
         the overlay with at least one node from the edited lib.rs; an \
         empty overlay here means the receiver panicked or never ran \
         `sync_overlay`"
    );

    // Second edit — verify the receiver task is still processing events
    // (this is the regression check for the panic).
    std::fs::write(&target, "pub fn existing() { /* second edit */ }\n").unwrap();
    wait_for_refresh("second edit".to_string()).await;

    // Re-read the overlay after the second edit. If the receiver task
    // had died after the first event, this call returns an empty
    // overlay (because no further `sync_overlay` runs) and the assertion
    // below fails — giving us a Rust-level signal that the watcher
    // panicked, not just a process-level "did the test crash".
    let after = overlay.get_all_nodes();
    assert!(
        !after.is_empty(),
        "after the second edit, the overlay should still be populated by \
         the receiver task; an empty overlay here means the receiver \
         stopped processing events after the first one"
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
    let jobs = std::sync::Arc::new(parking_lot::Mutex::new(HashMap::<
        String,
        lain::server::tools::JobInfo,
    >::new()));
    let last_outcome = std::sync::Arc::new(parking_lot::Mutex::new(
        lain::server::refresh::RefreshOutcome::default(),
    ));

    // `sync_state` is a sync function. Its jobs-registry argument is a
    // `parking_lot::Mutex`, so we can call it directly from the
    // `current_thread` runtime that `#[tokio::test]` defaults to — no
    // `spawn_blocking` hop required.
    sync_state(
        &graph,
        &git,
        &ingestion,
        &jobs,
        &last_outcome,
        Some(&fed),
    )
    .expect("sync_state should not error");

    // The spawned task is async; poll the overlay for up to ~15s.
    // Pre-fix, sync_state short-circuited on commit equality and the
    // overlay stayed empty. After the fix, sync_state walks every
    // repo in the federation and calls sync_overlay, which writes
    // the new file's symbols into the shared overlay.
    //
    // Content check: not just "non-empty" but specifically contains
    // the LSP-derived symbol from the new untracked file. A bare
    // `!is_empty()` would pass against a stale overlay (e.g., the
    // pre-existing `existing()` from lib.rs), which is the kind of
    // false-pass the original bug relied on.
    let expected = "post_sync_symbol";
    let mut found = false;
    for _ in 0..300 {
        let names: Vec<String> = shared_overlay
            .get_all_nodes()
            .into_iter()
            .map(|n| n.name)
            .collect();
        if names.iter().any(|n| n.contains(expected)) {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        found,
        "sync_state with fed=Some(&fed) should refresh the overlay so \
         the new untracked file's symbols become visible (looking for \
         a name containing {expected:?})"
    );

    // Hold both handles alive so the spawned background task doesn't
    // race with their drop.
    std::mem::forget(ri);
    std::mem::forget(fed);
}

/// Regression test for `sync_state` with more than one repo in the
/// federation. The pre-Phase-2-parallelism code iterated the repos
/// serially with `for ... .await`, so a federation with N repos paid
/// N×(per-repo sync_overlay cost) on every `sync_state` call. This
/// test stands up two repos with distinct uncommitted files and
/// asserts both files' symbols end up in the shared overlay.
///
/// Correctness-wise the test passes against the serial code too — it
/// exists to guard against regressions when Phase 2 moves to
/// `tokio::task::JoinSet` (any bug in the parallel handoff would
/// surface here).
#[tokio::test]
async fn sync_state_refreshes_overlay_for_multiple_repos() {
    use lain::federation::federated_index::FederatedIndex;
    use lain::federation::graph_backend::PetgraphBackend;
    use lain::federation::repo_id::RepoId as TestRepoId;
    use lain::federation::repo_source::WorkspaceDirSource;
    use lain::graph::GraphDatabase;
    use lain::overlay::VolatileOverlay;
    use lain::server::tools::handlers::enrichment::sync_state;
    use lain::tuning::IngestionConfig;
    use std::collections::HashMap;

    let tmp = tempfile::tempdir().unwrap();
    let repos_root = tmp.path().join("repos");
    std::fs::create_dir_all(&repos_root).unwrap();

    // Two repos, each with its own directory under repos_root. We
    // register both with the federation; each gets the shared overlay
    // wired in via `set_overlay` after construction (same trick the
    // single-repo test uses).
    let shared_overlay = Arc::new(VolatileOverlay::new());

    let mut repo_paths = Vec::new();
    for name in ["alpha", "beta"] {
        let repo_dir = repos_root.join(name);
        std::fs::create_dir_all(repo_dir.join("src")).unwrap();
        std::fs::write(
            repo_dir.join("src").join("lib.rs"),
            "pub fn existing() {}\n",
        )
        .unwrap();
        init_temp_git_repo(&repo_dir);
        repo_paths.push((name.to_string(), repo_dir));
    }

    // The graph + git fixtures only need to be valid; we exercise the
    // overlay-refresh phase, not the commit/co-change path.
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();

    let fed_data_dir = tmp.path().join("fed");
    std::fs::create_dir_all(&fed_data_dir).unwrap();
    let backend: Arc<dyn lain::server::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&fed_data_dir).expect("PetgraphBackend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.install_overlay(shared_overlay.clone());

    let mut registered_ids = Vec::new();
    for (name, repo_dir) in &repo_paths {
        let source = Box::new(
            WorkspaceDirSource::new(TestRepoId::new(name).unwrap(), repo_dir.clone()).unwrap(),
        );
        fed.add_repo(source, &fed_data_dir.join(name))
            .await
            .expect("add_repo");
        registered_ids.push((name.clone(), repo_dir));
    }

    // Pre-condition: the federation sees exactly two repos, both with
    // their default (empty) per-repo overlay.
    assert_eq!(fed.list_repos().len(), 2);
    assert!(
        shared_overlay.get_all_nodes().is_empty(),
        "shared overlay should be empty before sync_state"
    );

    // Drop a NEW untracked file into each repo BEFORE sync_state. The
    // overlay-refresh phase must end up reflecting both files.
    for (name, repo_dir) in &registered_ids {
        std::fs::write(
            repo_dir.join("src").join(format!("post_sync_{name}.rs")),
            format!("pub fn post_sync_{name}_symbol() {{}}\n"),
        )
        .unwrap();
    }

    // Build the sync_state args. Each repo's `GitSensor` is independent
    // (we only need one for the test; both repos are git-tracked).
    let primary_repo_dir = &repo_paths[0].1;
    let git = std::sync::Arc::new(parking_lot::Mutex::new(
        lain::git::GitSensor::new(primary_repo_dir).unwrap(),
    ));
    let ingestion = IngestionConfig::default();
    let jobs = std::sync::Arc::new(parking_lot::Mutex::new(HashMap::<
        String,
        lain::server::tools::JobInfo,
    >::new()));
    let last_outcome = std::sync::Arc::new(parking_lot::Mutex::new(
        lain::server::refresh::RefreshOutcome::default(),
    ));

    sync_state(
        &graph,
        &git,
        &ingestion,
        &jobs,
        &last_outcome,
        Some(&fed),
    )
    .expect("sync_state should not error");

    // Poll the overlay for up to ~15s. Cold LSP startup can take a
    // couple of seconds on the first repo, and with two repos the
    // slowest leg dominates. 15s is well under the test's overall
    // budget but generous enough to absorb first-call LSP warm-up
    // on slow CI runners.
    let mut populated_alpha = false;
    let mut populated_beta = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(15) {
        let names: Vec<String> = shared_overlay
            .get_all_nodes()
            .into_iter()
            .map(|n| n.name)
            .collect();
        populated_alpha = names.iter().any(|n| n.contains("post_sync_alpha"));
        populated_beta = names.iter().any(|n| n.contains("post_sync_beta"));
        if populated_alpha && populated_beta {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        populated_alpha,
        "shared overlay missing post_sync_alpha_symbol after sync_state over 2 repos; nodes: {:?}",
        shared_overlay.get_all_nodes().iter().map(|n| (&n.name, &n.node_type)).collect::<Vec<_>>()
    );
    assert!(
        populated_beta,
        "shared overlay missing post_sync_beta_symbol after sync_state over 2 repos; nodes: {:?}",
        shared_overlay.get_all_nodes().iter().map(|n| (&n.name, &n.node_type)).collect::<Vec<_>>()
    );

    // Hold handles alive so the spawned background task doesn't race
    // with their drop.
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

    // Subscribe to the receiver's "I finished an index + sync_overlay
    // cycle" signal. Each of the six writes will produce one
    // notification; the receiver processes events serially and fires
    // `notify_one()` per cycle, so permits accumulate if the test
    // hasn't awaited yet — the first `notified().await` returns as
    // soon as at least one cycle has finished.
    let overlay_notify = ri.overlay_updated();
    let wait_for_one_refresh = |label: String| {
        let overlay_notify = Arc::clone(&overlay_notify);
        async move {
            // 30 s budget: under six concurrent writers the receiver
            // processes events serially, so the slowest cycle dominates
            // and cold LSP warm-up on the first event can stack on top.
            // 15 s was tight even on Linux CI; 30 s gives headroom for
            // slow runners without making a healthy run noticeably
            // slower (a successful cycle finishes in well under a
            // second).
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                overlay_notify.notified(),
            )
            .await
            .unwrap_or_else(|_| panic!("{label}: receiver did not refresh overlay within 30s"))
        }
    };

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

    // Wait for the receiver to process at least one of the six events.
    // Once that signal arrives we know the channel handoff + receiver
    // task survived a concurrent storm — the regression we're
    // guarding against.
    wait_for_one_refresh("swarm".to_string()).await;

    // Regression check (strengthened from "no panic"): the receiver
    // task kept the overlay populated, AND it picked up at least one
    // symbol from the storm. A bare `!is_empty()` would pass if the
    // overlay still held the pre-existing `existing` symbol from
    // lib.rs but no agent_* entries landed — which is exactly the
    // failure mode a wedged receiver under load would produce.
    let after_swarm = overlay.get_all_nodes();
    let after_swarm_names: Vec<String> =
        after_swarm.into_iter().map(|n| n.name).collect();
    assert!(
        after_swarm_names.iter().any(|n| n.starts_with("agent_")),
        "after six concurrent writes + receiver signal, the overlay \
         should contain at least one agent_* symbol from the new \
         files; got: {after_swarm_names:?}"
    );

    // The receiver task is still alive if no panic has occurred. We
    // assert by sending one more event and verifying the overlay's
    // freshness advances (a follow-up edit flows through).
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* after the swarm */ }\n").unwrap();
    wait_for_one_refresh("follow-up".to_string()).await;

    // Second regression check: the receiver is still processing events
    // after the swarm. Content assertion that an `agent_*` symbol
    // still exists — the edit to lib.rs doesn't introduce new symbols,
    // so the agent_* entries from the swarm must still be present.
    let after_followup = overlay.get_all_nodes();
    let after_followup_names: Vec<String> =
        after_followup.into_iter().map(|n| n.name).collect();
    assert!(
        after_followup_names.iter().any(|n| n.starts_with("agent_")),
        "after the follow-up edit + receiver signal, the overlay should \
         still contain at least one agent_* symbol from the swarm; \
         got: {after_followup_names:?}"
    );

    // Hold the RepoIndex alive for the rest of the test process.
    std::mem::forget(ri);
}
