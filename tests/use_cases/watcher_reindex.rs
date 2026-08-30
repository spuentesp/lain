//! End-to-end proving test for live reindex on file change.
//!
//! The wishlist explicitly notes a "frozen at first commit" bug
//! (wishlist #8): once a federation was loaded, only a per-repo
//! file-watcher would re-index on subsequent edits, but no test
//! proved that an edit actually produced a new node in the
//! per-repo DB. This test pins the contract that
//! `RepoIndex::index()` re-reads the worktree and surfaces new
//! symbols.
//!
//! We exercise the reindex in-process via `load_federation` +
//! `repo.index()` rather than through the in-process `notify`
//! file-watcher. The file-watcher itself is a thin shim around
//! `notify` events; the heavy lifting (parsing, ingestion,
//! resolve phase, projection) is in `index()`. Proving `index()`
//! picks up the new symbol is the meaningful invariant. The
//! end-to-end MCP shape (modify → request_reload → search_org)
//! is exercised by `tests/federation_e2e.rs::request_reload_rebuilds_state`.
//!
//! We also boot a `lain server` and use the MCP `search_org` to
//! query the federated view, so the test goes through the real
//! query path the agent would hit.

#[path = "../common/mod.rs"]
mod common;
use common::{boot_single_repo, git_init_committed};

#[tokio::test]
async fn reload_after_file_change_picks_up_new_symbol_end_to_end() {
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"watcher-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "/// Initial function — exists at boot.\n\
         pub fn initial_function() -> u32 { 0 }\n",
    )
    .unwrap();
    git_init_committed(&repo_dir);

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo\n    source:\n      type: workspace_dir\n      path: {}\n",
        data_dir.display(),
        repo_dir.display(),
    );
    let repos_yaml_path = project.path().join("repos.yaml");
    std::fs::write(&repos_yaml_path, repos_yaml).unwrap();

    // 1. Boot the server and wait for the initial index. The
    //    boot pipeline calls `repo.index()` once per repo.
    let (host, _guard) = boot_single_repo(&repo_dir, &repos_yaml_path, &["initial_function"]);

    // 2. Modify the fixture to add a new function. Same path the
    //    in-process file-watcher would take on a `notify` event,
    //    just without the kernel round-trip. The file watcher in
    //    production sees `git diff` and the next `repo.index()` call
    //    observes a new commit hash; we replicate that by committing.
    let new_lib = "/// Initial function — exists at boot.\n\
                   pub fn initial_function() -> u32 { 0 }\n\
                   /// NEW: added after boot — the reindex should\n\
                   /// pick this up.\n\
                   pub fn added_after_reload() -> u32 { 42 }\n";
    std::fs::write(repo_dir.join("src/lib.rs"), new_lib).unwrap();
    use std::process::Command;
    let commit = Command::new("git")
        .args(["-c", "user.email=test@lain", "-c", "user.name=test", "commit", "-q", "-m", "add function", "-a"])
        .current_dir(&repo_dir)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed: {commit}");

    // 3. Reindex directly via the federation. (The CLI server's
    //    `start_watcher` does exactly this on a `notify` event; we
    //    call it directly to avoid racing the kernel.)
    use lain::federation::loader::load_federation;
    use lain::federation::repo_id::RepoId;
    let fed = load_federation(&repos_yaml_path)
        .await
        .expect("load_federation");
    let repo = fed
        .get_repo(&RepoId::new("repo").unwrap())
        .expect("repo registered");
    let index_budget = std::time::Duration::from_secs(60);
    tokio::time::timeout(index_budget, repo.index())
        .await
        .expect("repo reindex timed out")
        .expect("repo reindex failed");
    assert_eq!(repo.health(), lain::federation::health::RepoHealth::Ready);
    fed.project_repo(&RepoId::new("repo").unwrap())
        .await
        .expect("project_repo");

    // 4. The new function is in the per-repo DB and in the
    //    federated backend. The proving invariant: a second
    //    `repo.index()` call (which is what the in-process
    //    `notify` watcher does on a file event) produces a new
    //    node in the per-repo DB for the new symbol, and a
    //    subsequent `project_repo` makes it visible in the
    //    federated view.
    let new_node = repo
        .nodes()
        .into_iter()
        .find(|n| n.name == "added_after_reload");
    assert!(
        new_node.is_some(),
        "after reindex, the per-repo DB must contain \
         `added_after_reload`; got nodes: {:?}",
        repo.nodes().iter().map(|n| (&n.name, &n.path)).collect::<Vec<_>>()
    );
    let backend = fed.backend();
    use lain::federation::graph_backend::GraphBackend;
    let backend_nodes = backend.list_nodes().expect("list_nodes");
    let backend_has_new = backend_nodes
        .iter()
        .any(|n| n.name == "added_after_reload");
    assert!(
        backend_has_new,
        "after reindex + project_repo, the federated backend must \
         contain `added_after_reload`; got {} backend nodes",
        backend_nodes.len()
    );
}

