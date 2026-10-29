//! Direct tests for `trace_flow`'s core and seed resolution, on a real
//! `PetgraphBackend`. Symbol nodes carry 0-based line ranges like the
//! indexer's.

use super::*;
use crate::federation::contracts::model::{GraphqlOp, GraphqlProviderFact};
use crate::federation::graph_backend::PetgraphBackend;
use crate::federation::repo_id::RepoId;
use crate::schema::RepoNamespace;

struct Fixture {
    _tmp: tempfile::TempDir,
    backend: PetgraphBackend,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let backend = PetgraphBackend::new(tmp.path()).unwrap();
    Fixture { _tmp: tmp, backend }
}

fn node(repo: &str, kind: NodeType, name: &str, path: &str, line: u32) -> GraphNode {
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(kind.clone(), name.into(), path.into(), &ns);
    n.line_start = Some(line);
    n.line_end = Some(line + 3);
    n.id = GlobalId::new(&RepoId::new(repo).unwrap(), kind, path, name, Some(line))
        .as_str()
        .to_string();
    n
}

impl Fixture {
    fn add(&self, n: GraphNode) -> String {
        let id = n.id.clone();
        self.backend.upsert_node(n).unwrap();
        id
    }
    fn edge(&self, t: EdgeType, from: &str, to: &str) {
        self.backend
            .upsert_edge(GraphEdge::new(t, from.into(), to.into()))
            .unwrap();
    }
}

fn provider_node(repo: &str, field: &str) -> GraphNode {
    let mut n = node(
        repo,
        NodeType::Module,
        &format!("query:{field}"),
        "schema.graphql",
        3,
    );
    n.contract = vec![ContractFact::GraphqlProvider(GraphqlProviderFact {
        op: GraphqlOp::Query,
        field: field.into(),
        return_type: "Product".into(),
    })];
    n
}

#[allow(dead_code)]
struct Chain {
    hook: String,
    gq: String,
    provider: String,
    resolver: String,
    helper: String,
    table: String,
}

/// component-side hook -> graphql-call -> SDL provider -> resolver ->
/// helper -> table, one edge each, all static.
fn full_chain(f: &Fixture) -> Chain {
    let hook = f.add(node(
        "web",
        NodeType::Function,
        "useProductDetailsQuery",
        "src/hooks.ts",
        10,
    ));
    let gq = f.add(node(
        "web",
        NodeType::Function,
        "graphql-call:query:product",
        "src/ProductDetails.graphql",
        1,
    ));
    let provider = f.add(provider_node("api", "product"));
    let resolver = f.add(node(
        "api",
        NodeType::Method,
        "resolve_product",
        "api/schema.py",
        40,
    ));
    let helper = f.add(node(
        "api",
        NodeType::Function,
        "load_product",
        "api/loaders.py",
        8,
    ));
    let table = f.add(node("api", NodeType::Table, "product", "api/db.py", 0));
    f.edge(EdgeType::Calls, &hook, &gq);
    f.edge(EdgeType::Binds, &gq, &provider);
    f.edge(EdgeType::CallsGraphql, &provider, &resolver);
    f.edge(EdgeType::Calls, &resolver, &helper);
    f.edge(EdgeType::ReadsTable, &helper, &table);
    Chain {
        hook,
        gq,
        provider,
        resolver,
        helper,
        table,
    }
}

fn names(p: &FlowPath) -> Vec<&str> {
    p.hops.iter().map(|h| h.node.name.as_str()).collect()
}

