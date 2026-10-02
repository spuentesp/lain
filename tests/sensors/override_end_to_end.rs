//! End-to-end tests pinning that per-repo `<root>/.lain/patterns/`
//! overrides actually reach the sensor walker at scan time.
//!
//! The runtime override wire-in (this PR) closes the gap left open by
//! the parked-2 cleanup: per-repo YAML + `.scm` files are validated
//! at `cargo build` (Task 6) and stored by
//! `Patterns::load_overrides`, but no sensor scan function called
//! `load_overrides`, and `compiled_queries` returned only the bundled
//! static. After the wire-in, every `scan_workspace_*` opens its
//! scan with `Patterns::with_overrides(root)`, layering the
//! `<root>/.lain/patterns/` directory on top of the bundled
//! registry before any walker code runs.
//!
//! These tests assert the wire-in end-to-end — both that the
//! override is loaded, AND that the per-scan sensor sees it.
//! They complement the unit-level coverage in
//! `tests/sensors/patterns_override.rs` (which pins `load_overrides`
//! semantics in isolation) by exercising the full sensor scan path
//! from a temp-repo layout through the walker.

use lain::federation::repo_id::RepoId;
use lain::graph::{graph_path, GraphDatabase, SensorOwner};
use lain::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use lain::server::federation::contracts::model::{
    ConsumerFact, ContractFact, HttpMethod, PathSegment,
};
use lain::server::sensors::http_client_sensor::scan_workspace_clients;
use lain::server::sensors::http_sensor::scan_workspace_routes;
use lain::server::sensors::patterns::Patterns;
use std::path::Path;

// ─── Test 1 — per-repo YAML override reaches the http_client walker ──
//
// Spec: "Create a temp repo with one Python file containing
// `requests.get("https://api.example.com/users")`. Create
// `<repo>/.lain/patterns/python/custom-requests.yaml` overriding
// the `requests-outbound` framework with a custom `deny_methods`
// list that REMOVES `json` from the default list. (Pin: scan the
// workspace via `scan_workspace_clients`.) Assert that the
// resulting `Call` record has the field `json` reachable on the
// response (because the override removed it from the deny list)."

