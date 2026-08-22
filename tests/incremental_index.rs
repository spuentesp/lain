//! Incremental re-index must not destroy edges it does not rebuild.
//!
//! `replace_nodes_for_paths` removes a path's nodes with
//! `graph.remove_node`, and petgraph takes every incident edge with the
//! node — including *incoming* edges from files that are not part of
//! this pass. A full scan re-resolves everything afterwards so the loss
//! is invisible; an incremental pass only scans the changed files, so
//! callers in unchanged files are never re-resolved and their edges are
//! gone for good.
//!
//! The symptom this reproduces was found in a production graph: 37 of
//! 335 files had symbols with no edges at all, and functions that were
//! demonstrably called (`run_query`, `walk_up_for_git`,
//! `append_edit_event`) had zero incoming and outgoing edges.

use lain::server::federation::repo_id::RepoId;
use lain::server::federation::repo_index::RepoIndex;
use lain::server::federation::repo_source::WorkspaceDirSource;
use lain::server::schema::EdgeType;
use std::process::Command;
use std::sync::Arc;

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("git failed to start");
    assert!(status.success(), "git {args:?} failed: {status}");
}

fn init_repo(dir: &std::path::Path) {
    let status = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .arg(dir)
        .status()
        .expect("git init failed to start");
    assert!(status.success());
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Incremental Test"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "initial"]);
}

/// Count `Calls` edges pointing *into* the named symbol.
fn incoming_calls(ri: &Arc<RepoIndex>, name: &str) -> usize {
    let nodes = ri.nodes();
    let Some(target) = nodes.iter().find(|n| n.name == name) else {
        return 0;
    };
    ri.db()
        .get_edges_to(&target.id)
        .unwrap_or_default()
        .iter()
        .filter(|e| e.edge_type == EdgeType::Calls)
        .count()
}

#[tokio::test]
async fn incremental_reindex_keeps_callers_in_unchanged_files() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();

    // `caller.rs` is never touched again; `target.rs` is what changes.
    std::fs::write(
        src.join("target.rs"),
        "pub fn the_target() -> u32 { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("caller.rs"),
        "use crate::target::the_target;\npub fn calls_it() -> u32 { the_target() + 1 }\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("inc").unwrap(), repo.clone()).unwrap(),
    );
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    ri.index().await.expect("full index");
    let before = incoming_calls(&ri, "the_target");
    assert!(
        before > 0,
        "fixture is wrong: the full index found no caller for the_target"
    );

    // Change only `target.rs`, so the incremental pass re-scans it and
    // nothing else. `caller.rs` is untouched and will not be re-resolved.
    std::fs::write(
        src.join("target.rs"),
        "pub fn the_target() -> u32 { 2 }\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "touch target only"]);

    ri.index().await.expect("incremental index");
    let after = incoming_calls(&ri, "the_target");

    assert_eq!(
        after, before,
        "incremental re-index destroyed the caller edge: {before} incoming Calls before, \
         {after} after. `replace_nodes_for_paths` removed target.rs's nodes (taking the \
         incoming edge with them) and the pass only re-resolved refs from the files it \
         scanned, which did not include caller.rs."
    );
}

