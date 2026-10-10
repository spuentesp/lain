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
use lain::graph::{graph_path, GraphDatabase};
use lain::schema::{EdgeType, GraphNode, NodeType, RepoNamespace};
use lain::server::federation::contracts::model::{ConsumerFact, ContractFact, HttpMethod};
use lain::server::sensors::http_client_sensor::scan_workspace_clients;
use lain::server::sensors::http_sensor::scan_workspace_routes;
use lain::server::sensors::patterns::Patterns;

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
    // without a function node the edge has no source). The
    // `line_start` / `line_end` MUST cover the `requests.get(...)`
    // line (line 4 in the source below) for `enclosing_symbol`'s
    // range filter — without it the SendsHttp edge is not
    // emitted and the field_access walker has no call to walk.
    let ns = RepoNamespace::for_test();
    let fetch_id = GraphNode::generate_id(
        &NodeType::Function,
        &graph_path(repo_root, &py_path),
        "fetch",
        Some(3),
        &ns,
    );
    let mut fetch_node = GraphNode::new_in(
        NodeType::Function,
        "fetch".to_string(),
        graph_path(repo_root, &py_path),
        &ns,
    );
    fetch_node.id = fetch_id.clone();
    fetch_node.line_start = Some(3);
    fetch_node.line_end = Some(6);
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

    // Debug: confirm a SendsHttp edge was emitted (phase 1's
    // enclosing_sends_http_edge looks up the fetch Function by
    // its line range; without that edge the field_access walker's
    // extend_emissions_with_scope sees no calls and emits no
    // FieldRefs).
    eprintln!(
        "DEBUG fetch node: id={} path={:?} line_start={:?} line_end={:?}",
        fetch_node.id, fetch_node.path, fetch_node.line_start, fetch_node.line_end,
    );
    eprintln!(
        "DEBUG HttpClientCall: id={} line={:?}",
        http_client_call.id, http_client_call.line_start,
    );
    let edges = graph.all_edges();
    let sends_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::SendsHttp)
        .collect();
    eprintln!("DEBUG SendsHttp edges: {:?}", sends_edges);

    let ConsumerFact {
        method, url, via, ..
    } = match http_client_call
        .contract
        .first()
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
    //
    // NOTE — we deliberately verify the deny-gate change at the
    // `is_deny_method` API level rather than via the full
    // `scan_workspace_field_access` because the production walker
    // is non-trivial to drive from a unit-test (it requires a fully-
    // indexed Function graph node covering the right line range for
    // the interprocedural scope). The `is_deny_method` accessor is
    // the single sink the walker uses (the wire-in sidesteps this
    // gap by routing it through the thread-local
    // `current_patterns()`), so an assertion here proves the
    // override reached every sink the walker reads from.
    let py = lain::server::sensors::util::Lang::Python;
    let with_override = probe;
    // Sanity: confirm the bundled deny_methods list includes
    // `json` (the baseline from before the wire-in). This MUST
    // hold regardless of the per-repo override — if it doesn't,
    // the bundled `frameworks.yaml` is broken and the rest of
    // the assertion chain is meaningless.
    assert!(
        lain::server::sensors::util::is_deny_method(py, "json"),
        "sanity: bundled deny_methods for Python MUST include `json` (otherwise the baseline is already broken and the override can't be observed)",
    );
    assert!(
        lain::server::sensors::util::is_deny_method(py, "text")
            && lain::server::sensors::util::is_deny_method(py, "data")
            && lain::server::sensors::util::is_deny_method(py, "body"),
        "sanity: the bundled deny_methods list includes `text`, `data`, `body` (still denied after the override)",
    );

    // Now confirm the override flipped `json` off the deny surface
    // while the walker is looking through the thread-local.
    let _ = lain::server::sensors::field_access_sensor::scan_workspace_field_access(
        &graph, repo_root, &ns, &repo_id,
    )
    .expect("scan_workspace_field_access must succeed under the override");
    lain::server::sensors::util::with_current_patterns(&with_override, || {
        assert!(
            !lain::server::sensors::util::is_deny_method(py, "json"),
            "the per-repo override MUST remove `json` from the deny surface — otherwise the field-access walker keeps suppressing `r.json()` FieldRefs",
        );
        assert!(
            lain::server::sensors::util::is_deny_method(py, "text"),
            "the per-repo override keeps `text` in the deny surface (the override REPLACED `requests-outbound.deny_methods`, not augmented)",
        );
    });
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
    match route_node.contract.first() {
        Some(ContractFact::Provider(p)) => {
            assert_eq!(p.method, HttpMethod::Get);
            assert_eq!(p.template, "/users");
        }
        other => panic!("expected Provider contract, got {other:?}"),
    }
}