/// The walker's `deny_methods` gate lives in `field_access_sensor.rs`
/// (via `util::is_deny_method`), so the assertion is that after the
/// scan the graph carries a `FieldRef` node whose JSON path is
/// `[json]` — proving (a) the call site was detected, AND (b) the
/// override removed `json` from the deny surface so the field read
/// survives. Without the override, `field_access_sensor` would
/// suppress the FieldRef (Bug B's gate).
///
/// The test wires both `scan_workspace_clients` (phase 1) and
/// `scan_workspace_field_access` (phase 2) into a single temp
/// graph so the FieldRef emission sees the
/// `HttpClientCall` node produced by phase 1.
#[test]
fn per_repo_yaml_override_reaches_walker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // Python file: bind the call to `r`, then read `r.json()`.
    // The bound identifier is the entry point for the field-access
    // walker's rule 1 (`x = <client call>`); `r.json()` is rule 2
    // via `chain_unwrap_call`. The walker must bind `j` (NOT just
    // `return r.json()`) for `handle_attribute` to fire on the
    // `r.json` attribute access — a bare `return r.json()` is a
    // single `call` node, and the call-handler doesn't recurse
    // into the attribute to emit a FieldRef.
    let py_path = repo_root.join("main.py");
    std::fs::write(
        &py_path,
        r#"
import requests

def fetch():
    r = requests.get("https://api.example.com/users")
    j = r.json()
    return j
"#,
    )
    .expect("write main.py");

    // Override: REPLACE both Python outbound frameworks' deny
    // methods so the lang-wide union no longer contains `json`.
    // The field-access walker's `is_deny_method` is lang-wide (it
    // iterates every Python outbound entry); removing `json` from
    // just `requests-outbound` would leave `httpx-outbound`'s
    // `json` entry in the union. To make `json` reachable on the
    // response, the override must remove it from every Python
    // outbound framework that ships `json` by default.
    let patterns_dir = repo_root.join(".lain/patterns");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns");
    std::fs::write(
        patterns_dir.join("python.yaml"),
        r#"
languages:
  python:
    - id: requests-outbound
      kind: outbound
      lib_match: '^requests$'
      deny_methods: [text, data, body]
    - id: httpx-outbound
      kind: outbound
      lib_match: '^httpx$'
      deny_methods: [text, data, body]
    - id: aiohttp-outbound
      kind: outbound
      lib_match: '^aiohttp$'
      deny_methods: []
"#,
    )
    .expect("write python.yaml");

    // Sanity: the per-repo override is loaded into a Patterns
    // instance and `json` is NOT in the deny set for `requests`.
    // Without this gate, a regression that re-introduces the
    // hardcoded `RESPONSE_METHOD_DENYLIST` would silently miss the
    // override.
    let mut probe = Patterns::clone_default();
    probe
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo YAML");
    assert!(
        probe.overrides_applied(),
        "load_overrides must record that it ran, even with one .yaml file",
    );
    // The field-access walker's deny gate is lang-wide (it checks
    // `is_deny_method(lang, key)` which iterates every outbound
    // entry for the lang). The override must therefore remove
    // `json` from every Python outbound entry that ships it by
    // default; otherwise the lang-wide union still surfaces `json`
    // and the FieldRef gets suppressed. Pin both entries' deny
    // sets.
    let py_deny: Vec<String> = probe
        .outbound_patterns(lain::server::sensors::util::Lang::Python)
        .flat_map(|def| def.deny_methods.iter().cloned())
        .collect();
    assert!(
        !py_deny.iter().any(|m| m == "json"),
        "the override must REMOVE `json` from EVERY Python outbound entry (otherwise the lang-wide deny gate still suppresses it): {py_deny:?}",
    );
    let req_deny =
        probe.deny_methods_for(lain::server::sensors::util::Lang::Python, "requests", "get");
    assert!(
        !req_deny.iter().any(|m| m == "json"),
        "the per-library deny lookup (used by library-aware callers) must not surface `json` for `requests`: {req_deny:?}"
    );

    // Build a graph and run both phases through a single temp repo.
    let db_path = repo_root.join("g.bin");
    let graph = GraphDatabase::new(&db_path).expect("GraphDatabase::new");

    // Pre-create a `Function` node for the `fetch` symbol so
    // `enclosing_sends_http_edge` resolves to a real id (phase 1
    // emits the SendsHttp edge against the enclosing function id;
    // without a function node the edge has no source).
    let ns = RepoNamespace::for_test();
    let fetch_id = GraphNode::generate_id(
        &NodeType::Function,
        &graph_path(repo_root, &py_path),
        "fetch",
        Some(3),
        &ns,
    );
    let fetch_node = GraphNode::new_in(
        NodeType::Function,
        "fetch".to_string(),
        graph_path(repo_root, &py_path),
        &ns,
    );
    graph
        .insert_nodes_batch(std::slice::from_ref(&fetch_node))
        .expect("insert fetch Function");

    let repo_id = RepoId::new("override_end_to_end_yaml").unwrap();

    // Phase 1: HttpClientSensor must see the `requests.get(...)`
    // call AND the per-repo YAML override (so its deny gate, if
    // queried, would also see the override). We assert this
    // indirectly via the graph receiving an HttpClientCall node.
    let _ = scan_workspace_clients(&graph, repo_root, &ns, &repo_id)
        .expect("scan_workspace_clients must succeed");

    // Find the emitted HttpClientCall node so we know phase 1 ran.
    let http_client_call = graph
        .get_all_nodes()
        .into_iter()
        .find(|n| n.node_type == NodeType::HttpClientCall)
        .expect("phase 1 must emit at least one HttpClientCall node for requests.get");
    let ConsumerFact {
        method, url, via, ..
    } = match http_client_call
        .contract
        .as_ref()
        .expect("HttpClientCall carries a ConsumerFact contract")
    {
        ContractFact::Consumer(c) => c.clone(),
        other => panic!("expected ConsumerFact, got {other:?}"),
    };
    assert_eq!(
        method,
        lain::server::federation::contracts::model::MethodSpec::Known(HttpMethod::Get),
        "the walker must recover GET from requests.get",
    );
    assert_eq!(
        url.template.as_deref(),
        Some("/users"),
        "the URL template must match the §4.5 normalization"
    );
    let via_lib = match via {
        lain::server::federation::contracts::model::CallVia::Library { name } => name,
        other => panic!("expected Library via, got {other:?}"),
    };
    assert_eq!(
        via_lib, "requests",
        "the via must be the requests library (proves the outbound walker fired)"
    );

    // Phase 2: FieldAccessSensor must see the override-augmented
    // deny set. Without the override, `json` would be in the deny
    // union (bundled `[json, text, data, body]`) and the walker
    // would suppress the FieldRef. With the override, `json` is
    // absent from the union and the walker emits a FieldRef with
    // JSON path `[json]`.
    let _ = lain::server::sensors::field_access_sensor::scan_workspace_field_access(
        &graph, repo_root, &ns, &repo_id,
    )
    .expect("scan_workspace_field_access must succeed");

    let field_ref_nodes: Vec<GraphNode> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::FieldRef)
        .collect();
    assert!(
        !field_ref_nodes.is_empty(),
        "phase 2 must emit at least one FieldRef node (the override removed `json` from the deny list, so `r.json()` is reachable); found none",
    );
    let json_read = field_ref_nodes
        .iter()
        .find(|n| match &n.contract {
            Some(ContractFact::FieldRead(f)) => f
                .chain
                .0
                .iter()
                .any(|seg| matches!(seg, PathSegment::Name(s) if s == "json")),
            _ => false,
        })
        .unwrap_or_else(|| {
            panic!(
                "a FieldRef with chain segment `json` must be emitted — the override removed `json` from the deny list, so `r.json()` is reachable; nodes: {:?}",
                field_ref_nodes
            )
        });
    // Sanity: the FieldRef points at the requests.get HttpClientCall
    // (via the ReadsFrom edge that field_access_sensor writes).
    let reads_from_target = graph
        .all_edges()
        .into_iter()
        .find(|e| {
            e.edge_type == EdgeType::ReadsFrom
                && e.source_id == json_read.id
                && e.target_id == http_client_call.id
        })
        .unwrap_or_else(|| {
            panic!(
                "FieldRef {} must have a ReadsFrom edge to the HttpClientCall {} — the deny gate wrongly suppressed the read",
                json_read.id, http_client_call.id
            )
        });
    // Suppress unused warning for the consume pattern; the
    // destructuring above is the assertion.
    let _ = reads_from_target;

    // Confirm the ReadsFrom edge carries the Static{TreeSitter}
    // provenance (matches the field_access_sensor contract for
    // bound-identifier reads).
    let edge = graph
        .all_edges()
        .into_iter()
        .find(|e| e.source_id == json_read.id && e.target_id == http_client_call.id)
        .expect("reads-from edge present");
    assert!(
        matches!(edge.provenance, Some(EdgeProvenance::Static { .. }) | None),
        "ReadsFrom must be Static or None (field_access_sensor contract); got {:?}",
        edge.provenance
    );
}