#[tokio::test]
async fn deleting_a_symbol_still_drops_its_inbound_edges() {
    // The complement of the test above: preserving inbound edges must
    // not resurrect edges into symbols that genuinely went away. Node
    // ids are deterministic, so a deleted symbol simply never comes
    // back and its restored edge finds no endpoint.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("target.rs"), "pub fn the_target() -> u32 { 1 }\n").unwrap();
    std::fs::write(
        src.join("caller.rs"),
        "use crate::target::the_target;\npub fn calls_it() -> u32 { the_target() + 1 }\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("del").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());
    ri.index().await.expect("full index");
    assert!(incoming_calls(&ri, "the_target") > 0, "fixture: no caller found");

    // Delete the symbol outright.
    std::fs::write(src.join("target.rs"), "pub fn something_else() -> u32 { 2 }\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "delete the_target"]);
    ri.index().await.expect("incremental index");

    let nodes = ri.nodes();
    assert!(
        !nodes.iter().any(|n| n.name == "the_target"),
        "a deleted symbol must not survive the re-index"
    );
    assert_eq!(
        incoming_calls(&ri, "the_target"),
        0,
        "edges into a deleted symbol must stay dropped"
    );
}

#[tokio::test]
async fn repeated_reindex_does_not_accumulate_duplicate_edges() {
    // Restoring inbound edges must be idempotent: re-indexing in a loop
    // is the normal steady state (the file watcher drives it), and a
    // duplicate per pass would inflate every fan-in metric in the graph.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("target.rs"), "pub fn the_target() -> u32 { 0 }\n").unwrap();
    std::fs::write(
        src.join("caller.rs"),
        "use crate::target::the_target;\npub fn calls_it() -> u32 { the_target() + 1 }\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("dup").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());
    ri.index().await.expect("full index");
    let baseline = incoming_calls(&ri, "the_target");
    assert!(baseline > 0, "fixture: no caller found");

    for i in 1..=3 {
        std::fs::write(
            src.join("target.rs"),
            format!("pub fn the_target() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", &format!("bump {i}")]);
        ri.index().await.expect("incremental index");
        assert_eq!(
            incoming_calls(&ri, "the_target"),
            baseline,
            "pass {i}: inbound Calls count drifted from {baseline}"
        );
    }
}

/// Two independent full indexes of the same commit must produce the
/// same graph.
///
/// Two production indexes of commit `9756156` were observed at
/// 3769 nodes / 9869 edges and 3340 nodes / 15540 edges — same repo,
/// same binary, materially different graphs. Every metric derived from
/// the graph inherits that variance, so "the graph disagrees with
/// itself between runs" undermines every answer built on it.
#[tokio::test]
async fn two_full_indexes_of_one_commit_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Several files with cross-file calls, so the graph has real
    // structure to disagree about.
    std::fs::write(src.join("a.rs"), "pub fn a_one() -> u32 { 1 }\npub fn a_two() -> u32 { a_one() + 1 }\n").unwrap();
    std::fs::write(src.join("b.rs"), "use crate::a::a_one;\npub fn b_one() -> u32 { a_one() * 2 }\n").unwrap();
    std::fs::write(src.join("c.rs"), "use crate::b::b_one;\npub fn c_one() -> u32 { b_one() + a_helper() }\npub fn a_helper() -> u32 { 3 }\n").unwrap();
    init_repo(&repo);

    let index_once = |slot: &str| {
        let data_dir = tmp.path().join(slot);
        std::fs::create_dir_all(&data_dir).unwrap();
        let source = Box::new(
            WorkspaceDirSource::new(RepoId::new(slot).unwrap(), repo.clone()).unwrap(),
        );
        Arc::new(RepoIndex::new(source, &data_dir).unwrap())
    };

    let first = index_once("data-a");
    first.index().await.expect("first full index");
    let second = index_once("data-b");
    second.index().await.expect("second full index");

    let count_edges = |ri: &Arc<RepoIndex>| -> usize {
        ri.nodes()
            .iter()
            .map(|n| ri.db().get_edges_from(&n.id).unwrap_or_default().len())
            .sum()
    };

    assert_eq!(
        first.nodes().len(),
        second.nodes().len(),
        "two full indexes of one commit produced different node counts"
    );
    assert_eq!(
        count_edges(&first),
        count_edges(&second),
        "two full indexes of one commit produced different edge counts"
    );
}

/// A file deleted from git must lose its nodes on the next incremental
/// index.
///
/// `replace_nodes_for_paths` only touches paths present in the current
/// scan, and an incremental pass scans `get_changed_files_since`. If a
/// deletion does not put the path into that set with an empty node
/// list, the file's symbols stay in the graph forever — which would
/// explain a long-lived index carrying *more* nodes than a fresh one
/// of the same commit (observed: 3769 vs 3340).
#[tokio::test]
async fn deleting_a_file_removes_its_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    let src = repo.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("keep.rs"), "pub fn kept() -> u32 { 1 }\n").unwrap();
    std::fs::write(src.join("gone.rs"), "pub fn doomed() -> u32 { 2 }\n").unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("rm").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());
    ri.index().await.expect("full index");
    assert!(
        ri.nodes().iter().any(|n| n.name == "doomed"),
        "fixture: doomed was never indexed"
    );

    git(&repo, &["rm", "--quiet", "src/gone.rs"]);
    git(&repo, &["commit", "--quiet", "-m", "delete gone.rs"]);
    ri.index().await.expect("incremental index");

    assert!(
        !ri.nodes().iter().any(|n| n.name == "doomed"),
        "a symbol from a deleted file is still in the graph after re-index"
    );
    assert!(
        ri.nodes().iter().any(|n| n.name == "kept"),
        "the surviving file's symbols must not be collateral"
    );
}
