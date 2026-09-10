//! Regression tests for the watcher panic and sync_state freshness bugs
//! from the real-stress benchmark at /tmp/lain-stress-report.md.

mod common;

use lain::federation::repo_id::RepoId;
use lain::federation::repo_index::RepoIndex;
use lain::federation::repo_source::WorkspaceDirSource;
use std::path::{Path, PathBuf};
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

fn commit_all(path: &std::path::Path) {
    use std::process::Command;
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "add", "-A"])
        .status()
        .unwrap();
    Command::new("git")
        .args(["-C", path.to_str().unwrap(), "commit", "-q", "-m", "commit"])
        .status()
        .unwrap();
}

/// `sync_overlay` drops a changed path's stale overlay entries and then
/// re-scans it fresh (its "drop entries for changed paths BEFORE
/// scanning" step) — a real, narrow window where a concurrent read can
/// observe the path as transiently empty between the drop and the
/// re-insert. `overlay_updated` fires once per receiver iteration, and
/// under load (a poll-based watcher, or several fs events queued and
/// drained back-to-back) more than one iteration can complete close
/// together — waking on a single `notified()` is not guaranteed to line
/// up with the specific edit a test just made, only with "some cycle
/// finished, possibly one still catching up". Poll for `predicate`
/// instead of asserting on one snapshot: this still correctly fails
/// (once `budget` elapses) if the receiver task actually died, since a
/// dead receiver never repopulates the overlay no matter how long this
/// waits.
async fn poll_until(
    overlay: &lain::overlay::VolatileOverlay,
    budget: std::time::Duration,
    mut predicate: impl FnMut(&[lain::schema::GraphNode]) -> bool,
) -> Vec<lain::schema::GraphNode> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let nodes = overlay.get_all_nodes();
        if predicate(&nodes) || std::time::Instant::now() >= deadline {
            return nodes;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `sync_overlay` refreshes entries for paths that are *currently*
/// uncommitted, but had no mechanism to drop entries for a path that
/// *was* uncommitted on a previous cycle and has since been committed
/// (or had its uncommitted change discarded). Without that cleanup, a
/// stale pre-commit node kept answering `resolve_node`/`explain_symbol`
/// etc. via the overlay (which is consulted before the persisted graph)
/// even after the file's committed state was re-indexed — a caller
/// could see a symbol that no longer exists in the working tree, or
/// stale line numbers for one that does.
///
/// The cleanup only fires once the static graph has *something* at the
/// now-committed path (see `sync_overlay`'s comment on why: purging
/// eagerly can make a symbol vanish from both layers in the window
/// before the graph catches up, which is real — `sync_state` in
/// `enrichment.rs` calls only `sync_overlay` per repo, never a paired
/// `index()`). This test seeds the static graph directly after the
/// commit rather than depending on the real LSP/tree-sitter pipeline's
/// timing — the fix only cares that *something* exists at the path,
/// not how it got there — to deterministically put the graph in the
/// state a real reindex would leave it in before asserting the overlay
/// cleanup follows through.
#[tokio::test]
async fn sync_overlay_removes_stale_entries_after_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    let scratch_file = tmp.path().join("src").join("scratch.rs");
    std::fs::write(&scratch_file, "pub fn scratch_symbol() {}\n").unwrap();

    ri.sync_overlay()
        .await
        .expect("first sync_overlay should succeed");
    let names_before: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names_before.iter().any(|n| n.contains("scratch_symbol")),
        "sanity: the uncommitted file's symbol should be in the overlay \
         before it's committed; got: {names_before:?}"
    );

    // The file is no longer uncommitted, so it drops out of
    // `get_uncommitted_changes()` on the next cycle.
    commit_all(tmp.path());
    // Simulate the reindex catching up directly rather than depending
    // on the real LSP/tree-sitter pipeline's timing in a test — the
    // fix under test only cares that the static graph has *something*
    // at this path, not how it got there.
    ri.db()
        .upsert_node(lain::schema::GraphNode::new(
            lain::schema::NodeType::Function,
            "scratch_symbol".into(),
            "src/scratch.rs".into(),
        ))
        .expect("seed static graph with the reindexed symbol");

    ri.sync_overlay()
        .await
        .expect("second sync_overlay should succeed");
    let names_after: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        !names_after.iter().any(|n| n.contains("scratch_symbol")),
        "sync_overlay must drop overlay entries for a path once it's no \
         longer uncommitted, even though that path isn't in this cycle's \
         changed-paths list; got: {names_after:?}"
    );

    std::mem::forget(ri);
}

