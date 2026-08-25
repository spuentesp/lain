//! Stress test for the federation ingestion pipeline.
//!
//! Catches regressions where `RepoIndex::index()` (or any other
//! concurrent index path) would write to a `Clone` of `self.db` whose
//! `DashMap` shards are independent, leaving `&self.db.index_map` (and
//! `&self.db.path_index`) empty. The original bug was
//! `let db = self.db.clone();` followed by `index_one_repo(&path, &db,
//! ...)`. Mutations landed on the clone's `index_map`; the server's
//! bound `&self.db` (the one `RepoIndex::db()` returns, and the one
//! every id-keyed lookup like `get_edges_to` reads) kept its empty map,
//! so `get_edges_to(&id)` returned `[]` even when petgraph held the
//! edge. Federation blast-radius always reported `(no dependents)`, even
//! though the on-disk graph and the petgraph inside `self.db` both
//! held the real edge. Loading from disk "worked" because
//! `GraphDatabase::new` rebuilds the index from petgraph weights.
//!
//! The headline assertion below (`get_edges_to(&compute.id)` returning
//! at least one `Calls` edge) is the assertion that would have caught
//! the original bug: with the pre-fix `let db = self.db.clone()`,
//! `index_map.get(&compute.id)` returns `None` on the bound `&self.db`,
//! so `get_edges_to` short-circuits to `[]` — even though the
//! `StableGraph` (which is `Arc`-shared across clones) holds the edge.
//!
//! These tests pin the contract end-to-end through the same call sites
//! the live server uses: `RepoIndex::new` → `RepoIndex::index` →
//! `alpha.db()` → `get_edges_to` must see the `Calls` edges after each
//! invocation, both for repeated serial calls and for many concurrent
//! indexers on the same `RepoIndex`.
//!
//! Fixture shape (target ~1k–5k nodes / 3k–15k edges to stay well
//! inside the 30s budget but large enough that any "double-clone"
//! race would be visible):
//!
//! ```text
//! src/lib.rs
//!   pub fn compute(x: i32) -> i32 { x + 1 }
//!   pub fn helper_compute(x: i32) -> i32 { x + 2 }
//! src/helper_0000.rs .. src/helper_0220.rs   (220 files)
//!   pub fn caller_<i>_<j>(x: i32) -> i32 {
//!       let a = compute(x);
//!       let b = helper_compute(a);
//!       a + b + j
//!   }
//! ```
//!
//! After one full scan: ~1 `compute` node, ~1 `helper_compute` node,
//! 220 × 5 = 1100 `caller_*` function nodes, 220 file nodes, plus the
//! module/namespace hierarchy. Edges: 1100 Calls into `compute`, 1100
//! into `helper_compute`, plus Contains edges for the file/module/
//! symbol hierarchy and any co-change edges produced by the git stage.
//! All well inside the spec band.

use lain::federation::repo_id::RepoId;
use lain::federation::repo_index::RepoIndex;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::schema::EdgeType;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;

/// Stress fixture shape. Picked so the full scan stays well under the
/// 30s test budget even on a cold tree-sitter cache, while producing
/// enough nodes/edges that any "the indexer wrote to a `Clone`" race
/// would manifest as an empty `index_map` after the call returns.
const FILE_COUNT: usize = 220;
const FUNCS_PER_FILE: usize = 5;
/// Number of serial `index()` calls we expect to be idempotent on the
/// same commit. The first call does the full scan; subsequent calls
/// hit the `last_commit == latest_commit` short-circuit in
/// `index_one_repo`. Either way, the resulting graph state must be
/// identical.
const IDEMPOTENT_CALLS: usize = 5;
/// Number of concurrent `index()` workers on the same `RepoIndex`.
/// The git mutex inside `RepoIndex::index` serializes them, so the
/// effective concurrency is just lock contention + one short-circuit
/// fan-out — but if a future refactor accidentally clones `self.db`
/// per task (instead of `&self.db`), two parallel indexers could both
/// write to independent clones, and the second one's writes would be
/// discarded. The headline assertion below catches it either way.
const CONCURRENT_INDEXERS: usize = 8;
const TARGET_NAME: &str = "compute";
const HELPER_NAME: &str = "helper_compute";

/// Initialize a throwaway git repository at `dir` with a single commit
/// so `GitSensor::new` can open it. Mirrors the helper used by the
/// other federation tests.
fn init_git_repo(dir: &Path) {
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg("--initial-branch=main")
        .arg(dir)
        .status()
        .expect("git init failed to start");
    assert!(status.success(), "git init failed: {status}");

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git failed to start");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    run(&["config", "user.email", "stress@example.com"]);
    run(&["config", "user.name", "Stress Test"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "init"]);
}