#[test]
fn downstream_follows_the_whole_chain_to_the_table() {
    let f = fixture();
    let c = full_chain(&f);
    let r = compute_flow(
        &f.backend,
        &[c.hook.clone()],
        FlowDirection::Downstream,
        8,
        50,
    )
    .unwrap();
    assert_eq!(r.paths.len(), 1);
    let p = &r.paths[0];
    assert_eq!(
        names(p),
        vec![
            "useProductDetailsQuery",
            "graphql-call:query:product",
            "query:product",
            "resolve_product",
            "load_product",
            "product"
        ]
    );
    assert_eq!(
        p.boundary_hops, 4,
        "document link, Binds, CallsGraphql, ReadsTable"
    );
    assert!(p.hops[0].edge.is_none(), "the seed has no inbound edge");
    assert!(p
        .hops
        .iter()
        .all(|h| h.edge.as_ref().is_none_or(is_engine_fact)));
    assert!(r.gaps.is_empty(), "complete chain, no gaps: {:?}", r.gaps);
    assert!(r.truncation_reasons.is_empty());
}

#[test]
fn upstream_from_a_table_walks_back_to_the_component() {
    let f = fixture();
    let c = full_chain(&f);
    let r = compute_flow(
        &f.backend,
        &[c.table.clone()],
        FlowDirection::Upstream,
        8,
        50,
    )
    .unwrap();
    assert_eq!(r.paths.len(), 1);
    assert_eq!(
        names(&r.paths[0]),
        vec![
            "product",
            "load_product",
            "resolve_product",
            "query:product",
            "graphql-call:query:product",
            "useProductDetailsQuery"
        ]
    );
    assert!(r.gaps.is_empty(), "{:?}", r.gaps);
}

#[test]
fn a_provider_without_a_resolver_edge_is_a_named_gap() {
    let f = fixture();
    let gq = f.add(node(
        "web",
        NodeType::Function,
        "graphql-call:query:product",
        "a.graphql",
        1,
    ));
    let provider = f.add(provider_node("api", "product"));
    f.edge(EdgeType::Binds, &gq, &provider);
    let r = compute_flow(&f.backend, &[gq], FlowDirection::Downstream, 8, 50).unwrap();
    assert_eq!(r.paths.len(), 1, "the chain still reaches the provider");
    assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
    assert_eq!(r.gaps[0].kind, "no_handler");
    assert_eq!(r.gaps[0].node_id, provider);
    assert!(
        r.gaps[0].reason.contains("graphql:query:product"),
        "{}",
        r.gaps[0].reason
    );
}

#[test]
fn a_chain_that_never_reaches_a_table_says_why() {
    let f = fixture();
    let a = f.add(node("api", NodeType::Function, "resolve_x", "api/x.py", 1));
    let b = f.add(node("api", NodeType::Function, "helper", "api/x.py", 9));
    f.edge(EdgeType::Calls, &a, &b);
    let r = compute_flow(&f.backend, &[a], FlowDirection::Downstream, 8, 50).unwrap();
    let g = r
        .gaps
        .iter()
        .find(|g| g.kind == "no_data_sink")
        .expect("no_data_sink gap");
    assert!(g.reason.contains("ORM"), "{}", g.reason);
}

#[test]
fn an_unlinked_call_site_is_reported_when_walking_upstream() {
    let f = fixture();
    let gq = f.add(node(
        "web",
        NodeType::Function,
        "graphql-call:query:product",
        "a.graphql",
        1,
    ));
    let provider = f.add(provider_node("api", "product"));
    f.edge(EdgeType::Binds, &gq, &provider);
    let r = compute_flow(&f.backend, &[provider], FlowDirection::Upstream, 8, 50).unwrap();
    let g = r
        .gaps
        .iter()
        .find(|g| g.kind == "unlinked_call_site")
        .expect("unlinked_call_site gap");
    assert_eq!(g.node_id, gq);
    assert!(g.reason.contains("no linked function"), "{}", g.reason);
}

#[test]
fn a_table_nobody_touches_is_a_gap_upstream() {
    let f = fixture();
    let t = f.add(node("api", NodeType::Table, "orphan", "api/db.py", 0));
    let r = compute_flow(&f.backend, &[t], FlowDirection::Upstream, 8, 50).unwrap();
    assert_eq!(r.gaps.len(), 1);
    assert_eq!(r.gaps[0].kind, "no_table_access");
}

