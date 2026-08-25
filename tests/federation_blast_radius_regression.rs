//! Regression for a federation ingestion bug that made every id-keyed
//! read on the per-repo graph return empty after `index()`.
//!
//! `RepoIndex::index` cloned `self.db` before calling `index_one_repo`.
//! `GraphDatabase` derives `Clone`, but its `index_map` and
//! `path_index` are `DashMap` instances — not behind an `Arc` — so the
//! clone is independent. Every mutation in the indexer landed on the
//! clone, while `self.db` (the one the server's bound `ToolContext`
//! holds, and the one `RepoIndex::db()` returns) kept its original
//! empty `index_map`. `get_edges_to` needs that map to resolve the
//! target's `NodeIndex`; with no entry it returned `[]`, so blast
//! radius over a federation server reported `(no dependents)` even
//! though the on-disk graph and the petgraph inside `self.db` both
//! held the edges. Loading from disk "worked" because `GraphDatabase::new`
//! rebuilds the index from petgraph weights.
//!
//! This test pins the contract end-to-end through the same call sites
//! the live server uses: `load_federation` → `index()` → `db()` →
//! `get_edges_to` must see the Calls edge.

use lain::federation::loader::load_federation;
use lain::federation::repo_id::RepoId;
use lain::schema::EdgeType;

#[tokio::test]
async fn federation_index_populates_index_map_for_id_keyed_reads() {
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().to_path_buf();
    std::fs::create_dir_all(repo_path.join("src/helper")).unwrap();
    std::fs::write(
        repo_path.join("src/main.rs"),
        "mod helper;\nfn main() { let _ = helper::compute(41); }\n",
    )
    .unwrap();
    std::fs::write(
        repo_path.join("src/helper/mod.rs"),
        "pub fn compute(x: i32) -> i32 { x + 1 }\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(args)
            .status()
            .expect("git failed to start");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    run(&["init", "--quiet", "--initial-branch=main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Regression"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "init"]);

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("repos.yaml");
    let yaml = format!(
        "repos:\n- id: alpha\n  source:\n    type: workspace_dir\n    path: {}\ndata_dir: {}\n",
        repo_path.display(),
        cfg_dir.path().join("data").display(),
    );
    std::fs::write(&cfg_path, yaml).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();
    let id = RepoId::new("alpha").unwrap();
    let alpha = fed.get_repo(&id).expect("alpha present");
    alpha.index().await.expect("index alpha");

    let db = alpha.db();
    let compute = db.find_node_by_name("compute").expect("compute node");

    // The headline regression: an incoming Calls edge must be
    // reachable via the id-keyed index. Pre-fix this returned `[]`
    // because `self.db.index_map` was empty — the indexer had written
    // its mutations to a clone of `self.db`.
    let incoming = db.get_edges_to(&compute.id).expect("edges_to");
    assert!(
        incoming
            .iter()
            .any(|e| matches!(e.edge_type, EdgeType::Calls)),
        "federation repo db must have an incoming Calls edge for compute \
         after index(); got {:?}",
        incoming
    );

    // The duplicate-namespace fix: two files in the same directory
    // emit the same Namespace node (deterministic id). Pre-fix that
    // produced an orphan petgraph entry per file, so the node count
    // exceeded the unique-id count and Contains edges from the first
    // batch pointed at unreachable indices. 2 source files in `src/`
    // + 2 in `src/helper/` → 8 nodes, but only 7 unique ids.
    let nodes = db.all_nodes();
    let mut ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        nodes.len(),
        ids.len(),
        "every petgraph node must have a unique id (no duplicate namespaces)"
    );
}