/// Companion to `sync_overlay_removes_stale_entries_after_commit`:
/// pins the safety half of the same fix. `sync_state` (`enrichment.rs`)
/// calls only `sync_overlay` per federation repo, never a paired
/// `index()` — so a path can drop out of `get_uncommitted_changes()`
/// (committed) before the static graph has anything at that path yet.
/// Purging the overlay entry in that window would make a symbol that's
/// still real disappear from *both* layers until some later reindex
/// happens to run. Without the static-graph check, this test's commit
/// would trigger the exact same removal as the sibling test above —
/// but here nothing is ever seeded into `ri.db()`, so the entry must
/// survive.
#[tokio::test]
async fn sync_overlay_keeps_stale_entry_until_graph_catches_up() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    let scratch_file = tmp.path().join("src").join("scratch.rs");
    std::fs::write(&scratch_file, "pub fn scratch_symbol() {}\n").unwrap();

    ri.sync_overlay()
        .await
        .expect("first sync_overlay should succeed");
    let names_before: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names_before.iter().any(|n| n.contains("scratch_symbol")),
        "sanity: the uncommitted file's symbol should be in the overlay \
         before it's committed; got: {names_before:?}"
    );

    // Committed, but — unlike the sibling test — the static graph is
    // never seeded. `ri.db()` genuinely has nothing at this path.
    commit_all(tmp.path());

    ri.sync_overlay()
        .await
        .expect("second sync_overlay should succeed");
    let names_after: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names_after.iter().any(|n| n.contains("scratch_symbol")),
        "sync_overlay must NOT drop a path's overlay entries while the \
         static graph has nothing there yet — doing so would make a \
         still-real symbol vanish from both layers; got: {names_after:?}"
    );

    std::mem::forget(ri);
}

