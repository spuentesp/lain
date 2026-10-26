//! Federation performance tests on synthetic graphs.
//!
//! The graph is written straight into a petgraph backend, not through repo
//! sensors, so the numbers measure traversal and bookkeeping rather than
//! parsing. Each repo `r{i}` holds a chain `r{i}f{j} --Calls--> r{i}f{j-1}`.
//!
//! - `small_fixture_blast_radius_under_100ms_p99`: 10 repos x 5 000 nodes,
//!   runs with the ordinary test suite. It checks every answer and reports
//!   the latency. The 100 ms p99 target is enforced with
//!   `LAIN_BENCH_ENFORCE=1` on a `--release` build.
//! - `large_fixture_health_at_scale`: 200 repos x 50 000 nodes. Ignored by
//!   default because it needs several gigabytes; run it with
//!   `cargo test --test federation_benchmark -- --ignored --nocapture`.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::mcp::federation_tools::{get_cross_repo_blast_radius, get_federation_health};
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

const FILE: &str = "src/lib.rs";

fn name(repo: usize, j: usize) -> String {
    format!("r{repo}f{j}")
}

/// A federation of `repos` repos with `nodes_per_repo` functions each, every
/// function calling the one before it in its repo. Written in batches under a
/// bulk section, so the backing file is saved once at the end.
fn synthetic_federation(dir: &Path, repos: usize, nodes_per_repo: usize) -> FederatedIndex {
    let backend = Arc::new(PetgraphBackend::new(dir).expect("petgraph backend"));
    let fed = FederatedIndex::new(backend.clone());
    backend.begin_bulk();
    for r in 0..repos {
        let repo = RepoId::new(&format!("r{r}")).expect("repo id");
        let ids: Vec<String> = (0..nodes_per_repo)
            .map(|j| {
                fed.global_id(&repo, NodeType::Function, FILE, &name(r, j), None)
                    .as_str()
                    .to_string()
            })
            .collect();
        let nodes: Vec<GraphNode> = ids
            .iter()
            .enumerate()
            .map(|(j, id)| {
                let mut node = GraphNode::new(NodeType::Function, name(r, j), FILE.into());
                node.id = id.clone();
                node
            })
            .collect();
        backend.upsert_nodes_batch(&nodes).expect("node batch");
        let edges: Vec<GraphEdge> = (1..nodes_per_repo)
            .map(|j| GraphEdge::new(EdgeType::Calls, ids[j].clone(), ids[j - 1].clone()))
            .collect();
        backend.upsert_edges_batch(&edges).expect("edge batch");
    }
    backend.end_bulk().expect("end bulk");
    fed
}

#[test]
fn small_fixture_blast_radius_under_100ms_p99() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = synthetic_federation(tmp.path(), 10, 5_000);
    assert_eq!(fed.backend().node_count(), 50_000);

    // Callers of r0f5 over depths 1..5. The walk follows incoming Calls
    // edges, so it climbs the chain to r0f10. The report lists leaves of the
    // walk: one leaf, reached at the depth limit, which is why `truncated` is
    // set. Every call must return exactly that, or the timing measures
    // something else.
    let _ = get_cross_repo_blast_radius(&fed, "r0f5", 1..5, Some("r0"));
    let mut durations = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let result = get_cross_repo_blast_radius(&fed, "r0f5", 1..5, Some("r0")).unwrap();
        durations.push(start.elapsed().as_millis());
        assert_eq!(
            result.total_count, 1,
            "a silent change in traversal would skew the timing"
        );
        assert!(result.truncated, "the walk stops at the depth limit");
        assert_eq!(
            result.by_repo.get("r0").map(Vec::as_slice),
            Some(["r0:Function:src/lib.rs:r0f10:0".to_string()].as_slice())
        );
    }
    durations.sort();
    let (p99, median, min, max) = (durations[98], durations[50], durations[0], durations[99]);
    eprintln!("small_fixture_blast_radius: p99 = {p99}ms, median = {median}ms, min = {min}ms, max = {max}ms");
    // The latency target is enforced only when asked for. The timing varies
    // with the machine, so a default run reports it and checks correctness;
    // `LAIN_BENCH_ENFORCE=1` turns a missed target into a failure. Enforce it
    // on an optimized build only: `cargo test --release --test
    // federation_benchmark` (the default test profile is unoptimized and runs
    // about twice as slow). On the reference host the release run measures
    // p99 about 66 ms.
    if std::env::var_os("LAIN_BENCH_ENFORCE").is_some() {
        assert!(
            !cfg!(debug_assertions),
            "LAIN_BENCH_ENFORCE needs an optimized build: run with --release"
        );
        assert!(
            p99 < 100,
            "p99 = {p99}ms, target < 100ms (median = {median}ms, min = {min}ms, max = {max}ms)"
        );
    }
}

#[test]
#[ignore]
fn large_fixture_health_at_scale() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = synthetic_federation(tmp.path(), 200, 50_000);
    let h = get_federation_health(&fed);
    assert_eq!(h.total_repos, 200);
    assert!(
        h.total_nodes >= 200 * 50_000,
        "expected at least 10M nodes, got {}",
        h.total_nodes
    );
    assert!(
        h.memory_estimate_bytes < 32 * 1024 * 1024 * 1024,
        "expected < 32 GB, estimated {} bytes",
        h.memory_estimate_bytes
    );
}