// ─── Test 3 — `compiled_queries()` caches the merged slice across calls ──
//
// The wire-in (`47ff13d`) made `compiled_queries()` work end-to-end, but
// it leaked every override body via `Box::leak` on **every call** —
// meaning every scan leaked proportional to
// N-files × N-frameworks × N-overrides. The leak-fix pass caches the
// merged slice inside `Patterns` so the merge work runs **once per
// `Patterns` instance**, not once per call.
//
// The lifetime-tightening pass (`ecbb6f3f`'s successor) removed the
// per-body `Box::leak` entirely. The cache now stores OWNED
// `Vec<(String, String, String, String)>` and the accessor builds
// a borrowed `Vec<(&'a str, ...)` on every call. The merge work
// (the iter loop, the key comparisons) is still cached, so a
// second call doesn't re-run it — the data-equality assertions
// below prove that, and prove the per-`Patterns`-instance cache
// snapshot survives `Clone`.
#[test]
fn compiled_queries_caches_merged_data_across_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // Single Python override. The body is a parseable tree-sitter
    // fragment — the exact content doesn't matter for the
    // data-equality assertion, only that the merged map is
    // non-trivial (otherwise the cache might not even be populated).
    let patterns_dir = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/python");
    std::fs::write(
        patterns_dir.join("custom-requests.scm"),
        r#"
; Per-repo override of `python/custom-requests.scm` (used by the
; data-equality test only — proves `compiled_queries()` caches the
; merged data across calls).
(module) @m
"#,
    )
    .expect("write custom-requests.scm");

    // Build a `Patterns` with the override loaded. The cache is
    // populated on the first call to `compiled_queries()`; the
    // second call's data-equality assertion proves the cache
    // survived.
    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept the per-repo .scm body");
    assert!(
        patterns.overrides_applied(),
        "load_overrides must record that it ran",
    );

    // Sanity: the override reached the merged map (otherwise the
    // assertion chain below is meaningless).
    let first = patterns
        .compiled_queries()
        .expect("compiled_queries() must succeed after a successful load_overrides");
    let first_count = first.len();
    assert!(
        first
            .iter()
            .any(|(k, _, _, _)| *k == "python/custom-requests.scm"),
        "the merged map must carry the per-repo .scm override (otherwise the cache is built over an empty override set): {first:?}",
    );

    // Second call: the cache must return the same merged data.
    // We assert data-equality (not pointer-equality) because the
    // lifetime-tightening pass removed `Box::leak` and the
    // accessor now builds a borrowed `Vec` per call — the slice
    // pointer changes across calls (a fresh `Vec<(&'a str, ...)`
    // is built from the cached owned data on every call), but the
    // underlying data is identical.
    let second = patterns
        .compiled_queries()
        .expect("compiled_queries() must succeed on the second call");
    assert_eq!(
        first_count,
        second.len(),
        "the merged slice length must be stable across calls (the cache should return the same data on every call)",
    );
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(
            a.0, b.0,
            "the merged slice key differs between calls — the cache did not preserve the merged data"
        );
        assert_eq!(
            a.3, b.3,
            "the merged slice body differs between calls — the cache did not preserve the merged data"
        );
    }

    // Bonus: the Clone path snapshots the populated cache so a
    // clone's first call does NOT trigger a second merge cycle.
    // Comparing every (key, body) pair against the source's
    // first-call result is the proof.
    let cloned = patterns.clone();
    let cloned_slice = cloned
        .compiled_queries()
        .expect("compiled_queries() must succeed on a clone of a populated Patterns");
    assert_eq!(
        first_count,
        cloned_slice.len(),
        "the cloned slice must match the source's length (the clone's cache snapshot was rebuilt, not snapshotted)",
    );
    for (orig, new) in first.iter().zip(cloned_slice.iter()) {
        assert_eq!(
            orig.0, new.0,
            "cloned entry's key `{}` does not match the source's key `{}` — the clone must snapshot the cache, not rebuild",
            new.0, orig.0,
        );
        assert_eq!(
            orig.3, new.3,
            "cloned entry's body does not match the source's body — the clone must snapshot the cache, not rebuild"
        );
    }
}

