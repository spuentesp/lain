//! Property-based tests for graph invariants.
//!
//! Each `#[test]` in the `proptest!` block runs the default 256 cases. The
//! fixtures are kept small (≤50 nodes / ≤100 edges) and the workspace is an
//! empty tempdir, so the analysis filters that walk the filesystem
//! (`names_referenced_anywhere`) never fire spuriously and the suite stays
//! under one second per test.
//!
//! Generated node names use the `propfn_<idx>` scheme, paths use
//! `proppath_<idx>/f.rs`. That dodges every entry in
//! `metrics::FALSE_POSITIVE_PATTERNS`, the trait-context heuristic, and the
//! test-path heuristic. Each node sits alone in its file, so the
//! `unindexed_files` threshold (≥3 functions / file with zero outgoing calls)
//! never triggers and the invariant "every calls_in==0 function reaches the
//! report" holds.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::tools::handlers::impact::get_blast_radius;
use crate::server::tools::handlers::metrics::analyze_dead_code;
use crate::server::tools::handlers::navigation::get_call_chain;
use proptest::prelude::*;
use std::collections::{HashSet, VecDeque};

/// A function node whose name dodges every filter in `analyze_dead_code`.
fn unique_fn_node(idx: u32, calls_in: u32, calls_out: u32, line_end: u32) -> GraphNode {
    let mut n = GraphNode::new(
        NodeType::Function,
        format!("propfn_{idx}"),
        format!("/proppath_{idx}/f.rs"),
    );
    n.line_start = Some(1);
    n.line_end = Some(1 + line_end);
    n.calls_in = Some(calls_in);
    n.calls_out = Some(calls_out);
    n
}

/// Build a graph from `nodes` and `edges`. Edges are silently dropped when
/// their endpoints are missing from `nodes` (mirrors what the indexer does
/// in production). The graph is persisted at `path`; pass a fresh path
/// every call so successive invocations don't collide.
fn build_graph_at(
    nodes: &[GraphNode],
    edges: &[(usize, usize)],
    path: &std::path::Path,
) -> GraphDatabase {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(path);
    let g = GraphDatabase::new(path).expect("graph new");
    for n in nodes {
        g.upsert_node(n.clone()).expect("upsert node");
    }
    for &(i, j) in edges {
        if i >= nodes.len() || j >= nodes.len() {
            continue;
        }
        let e = GraphEdge::new(EdgeType::Calls, nodes[i].id.clone(), nodes[j].id.clone());
        // upsert_edge silently dedups; that's the production contract.
        let _ = g.upsert_edge(e);
    }
    g
}

/// Build a graph at a unique temp path (used by tests that don't care
/// about persistence).
fn build_graph(nodes: &[GraphNode], edges: &[(usize, usize)]) -> GraphDatabase {
    let tmp = std::env::temp_dir().join(format!(
        "lain_proptest_{}",
        uuid::Uuid::new_v4().simple()
    ));
    build_graph_at(nodes, edges, &tmp)
}