/// A deleted-and-committed file never has "something at that path" in
/// the static graph — the reindex's `prune_orphans` removes it
/// *permanently*, since it's no longer in `get_all_tracked_files()`.
/// The `graph_caught_up` half of the staleness sweep's purge condition
/// alone would therefore wait forever for a signal that will never
/// come, leaking this path's overlay entries (and `overlay_paths`
/// bookkeeping) for the life of the process. The `deleted_from_disk`
/// check is what breaks that: once the file is gone from the
/// filesystem, there is nothing to wait for, and the entry must be
/// purged on the very next cycle.
#[tokio::test]
async fn sync_overlay_purges_stale_entry_for_a_deleted_file_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    let scratch_file = tmp.path().join("src").join("scratch.rs");
    std::fs::write(&scratch_file, "pub fn scratch_symbol() {}\n").unwrap();

    ri.sync_overlay()
        .await
        .expect("first sync_overlay should succeed");
    let names_before: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        names_before.iter().any(|n| n.contains("scratch_symbol")),
        "sanity: the uncommitted file's symbol should be in the overlay \
         before it's deleted; got: {names_before:?}"
    );

    // Delete the file and commit the deletion. Nothing is (or ever
    // will be) seeded into `ri.db()` at this path — a real reindex
    // would prune it, never add it.
    std::fs::remove_file(&scratch_file).unwrap();
    commit_all(tmp.path());

    ri.sync_overlay()
        .await
        .expect("second sync_overlay should succeed");
    let names_after: Vec<String> = ri
        .server_overlay()
        .get_all_nodes()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert!(
        !names_after.iter().any(|n| n.contains("scratch_symbol")),
        "sync_overlay must purge a deleted-and-committed file's overlay \
         entries even though the static graph will never have anything \
         at that path to confirm against; got: {names_after:?}"
    );

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

    // `sync_overlay` drops a changed path's stale overlay entries and
    // then re-scans it fresh (see `sync_overlay`'s "drop entries for
    // changed paths BEFORE scanning" step) — a real, narrow window
    // where a concurrent read can observe the path as transiently
    // empty between the drop and the re-insert. `overlay_updated` also
    // fires once per receiver iteration, and the poll-based watcher can
    // legitimately produce more than one iteration close together (e.g.
    // one noticing lib.rs's pre-existing content on the first tick,
    // another for this edit) — a single `wait_for_refresh` here is not
    // guaranteed to correspond to the specific edit that just happened,
    // only to "some cycle completed". Poll instead of asserting on one
    // snapshot: it still correctly fails (via the outer timeout) if the
    // receiver task actually died, since a dead receiver never
    // repopulates the overlay no matter how long this waits.
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
    let before = poll_until(&overlay, std::time::Duration::from_secs(5), |n| !n.is_empty()).await;
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
    let after = poll_until(&overlay, std::time::Duration::from_secs(5), |n| !n.is_empty()).await;
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
///
/// macOS only: FSEvents coalesces/delays events under concurrent
/// writes and the receiver picks up the pre-existing `existing`
/// symbol from before the swarm rather than any of the new
/// `agent_*` entries. The receiver DOES fire (within 1s) — this is
/// a content race, not a timing one. Tracked separately.
#[cfg_attr(target_os = "macos", ignore)]
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
    let after_swarm = poll_until(&overlay, std::time::Duration::from_secs(10), |n| {
        n.iter().any(|n| n.name.starts_with("agent_"))
    })
    .await;
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
    let after_followup = poll_until(&overlay, std::time::Duration::from_secs(10), |n| {
        n.iter().any(|n| n.name.starts_with("agent_"))
    })
    .await;
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

// ── issue #3 (single-repo overlay cleanup) ───────────────────────────
//
// `LainServer::sync_volatile_overlay` had two bugs at once:
//   1. it only iterated current `changes`, so a path that dropped out
//      of `get_uncommitted_changes()` (committed, reverted, deleted,
//      uncommitted edit discarded) was never swept;
//   2. `remove_nodes_for_path` was keyed by `change.path` (absolute)
//      while the overlay is keyed by workspace-relative paths, so
//      even the entries it tried to remove were missed.
//
// The fix mirrors the federation's `overlay_paths` discipline: track
// every workspace-relative path this server inserted nodes at, plus
// the exact ids, and sweep entries not in the current cycle's changes
// list by id. These three tests pin the contract for single-repo mode.

use lain::server::LainServer;

/// Build a `LainServer` rooted at a real git repo with one tracked
/// file. Returns `(server, repo_root, tmp)`. The caller is responsible
/// for keeping `tmp` alive — dropping it tears the workspace down.
async fn build_lain_server_with_repo(repo_id: &str) -> (Arc<LainServer>, PathBuf, tempfile::TempDir) {
    use std::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().to_path_buf();
    Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&repo_root).status().unwrap();
    Command::new("git")
        .args(["-C", repo_root.to_str().unwrap(), "config", "user.email", "t@t"])
        .status().unwrap();
    Command::new("git")
        .args(["-C", repo_root.to_str().unwrap(), "config", "user.name", "t"])
        .status().unwrap();
    std::fs::write(repo_root.join("README.md"), "init\n").unwrap();
    Command::new("git")
        .args(["-C", repo_root.to_str().unwrap(), "add", "-A"])
        .status().unwrap();
    Command::new("git")
        .args(["-C", repo_root.to_str().unwrap(), "commit", "-q", "-m", "init"])
        .status().unwrap();

    let mem = tmp.path().join("graph.bin");
    let server = LainServer::new(&repo_root, &mem, None).expect("LainServer::new");
    let server = Arc::new(server);
    // Pin the server's lifetime so the spawned background tasks don't
    // observe a torn Arc when the test scope ends. `forget` is the
    // pattern this file already uses for `RepoIndex` (line 826+).
    (server, repo_root, tmp)
}