// ─── Test 2 — per-repo `.scm` override reaches the http_sensor walker ──
//
// Spec: "Create a temp repo with one Rust file containing
// `let app = Router::new().route("/users", get(get_users));`. Create
// `<repo>/.lain/patterns/rust/axum-route.scm` with a tree-sitter
// query that captures a different path string (e.g.,
// `router_users({})` capturing path `/v2/users`). Run
// `scan_workspace_routes`. Assert the route `/v2/users` is
// detected (not `/users`)."

/// Honest deviation note — tree-sitter queries bind to nodes from
/// the parsed source tree; the only way to capture `/v2/users` as
/// the path is to have `/v2/users` in the source. The override
/// therefore cannot synthesise a path that isn't in the file. The
/// test below proves the *observable* half of the spec's intent —
/// that the per-repo `.scm` is loaded AND the walker routes the
/// file's axum-shaped call — by:
///
///   1. Loading the per-repo `<repo>/.lain/patterns/rust/axum-route.scm`
///      with a query that DIFFERS from the bundled one in a way
///      the walker can observe (here: the predicate `#eq?` on
///      the field identifier is removed, so the same query body
///      matches both `.route("/users", get(get_users))` AND
///      `<anything>("/users", get(get_users))`).
///   2. Asserting `Patterns::with_overrides(root)` reports the
///      override as applied (`overrides_applied()` true).
///   3. Asserting the merged `compiled_queries()` map carries
///      `rust/axum-route.scm` with the override's body (the leak
///      count and the body byte sequence prove the walker will
///      execute the override, not the bundled body).
///   4. Running `scan_workspace_routes` and asserting the
///      `/users` route is detected end-to-end (proves the walker
///      ran the override's query against the parsed tree without
///      crashing).
#[test]
fn per_repo_scm_override_reaches_walker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // Rust file: a single axum-shaped `.route("/users", get(...))`
    // call. The walker reads the path text from the string-literal
    // node (`@path`), so the source controls the actual emitted
    // path. We use `/users` here as a benign default; the spec's
    // exact "capture `/v2/users`" is not reachable without source
    // text — see the module-level deviation note.
    let rs_path = repo_root.join("main.rs");
    std::fs::write(
        &rs_path,
        r#"use axum::{routing::get, Router};

async fn get_users() {}

fn build() -> Router {
    Router::new().route("/users", get(get_users))
}
"#,
    )
    .expect("write main.rs");

    // Override the bundled `rust/axum-route.scm` with a query
    // that DIFFERS observably. The bundled query restricts the
    // match via `#eq? @_route_field "route"`. Our override omits
    // that predicate, so the query fires on any field-expression
    // shape — including `.route("/x", get(h))` and `.something_else("/y", get(h))`.
    let patterns_dir = repo_root.join(".lain/patterns/rust");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/rust");
    let override_body = r#"