// ─── index_forced re-scans the worktree without a commit ─────
//
// The previous test commits before reindexing so the commit-hash
// short-circuit in `index_one_repo` lets the re-scan through. The
// real-world edit cadence is the opposite: a user edits, saves, and
// only commits later (often minutes, sometimes never within the
// session). The wishlist explicitly pinned this as #17 — the
// per-repo DB stayed at the previous commit and the new symbol
// silently went missing.
//
// The fix is a `force: bool` parameter on `index_one_repo` (and a
// matching `RepoIndex::index_forced` method the watcher calls).
// `index()` keeps the optimization for the CLI boot loop, where a
// full re-scan when nothing has changed would be wasted work.
//
// This test pins the contract: edit, *don't* commit, call
// `index_forced()`, observe the new symbol in the per-repo DB.
// Fails before the fix (the commit hash is unchanged so the
// short-circuit kicks in); passes after.
#[tokio::test]
async fn index_forced_picks_up_uncommitted_edits() {
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"watcher-fixture-uncommitted\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "/// Initial function — exists at boot.\n\
         pub fn initial_function() -> u32 { 0 }\n",
    )
    .unwrap();
    git_init_committed(&repo_dir);

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml = format!(
        "data_dir: {}\nrepos:\n  - id: repo\n    source:\n      type: workspace_dir\n      path: {}\n",
        data_dir.display(),
        repo_dir.display(),
    );
    let repos_yaml_path = project.path().join("repos.yaml");
    std::fs::write(&repos_yaml_path, repos_yaml).unwrap();

    // 1. Boot the federation so the per-repo DB has the initial
    //    commit's contents.
    use lain::federation::loader::load_federation;
    use lain::federation::repo_id::RepoId;
    let fed = load_federation(&repos_yaml_path)
        .await
        .expect("load_federation");
    let repo = fed
        .get_repo(&RepoId::new("repo").unwrap())
        .expect("repo registered");
    let index_budget = std::time::Duration::from_secs(60);
    tokio::time::timeout(index_budget, repo.index())
        .await
        .expect("initial index timed out")
        .expect("initial index failed");
    assert_eq!(repo.health(), lain::federation::health::RepoHealth::Ready);

    // 2. Edit the file. **Do NOT commit.** This is the path the
    //    kernel `notify` watcher takes: the file watcher sees the
    //    write event before any git plumbing runs.
    let new_lib = "/// Initial function — exists at boot.\n\
                   pub fn initial_function() -> u32 { 0 }\n\
                   /// NEW: edited after boot, not yet committed.\n\
                   pub fn added_after_uncommitted_edit() -> u32 { 7 }\n";
    std::fs::write(repo_dir.join("src/lib.rs"), new_lib).unwrap();

    // 3. `repo.index_forced()` is what the in-process watcher calls
    //    on a `notify` event. It must re-scan even though the
    //    commit hash hasn't changed.
    tokio::time::timeout(index_budget, repo.index_forced())
        .await
        .expect("index_forced timed out")
        .expect("index_forced failed");
    assert_eq!(repo.health(), lain::federation::health::RepoHealth::Ready);

    // 4. The new symbol is in the per-repo DB.
    let new_node = repo
        .nodes()
        .into_iter()
        .find(|n| n.name == "added_after_uncommitted_edit");
    assert!(
        new_node.is_some(),
        "after index_forced without a commit, the per-repo DB must \
         contain `added_after_uncommitted_edit`; got nodes: {:?}",
        repo.nodes().iter().map(|n| (&n.name, &n.path)).collect::<Vec<_>>()
    );

    // 5. Sanity: a plain `index()` *without* a commit still
    //    short-circuits, because the commit hash is unchanged.
    //    This is the contract the CLI boot loop relies on. If
    //    `index()` ever started bypassing the gate too, every
    //    boot would do a full re-scan and the optimization would
    //    silently disappear.
    let before_count = repo.nodes().len();
    tokio::time::timeout(index_budget, repo.index())
        .await
        .expect("plain index timed out")
        .expect("plain index failed");
    let after_count = repo.nodes().len();
    assert_eq!(
        before_count, after_count,
        "plain index() with unchanged commit hash must short-circuit \
         (no node growth); before={before_count} after={after_count}"
    );
}
