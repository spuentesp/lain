//! Latency + agent-coordination benchmark (evidence for RNF-01 and the
//! multiplayer layer).
//!
//! Two halves:
//!
//! 1. **Tool latency** — sequential calls into the real MCP tool handlers
//!    (`run_claim_files`, `run_who_am_i`, `run_list_occupancy`,
//!    `run_get_world_state`, `run_get_recent_activity`, `run_get_audit_log`,
//!    `run_release_files`) on a live `LainServer`, plus `get_blast_radius`
//!    against a 10k-function synthetic call graph. Reports p50/p90/p99 and
//!    asserts p99 < 2 s (the RNF-01 budget).
//!
//! 2. **Concurrent coordination** — 8 agents on 8 OS threads hammering
//!    claim/release cycles over a shared pool of 12 files, forcing real
//!    advisory conflicts. Asserts the run completes without errors, that
//!    conflicts were actually detected (i.e. contention happened), that
//!    p99 claim latency under contention stays under 2 s, and that the
//!    occupancy map is empty once everyone releases.
//!
//! Run with: cargo test --test coordination_benchmark -- --nocapture

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use lain::server::mcp::audit_tools::{run_get_audit_log, run_get_recent_activity};
use lain::server::mcp::presence_tools::{
    run_claim_files, run_get_world_state, run_list_occupancy, run_my_claims, run_register_agent,
    run_release_files, run_who_am_i,
};
use lain::server::LainServer;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Generous RNF-01 bound: every measured operation must answer in under
/// 2 s at p99. The interesting signal is the printed distribution, not
/// the assertion — this just catches order-of-magnitude regressions.
const P99_BUDGET: Duration = Duration::from_secs(2);

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn report(name: &str, samples: &mut Vec<Duration>) {
    samples.sort();
    let p50 = percentile(samples, 0.50);
    let p90 = percentile(samples, 0.90);
    let p99 = percentile(samples, 0.99);
    println!(
        "{name:<28} n={:<4} p50={:>8.2?}  p90={:>8.2?}  p99={:>8.2?}",
        samples.len(),
        p50,
        p90,
        p99
    );
    assert!(
        p99 < P99_BUDGET,
        "{name}: p99 {p99:?} exceeds the 2s RNF-01 budget"
    );
}