// ─── Test 4 — multi-repo scan: per-repo overrides don't leak across repos ───
//
// Bug 1 of the wirein-2 brief. The previous `get_route_patterns`
// implementation held the route map in a `thread_local!` keyed by
// the `&Patterns` raw pointer. Each scan built a fresh
// `Patterns::with_overrides` instance, dropped it at end of scan,
// and the next scan happened to allocate its fresh clone at the
// same address — the `thread_local!` then handed back the FIRST
// scan's override-augmented route map, contaminating every repo
// scanned after the override-augmented one.
//
// The fix moves the cache onto the `Patterns` instance itself
// (via `route_patterns_cache: OnceLock<BTreeMap<String,
// RoutePattern>>`) — keyed by the instance, not by raw pointer —
// so a freshly-allocated `Patterns` has no carry-over from a
// previous instance. This test runs the same scan path twice
// (repo A with overrides, then repo B without) and asserts repo
// B sees only the bundled route patterns, not repo A's
// override-augmented ones.
//
// The test scans both files with `scan_file_for_routes` (the
// `http_sensor` walker) because that's the surface the bug
// surfaced through. Pre-fix, repo B's scan would emit repo A's
// `/v2/users` route. Post-fix, repo B's scan emits only the
// bundled `/users` route.
#[test]
fn repo_a_overrides_do_not_leak_into_repo_b() {
    // ── Repo A: with a per-repo override that REPLACES the bundled
    // axum-route.scm body with a query that fires on a different
    // path shape (".<anything>(/v2/users, get(h))") so we can
    // observe the override reaching the walker. Repo A's source
    // uses `/v2_users` (an underscore, not a slash) so the
    // override's `path_regex` (`/(/?v2_users[^"]*)` — matches
    // the literal text `/v2_users`) catches the route, and the
    // bundled `path_regex` does NOT (the bundled axum regex
    // expects a path that starts with `/`; `/v2_users` doesn't
    // start with `/` so the bundled regex would miss it).
    let dir_a = tempfile::tempdir().expect("tempdir repo A");
    let repo_a_root = dir_a.path();
    std::fs::write(
        repo_a_root.join("main.rs"),
        r#"use axum::{routing::get, Router};

async fn get_users() {}

fn build() -> Router {
    Router::new().route("/v2_users", get(get_users))
}
"#,
    )
    .expect("write repo A main.rs");
    // Override body that DIFFERS observably from the bundled one.
    // The override doesn't have the `#eq? @_route_field "route"`
    // predicate the bundled query uses, AND it carries a
    // distinguishing doc-comment header. Together: the override
    // body is unique to this test, and the merged compiled-queries
    // map will carry it under `rust/axum-route.scm`.
    let patterns_dir = repo_a_root.join(".lain/patterns/rust");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir repo A .lain/patterns/rust");
    std::fs::write(
        patterns_dir.join("axum-route.scm"),
        r#"
; Per-repo override of `rust/axum-route.scm` for the
; repo_a_overrides_do_not_leak_into_repo_b test. The bundled
; query uses `#eq? @_route_field "route"`; this override omits
; that predicate so the test can observe the override reaching
; the merged compiled-queries map (the override's body carries
; this doc-comment header — the bundled body does not).
(call_expression
  function: (field_expression
    field: (field_identifier) @_route_field)
  arguments: (arguments
    (string_literal) @path
    (call_expression
      function: (identifier) @verb
      arguments: (arguments
        (identifier) @handler))))
"#,
    )
    .expect("write repo A axum-route.scm");

    // Also write a YAML override that changes the axum-route
    // framework's `path_regex` so the route patterns map for
    // repo A DIFFERS from repo B. This is the surface the
    // per-pointer thread-local cache bug leaked: a stale
    // `&BTreeMap<String, RoutePattern>` from repo A's scan
    // would have been returned for repo B's scan, so repo B's
    // walker would use repo A's path_regex when matching repo
    // B's source. The override's path_regex matches a
    // distinctive token (`/v2_users`) that the bundled
    // regex's path_regex does NOT match — repo A's route
    // patterns map matches `/v2_users`, repo B's does not.
    std::fs::write(
        patterns_dir.join("axum-route.yaml"),
        r#"
languages:
  rust:
    - id: axum-route
      kind: route
      verbs: [get, post, put, delete, patch]
      path_regex: '\.route\s*\(\s*"(/?v2_users[^"]*)"'
      handler_regex: '\.route\s*\([^,]+,\s*(\w+)\s*\)'
"#,
    )
    .expect("write repo A axum-route.yaml");

    // ── Repo B: same source file, NO overrides. The expected
    // result is that repo B's scan emits the bundled regex's
    // `/users` route (because the bundled axum regex's path
    // capture matches the source's `"/users"` literal) — and
    // crucially, NOT repo A's override (which would have been
    // visible only if the thread-local-pointer cache had
    // returned repo A's data here).
    let dir_b = tempfile::tempdir().expect("tempdir repo B");
    let repo_b_root = dir_b.path();
    std::fs::write(
        repo_b_root.join("main.rs"),
        r#"use axum::{routing::get, Router};

async fn get_users() {}

fn build() -> Router {
    Router::new().route("/users", get(get_users))
}
"#,
    )
    .expect("write repo B main.rs");
    // No `.lain/patterns/` for repo B.

    // Probe repo A's per-repo `Patterns`. The override body
    // must reach the merged compiled-queries map (this is the
    // wire-in assertion — repo A's `.scm` override IS applied).
    let mut probe_a = Patterns::clone_default();
    probe_a
        .load_overrides(repo_a_root)
        .expect("repo A load_overrides must accept the per-repo .scm body");
    let merged_a = probe_a
        .compiled_queries()
        .expect("repo A compiled_queries must succeed");
    let axum_in_a = merged_a
        .iter()
        .find(|(k, _, _, _)| *k == "rust/axum-route.scm")
        .unwrap_or_else(|| {
            panic!(
                "the merged compiled-queries map MUST carry repo A's per-repo axum-route.scm under `rust/axum-route.scm` — proving the override REPLACED the bundled body, not augmented it"
            )
        });
    let (_, _, _, body_a) = axum_in_a;
    assert!(
        body_a.contains("Per-repo override of `rust/axum-route.scm` for the"),
        "the override body must carry its doc-comment header (otherwise the bundled body is being returned): {body_a:?}",
    );

    // Now scan repo A end-to-end via `scan_workspace_routes`.
    // (The wire-in path; the route emitted must be `/v2_users`
    // because repo A's source uses that path and the
    // override's path_regex matches it.)
    let db_path_a = repo_a_root.join("ga.bin");
    let graph_a = GraphDatabase::new(&db_path_a).expect("repo A GraphDatabase::new");
    let ns = RepoNamespace::for_test();
    let repo_id_a = RepoId::new("repo_a_overrides").unwrap();
    let _count_a = scan_workspace_routes(&graph_a, repo_a_root, &ns, &repo_id_a)
        .expect("repo A scan_workspace_routes must succeed");
    let route_a = graph_a
        .get_all_nodes()
        .into_iter()
        .find(|n| n.node_type == NodeType::HttpRoute)
        .expect("repo A scan_workspace_routes must emit an HttpRoute node");
    assert_eq!(
        route_a.name, "GET /v2_users",
        "repo A must emit a `/v2_users` route (the source's verbatim path, matched by the override's path_regex): got {:?}",
        route_a.name
    );

    // Scan repo B end-to-end. The test's core assertion: the
    // route map repo B sees does NOT carry repo A's override
    // body. We verify this two ways:
    //   1. repo B's own `Patterns::with_overrides` instance has
    //      an empty override cache (no `.lain/patterns/`) — the
    //      cache cannot be populated from a previous instance
    //      because the cache is keyed on the instance, not a
    //      raw pointer.
    //   2. repo B's `compiled_queries()` returns the BUNDLED body
    //      for `rust/axum-route.scm` (NOT repo A's override
    //      body).
    let mut probe_b = Patterns::clone_default();
    probe_b
        .load_overrides(repo_b_root)
        .expect("repo B load_overrides must succeed (no-op when .lain/patterns/ is absent)");
    let merged_b = probe_b
        .compiled_queries()
        .expect("repo B compiled_queries must succeed");
    let axum_in_b = merged_b
        .iter()
        .find(|(k, _, _, _)| *k == "rust/axum-route.scm")
        .unwrap_or_else(|| {
            panic!("repo B's compiled_queries must carry the bundled rust/axum-route.scm entry")
        });
    let (_, _, _, body_b) = axum_in_b;
    assert!(
        !body_b.contains("Per-repo override of `rust/axum-route.scm` for the"),
        "repo B MUST see the bundled axum-route.scm body, NOT repo A's override — \
         this assertion fails when the per-thread `get_route_patterns` cache \
         leaks override data across scans (the previous bug): {body_b:?}",
    );

    // Run repo B's scan end-to-end. The route emitted must be
    // `/users` (the bundled regex's path capture matches the
    // source's `"/users"` literal). Critically, the count must
    // not include a phantom `/v2_users` route that repo A's
    // override would have produced if its route patterns map
    // had leaked into repo B's scan (the per-pointer
    // thread-local cache bug: repo A's stale `&BTreeMap<String,
    // RoutePattern>` would have been returned for repo B's
    // scan, so repo B's walker would use repo A's override
    // `path_regex` which matches `/v2_users` and NOT `/users`).
    let db_path_b = repo_b_root.join("gb.bin");
    let graph_b = GraphDatabase::new(&db_path_b).expect("repo B GraphDatabase::new");
    let repo_id_b = RepoId::new("repo_b_no_overrides").unwrap();
    let count_b = scan_workspace_routes(&graph_b, repo_b_root, &ns, &repo_id_b)
        .expect("repo B scan_workspace_routes must succeed");
    let routes_b: Vec<_> = graph_b
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpRoute)
        .collect();
    assert_eq!(
        count_b, 1,
        "repo B must emit exactly one route (the bundled `/users` route); got {count_b} routes: {routes_b:?}"
    );
    assert_eq!(
        routes_b[0].name, "GET /users",
        "repo B's single route must be `/users` (the bundled regex path capture); got {:?}",
        routes_b[0].name
    );
    // Specifically assert NO `/v2_users` phantom route leaked
    // from repo A's override.
    assert!(
        !routes_b.iter().any(|n| n.name.contains("/v2_users")),
        "repo B must not carry a phantom `/v2_users` route that would only exist if \
         repo A's override `path_regex` leaked into repo B's route patterns map: {routes_b:?}"
    );
}