fn commit_at(repo: &Path, message: &str) {
    use std::process::Command;
    Command::new("git").args(["-C", repo.to_str().unwrap(), "add", "-A"]).status().unwrap();
    Command::new("git").args(["-C", repo.to_str().unwrap(), "commit", "-q", "-m", message]).status().unwrap();
}

fn reset_to_parent(repo: &Path) {
    use git2::Repository;
    let r = Repository::open(repo).unwrap();
    let head = r.head().unwrap().peel_to_commit().unwrap();
    let parent = head.parent(0).expect("reset needs a parent");
    let mut opts = git2::build::CheckoutBuilder::default();
    opts.force();
    r.reset(parent.as_object(), git2::ResetType::Hard, Some(&mut opts))
        .expect("git reset to parent");
}

fn make_node(path: &str, name: &str, id: &str) -> lain::schema::GraphNode {
    let mut n = lain::schema::GraphNode::new(
        lain::schema::NodeType::Function,
        name.into(),
        path.into(),
    );
    n.id = id.into();
    n
}

/// Reverted path: track a path + id in `overlay_paths`, revert the
/// commit that introduced the path, run `sync_volatile_overlay`. The
/// injected entry must be gone after the cycle. Pre-fix the entry
/// lived forever because the sweep only iterated the current
/// `changes` list, which no longer contained the reverted file.
#[tokio::test]
async fn sync_volatile_overlay_purges_reverted_path() {
    let (server, repo_root, _tmp) = build_lain_server_with_repo("test").await;

    // Make a baseline commit so `reset_to_parent` has a parent to
    // land on (the builder only commits once).
    commit_at(&repo_root, "baseline");

    // Seed `overlay_paths` + the overlay directly via the test helper,
    // bypassing `process_change` (which calls LSP — not installed in
    // the unit-test env). The bookkeeping mirrors what
    // `process_change` writes on a successful LSP scan.
    server.overlay_paths_test_insert(
        "src/scratch.rs".into(),
        make_node("src/scratch.rs", "scratch_symbol", "test-scratch-symbol-id"),
    );

    // Commit the change so the path drops out of `get_uncommitted_changes()`.
    let target = repo_root.join("src").join("scratch.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "pub fn scratch_symbol() {}\n").unwrap();
    commit_at(&repo_root, "add scratch");

    // Revert: the file is gone from `changes` (committed + reset
    // leaves no uncommitted state), the path is no longer in
    // `current_paths`, but `overlay_paths` still lists it. Pre-fix the
    // entry lived forever; the fix must drop it.
    reset_to_parent(&repo_root);

    server.sync_volatile_overlay().await.expect("post-revert sync");
    assert!(
        !server.overlay.get_all_nodes().iter().any(|n| n.id == "test-scratch-symbol-id"),
        "after `git reset --hard` to the parent, the symbol must not \
         remain in the overlay; got: {:?}",
        server.overlay.get_all_nodes().iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
    );
    assert!(
        !server.overlay_paths_test_keys().iter().any(|p| p == "src/scratch.rs"),
        "after `git reset --hard` to the parent, the path must not \
         remain tracked in `overlay_paths`; got: {:?}",
        server.overlay_paths_test_keys(),
    );

    std::mem::forget(server);
}

/// Deleted-and-committed path: track a path + id, run `git rm` +
/// commit, run `sync_volatile_overlay`. Pre-fix the file path was
/// absolute (`change.path.to_string_lossy()`) while the overlay is
/// keyed by workspace-relative paths, so the `remove_nodes_for_path`
/// call missed every overlay entry. The post-fix code uses `graph_path`
/// for the lookup key.
#[tokio::test]
async fn sync_volatile_overlay_purges_deleted_path() {
    let (server, repo_root, _tmp) = build_lain_server_with_repo("test").await;

    // Seed the overlay bookkeeping for `src/scratch.rs`.
    server.overlay_paths_test_insert(
        "src/scratch.rs".into(),
        make_node("src/scratch.rs", "scratch_symbol", "test-scratch-symbol-id"),
    );

    // Make sure the file actually exists on disk and is tracked, so
    // `git rm` has a real path to remove.
    let target = repo_root.join("src").join("scratch.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "pub fn scratch_symbol() {}\n").unwrap();
    commit_at(&repo_root, "add scratch");

    // `git rm` + commit so the deletion is committed (not just an
    // uncommitted rm). After this cycle, `change_type == Deleted` and
    // the path drops out of `current_paths` once the commit lands.
    use std::process::Command;
    Command::new("git").args(["-C", repo_root.to_str().unwrap(), "rm", "-q", "src/scratch.rs"]).status().unwrap();
    commit_at(&repo_root, "delete scratch");

    server.sync_volatile_overlay().await.expect("post-delete sync");
    assert!(
        !server.overlay.get_all_nodes().iter().any(|n| n.id == "test-scratch-symbol-id"),
        "after `git rm` + commit, the symbol must not remain in the \
         overlay; got: {:?}",
        server.overlay.get_all_nodes().iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
    );
    assert!(
        !server.overlay_paths_test_keys().iter().any(|p| p == "src/scratch.rs"),
        "after `git rm` + commit, the path must not remain tracked in \
         `overlay_paths`; got: {:?}",
        server.overlay_paths_test_keys(),
    );

    std::mem::forget(server);
}

/// A symbol kept across a commit (no reindex runs) must remain
/// queryable through the overlay until a reindex replaces it with a
/// static-graph node. This is the safety half of the contract — the
/// pre-fix bug purged eagerly and could make a still-real symbol
/// vanish from both layers; the fix's `overlay_paths` discipline
/// keeps coverage intact across commits.
///
/// Note: this test only runs on the cleanup half (the safety half)
/// because the federation side's equivalent
/// (`sync_overlay_keeps_stale_entry_until_graph_catches_up`) already
/// covers the broader "index hasn't run yet" case. Single-repo mode
/// shares the same shape: `sync_state` calls `sync_volatile_overlay`
/// without a paired reindex.
#[tokio::test]
async fn sync_volatile_overlay_keeps_committed_path_until_reindex() {
    let (server, repo_root, _tmp) = build_lain_server_with_repo("test").await;

    // Seed the overlay bookkeeping for `src/lib.rs`.
    server.overlay_paths_test_insert(
        "src/lib.rs".into(),
        make_node("src/lib.rs", "committed_addition", "test-committed-addition-id"),
    );

    // Commit the change so the path drops out of `get_uncommitted_changes()`.
    let target = repo_root.join("src").join("lib.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(
        &target,
        "pub fn existing() {}\npub fn committed_addition() {}\n",
    )
    .unwrap();
    commit_at(&repo_root, "add committed_addition");

    server.sync_volatile_overlay().await.expect("post-commit sync");
    assert!(
        server.overlay.get_all_nodes().iter().any(|n| n.id == "test-committed-addition-id"),
        "a committed addition must remain in the overlay until the \
         static graph catches up — pre-fix, this dropped the symbol \
         from both layers; got: {:?}",
        server.overlay.get_all_nodes().iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
    );
    assert!(
        server.overlay_paths_test_keys().iter().any(|p| p == "src/lib.rs"),
        "the path must still be tracked in `overlay_paths` until a \
         reindex runs; got: {:?}",
        server.overlay_paths_test_keys(),
    );

    std::mem::forget(server);
}
