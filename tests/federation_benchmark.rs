//! Small-fixture performance test. Validates the cross-repo blast-radius
//! latency target (`p99 < 100ms` on a 50K-node federation). Runs on every
//! PR.
//!
//! Build with `--features test-utils` so the `federation_index_for_test`
//! helper (gated in `src/federation/mod.rs`) is visible to integration tests.
//! Without that feature, the helper is `cfg`-stripped and this test fails
//! to compile.
//!
//! Run with:
//! ```text
//! cargo test --features test-utils --test federation_benchmark \
//!     small_fixture -- --nocapture --test-threads=1
//! ```

use lain::federation::federation_index_for_test;
use lain::mcp::get_cross_repo_blast_radius;
use std::time::Instant;

#[test]
fn small_fixture_blast_radius_under_100ms_p99() {
    // 10 repos × 5_000 nodes per repo = 50K total nodes — the "small fixture"
    // called out in the brief. Each repo holds a chain
    // `r{i}f{j} --Calls--> r{i}f{j-1}` for `j > 0`.
    //
    // Brief deviation: the brief's text queries `r0f0`, but `r0f0` has no
    // outgoing `Calls` edges under this chain direction (it's the chain's
    // tail), so an outgoing-traverse blast radius is empty. Querying
    // `r0f5` walks the chain backwards via outgoing Calls (depth 1..5 →
    // r0f4, r0f3, r0f2, r0f1, r0f0). Same shape, same depth, same workload
    // — five `traverse` hops against a 50K-node backend.
    let tmp = tempfile::tempdir().unwrap();
    let fed = federation_index_for_test(tmp.path(), 10, 5_000).unwrap();
    assert_eq!(fed.backend().node_count(), 50_000);

    // Warm up: prime the petgraph cache and any first-call paths so the
    // measured loop reflects steady-state latency, not cold-start cost.
    let _ = get_cross_repo_blast_radius(&fed, "r0f5", 1..5);

    // Measure 100 calls, take p99 (the 99th-smallest of 100 sorted samples
    // is `durations[98]`).
    let mut durations = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let result = get_cross_repo_blast_radius(&fed, "r0f5", 1..5).unwrap();
        durations.push(start.elapsed().as_millis());
        // Sanity: depth 1..5 against the chain in repo0 visits exactly 5
        // nodes (r0f4..r0f0) — guard against silent regressions in the
        // helper or traverse logic that would invalidate the perf number.
        assert_eq!(result.total_count, 5);
    }
    durations.sort();
    let p99 = durations[98];
    let median = durations[50];
    let min = durations[0];
    let max = durations[99];
    eprintln!(
        "small_fixture_blast_radius: p99 = {p99}ms, median = {median}ms, \
         min = {min}ms, max = {max}ms, n = {}",
        durations.len()
    );
    assert!(
        p99 < 100,
        "p99 = {p99}ms, target < 100ms (median = {median}ms, min = {min}ms, \
         max = {max}ms)"
    );
}