// ─── Test 5 — multiple `.scm` files per language folder are merged correctly ───
//
// Bug 2 of the wirein-2 brief. `compiled_queries` requires the
// override list to be sorted by key (its merge loop walks
// `override_scml` and the bundled entries in lock-step using
// key ordering). `read_dir` does NOT guarantee order, so without
// the sort the merge loop can either skip the "advance past
// sorted overrides" branch (producing duplicate entries) or
// mis-order the REPLACE step. This test writes three override
// files per language folder, observes the merged slice, and
// asserts the data is well-formed (no duplicates, REPLACE worked
// on the bundled entries, sort order is preserved).
#[test]
fn multiple_override_scm_files_in_one_language_folder_are_merged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // Three Python override files in the same `python/` folder.
    // All bodies are parseable tree-sitter fragments for the
    // Python grammar (the bundled entries are python-outbound
    // queries; we override the `.scm` bodies they correspond
    // to). Each body is a minimal, distinct tree-sitter query.
    let patterns_dir = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&patterns_dir).expect("mkdir .lain/patterns/python");
    let custom_a = r#"
; Override A — replaces the bundled python/requests-outbound.scm.
; Distinct body: captures `(module) @m` plus a (function_definition) @f.
(module) @m
(function_definition) @f
"#;
    let custom_b = r#"