/// Build the synthetic fixture described in the module docs.
fn build_fixture(root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");

    // Central target. Every helper file calls both `compute` and
    // `helper_compute`; together they pin the headline invariant on
    // two distinct symbols so a partial fix that only repopulates
    // some index entries is still caught.
    std::fs::write(
        src.join("lib.rs"),
        "/// Central function every stress helper calls.\n\
         pub fn compute(x: i32) -> i32 { x + 1 }\n\
         /// Sibling target so a fix-that-only-handles-one-symbol\n\
         /// cannot satisfy the assertion.\n\
         pub fn helper_compute(x: i32) -> i32 { x + 2 }\n",
    )
    .expect("write lib.rs");

    for i in 0..FILE_COUNT {
        let path = src.join(format!("helper_{i:04}.rs"));
        let mut body = String::with_capacity(FUNCS_PER_FILE * 96);
        body.push_str(&format!("use crate::compute;\nuse crate::helper_compute;\n"));
        for j in 0..FUNCS_PER_FILE {
            // Two user-defined Calls per function body (compute +
            // helper_compute), so each caller emits exactly two static
            // refs into the resolve phase. That gives 2 × 1100 = 2200
            // Calls edges into the two targets, comfortably inside
            // the spec edge band.
            body.push_str(&format!(
                "pub fn caller_{i}_{j}(x: i32) -> i32 {{\n    \
                    let a = compute(x);\n    \
                    let b = helper_compute(a);\n    \
                    a + b + {j}\n    \
                 }}\n"
            ));
        }
        std::fs::write(&path, body).expect("write helper");
    }
}

/// Check the per-repo graph after `index()`. Returns the
/// `(node_count, edge_count)` so the caller can assert stability
/// across repeated invocations. Panics with a regression-pinning
/// message if any invariant fails.
fn assert_post_index_invariants(alpha: &Arc<RepoIndex>, label: &str) -> (usize, usize) {
    let db = alpha.db();
    let nodes = db.all_nodes();
    let edges = db.all_edges();

    assert!(
        !nodes.is_empty(),
        "[{label}] expected non-empty nodes after index(); got 0"
    );

    // ─── Headline regression ───────────────────────────────────────────
    // An incoming `Calls` edge must be reachable via the id-keyed
    // index. Pre-fix, `self.db.index_map` was empty because the
    // indexer had written to a `Clone` of `self.db` whose DashMap
    // shards are independent. `petgraph` IS `Arc`-shared across
    // clones, so the nodes/edges land in `self.db.graph`; but
    // `get_edges_to` needs `index_map` to resolve the target's
    // NodeIndex. Empty `index_map` → empty result → federation
    // blast-radius always reports "(no dependents)". If this
    // assertion fails after a refactor, the original bug is back.
    let compute = db.find_node_by_name(TARGET_NAME).unwrap_or_else(|| {
        panic!(
            "[{label}] expected a node named `{TARGET_NAME}` after index(); \
             got {} nodes total",
            nodes.len()
        )
    });
    let incoming = db.get_edges_to(&compute.id).unwrap_or_else(|e| {
        panic!("[{label}] get_edges_to({}) failed: {e}", compute.id)
    });
    let has_calls = incoming
        .iter()
        .any(|e| matches!(e.edge_type, EdgeType::Calls));
    assert!(
        has_calls,
        "[{label}] federation RepoIndex::db MUST have an incoming Calls edge \
         for `{TARGET_NAME}` after index(); got edges={incoming:?}. This is the \
         regression: `self.db.index_map` is empty because the indexer wrote \
         to a Clone of `self.db` whose DashMap shards are independent. \
         Restore the `&self.db` borrow in RepoIndex::index (see \
         tests/federation_blast_radius_regression.rs for the original fix)."
    );

    // Pin the second target too: if a future refactor accidentally
    // fixes only the first symbol's id resolution, this still fails.
    let helper = db
        .find_node_by_name(HELPER_NAME)
        .unwrap_or_else(|| panic!("[{label}] expected a `{HELPER_NAME}` node"));
    let helper_incoming = db.get_edges_to(&helper.id).unwrap_or_default();
    assert!(
        helper_incoming
            .iter()
            .any(|e| matches!(e.edge_type, EdgeType::Calls)),
        "[{label}] expected at least one Calls edge into `{HELPER_NAME}`; got {:?}",
        helper_incoming
    );

    // Id-keyed lookup by name also still works (sanity; this one
    // uses the Arc-shared petgraph directly, not the index_map, so
    // it does NOT catch the bug on its own).
    assert!(
        db.find_node_by_name(TARGET_NAME).is_some(),
        "[{label}] find_node_by_name(`{TARGET_NAME}`) must return Some"
    );

    (nodes.len(), edges.len())
}