#[test]
fn depth_truncation_is_flagged() {
    let f = fixture();
    let ids: Vec<String> = (0..6)
        .map(|i| {
            f.add(node(
                "api",
                NodeType::Function,
                &format!("f{i}"),
                "api/c.py",
                i * 10,
            ))
        })
        .collect();
    for w in ids.windows(2) {
        f.edge(EdgeType::Calls, &w[0], &w[1]);
    }
    let r = compute_flow(
        &f.backend,
        &[ids[0].clone()],
        FlowDirection::Downstream,
        2,
        50,
    )
    .unwrap();
    assert_eq!(r.truncation_reasons, vec!["depth"]);
    assert_eq!(names(&r.paths[0]), vec!["f0", "f1", "f2"]);
    // Deep enough: no truncation.
    let r = compute_flow(
        &f.backend,
        &[ids[0].clone()],
        FlowDirection::Downstream,
        8,
        50,
    )
    .unwrap();
    assert!(r.truncation_reasons.is_empty());
}

#[test]
fn cap_truncation_is_flagged_and_boundary_chains_rank_first() {
    let f = fixture();
    let root = f.add(node("api", NodeType::Function, "root", "api/c.py", 0));
    let plain: Vec<String> = (0..3)
        .map(|i| {
            f.add(node(
                "api",
                NodeType::Function,
                &format!("leaf{i}"),
                "api/c.py",
                10 + i,
            ))
        })
        .collect();
    for p in &plain {
        f.edge(EdgeType::Calls, &root, p);
    }
    let table = f.add(node("api", NodeType::Table, "t", "api/db.py", 0));
    f.edge(EdgeType::WritesTable, &root, &table);
    let r = compute_flow(&f.backend, &[root], FlowDirection::Downstream, 8, 2).unwrap();
    assert_eq!(r.paths.len(), 2);
    assert_eq!(r.truncation_reasons, vec!["cap"]);
    assert_eq!(
        names(&r.paths[0]),
        vec!["root", "t"],
        "the chain crossing a boundary ranks first"
    );
}

#[test]
fn heuristic_edges_are_not_engine_facts() {
    let f = fixture();
    let a = f.add(node("api", NodeType::Function, "a", "api/c.py", 0));
    let b = f.add(node("api", NodeType::Function, "b", "api/c.py", 10));
    let mut e = GraphEdge::new(EdgeType::Calls, a.clone(), b);
    e.provenance = Some(EdgeProvenance::Heuristic {
        confidence: 0.4,
        detector: "test".into(),
    });
    f.backend.upsert_edge(e).unwrap();
    let r = compute_flow(&f.backend, &[a], FlowDirection::Downstream, 8, 50).unwrap();
    let hop = &r.paths[0].hops[1];
    assert!(!hop_to_value(hop)["engine_fact"].as_bool().unwrap());
    assert!((r.paths[0].min_confidence - 0.4).abs() < 1e-6);
}

#[test]
fn symbol_resolution_by_id_name_and_ambiguity() {
    let f = fixture();
    let a = f.add(node(
        "api",
        NodeType::Function,
        "resolve_product",
        "api/a.py",
        1,
    ));
    let b = f.add(node(
        "web",
        NodeType::Function,
        "resolve_product",
        "web/b.ts",
        2,
    ));

    // Exact id.
    assert_eq!(
        resolve_symbol_seed(&f.backend, &a, None).unwrap().ids,
        vec![a.clone()]
    );
    // Name shared by two repos: ambiguous, with candidates.
    match resolve_symbol_seed(&f.backend, "resolve_product", None) {
        Err(SeedError::Ambiguous { candidates, .. }) => assert_eq!(candidates.len(), 2),
        other => panic!("expected ambiguity, got {other:?}"),
    }
    // `repo_id` disambiguates.
    assert_eq!(
        resolve_symbol_seed(&f.backend, "resolve_product", Some("web"))
            .unwrap()
            .ids,
        vec![b]
    );
    // Unknown.
    assert!(matches!(
        resolve_symbol_seed(&f.backend, "nope", None),
        Err(SeedError::NotFound { what: "symbol", .. })
    ));
}