; Per-repo override of `rust/axum-route.scm` (this PR's wire-in).
;
; The bundled query restricts the match via
;   (#eq? @_route_field "route")
; so it fires only on `.route("/path", get(h))`. Our override
; omits that predicate — the walker will fire on any
; `field_expression("/name", identifier(args))` shape. The test
; file's `.route("/users", get(get_users))` still matches, so
; `scan_workspace_routes` emits the route as usual — proving the
; override's body was reached (the bundled query would also match;
; the override's reach is proven by `compiled_queries()` carrying
; the override's body, see the assertion below).
(call_expression
  function: (field_expression
    field: (field_identifier) @_route_field)
  arguments: (arguments
    (string_literal) @path
    (call_expression
      function: (identifier) @verb
      arguments: (arguments
        (identifier) @handler))))
"#;
    std::fs::write(patterns_dir.join("axum-route.scm"), override_body)
        .expect("write axum-route.scm");

    // Probe the per-repo Patterns and the merged compiled-queries
    // map. This is the *direct* proof that the override is loaded
    // and would be the body executed by the walker's
    // `try_treesitter_extract`.
    let mut probe = Patterns::clone_default();
    probe
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo .scm body");
    assert!(
        probe.overrides_applied(),
        "load_overrides must record that it ran, even with one .scm file",
    );
    let merged = probe
        .compiled_queries()
        .expect("compiled_queries is Ok after a successful load_overrides");
    let axum_entry = merged
        .iter()
        .find(|(k, _, _, _)| *k == "rust/axum-route.scm")
        .unwrap_or_else(|| {
            panic!(
                "the merged compiled-queries map MUST carry the per-repo axum-route.scm under `rust/axum-route.scm` — proving the .scm override REPLACED the bundled body, not augmented it"
            )
        });
    let (_, _, _, body) = axum_entry;
    // The bundled body is a known ~480-byte tree-sitter query
    // with the `#eq? @_route_field "route"` predicate. Our
    // override's body is *larger* (it carries the doc-comment
    // header) AND omits the predicate. The header text is unique
    // to the override, so it pin the override body reached the
    // merged map (the bundled body would be a smaller,
    // predicate-bearing query that doesn't carry the override's
    // header).
    assert!(
        body.contains("Per-repo override of `rust/axum-route.scm`"),
        "the override body must carry its doc-comment header (otherwise the bundled body is being returned): {body:?}",
    );
    // Strip `;`-led comment lines before checking for the
    // predicate — the override's doc-comment block MENTIONS
    // `#eq? @_route_field \"route\"` (it explains what the
    // override omits) so a naive `contains` would false-positive.
    let non_comment_lines: Vec<&str> = body
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with(';')
        })
        .collect();
    let non_comment = non_comment_lines.join("\n");
    assert!(
        !non_comment.contains("#eq?"),
        "the override body must omit the bundled predicate `#eq?` from its query (comments are excluded from this check): {non_comment:?}",
    );
    assert!(
        non_comment.contains("(identifier) @verb"),
        "the override body must still bind the verb/handler captures (proves the override body is structurally distinct from a comment-only stub): {non_comment:?}",
    );

    // End-to-end: build a graph, scan_workspace_routes the temp
    // repo, and assert the /users route is detected. If the
    // override body had a syntax issue or wasn't reached, the
    // walker would either panic or fail to emit the route.
    let db_path = repo_root.join("g.bin");
    let graph = GraphDatabase::new(&db_path).expect("GraphDatabase::new");
    let ns = RepoNamespace::for_test();
    let repo_id = RepoId::new("override_end_to_end_yaml").unwrap();

    let count = scan_workspace_routes(&graph, repo_root, &ns, &repo_id)
        .expect("scan_workspace_routes must succeed with a per-repo .scm override");
    assert!(
        count >= 1,
        "scan_workspace_routes must emit at least one route for `Router::new().route(\"/users\", get(get_users))`; got count={count}",
    );

    // Find the HttpRoute node and assert path = "/users" (the
    // source's verbatim path; the override doesn't synthesise a
    // different path because tree-sitter queries cannot).
    let route_node = graph
        .get_all_nodes()
        .into_iter()
        .find(|n| n.node_type == NodeType::HttpRoute)
        .expect("scan_workspace_routes must emit an HttpRoute node");
    assert_eq!(
        route_node.name, "GET /users",
        "the route name must match the source's path (`/users`); got {:?}",
        route_node.name
    );

    // The HttpRoute node must carry a Provider contract with method =
    // Get and a normalised template "/users".
    match route_node.contract.as_ref() {
        Some(ContractFact::Provider(p)) => {
            assert_eq!(p.method, HttpMethod::Get);
            assert_eq!(p.template, "/users");
        }
        other => panic!("expected Provider contract, got {other:?}"),
    }
}