fn build_repoindex(repo_path: &std::path::PathBuf, data_dir: &Path, repo_id: &str) -> Arc<RepoIndex> {
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new(repo_id).unwrap(), repo_path.clone())
            .expect("workspace dir source"),
    );
    Arc::new(RepoIndex::new(source, data_dir).expect("RepoIndex::new"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_is_idempotent_across_five_serial_calls() {
    let started = Instant::now();

    let repo_dir = tempfile::tempdir().expect("tempdir repo");
    let repo_path = repo_dir.path().to_path_buf();
    build_fixture(&repo_path);
    init_git_repo(&repo_path);

    let data_dir = repo_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let alpha = build_repoindex(&repo_path, &data_dir, "alpha");

    // First call does the full scan; subsequent calls hit the
    // `last_commit == latest_commit` short-circuit in
    // `index_one_repo` and return Ok without mutating the graph.
    // Every call must produce identical (nodes, edges) counts and
    // every call must satisfy the headline `get_edges_to` invariant.
    let mut counts: Vec<(usize, usize)> = Vec::with_capacity(IDEMPOTENT_CALLS);
    for i in 0..IDEMPOTENT_CALLS {
        alpha
            .index()
            .await
            .unwrap_or_else(|e| panic!("index call #{i} failed: {e}"));
        let (n, e) = assert_post_index_invariants(&alpha, &format!("serial-#{i}"));
        counts.push((n, e));
    }

    let (n0, e0) = counts[0];
    for (i, (n, e)) in counts.iter().enumerate().skip(1) {
        assert_eq!(
            (*n, *e),
            (n0, e0),
            "iteration {i} produced ({n}, {e}); expected stable ({n0}, {e0})"
        );
    }

    // Spec band: nodes in 1k..=5k, edges in 3k..=15k. The exact
    // counts depend on how many helpers / contains / co-change
    // edges the pipeline emits, but the fixture is sized to land
    // inside the band on any reasonable run.
    assert!(
        (1000..=5000).contains(&n0),
        "fixture produced {n0} nodes; expected 1000..=5000"
    );
    assert!(
        (3000..=15000).contains(&e0),
        "fixture produced {e0} edges; expected 3000..=15000"
    );

    eprintln!(
        "[stress] serial: {FILE_COUNT} files × {FUNCS_PER_FILE} funcs + lib.rs → \
         n={n0} e={e0} after {IDEMPOTENT_CALLS} index() calls in {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_indexers_on_same_repoindex_preserve_index_map() {
    let started = Instant::now();

    let repo_dir = tempfile::tempdir().expect("tempdir repo");
    let repo_path = repo_dir.path().to_path_buf();
    build_fixture(&repo_path);
    init_git_repo(&repo_path);

    let data_dir = repo_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let alpha = build_repoindex(&repo_path, &data_dir, "concurrent");

    // Race N concurrent `index()` calls on the same `RepoIndex`.
    // The git mutex inside `RepoIndex::index` serializes them, so
    // the first worker that grabs the lock does the full scan and
    // subsequent workers see the same commit and short-circuit.
    // The point isn't to win the race — it's to catch any future
    // regression where two parallel indexers both write to
    // independent clones of `self.db`. The git mutex is the only
    // thing keeping the writes ordered today, and this test pins
    // the final invariant regardless of who wins the lock.
    let mut joinset: JoinSet<()> = JoinSet::new();
    for worker in 0..CONCURRENT_INDEXERS {
        let me = Arc::clone(&alpha);
        joinset.spawn(async move {
            me.index()
                .await
                .unwrap_or_else(|e| panic!("worker {worker} index failed: {e}"));
        });
    }
    while let Some(res) = joinset.join_next().await {
        res.expect("JoinSet task panicked");
    }

    let (n, e) = assert_post_index_invariants(&alpha, "concurrent");

    // Same spec band as the serial test — concurrent indexing must
    // produce the same graph.
    assert!(
        (1000..=5000).contains(&n),
        "concurrent fixture produced {n} nodes; expected 1000..=5000"
    );
    assert!(
        (3000..=15000).contains(&e),
        "concurrent fixture produced {e} edges; expected 3000..=15000"
    );

    eprintln!(
        "[stress] concurrent: {CONCURRENT_INDEXERS} indexers on one RepoIndex → \
         n={n} e={e} in {:?}",
        started.elapsed()
    );
}