//! LSP wire-path integration test.
//!
//! Spawns a fake LSP server that listens on a real TCP port and speaks
//! the LSP JSON-RPC protocol, points the scanner at it, and asserts
//! the full wire path produces the expected `caller -> callee` edge.
//!
//! The previous F04 fixture was an in-process hook on `LspMultiplexer`
//! that bypassed the JSON-RPC layer entirely. This test makes the
//! scanner encode requests, transmit them over a real socket, have a
//! real server read `Content-Length`-prefixed frames, and have the
//! response decoded back into the scanner's data structures — the
//! same wire path a real rust-analyzer would take.

use lain::graph::GraphDatabase;
use lain::lsp::test_support::FakeLspServer;
use lain::lsp::LspMultiplexer;
use lain::schema::{EdgeType, NodeType};
use lain::server::ingest::resolve::resolve_call_edges;
use lain::server::ingest::scan::scan_file_structure;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

fn symbol_response(name: &str, line: u32, col: u32, end_col: u32) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "kind": 12,
        "range": {
            "start": { "line": line, "character": 0 },
            "end":   { "line": line, "character": end_col }
        },
        "selectionRange": {
            "start": { "line": line, "character": col },
            "end":   { "line": line, "character": end_col }
        }
    })
}

fn two_symbols(
    helper_line: u32, helper_col: u32, helper_end: u32,
    caller_line: u32, caller_col: u32, caller_end: u32,
) -> serde_json::Value {
    serde_json::json!([
        symbol_response("helper", helper_line, helper_col, helper_end),
        symbol_response("caller", caller_line, caller_col, caller_end)
    ])
}

fn location(uri: &str, line: u32, col: u32, end_col: u32) -> serde_json::Value {
    serde_json::json!({
        "uri": uri,
        "range": {
            "start": { "line": line, "character": col },
            "end":   { "line": line, "character": end_col }
        }
    })
}

/// End-to-end: scanner asks the fake LSP server for document symbols,
/// gets a `helper` declaration, asks for references at the symbol's
/// selection position, gets one reference inside `caller`, and the
/// resolve phase emits `caller -> helper`. The fake server is a real
/// process listening on a real TCP port — no test-only multiplexer
/// hook, no in-process stub.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lsp_wire_path_emits_caller_to_callee_edge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file: PathBuf = tmp.path().join("lib.rs");
    std::fs::write(
        &file,
        "pub fn helper() {}\npub fn caller() { helper(); }\n",
    )
    .expect("write fixture");

    let uri = format!("file://{}", file.display());

    let fake = FakeLspServer::bind().await.expect("bind fake lsp");
    let host = fake.host();
    let port = fake.port();
    fake.set_document_symbols(
        &uri,
        two_symbols(0, 7, 13, 1, 7, 13),
    )
    .await;
    fake.set_references(&uri, 0, 7, serde_json::json!([location(&uri, 1, 17, 23)]))
        .await;
    let received = fake.received();
    let handle = fake.spawn();

    let lsp = LspMultiplexer::with_server_url(
        tmp.path(),
        &host,
        port,
        &lain::tuning::RuntimeConfig::default(),
    )
    .await
    .expect("connect to fake lsp");

    let lsp = Arc::new(AsyncMutex::new(lsp));
    let result = scan_file_structure(
        file.clone(),
        tmp.path().to_path_buf(),
        lsp,
        0,
        0,
        "abc".to_string(),
        &lain::schema::RepoNamespace::for_test(),
        CancellationToken::new(),
        None,
    )
    .await
    .expect("scan ok");

    let helper = result
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Function) && n.name == "helper")
        .expect("helper node");
    let caller = result
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Function) && n.name == "caller")
        .expect("caller node");

    assert_eq!(
        result.external_references.len(),
        1,
        "the canned reference at (1, 17) inside caller must surface; got {:?}",
        result
            .external_references
            .iter()
            .map(|(id, r)| (id, r.line, r.col))
            .collect::<Vec<_>>()
    );
    let (callee_id, ref_loc) = &result.external_references[0];
    assert_eq!(callee_id, &helper.id, "ref must be paired with helper");
    assert_eq!(ref_loc.line, 1);
    assert_eq!(ref_loc.col, 17);

    let db_tmp = tempfile::tempdir().expect("db tmp");
    let db_path = db_tmp.path().join("graph.bin");
    let db = GraphDatabase::new(&db_path).expect("graph db");
    let helper_id = helper.id.clone();
    let caller_id = caller.id.clone();
    let mut helper_node = helper.clone();
    let mut caller_node = caller.clone();
    helper_node.last_lsp_sync = None;
    caller_node.last_lsp_sync = None;
    db.upsert_node(helper_node).expect("upsert helper");
    db.upsert_node(caller_node).expect("upsert caller");

    let edges = resolve_call_edges(&db, tmp.path(), &result.external_references, None, None);
    assert_eq!(edges.len(), 1, "exactly one Calls edge");
    let edge = &edges[0];
    assert_eq!(edge.edge_type, EdgeType::Calls);
    assert_eq!(edge.source_id, caller_id, "source must be the caller");
    assert_eq!(edge.target_id, helper_id, "target must be the callee (helper)");

    let doc_sym_calls = received.document_symbol.lock().expect("recv lock");
    assert!(
        !doc_sym_calls.is_empty(),
        "scanner must have called textDocument/documentSymbol over the wire"
    );
    drop(doc_sym_calls);

    let ref_calls: Vec<serde_json::Value> = received
        .references
        .lock()
        .expect("recv lock")
        .clone();
    assert!(
        !ref_calls.is_empty(),
        "scanner must have called textDocument/references over the wire"
    );
    let ref_params = ref_calls[0]
        .get("params")
        .cloned()
        .expect("references request must carry params");
    let pos = ref_params
        .get("position")
        .expect("references params must have a position");
    let line = pos
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .expect("position.line must be a number");
    let character = pos
        .get("character")
        .and_then(serde_json::Value::as_u64)
        .expect("position.character must be a number");
    assert_eq!(
        line, 0,
        "scanner must request references at the symbol's selection line"
    );
    assert_eq!(
        character, 7,
        "scanner must request references at the symbol's selection column, not (0, 0)"
    );

    handle.shutdown().await;
}