#[test]
fn table_seed_accepts_the_prefix_and_explains_a_miss() {
    let f = fixture();
    let t = f.add(node("api", NodeType::Table, "user", "api/db.py", 0));
    assert_eq!(
        resolve_table_seed(&f.backend, "table:user").unwrap().ids,
        vec![t.clone()]
    );
    assert_eq!(resolve_table_seed(&f.backend, "user").unwrap().ids, vec![t]);
    match resolve_table_seed(&f.backend, "product") {
        Err(SeedError::NotFound { hint, .. }) => assert!(hint.contains("ORM"), "{hint}"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn output_value_matches_the_published_shape() {
    let f = fixture();
    let c = full_chain(&f);
    let r = compute_flow(
        &f.backend,
        &[c.resolver.clone()],
        FlowDirection::Downstream,
        8,
        50,
    )
    .unwrap();
    let v = flow_to_value(
        &json!({"symbol": "resolve_product"}),
        FlowDirection::Downstream,
        &[c.resolver.clone()],
        &r,
        &json!({"reviewed": [], "unreviewed": [], "configured_only": true}),
    );
    assert_eq!(v["direction"], json!("downstream"));
    assert_eq!(v["truncated"], json!(false));
    let first = &v["paths"][0]["hops"][0];
    assert_eq!(first["edge_type"], Value::Null);
    assert_eq!(first["engine_fact"], json!(true));
    let second = &v["paths"][0]["hops"][1];
    assert_eq!(second["edge_type"], json!("Calls"));
    assert_eq!(second["repo"], json!("api"));
    let text = render_trace_flow(&v);
    assert!(
        text.starts_with("# trace_flow downstream (1 path(s), 0 gap(s))"),
        "{text}"
    );
}

// ─── strategy: breadth vs depth ───────────────────────────────────────

/// root -> a, root -> b, a -> shared, b -> shared, shared -ReadsTable-> t.
fn diamond(f: &Fixture) -> String {
    let root = f.add(node("api", NodeType::Function, "root", "api/d.py", 0));
    let a = f.add(node("api", NodeType::Function, "a", "api/d.py", 10));
    let b = f.add(node("api", NodeType::Function, "b", "api/d.py", 20));
    let shared = f.add(node("api", NodeType::Function, "shared", "api/d.py", 30));
    let t = f.add(node("api", NodeType::Table, "t", "api/db.py", 0));
    f.edge(EdgeType::Calls, &root, &a);
    f.edge(EdgeType::Calls, &root, &b);
    f.edge(EdgeType::Calls, &a, &shared);
    f.edge(EdgeType::Calls, &b, &shared);
    f.edge(EdgeType::ReadsTable, &shared, &t);
    root
}

fn call_chain(f: &Fixture, len: u32) -> Vec<String> {
    let ids: Vec<String> = (0..len)
        .map(|i| {
            f.add(node(
                "api",
                NodeType::Function,
                &format!("f{i}"),
                "api/c.py",
                i * 10,
            ))
        })
        .collect();
    for w in ids.windows(2) {
        f.edge(EdgeType::Calls, &w[0], &w[1]);
    }
    ids
}

fn sorted_names(r: &FlowResult) -> Vec<Vec<String>> {
    let mut v: Vec<Vec<String>> = r
        .paths
        .iter()
        .map(|p| p.hops.iter().map(|h| h.node.name.clone()).collect())
        .collect();
    v.sort();
    v
}

fn walk(f: &Fixture, seed: &str, strategy: FlowStrategy, depth: u32, cap: usize) -> FlowResult {
    compute_flow_with(
        &f.backend,
        &[seed.to_string()],
        FlowDirection::Downstream,
        depth,
        cap,
        strategy,
        NODE_BUDGET,
    )
    .unwrap()
}

#[test]
fn depth_reports_both_chains_through_a_shared_node_where_breadth_reports_one() {
    let f = fixture();
    let root = diamond(&f);

    let breadth = walk(&f, &root, FlowStrategy::Breadth, 8, 50);
    let through_shared = breadth
        .paths
        .iter()
        .filter(|p| names(p).contains(&"shared"))
        .count();
    assert_eq!(through_shared, 1, "breadth reaches `shared` once");

    let depth = walk(&f, &root, FlowStrategy::Depth, 8, 50);
    assert_eq!(
        sorted_names(&depth),
        vec![
            vec!["root", "a", "shared", "t"],
            vec!["root", "b", "shared", "t"],
        ]
    );
    assert!(depth.truncation_reasons.is_empty());
    assert_eq!(depth.strategy, FlowStrategy::Depth);
    // `nodes_visited` counts distinct nodes, not node entries.
    assert_eq!(depth.nodes_visited, 5);
}

#[test]
fn depth_follows_a_chain_to_its_end_and_flags_the_depth_limit() {
    let f = fixture();
    let ids = call_chain(&f, 6);
    let r = walk(&f, &ids[0], FlowStrategy::Depth, 3, 50);
    assert_eq!(r.truncation_reasons, vec!["depth"]);
    assert_eq!(names(&r.paths[0]), vec!["f0", "f1", "f2", "f3"]);
    let r = walk(&f, &ids[0], FlowStrategy::Depth, 8, 50);
    assert!(r.truncation_reasons.is_empty());
    assert_eq!(names(&r.paths[0]).len(), 6);
}

#[test]
fn depth_is_cycle_safe() {
    let f = fixture();
    let a = f.add(node("api", NodeType::Function, "a", "api/c.py", 0));
    let b = f.add(node("api", NodeType::Function, "b", "api/c.py", 10));
    let c = f.add(node("api", NodeType::Function, "c", "api/c.py", 20));
    f.edge(EdgeType::Calls, &a, &b);
    f.edge(EdgeType::Calls, &b, &c);
    f.edge(EdgeType::Calls, &c, &a);
    let r = walk(&f, &a, FlowStrategy::Depth, 12, 50);
    assert_eq!(sorted_names(&r), vec![vec!["a", "b", "c"]]);
    assert!(r.truncation_reasons.is_empty());
}

#[test]
fn depth_node_budget_truncation_is_flagged() {
    let f = fixture();
    let ids = call_chain(&f, 6);
    let r = compute_flow_with(
        &f.backend,
        &[ids[0].clone()],
        FlowDirection::Downstream,
        12,
        50,
        FlowStrategy::Depth,
        3,
    )
    .unwrap();
    assert_eq!(r.truncation_reasons, vec!["node_budget"]);
    assert!(r.nodes_visited <= 3);
}

#[test]
fn breadth_node_budget_truncation_is_flagged_too() {
    let f = fixture();
    let root = diamond(&f);
    let r = compute_flow_with(
        &f.backend,
        &[root],
        FlowDirection::Downstream,
        8,
        50,
        FlowStrategy::Breadth,
        2,
    )
    .unwrap();
    assert!(r.truncation_reasons.contains(&"node_budget"));
}

#[test]
fn depth_cap_keeps_the_boundary_chain_and_is_deterministic() {
    let f = fixture();
    let root = f.add(node("api", NodeType::Function, "root", "api/c.py", 0));
    let leaf = f.add(node("api", NodeType::Function, "aaa", "api/c.py", 10));
    let t = f.add(node("api", NodeType::Table, "t", "api/db.py", 0));
    f.edge(EdgeType::Calls, &root, &leaf);
    f.edge(EdgeType::ReadsTable, &root, &t);
    let first = walk(&f, &root, FlowStrategy::Depth, 8, 1);
    let second = walk(&f, &root, FlowStrategy::Depth, 8, 1);
    assert_eq!(first.truncation_reasons, vec!["cap"]);
    assert_eq!(first.paths.len(), 1);
    assert_eq!(names(&first.paths[0]), vec!["root", "t"]);
    assert_eq!(sorted_names(&first), sorted_names(&second));
}

#[test]
fn depth_walks_the_full_component_to_table_chain_upstream() {
    let f = fixture();
    let c = full_chain(&f);
    let r = compute_flow_with(
        &f.backend,
        &[c.table.clone()],
        FlowDirection::Upstream,
        12,
        50,
        FlowStrategy::Depth,
        NODE_BUDGET,
    )
    .unwrap();
    assert_eq!(
        names(&r.paths[0]),
        vec![
            "product",
            "load_product",
            "resolve_product",
            "query:product",
            "graphql-call:query:product",
            "useProductDetailsQuery",
        ]
    );
}

#[test]
fn default_strategy_is_breadth_and_the_output_shape_is_unchanged() {
    let f = fixture();
    let root = diamond(&f);
    let default = compute_flow(
        &f.backend,
        &[root.clone()],
        FlowDirection::Downstream,
        8,
        50,
    )
    .unwrap();
    let explicit = walk(&f, &root, FlowStrategy::Breadth, 8, 50);
    assert_eq!(sorted_names(&default), sorted_names(&explicit));
    assert_eq!(default.truncation_reasons, explicit.truncation_reasons);
    assert_eq!(default.gaps, explicit.gaps);
    assert_eq!(default.nodes_visited, explicit.nodes_visited);

    let scope = json!({"reviewed": [], "unreviewed": [], "configured_only": true});
    let v = flow_to_value(
        &json!({"symbol": "root"}),
        FlowDirection::Downstream,
        &[root.clone()],
        &default,
        &scope,
    );
    assert!(v.get("strategy").is_none(), "default output omits strategy");
    assert!(v.get("repos_not_ready").is_none());

    let depth = walk(&f, &root, FlowStrategy::Depth, 8, 50);
    let v = flow_to_value(
        &json!({"symbol": "root"}),
        FlowDirection::Downstream,
        &[root],
        &depth,
        &scope,
    );
    assert_eq!(v["strategy"], json!("depth"));
}

// ─── repos still indexing ─────────────────────────────────────────────

#[test]
fn no_consumers_while_repos_are_indexing_is_reported_as_pending() {
    let f = fixture();
    let provider = f.add(provider_node("api", "product"));
    let mut r = compute_flow(&f.backend, &[provider], FlowDirection::Upstream, 8, 50).unwrap();
    assert_eq!(r.gaps[0].kind, "no_consumers");

    // Every repo reviewed: the gap is left exactly as it was.
    let ready = json!({"reviewed": [], "unreviewed": [], "configured_only": true});
    mark_pending_gaps(&mut r, &ready);
    assert_eq!(r.gaps[0].kind, "no_consumers");
    assert!(r.gaps[0].reason.contains("does not index"));

    let warming = json!({
        "reviewed": [],
        "unreviewed": [
            {"repo": "storefront", "reason": "not_ready"},
            {"repo": "saleor-dashboard", "reason": "not_ready"}
        ],
        "configured_only": true
    });
    mark_pending_gaps(&mut r, &warming);
    assert_eq!(r.gaps[0].kind, "consumers_pending");
    let reason = &r.gaps[0].reason;
    assert!(reason.contains("still indexing"), "{reason}");
    assert!(reason.contains("saleor-dashboard (not_ready)"), "{reason}");
    assert!(reason.contains("storefront (not_ready)"), "{reason}");
    assert!(!reason.contains("does not index"), "{reason}");

    let v = flow_to_value(
        &json!({"endpoint": {}}),
        FlowDirection::Upstream,
        &[],
        &r,
        &warming,
    );
    assert_eq!(
        v["repos_not_ready"],
        json!(["saleor-dashboard (not_ready)", "storefront (not_ready)"])
    );
    assert_eq!(v["gaps"][0]["kind"], json!("consumers_pending"));
}