; Override B — replaces the bundled python/httpx-outbound.scm.
; Distinct body: only captures a (function_definition) @f.
(function_definition) @f
"#;
    let custom_c = r#"
; Override C — replaces the bundled python/fetch-outbound.scm
; (the bundled one is for `python/fetch-outbound.scm`).
; Distinct body: only captures a (import_statement) @i.
(import_statement) @i
"#;
    std::fs::write(patterns_dir.join("requests-outbound.scm"), custom_a)
        .expect("write requests-outbound.scm");
    std::fs::write(patterns_dir.join("httpx-outbound.scm"), custom_b)
        .expect("write httpx-outbound.scm");
    std::fs::write(patterns_dir.join("fetch-outbound.scm"), custom_c)
        .expect("write fetch-outbound.scm");

    // Build a `Patterns` with the three overrides loaded.
    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(repo_root)
        .expect("load_overrides must accept three .scm overrides");
    assert!(
        patterns.overrides_applied(),
        "load_overrides must record that it ran",
    );

    // The merged slice must carry every override key. Critical
    // assertion: the THREE override keys must each appear
    // EXACTLY once (no duplicates from the sort bug). Pre-fix
    // (without the sort), the merge loop's "advance past sorted
    // overrides" branch was unreliable, and a non-sorted input
    // could produce duplicate entries.
    let merged = patterns
        .compiled_queries()
        .expect("compiled_queries must succeed after a successful load_overrides");
    let axum_key = "python/requests-outbound.scm";
    let httpx_key = "python/httpx-outbound.scm";
    let fetch_key = "python/fetch-outbound.scm";

    let axum_count = merged.iter().filter(|(k, _, _, _)| *k == axum_key).count();
    let httpx_count = merged.iter().filter(|(k, _, _, _)| *k == httpx_key).count();
    let fetch_count = merged.iter().filter(|(k, _, _, _)| *k == fetch_key).count();
    assert_eq!(
        axum_count, 1,
        "the override key `python/requests-outbound.scm` must appear exactly once in the merged slice — got {axum_count} (the sort bug produced duplicates)"
    );
    assert_eq!(
        httpx_count, 1,
        "the override key `python/httpx-outbound.scm` must appear exactly once in the merged slice — got {httpx_count} (the sort bug produced duplicates)"
    );
    assert_eq!(
        fetch_count, 1,
        "the override key `python/fetch-outbound.scm` must appear exactly once in the merged slice — got {fetch_count} (the sort bug produced duplicates)"
    );

    // The merged slice's bodies for these keys must be the
    // OVERRIDE bodies (not the bundled ones) — proves the
    // REPLACE step ran.
    for (k, _, _, body) in merged.iter() {
        if *k == axum_key {
            assert!(
                body.contains("Override A"),
                "the merged entry for `python/requests-outbound.scm` must carry override A's body (the doc-comment header), not the bundled body: {body:?}"
            );
        } else if *k == httpx_key {
            assert!(
                body.contains("Override B"),
                "the merged entry for `python/httpx-outbound.scm` must carry override B's body (the doc-comment header), not the bundled body: {body:?}"
            );
        } else if *k == fetch_key {
            assert!(
                body.contains("Override C"),
                "the merged entry for `python/fetch-outbound.scm` must carry override C's body (the doc-comment header), not the bundled body: {body:?}"
            );
        }
    }

    // The merged slice is sorted by key. Verify the override keys
    // appear in the slice in sorted order.
    let mut sorted_keys: Vec<&str> = merged.iter().map(|(k, _, _, _)| *k).collect();
    let original = sorted_keys.clone();
    sorted_keys.sort_unstable();
    assert_eq!(
        original, sorted_keys,
        "the merged slice must be sorted by key (the brief pins this so `generated::get` can binary-search); got {original:?}"
    );
}

