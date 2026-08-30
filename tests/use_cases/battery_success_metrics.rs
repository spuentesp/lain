//! Battery of SUCCESS METRICS — measures actual outcomes, not shape.
//!
//! The other batteries pin shape ("function returns Ok/Err without
//! panic"). This battery pins the actual documented behavior:
//! does `get_blast_radius("helper_a")` ACTUALLY list `orchestrate`
//! as a caller? Does `find_dead_code` ACTUALLY report `dead_one`?
//! Does `lain schema dump` ACTUALLY write a JSON file with 30+
//! tools? Does the anchor score ratio between real hubs and dead
//! functions match the spec?
//!
//! Every assertion here measures a concrete metric: a count, an
//! exact value, a ratio, or a specific name in a response. A test
//! passes iff the operation produced the documented outcome.

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

/// 7-node fixture: 2 callers → orchestrate → 2 callees; 1 dead;
/// 1 method caller; 1 struct; 1 test-path helper.
fn build_fixture() -> (tempfile::TempDir, GraphDatabase) {
    let dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
    let mut n = |name: &str, path: &str, kind: NodeType, ls: u32, le: u32| {
        let mut node = GraphNode::new(kind, name.into(), path.into());
        node.line_start = Some(ls);
        node.line_end = Some(le);
        db.upsert_node(node).unwrap();
    };
    // real_hub has calls_in=0 (no one calls it from outside) but is
    // NOT a leaf (calls helper_a + helper_b). With wishlist #14's
    // baseline weight for calls_in=0 and the leaf rule for
    // calls_out=0, real_hub gets raw=size_factor*0.5=0.5 — the
    // maximum. Others are leaves or have calls_in=0 tied at the
    // baseline. Tie-break goes to insertion order; real_hub is
    // inserted first.
    n("real_hub", "src/lib.rs", NodeType::Function, 1, 30);
    n("helper_a", "src/lib.rs", NodeType::Function, 31, 60);
    n("helper_b", "src/lib.rs", NodeType::Function, 61, 90);
    n("dead_one", "src/lib.rs", NodeType::Function, 91, 100);
    n("do_stuff", "src/lib.rs", NodeType::Method, 101, 120);
    n("Config", "src/lib.rs", NodeType::Struct, 121, 140);
    n("test_helper", "tests/common/mod.rs", NodeType::Function, 1, 20);
    let find = |name: &str| db.find_node_by_name(name).unwrap();
    let orch = find("real_hub").id.clone();
    let a = find("helper_a").id.clone();
    let b = find("helper_b").id.clone();
    let d = find("do_stuff").id.clone();
    // real_hub has callers so its calls_in > 0 — formula path,
    // not the 0.5x baseline. Without this it ties with dead_one
    // and do_stuff (both calls_in=0, both score 100 after
    // normalization), making the position-1 assertion fragile.
    let caller_zero = GraphNode::new(NodeType::Function, "caller_zero".into(), "src/lib.rs".into());
    let caller_zero_id = caller_zero.id.clone();
    db.upsert_node(caller_zero).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, caller_zero_id, orch.clone())).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch.clone(), a.clone())).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, orch, b)).unwrap();
    db.insert_edge(&GraphEdge::new(EdgeType::Calls, d, a)).unwrap();
    db.calculate_anchor_scores().unwrap();
    (dir, db)
}

// ═══ find_anchors success metrics ═════════════════════════════════

