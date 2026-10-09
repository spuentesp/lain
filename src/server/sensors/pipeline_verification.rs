//! The whole sensor pipeline (`run_all`) over adversarial workspaces:
//! files of every kind a sensor reads, filled with code-like fragments in
//! broken states (unterminated strings and comments, deep nesting, CRLF,
//! multi-byte text, merge-conflict markers).
//!
//! * never panics;
//! * deterministic: the same workspace yields the same nodes on a fresh graph;
//! * idempotent: running it again on the same graph does not change the set of
//!   nodes (the indexer re-runs sensors on every pass).
use super::*;
use crate::graph::GraphDatabase;
use proptest::prelude::*;
use std::collections::BTreeSet;

const FILES: &[&str] = &[
    "app.py",
    "server.js",
    "api.ts",
    "main.go",
    "Handler.java",
    "lib.rs",
    "Controller.cs",
    "schema.graphql",
    "svc.proto",
    "openapi.yaml",
    "queries.sql",
    "CODEOWNERS",
    ".env",
    "docker-compose.yml",
    "events.js",
];

fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("@app.get(\"/users/{id}\")\n".to_string()),
        Just("router.get('/x', handler);\n".to_string()),
        Just("http.HandleFunc(\"/p\", h)\n".to_string()),
        Just("type Query {\n".to_string()),
        Just("\"\"\"desc\n".to_string()),
        Just("service Greeter {\n  rpc Say (A) returns (B);\n".to_string()),
        Just("message A { string s = 1; }\n".to_string()),
        Just("openapi: 3.0.0\npaths:\n  /a:\n    get:\n".to_string()),
        Just("SELECT * FROM users WHERE id = ?;\n".to_string()),
        Just("process.env.SECRET_KEY\n".to_string()),
        Just("os.environ[\"X\"]\n".to_string()),
        Just("* @team\n/src/ @core\n".to_string()),
        Just("ws.on('message', cb);\n".to_string()),
        Just("emitter.emit('evt', 1);\n".to_string()),
        Just("fetch(`${base}/v1/items`)\n".to_string()),
        Just("<<<<<<< HEAD\n=======\n>>>>>>> x\n".to_string()),
        Just("{".to_string()),
        Just("}".to_string()),
        Just("\"".to_string()),
        Just("/*".to_string()),
        Just("\r\n".to_string()),
        Just("\n".to_string()),
        Just("é日本😀".to_string()),
        Just("\u{0}".to_string()),
        "[a-zA-Z_][a-zA-Z0-9_]{0,6}",
        "\\PC{0,5}",
    ]
}

fn workspace_files() -> impl Strategy<Value = Vec<(usize, String)>> {
    prop::collection::vec(
        (
            0..FILES.len(),
            prop::collection::vec(fragment(), 0..14).prop_map(|v| v.concat()),
        ),
        0..8,
    )
}

fn write_workspace(root: &std::path::Path, files: &[(usize, String)]) {
    for (i, (idx, body)) in files.iter().enumerate() {
        // Distinct directories so two picks of the same name both exist.
        let dir = root.join(format!("d{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILES[*idx]), body).unwrap();
    }
}

fn node_ids(g: &GraphDatabase) -> BTreeSet<String> {
    g.get_all_nodes().into_iter().map(|n| n.id).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, max_shrink_iters: 150, ..ProptestConfig::default() })]

    #[test]
    fn run_all_is_total_deterministic_and_idempotent(files in workspace_files()) {
        let ns = RepoNamespace::for_test();
        let ws = tempfile::tempdir().unwrap();
        write_workspace(ws.path(), &files);

        let g1 = GraphDatabase::empty_writable();
        let _ = run_all(&g1, ws.path(), &ns, "svc");
        let first = node_ids(&g1);

        // Deterministic across a fresh graph.
        let g2 = GraphDatabase::empty_writable();
        let _ = run_all(&g2, ws.path(), &ns, "svc");
        prop_assert_eq!(node_ids(&g2), first.clone(), "same workspace, different nodes");

        // Idempotent on the same graph.
        let _ = run_all(&g1, ws.path(), &ns, "svc");
        prop_assert_eq!(node_ids(&g1), first, "a second pass changed the node set");
    }
}

/// Guard against a vacuous property: a workspace with a route and an SQL
/// query must actually produce nodes through the same path the property uses.
#[test]
fn the_property_workspace_shape_really_produces_nodes() {
    let ns = RepoNamespace::for_test();
    let ws = tempfile::tempdir().unwrap();
    write_workspace(
        ws.path(),
        &[
            (0, "from flask import Flask\napp = Flask(__name__)\n@app.get(\"/users/<id>\")\ndef get_user(id):\n    return 1\n".into()),
            (10, "SELECT * FROM users WHERE id = ?;\n".into()),
            (7, "type Query {\n  a: Int\n}\n".into()),
        ],
    );
    let g = GraphDatabase::empty_writable();
    let counts = run_all(&g, ws.path(), &ns, "svc");
    let total = counts.http_routes + counts.graphql + counts.openapi + counts.proto;
    assert!(
        !node_ids(&g).is_empty() || total > 0,
        "run_all saw nothing in a workspace with a route, a query and a schema: \
         the property above would be vacuous"
    );
}