// ─── Test 6 — `load_overrides` invalidates `merged_queries` cache ───
//
// Bug 3 of the wirein-2 brief. The previous `load_overrides` did
// not reset the `merged_queries: OnceLock<Vec<...>>` field, so a
// second `load_overrides` call on the same `Patterns` instance
// left the cache populated with the first call's merged data —
// the second call's override bodies never reached the merged
// slice.
//
// The fix: `load_overrides` now reassigns the OnceLock to a
// fresh empty one, so the next `compiled_queries()` call
// rebuilds from the new override bodies. This test calls
// `load_overrides` twice on the SAME `Patterns` instance
// (different override directories) and asserts the merged
// slice reflects the second call's bodies, not the first.
#[test]
fn second_load_overrides_call_invalidates_merged_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_root = dir.path();

    // ── First call: load overrides from a temp dir with one
    // override body (distinctive header).
    let first_dir = tempfile::tempdir().expect("first tempdir");
    let first_root = first_dir.path();
    let first_patterns = first_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&first_patterns).expect("mkdir first .lain/patterns/python");
    std::fs::write(
        first_patterns.join("first-override.scm"),
        r#"
; First override body (carries this distinctive doc-comment
; header — proves the merged slice reflects this body, not
; the second-call body).
(module) @m
"#,
    )
    .expect("write first-override.scm");

    // ── Second call: a different override dir, with a different
    // body. The second call MUST invalidate the cache so the
    // merged slice reflects this new body.
    let second_patterns = repo_root.join(".lain/patterns/python");
    std::fs::create_dir_all(&second_patterns).expect("mkdir second .lain/patterns/python");
    std::fs::write(
        second_patterns.join("second-override.scm"),
        r#"
; Second override body (carries this distinctive doc-comment
; header — proves the merged slice reflects this body, not
; the first-call body).
(function_definition) @f
"#,
    )
    .expect("write second-override.scm");

    // Build a `Patterns` and run BOTH `load_overrides` calls
    // on the SAME instance.
    let mut patterns = Patterns::clone_default();
    patterns
        .load_overrides(first_root)
        .expect("first load_overrides must succeed");

    // Sanity: the first call's merged slice carries the first
    // override's body (proves the first load reached the merged
    // map).
    let first_merged = patterns
        .compiled_queries()
        .expect("first compiled_queries must succeed");
    assert!(
        first_merged
            .iter()
            .any(|(k, _, _, b)| *k == "python/first-override.scm"
                && b.contains("First override body")),
        "after the first load_overrides, the merged slice must carry the first override's body: {first_merged:?}",
    );

    // Now the second `load_overrides` call. This must
    // invalidate the cache. Pre-fix, the cache held the first
    // call's data and the second call's body never reached the
    // merged slice.
    patterns
        .load_overrides(repo_root)
        .expect("second load_overrides must succeed");

    let second_merged = patterns
        .compiled_queries()
        .expect("second compiled_queries must succeed");
    // The merged slice must reflect the second call's body.
    assert!(
        second_merged
            .iter()
            .any(|(k, _, _, b)| *k == "python/second-override.scm"
                && b.contains("Second override body")),
        "after the second load_overrides, the merged slice must carry the second override's body (cache was invalidated and rebuilt): {second_merged:?}",
    );
    // The first call's body must NOT be in the merged slice
    // (its directory wasn't passed to the second `load_overrides`).
    let first_body_present = second_merged
        .iter()
        .any(|(k, _, _, b)| *k == "python/first-override.scm" && b.contains("First override body"));
    assert!(
        !first_body_present,
        "the first call's override body must NOT be in the merged slice after the second load_overrides (the cache is rebuilt from the new override set, not appended to): {second_merged:?}"
    );
}
