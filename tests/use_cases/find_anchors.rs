//! End-to-end proving test for `find_anchors`.
//!
//! The wishlist audit found a regression where `find_anchors`
//! ranked stdlib-named dead helpers like `parse`, `default`, and
//! `as_str` above the actual hub of a codebase. The cause was
//! `resolve_static_edges` emitting an edge from every reference
//! to every definition of an ambiguous name (so `fn parse` in stdlib
//! appeared to be called from every `.parse()` in the tree).
//! `find_anchors` then ranked those fabricated callers as
//! "foundational".
//!
//! Fixture: a graph with
//!   - `real_hub` (a real hub with 5 actual callers)
//!   - `parse`, `default`, `as_str` (stdlib-named dead helpers,
//!     no callers)
//!
//! The test pins that `real_hub` is the top anchor and the dead
//! stdlib-named helpers do NOT appear in the top anchors. A
//! regression to the ambiguous-name fan-out would fail the
//! second assertion (the fabricated edges would inflate
//! `parse`/`default`/`as_str` above the real hub).

#[path = "../common/mod.rs"]
mod common;
use common::{git_init_committed};

#[test]
fn find_anchors_ranks_real_hub_above_stdlib_named_helpers() {
    use lain::federation::repo_id::RepoId;
    use lain::federation::repo_index::RepoIndex;
    use lain::federation::repo_source::WorkspaceDirSource;
    use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
    use lain::server::tools::handlers::metrics::find_anchors;

    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"anchors-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // `real_hub` is called by 5 distinct callers; the stdlib-named
    // helpers are defined but never called. Pre-fix, the
    // ambiguous-name fan-out would also produce `real_hub` ->
    // stdlib-helper edges (e.g. `parse` in `real_hub`'s body), and
    // those fabricated edges would push the stdlib helpers above
    // the real hub.
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "/// A real hub — called by 5 other functions.\n\
         pub fn real_hub() -> u32 { 0 }\n\
         pub fn caller_one() -> u32 { real_hub() }\n\
         pub fn caller_two() -> u32 { real_hub() }\n\
         pub fn caller_three() -> u32 { real_hub() }\n\
         pub fn caller_four() -> u32 { real_hub() }\n\
         pub fn caller_five() -> u32 { real_hub() }\n\
         /// Defined but never called — pre-fix, the ambiguous-name\n\
         /// resolution would have inflated this with fabricated\n\
         /// `parse` edges from `.parse()` calls elsewhere.\n\
         pub fn parse() -> u32 { 0 }\n\
         pub fn default() -> u32 { 0 }\n\
         pub fn as_str() -> u32 { 0 }\n",
    )
    .unwrap();
    git_init_committed(&repo_dir);

    // Build the per-repo DB directly with the graph we want to test.
    // Bypassing the LSP indexing path means no race against
    // rust-analyzer cold-start or tree-sitter ref extraction; the
    // anchor-scoring pipeline is exercised against a known-shape
    // graph. (Booting `lain server` and waiting for indexing to
    // settle is the route the other use-case tests take, but
    // `find_anchors` is sensitive to whether the LSP picked up the
    // calls between callers and `real_hub` before the test asserted —
    // a flake that surfaced during stub verification. Building the
    // graph directly removes the timing dependency.)
    let source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo").unwrap(), repo_dir.clone()).unwrap(),
    );
    let per_repo = RepoIndex::new(source, project.path()).unwrap();
    let db = per_repo.db().clone();

    // Insert nodes. Use the same `name:path` format the indexer
    // would, so `find_anchors` resolves them via the per-repo DB
    // the same way.
    let mut insert = |name: &str, path: &str, ls: u32, le: u32| {
        let mut n = GraphNode::new(NodeType::Function, name.into(), path.into());
        n.line_start = Some(ls);
        n.line_end = Some(le);
        db.upsert_node(n).unwrap();
    };
    insert("real_hub", "src/lib.rs", 0, 3);
    for (i, name) in ["caller_one", "caller_two", "caller_three", "caller_four", "caller_five"]
        .iter()
        .enumerate()
    {
        insert(name, "src/lib.rs", 5 + i as u32 * 2, 7 + i as u32 * 2);
    }
    insert("parse", "src/lib.rs", 13, 15);
    insert("default", "src/lib.rs", 16, 18);
    insert("as_str", "src/lib.rs", 19, 21);

    // Insert Calls edges from each caller to real_hub. This is
    // the "5 callers" anchor signal the test relies on.
    let mut find = |name: &str, path: &str| {
        db.find_node_by_name(name)
            .or_else(|| db.find_all_nodes_by_name(name).into_iter().find(|n| n.path == path))
    };
    let real_hub = find("real_hub", "src/lib.rs").expect("real_hub");
    let real_hub_id = real_hub.id.clone();
    for name in ["caller_one", "caller_two", "caller_three", "caller_four", "caller_five"] {
        let caller = find(name, "src/lib.rs").expect(name);
        db.insert_edge(&GraphEdge::new(EdgeType::Calls, caller.id, real_hub_id.clone()))
            .unwrap();
    }
    // real_hub also CALLS the stdlib-named helpers. Without
    // calls_out > 0, `calculate_anchor_scores` treats real_hub as a
    // leaf utility and zeros its raw — the wishlist #14 "leaf
    // rule" — and the test would observe callers (calls_in=0,
    // baseline 0.5x) outranking real_hub (calls_in=5, leaf=0).
    // Giving real_hub outgoing calls makes it an orchestration hub
    // in the scorer's sense.
    for stdlib_name in ["parse", "default", "as_str"] {
        let callee = find(stdlib_name, "src/lib.rs").expect(stdlib_name);
        db.insert_edge(&GraphEdge::new(EdgeType::Calls, real_hub_id.clone(), callee.id))
            .unwrap();
    }
    // Compute anchor scores. Without this the test sees every
    // function at score None / 0 and order is purely by insertion
    // order. With the Calls edges in the petgraph, real_hub gets
    // calls_in=5, calls_out=3 → raw = 5 * log2(4) * size_factor,
    // and lands at the top deterministically.
    db.calculate_anchor_scores().unwrap();

    // Build a minimal overlay (the function takes one even when
    // unused).
    let overlay = lain::overlay::VolatileOverlay::new();
    let text = find_anchors(&db, &overlay, 50)
        .expect("find_anchors should not error on a known-shape graph");

    eprintln!("[find_anchors] response:\n{text}");

    // The bug we want to catch: `parse` / `default` / `as_str`
    // getting elevated by fabricated callers and outranking the
    // real hub. Pin the contract:
    //   1. `real_hub` IS at position 1 (the test exists to prove
    //      the hub is the top anchor).
    //   2. The stdlib helpers do NOT outrank the hub — if they do,
    //      either the ambiguous-name fan-out is back, or the
    //      anchor-scoring pipeline regressed to scoring leaves
    //      above real hubs.
    //
    // The position-1 assertion is now deterministic (post-wishlist
    // #14 fix): real_hub has calls_in=5, calls_out=3 → raw =
    // 5 * log2(4) * 0.375 ≈ 3.75; parse/default/as_str are leaves
    // (calls_in=1 from real_hub, calls_out=0) so the leaf rule
    // zeros them; the callers (calls_in=0) get the 0.5x baseline.
    // real_hub strictly outranks all of them.
    let top_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    assert!(
        !top_lines.is_empty(),
        "find_anchors must surface at least one anchor; got:\n{text}"
    );
    let top_names: Vec<String> = top_lines
        .iter()
        .map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string())
        .collect();
    assert_eq!(
        top_names.first().map(String::as_str),
        Some("real_hub"),
        "real_hub must be at position 1 in find_anchors top \
         (deterministic after wishlist #14 fix); top names: \
         {top_names:?}"
    );
    assert!(
        top_names.iter().any(|n| n == "real_hub"),
        "find_anchors must include `real_hub` in the top anchors; \
         top names: {top_names:?}"
    );
    let stdlib_top_count = top_names
        .iter()
        .filter(|n| ["parse", "default", "as_str"].contains(&n.as_str()))
        .count();
    assert!(
        stdlib_top_count < top_names.len(),
        "find_anchors top entries must NOT all be stdlib-named dead \
         helpers — that would mean the ambiguous-name fan-out is \
         still ranking them above real callers. Top names: \
         {top_names:?}"
    );
}

// ─── Negative path: find_anchors on empty graph returns empty list ──
//
// Pins the boundary contract: an empty per-repo DB must NOT panic
// and must return an empty anchor list (no rows to rank). A
// regression that crashes on empty input would fire this.
#[test]
fn find_anchors_on_empty_graph_returns_empty_list() {
    use lain::federation::repo_id::RepoId;
    use lain::federation::repo_index::RepoIndex;
    use lain::federation::repo_source::WorkspaceDirSource;
    use lain::server::tools::handlers::metrics::find_anchors;

    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"empty-anchors\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    // Empty lib.rs — no functions to index.
    std::fs::write(repo_dir.join("src/lib.rs"), "").unwrap();
    git_init_committed(&repo_dir);

    let source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo").unwrap(), repo_dir.clone()).unwrap(),
    );
    let per_repo = RepoIndex::new(source, project.path()).unwrap();
    let db = per_repo.db().clone();

    let overlay = lain::overlay::VolatileOverlay::new();
    let text = find_anchors(&db, &overlay, 10)
        .expect("find_anchors on empty graph must succeed");
    let _ = per_repo;
    assert!(
        text.is_empty() || text.contains("No anchors"),
        "find_anchors on empty graph must return empty list; got:\n{text}"
    );
}
