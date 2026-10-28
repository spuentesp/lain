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