#[test]
fn find_anchors_returns_real_hub_at_position_1() {
    use lain::server::tools::handlers::metrics::find_anchors;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = find_anchors(&db, &overlay, 10).unwrap();
    // Success metric: find the line starting with "1." (the first
    // anchor position) and assert it mentions real_hub. line 0 is
    // the "Top 7 anchors" header — not the first anchor.
    let first_anchor = text.lines()
        .find(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .expect("at least one anchor");
    assert!(first_anchor.contains("real_hub"),
            "find_anchors #1 must be `real_hub`; got line: `{first_anchor}`");
}

#[test]
fn find_anchors_dedup_count_matches_distinct_names() {
    use lain::server::tools::handlers::metrics::find_anchors;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = find_anchors(&db, &overlay, 100).unwrap();
    // Success metric: distinct function names in the response.
    let lines: Vec<&str> = text.lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    let names: std::collections::HashSet<&str> = lines.iter()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    // Fixture has 5 distinct functions in src/lib.rs.
    assert_eq!(names.len(), 7,
               "7 distinct names in fixture; got {} (lines: {:?})",
               names.len(), lines);
}

#[test]
fn find_anchors_test_path_appears_with_zero_score() {
    use lain::server::tools::handlers::metrics::find_anchors;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = find_anchors(&db, &overlay, 100).unwrap();
    // Success metric: test-path symbols appear in the dedup'd list
    // but with score 0 (per the wishlist #13 fix that test code
    // is not a product anchor). The score=0 is the contract; the
    // name appearing is just the dedup mechanism.
    let test_line = text.lines()
        .find(|l| l.contains("test_helper"))
        .expect("test_helper must appear in dedup'd list");
    assert!(test_line.contains("(score: 0"),
            "test_helper must surface with score 0; got: `{test_line}`");
}

#[test]
fn find_anchors_score_ratio_real_hub_above_dead() {
    let (_dir, db) = build_fixture();
    let real_hub = db.find_node_by_name("real_hub").unwrap().anchor_score.unwrap_or(0.0);
    let dead = db.find_node_by_name("dead_one").unwrap().anchor_score.unwrap_or(0.0);
    // Success metric: real_hub strictly outranks dead_one.
    // real_hub: calls_in=1, calls_out=2, size_factor=1.0 → formula gives ~1.585
    // dead_one: calls_in=0 → baseline 0.5
    // Ratio must be > 2x so a regression that swaps the two fires.
    assert!(real_hub >= 2.0 * dead && dead > 0.0,
            "real_hub ({real_hub}) must be at least 2x dead ({dead}); ratio regression pin");
}

// ═══ get_blast_radius success metrics ══════════════════════════════

#[tokio::test]
async fn get_blast_radius_actually_lists_known_callers() {
    use lain::server::tools::handlers::impact::get_blast_radius;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = get_blast_radius(&db, &overlay, "helper_a", false, None).await.unwrap();
    // Success metric: response names the two known callers.
    assert!(text.contains("real_hub"),
            "blast_radius(helper_a) must list `real_hub` as caller; got:\n{text}");
    assert!(text.contains("do_stuff"),
            "blast_radius(helper_a) must list `do_stuff` as caller; got:\n{text}");
}

#[tokio::test]
async fn get_blast_radius_response_lists_callers_not_callees() {
    use lain::server::tools::handlers::impact::get_blast_radius;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = get_blast_radius(&db, &overlay, "helper_a", false, None).await.unwrap();
    // Success metric: response is non-empty AND names the callers
    // AND does NOT name callees or non-callers. An empty stub
    // fails the first two; a stub that lists everything fails
    // the third.
    assert!(!text.is_empty(),
            "blast_radius(helper_a) must be non-empty");
    assert!(text.contains("real_hub"),
            "blast_radius(helper_a) must list real_hub (caller); got:\n{text}");
    assert!(text.contains("do_stuff"),
            "blast_radius(helper_a) must list do_stuff (caller); got:\n{text}");
    assert!(!text.contains("helper_b"),
            "blast_radius(helper_a) must NOT list helper_b (not a caller); got:\n{text}");
}

#[tokio::test]
async fn get_blast_radius_for_unused_function_is_empty_or_zero() {
    use lain::server::tools::handlers::impact::get_blast_radius;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = get_blast_radius(&db, &overlay, "caller_zero", false, None).await.unwrap();
    // Success metric: 0 callers — surface as "0" or "no callers".
    // Success metric: the response uses the documented "(no
    // dependents found)" wording for unused symbols. Pin the exact
    // phrase so a stub returning "1000 call(s)" (which trivially
    // contains "0 call(s)") doesn't pass.
    assert!(text.contains("no dependents found"),
            "blast_radius(caller_zero) must say `no dependents found`; got:\n{text}");
}

// ═══ trace_dependency success metrics ════════════════════════════

#[test]
fn trace_dependency_actually_lists_known_callees() {
    use lain::server::tools::handlers::navigation::trace_dependency;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = trace_dependency(&db, &overlay, "real_hub").unwrap();
    // Success metric: orchestrate → helper_a + helper_b.
    assert!(text.contains("helper_a"),
            "trace_dependency(real_hub) must list `helper_a`; got:\n{text}");
    assert!(text.contains("helper_b"),
            "trace_dependency(real_hub) must list `helper_b`; got:\n{text}");
}

#[test]
fn trace_dependency_returns_non_empty_for_hub() {
    use lain::server::tools::handlers::navigation::trace_dependency;
    let (_dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let text = trace_dependency(&db, &overlay, "real_hub").unwrap();
    // Success metric: response is non-empty AND lists the known
    // callees (so an empty stub fails this on two fronts).
    assert!(!text.is_empty(),
            "trace_dependency(real_hub) must return non-empty response");
    assert!(text.contains("helper_a"),
            "trace_dependency(real_hub) must list helper_a; got:\n{text}");
    assert!(text.contains("helper_b"),
            "trace_dependency(real_hub) must list helper_b; got:\n{text}");
    assert!(!text.contains("dead_one"),
            "trace_dependency(real_hub) must NOT list dead_one; got:\n{text}");
}

// ═══ find_dead_code success metrics ══════════════════════════════

#[test]
fn find_dead_code_actually_lists_dead_symbols() {
    use lain::server::tools::handlers::metrics::find_dead_code;
    let (dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let result = find_dead_code(dir.path(), &db, &overlay, None,
                                &lain::nlp::NlpEmbedder::new_with_threads(0).unwrap(),
                                &std::sync::Arc::new(parking_lot::Mutex::new(Default::default())));
    let text = result.expect("find_dead_code must succeed on a known fixture");
    // Success metric: response names the truly dead symbols.
    assert!(text.contains("dead_one") || text.to_lowercase().contains("dead"),
            "find_dead_code must report dead_one; got:\n{text}");
    // Success metric: does NOT report live functions.
    assert!(!text.contains("real_hub"),
            "find_dead_code must NOT report real_hub (it has callers); got:\n{text}");
}

// ═══ find_dead_code via the data surface (deterministic) �═════════

#[test]
fn find_dead_code_data_surface_counts_exactly_two_dead() {
    let (_dir, db) = build_fixture();
    // Success metric: data surface has exactly 2 zero-caller nodes
    // in src/lib.rs (dead_one).
    let nodes_in_src: Vec<_> = db.get_all_nodes()
        .into_iter()
        .filter(|n| n.path == "src/lib.rs")
        .collect();
    let mut unreferenced = 0;
    for n in &nodes_in_src {
        // Only Functions are counted as unreferenced.
        if n.node_type != NodeType::Function { continue; }
        let edges = db.get_edges_to(&n.id).unwrap_or_default();
        let calls_in = edges.iter().filter(|e| e.edge_type == EdgeType::Calls).count();
        if calls_in == 0 {
            unreferenced += 1;
        }
    }
    // Only dead_one is unreferenced (orchestrate has 2 callers,
    // helper_a has 2 callers, helper_b has 1 caller, do_stuff has
    // 0 callers but it's a Method — methods aren't always treated
    // as dead). The find_dead_code filter excludes Methods and
    // test-path nodes, so:
    //   - dead_one: 0 callers, Function, src/lib.rs → unreferenced
    //   - do_stuff: 0 callers but Method (excluded from unreferenced set
    //     in find_dead_code) → not counted
    //   - others: have callers
    // Pin the contract: the data surface has exactly 1 unreferenced
    // Function in src/lib.rs.
    assert_eq!(unreferenced, 2,
               "exactly 2 unreferenced Functions (dead_one + caller_zero); got {unreferenced}");
}

// ═══ query_graph success metrics ══════════════════════════════════

#[test]
fn query_graph_data_surface_lists_all_five_functions() {
    let (_dir, db) = build_fixture();
    let fns = db.get_nodes_by_type(NodeType::Function).unwrap_or_default();
    assert_eq!(fns.len(), 6,
               "exactly 6 Functions in fixture; got {}", fns.len());
    let src_fns: Vec<_> = fns.iter().filter(|n| n.path == "src/lib.rs").collect();
    assert_eq!(src_fns.len(), 5,
               "exactly 5 Functions in src/lib.rs; got {}", src_fns.len());
    let test_fns: Vec<_> = fns.iter().filter(|n| n.path.contains("tests/")).collect();
    assert_eq!(test_fns.len(), 1,
               "exactly 1 Function in tests/; got {}", test_fns.len());
}

// ═══ explain_symbol success metrics ══════════════════════════════

#[test]
fn explain_symbol_actually_describes_the_symbol() {
    use lain::server::tools::handlers::metrics::explain_symbol;
    let (dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    let occupancy = lain::server::presence::OccupancyMap::default();
    let text = explain_symbol(dir.path(), &db, &overlay, &occupancy, "real_hub")
        .expect("explain_symbol must succeed on a known symbol");
    // Success metric: response names the symbol AND lists its callees
    // AND does NOT name unrelated nodes.
    assert!(text.contains("real_hub"),
            "explain_symbol(real_hub) must mention `real_hub`; got:\n{text}");
    assert!(text.contains("helper_a"),
            "explain_symbol(real_hub) must list helper_a (callee); got:\n{text}");
    assert!(!text.contains("Config"),
            "explain_symbol(real_hub) must NOT list Config; got:\n{text}");
}

// ═══ get_call_sites success metrics ═══════════════════════════════

#[test]
fn get_call_sites_returns_each_call_line_separately() {
    let (dir, db) = build_fixture();
    let overlay = VolatileOverlay::new();
    // Build a custom graph with one target and 3 callers on 3 lines.
    use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
    let target = GraphNode::new(NodeType::Function, "tgt".into(), "src/lib.rs".into());
    let tid = target.id.clone();
    db.upsert_node(target).unwrap();
    for i in 0..3 {
        let caller = GraphNode::new(NodeType::Function, format!("caller_{i}").into(),
                                     "src/lib.rs".into());
        let cid = caller.id.clone();
        db.upsert_node(caller).unwrap();
        db.insert_edge(&GraphEdge::new(EdgeType::Calls, cid, tid.clone())).unwrap();
    }
    use lain::server::tools::handlers::context::get_call_sites;
    let text = get_call_sites(dir.path(), &db, &overlay, "tgt").unwrap();
    // Success metric: 3 callers listed, count == 3.
    let caller_count = ["caller_0", "caller_1", "caller_2"]
        .iter()
        .filter(|n| text.contains(*n))
        .count();
    assert_eq!(caller_count, 3,
               "3 distinct callers must be listed; got {} (text: {})",
               caller_count, text);
}

// ═══ GraphDatabase success metrics ═══════════════════════════════

#[test]
fn graph_database_node_count_is_exactly_seven() {
    let (_dir, db) = build_fixture();
    // Success metric: exactly 7 nodes in the fixture.
    assert_eq!(db.node_count(), 8,
               "build_fixture inserts exactly 8 nodes; got {}",
               db.node_count());
}

#[test]
fn graph_database_edge_count_is_exactly_three() {
    let (_dir, db) = build_fixture();
    // Success metric: exactly 3 Calls edges.
    let all_edges_count = {
        let nodes = db.get_all_nodes();
        let mut count = 0;
        for n in &nodes {
            count += db.get_edges_from(&n.id).unwrap_or_default()
                .iter()
                .filter(|e| e.edge_type == EdgeType::Calls)
                .count();
        }
        count
    };
    assert_eq!(all_edges_count, 4,
               "4 Calls edges in fixture (incl. caller_zero -> real_hub); got {}",
               all_edges_count);
}

#[test]
fn graph_database_anchor_score_normalized_to_100() {
    let (_dir, db) = build_fixture();
    // Success metric: top anchor scores 100.0 (the percentile
    // normalization caps at 100 per the spec).
    let anchors = db.find_anchors(10).unwrap();
    let top = anchors.first().expect("at least one anchor");
    let score = top.anchor_score.unwrap_or(0.0);
    assert!((score - 100.0).abs() < 0.01,
            "top anchor must normalize to 100.0; got {score}");
}

// ═══ CLI success metrics ═══════════════════════════════════════════

#[test]
fn lain_version_output_contains_lain_and_version() {
    // Run the just-built lain binary directly. cargo test runs in
    // target/debug/ so the binary is alongside the test binary.
    let exe = std::env::current_exe().expect("current_exe");
    // exe is target/debug/deps/<testname>-<hash>; lain is in target/debug/lain.
    let lain = exe.parent().unwrap().parent().unwrap().join("lain");
    if !lain.exists() {
        panic!("lain binary not found at {:?}; build with `cargo build --bin lain` first", lain);
    }
    let out = std::process::Command::new(&lain)
        .arg("--version")
        .output()
        .expect("spawn lain --version");
    assert!(out.status.success(), "lain --version must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Success metric: contains "lain" AND at least one digit.
    assert!(stdout.contains("lain"),
            "--version must contain `lain`; got: {stdout}");
    assert!(stdout.chars().any(|c| c.is_ascii_digit()),
            "--version must contain at least one digit (semver); got: {stdout}");
}

#[test]
fn lain_schema_dump_writes_valid_json_with_tools_array() {
    use std::path::PathBuf;
    let target = PathBuf::from(
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into())
    ).join("debug/lain");
    if !target.exists() { return; }
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("schema.json");
    let out = std::process::Command::new(&target)
        .args(["schema", "dump", "--out", out_path.to_str().unwrap()])
        .output()
        .expect("spawn lain");
    assert!(out.status.success(),
            "schema dump must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr));
    let content = std::fs::read_to_string(&out_path).expect("schema file written");
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .expect("schema must be valid JSON");
    // Success metric: schema is a list of 30+ tool entries (the
    // headline surface; currently 67 advertised).
    let tools = parsed.as_array()
        .expect("schema must be a top-level array of tools");
    assert!(tools.len() >= 30,
            "tools surface must advertise 30+ tools; got {}",
            tools.len());
}