/// A `LainServer` over a throwaway git repo with `n_files` Rust files,
/// each defining `fn f<i>()`. Returns the server plus the registered
/// agent's `(agent_id, session_token)`.
fn fresh_server(n_files: usize) -> (tempfile::TempDir, Arc<LainServer>, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    for i in 0..n_files {
        std::fs::write(tmp.path().join(format!("f{i}.rs")), format!("pub fn f{i}() {{}}\n"))
            .unwrap();
    }
    let mem = tmp.path().join(".lain/graph.bin");
    let server = Arc::new(LainServer::new(tmp.path(), &mem, None).expect("server"));
    let v = run_register_agent(&server, serde_json::json!({"name": "bench", "kind": "kimi"}))
        .unwrap();
    let agent_id = v["agent_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();
    (tmp, server, agent_id, token)
}

#[test]
fn tool_latency_benchmark() {
    let (_tmp, server, agent_id, token) = fresh_server(12);
    let path = "f0.rs";
    const ITERS: usize = 200;

    let mut claim: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut release: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut who_am_i: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut my_claims: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut occupancy: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut world_state: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut activity: Vec<Duration> = Vec::with_capacity(ITERS);
    let mut audit: Vec<Duration> = Vec::with_capacity(ITERS);

    for _ in 0..ITERS {
        let t = Instant::now();
        run_who_am_i(&server, serde_json::json!({"session_token": token})).unwrap();
        who_am_i.push(t.elapsed());

        let t = Instant::now();
        run_claim_files(
            &server,
            serde_json::json!({
                "agent_id": agent_id,
                "session_token": token,
                "files": [{"path": path, "symbols": ["f0"]}],
            }),
        )
        .unwrap();
        claim.push(t.elapsed());

        let t = Instant::now();
        run_my_claims(&server, serde_json::json!({"agent_id": agent_id, "session_token": token}))
            .unwrap();
        my_claims.push(t.elapsed());

        let t = Instant::now();
        run_list_occupancy(&server, serde_json::json!({})).unwrap();
        occupancy.push(t.elapsed());

        let t = Instant::now();
        run_get_world_state(&server, serde_json::json!({})).unwrap();
        world_state.push(t.elapsed());

        let t = Instant::now();
        run_get_recent_activity(&server, serde_json::json!({})).unwrap();
        activity.push(t.elapsed());

        let t = Instant::now();
        run_get_audit_log(&server, serde_json::json!({})).unwrap();
        audit.push(t.elapsed());

        let t = Instant::now();
        run_release_files(
            &server,
            serde_json::json!({
                "agent_id": agent_id,
                "session_token": token,
                "files": [{"path": path}],
            }),
        )
        .unwrap();
        release.push(t.elapsed());
    }

    println!("\n=== Tool latency (sequential, n={ITERS} per op) ===");
    report("who_am_i", &mut who_am_i);
    report("claim_files", &mut claim);
    report("my_claims", &mut my_claims);
    report("list_occupancy", &mut occupancy);
    report("get_world_state", &mut world_state);
    report("get_recent_activity", &mut activity);
    report("get_audit_log", &mut audit);
    report("release_files", &mut release);
}

#[tokio::test]
async fn blast_radius_latency_benchmark() {
    // 10k-function chain with a hot anchor — the shape `bench_graph_traverse_blast_radius`
    // uses in tests/graph_benchmark.rs, measured here end-to-end through the
    // real `get_blast_radius` handler.
    let tmp = std::env::temp_dir().join("coord_bench_graph");
    let _ = std::fs::remove_dir_all(&tmp);
    let graph = GraphDatabase::new(&tmp).unwrap();
    let overlay = VolatileOverlay::new();

    let file = GraphNode::new(NodeType::File, "mod.rs".to_string(), "/src/mod.rs".to_string());
    graph.upsert_node(file.clone()).unwrap();
    let mut ids = Vec::new();
    for i in 0..10_000 {
        let f = GraphNode::new(
            NodeType::Function,
            format!("function_{i}"),
            format!("/src/mod.rs:{}", i * 10),
        );
        ids.push(f.id.clone());
        graph.upsert_node(f.clone()).unwrap();
        graph
            .insert_edge(&GraphEdge::new(EdgeType::Contains, file.id.clone(), f.id.clone()))
            .unwrap();
    }
    for w in ids.windows(2) {
        graph
            .insert_edge(&GraphEdge::new(EdgeType::Calls, w[0].clone(), w[1].clone()))
            .unwrap();
    }

    const ITERS: usize = 50;
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        let out = lain::tools::handlers::impact::get_blast_radius(
            &graph,
            &overlay,
            "function_0",
            false,
            None,
        )
        .await
        .expect("blast radius");
        assert!(!out.is_empty());
        samples.push(t.elapsed());
    }

    println!("\n=== get_blast_radius (10k-fn chain, n={ITERS}) ===");
    report("get_blast_radius", &mut samples);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn concurrent_agents_contention_benchmark() {
    const AGENTS: usize = 8;
    const CYCLES: usize = 30;
    const FILES: usize = 12;

    let (_tmp, server, _, _) = fresh_server(FILES);

    // Register all agents up front so the timed loop only measures
    // claim/release, not registration.
    let creds: Vec<(String, String)> = (0..AGENTS)
        .map(|i| {
            let v = run_register_agent(
                &server,
                serde_json::json!({"name": format!("bench-{i}"), "kind": "kimi"}),
            )
            .unwrap();
            (
                v["agent_id"].as_str().unwrap().to_string(),
                v["session_token"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    let mut handles = Vec::new();
    for (agent_id, token) in creds {
        let server = server.clone();
        handles.push(std::thread::spawn(move || {
            let mut latencies = Vec::with_capacity(CYCLES);
            let mut conflicts = 0usize;
            for cycle in 0..CYCLES {
                // All agents contend over the same pool; the file picked
                // rotates so every pair of agents overlaps eventually.
                let file = format!("f{}.rs", cycle % FILES);
                let sym = format!("f{}", cycle % FILES);
                let t = Instant::now();
                let v = run_claim_files(
                    &server,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "session_token": token,
                        "files": [{"path": file.clone(), "symbols": [sym]}],
                    }),
                )
                .expect("claim_files must not error under contention");
                latencies.push(t.elapsed());
                conflicts += v["conflicts"].as_array().unwrap().len();

                // Release immediately so the next cycle contends again;
                // a release of a non-held file is a no-op by design.
                let _ = run_release_files(
                    &server,
                    serde_json::json!({
                        "agent_id": agent_id,
                        "session_token": token,
                        "files": [{"path": file}],
                    }),
                )
                .expect("release_files must not error under contention");
            }
            (latencies, conflicts)
        }));
    }

    let mut all: Vec<Duration> = Vec::with_capacity(AGENTS * CYCLES);
    let mut total_conflicts = 0usize;
    for h in handles {
        let (mut latencies, conflicts) = h.join().expect("agent thread panicked");
        all.append(&mut latencies);
        total_conflicts += conflicts;
    }

    println!("\n=== Concurrent coordination ({AGENTS} agents x {CYCLES} cycles, {FILES} shared files) ===");
    println!("total conflicts detected: {total_conflicts}");
    report("claim_files (contended)", &mut all);
    assert!(
        total_conflicts > 0,
        "expected real advisory conflicts under this contention pattern"
    );

    // Everyone released at the end of their last cycle: the occupancy map
    // must drain back to empty — no leaked claims from the contention.
    let occ = run_list_occupancy(&server, serde_json::json!({})).unwrap();
    assert_eq!(
        occ.as_array().unwrap().len(),
        0,
        "occupancy must be empty after all agents released"
    );
}