/// Strategy: a vector of `(calls_in, calls_out, line_end)` tuples.
fn arb_fn_shape() -> impl Strategy<Value = (u32, u32, u32)> {
    (0u32..=3, 0u32..=3, 1u32..=30)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every symbol `analyze_dead_code` reports as dead must come from the
    /// input set, and every input with `calls_in == 0` must reach the
    /// report (no silent false negatives — that was the regression hidden
    /// behind `fan_in == 0` once `Contains` edges were added).
    #[test]
    fn proptest_analyze_dead_code_subset_of_functions(
        shapes in prop::collection::vec(arb_fn_shape(), 0..=50)
    ) {
        let nodes: Vec<GraphNode> = shapes
            .iter()
            .enumerate()
            .map(|(i, &(ci, co, le))| unique_fn_node(i as u32, ci, co, le))
            .collect();
        let input_ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let graph = build_graph(&nodes, &[]);
        let ws = tempfile::tempdir().expect("tempdir");
        let report = analyze_dead_code(&graph, ws.path()).expect("analyze_dead_code");

        // Subset property: every reported candidate is in the input set.
        let reported: HashSet<String> = report
            .unreferenced
            .iter()
            .chain(report.calls_out.iter())
            .map(|n| n.id.clone())
            .collect();
        for id in &reported {
            prop_assert!(
                input_ids.contains(id),
                "dead-code candidate {id} was not in the input function set"
            );
        }

        // No false negatives: every calls_in==0 input reaches the report.
        // Our fixture dodges every filter (false-positive names, trait
        // paths, test paths, unindexed-file threshold) and the workspace
        // is empty, so names_referenced_anywhere filters nothing.
        for n in &nodes {
            if n.calls_in.unwrap_or(0) == 0 && !reported.contains(&n.id) {
                prop_assert!(
                    false,
                    "calls_in==0 function {}({}) was silently dropped",
                    n.name,
                    n.id
                );
            }
        }
    }

    /// `find_anchors` returns nodes sorted by `anchor_score` descending,
    /// and every score is finite and non-negative. Catches a future
    /// regression that produces NaN, infinite, or negative scores.
    #[test]
    fn proptest_find_anchors_score_is_finite_and_nonnegative(
        shapes in prop::collection::vec(arb_fn_shape(), 0..=40),
        edges in prop::collection::vec((0u32..40, 0u32..40), 0..=80),
        limit in 1usize..=20
    ) {
        let nodes: Vec<GraphNode> = shapes
            .iter()
            .enumerate()
            .map(|(i, &(ci, co, le))| unique_fn_node(i as u32, ci, co, le))
            .collect();
        // Map u32s to indices, drop self-loops.
        let real_edges: Vec<(usize, usize)> = edges
            .into_iter()
            .map(|(a, b)| (a as usize, b as usize))
            .filter(|&(a, b)| a < nodes.len() && b < nodes.len() && a != b)
            .collect();
        let graph = build_graph(&nodes, &real_edges);
        graph.calculate_anchor_scores().expect("calculate_anchor_scores");
        let anchors = graph.find_anchors(limit).expect("find_anchors");

        // Every score is finite and non-negative.
        for a in &anchors {
            let s = a.anchor_score.unwrap_or(0.0);
            prop_assert!(s.is_finite(), "anchor {} has non-finite score {}", a.name, s);
            prop_assert!(s >= 0.0, "anchor {} has negative score {}", a.name, s);
        }

        // Sorted by score descending. f32 lacks Ord, so use total_cmp.
        for w in anchors.windows(2) {
            let sa = w[0].anchor_score.unwrap_or(0.0);
            let sb = w[1].anchor_score.unwrap_or(0.0);
            prop_assert!(
                sa.total_cmp(&sb) != std::cmp::Ordering::Less,
                "anchors out of order: {sa} then {sb}"
            );
        }
    }

    /// `get_blast_radius` walks incoming `Calls`/`Uses` edges. Since the
    /// strategy builds a DAG (`i < j` edges), the source cannot reach
    /// itself through that BFS — and the algorithm must not include the
    /// source as one of its own dependents.
    #[test]
    fn proptest_blast_radius_no_self_reference(
        n_nodes in 1u32..=20,
        edges in prop::collection::vec((0u32..20, 0u32..20), 0..=30)
    ) {
        let n = n_nodes as usize;
        let nodes: Vec<GraphNode> = (0..n)
            .map(|i| unique_fn_node(i as u32, 0, 0, 5))
            .collect();
        let real_edges: Vec<(usize, usize)> = edges
            .into_iter()
            .map(|(a, b)| (a as usize, b as usize))
            .filter(|&(a, b)| a < n && b < n && a < b)
            .collect();
        let graph = build_graph(&nodes, &real_edges);
        let overlay = VolatileOverlay::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        for node in &nodes {
            let output = rt
                .block_on(get_blast_radius(
                    &graph,
                    &overlay,
                    &node.id,
                    false,
                    None,
                ))
                .expect("get_blast_radius");

            // Lines that look like dependent listings start with two
            // spaces + dash + space. The header line "- <name> (Kind) in
            // <path>" begins with a single space; dependent lines start
            // with two. That single/double-space difference is the
            // unambiguous signal that separates "header repeats the
            // source name" from "source is listed as a dependent of itself".
            let source_name = node.name.clone();
            let pattern = format!("- {source_name} (");
            for line in output.lines() {
                if !line.starts_with("  - ") {
                    continue;
                }
                prop_assert!(
                    !line.contains(&pattern),
                    "blast radius for {source_name} listed the source as a dependent: {line:?}"
                );
            }
        }
    }

    /// `save_to_disk_sync` then `load_from_disk` via a fresh
    /// `GraphDatabase::new` preserves node count, edge count, and the
    /// exact set of node ids. Catches drift in the on-disk schema that
    /// hand-written round-trip tests miss.
    #[test]
    fn proptest_graph_round_trip_serialization(
        shapes in prop::collection::vec(arb_fn_shape(), 0..=30),
        edges in prop::collection::vec((0u32..30, 0u32..30), 0..=50)
    ) {
        let nodes: Vec<GraphNode> = shapes
            .iter()
            .enumerate()
            .map(|(i, &(ci, co, le))| unique_fn_node(i as u32, ci, co, le))
            .collect();
        let real_edges: Vec<(usize, usize)> = edges
            .into_iter()
            .map(|(a, b)| (a as usize, b as usize))
            .filter(|&(a, b)| a < nodes.len() && b < nodes.len() && a != b)
            .collect();

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("g.bin");

        // Build g1 with the same persistence path g2 will hydrate from,
        // otherwise save_to_disk_sync writes to one file and load_from_disk
        // reads from another.
        let g1 = build_graph_at(&nodes, &real_edges, &path);
        g1.save_to_disk_sync().expect("save_to_disk_sync");

        // A fresh GraphDatabase whose persistence_path points at the
        // saved file loads from disk on construction.
        let g2 = GraphDatabase::new(&path).expect("reload");

        prop_assert_eq!(g1.node_count(), g2.node_count(), "node count");
        prop_assert_eq!(g1.edge_count(), g2.edge_count(), "edge count");

        let ids1: HashSet<String> = g1.all_nodes().iter().map(|n| n.id.clone()).collect();
        let ids2: HashSet<String> = g2.all_nodes().iter().map(|n| n.id.clone()).collect();
        prop_assert_eq!(ids1, ids2, "node id set after round-trip");
    }

    /// When `get_call_chain` reports a path between two nodes, the
    /// formatted chain length can never exceed the BFS distance between
    /// them (the diameter of the underlying DAG). The strategy ensures
    /// the graph is acyclic (`i < j` edges), so the BFS terminates.
    #[test]
    fn proptest_call_chain_terminates(
        n_nodes in 1u32..=15,
        edges in prop::collection::vec((0u32..15, 0u32..15), 0..=20)
    ) {
        let n = n_nodes as usize;
        let nodes: Vec<GraphNode> = (0..n)
            .map(|i| unique_fn_node(i as u32, 0, 0, 5))
            .collect();
        let real_edges: Vec<(usize, usize)> = edges
            .into_iter()
            .map(|(a, b)| (a as usize, b as usize))
            .filter(|&(a, b)| a < n && b < n && a < b)
            .collect();
        let graph = build_graph(&nodes, &real_edges);
        let overlay = VolatileOverlay::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        // BFS distance from `from_id` to `target_id` over outgoing
        // Calls/Uses edges, or None if `target_id` is unreachable.
        let bfs_distance = |from_id: &str, target_id: &str| -> Option<usize> {
            let mut visited: HashSet<String> = HashSet::new();
            let mut queue: VecDeque<(String, usize)> = VecDeque::new();
            queue.push_back((from_id.to_string(), 0));
            while let Some((cur, d)) = queue.pop_front() {
                if !visited.insert(cur.clone()) {
                    continue;
                }
                if cur == target_id {
                    return Some(d);
                }
                let outgoing = graph.get_edges_from(&cur).unwrap_or_default();
                for e in outgoing {
                    if !matches!(e.edge_type, EdgeType::Calls | EdgeType::Uses) {
                        continue;
                    }
                    if !visited.contains(&e.target_id) {
                        queue.push_back((e.target_id, d + 1));
                    }
                }
            }
            None
        };

        for (i, from) in nodes.iter().enumerate() {
            for (j, to) in nodes.iter().enumerate() {
                if i == j {
                    continue;
                }
                let expected_distance = bfs_distance(&from.id, &to.id);
                let output = rt
                    .block_on(get_call_chain(&graph, &overlay, &from.id, &to.id, None))
                    .expect("get_call_chain");

                if expected_distance.is_none() {
                    prop_assert!(
                        output.contains("No call path found"),
                        "BFS says no path from {} to {} but call_chain returned a chain",
                        from.name,
                        to.name
                    );
                    continue;
                }

                // Format: "## Call Chain: a -> b\n\n<path>". The path
                // section is a `name1 → name2 → …` string. Split on the
                // arrow and count names.
                let chain_part = output
                    .split("\n\n")
                    .nth(1)
                    .unwrap_or("")
                    .trim_end_matches(|c: char| c == ' ' || c == ']');
                // Drop the trailing "[Interactive …]" tail if ui_sessions
                // added one (None here, so absent).
                let names: Vec<&str> = chain_part.split(" → ").collect();
                let chain_len = names.len();
                let dist = expected_distance.expect("Some checked above");

                prop_assert!(
                    chain_len >= 2,
                    "chain {} -> {} should have at least start+end names, got {}",
                    from.name,
                    to.name,
                    chain_len
                );
                prop_assert!(
                    chain_len <= dist + 1,
                    "chain length {} from {} to {} exceeds BFS distance {}",
                    chain_len,
                    from.name,
                    to.name,
                    dist + 1
                );
            }
        }
    }
}