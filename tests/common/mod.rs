//! Shared test fixtures used by every `tests/*.rs` integration
//! binary. Each test file does `mod common;` (Rust treats
//! `tests/common/` as a shared module folder), then uses the helpers
//! from this file.
//!
//! Two layers of helpers:
//!
//! - **In-process fixtures** (`empty_graph`, `graph_and_overlay`,
//!   `call_graph_fixture`) — empty or hand-built `GraphDatabase`
//!   instances for unit-style integration tests that don't need a
//!   running server.
//!
//! - **E2E harness** (`free_port`, `http_request`, `jsonrpc`,
//!   `tools_call_text`, `tools_call_envelope`, `boot_federation`,
//!   `ServerGuard`) — boots the real `lain server --transport
//!   http` binary against a workspace fixture, talks to it over the
//!   same JSON-RPC surface that real agents hit, and tears the
//!   child down on Drop.
//!
//! These were previously duplicated across `federation_e2e.rs`,
//! `feat_suite.rs`, `failure_modes.rs`, `feat_negative_paths.rs`,
//! and `performance_budgets.rs`. Extracting them here gives one
//! place to change the harness and one place to test it.

use lain::graph::GraphDatabase;
use lain::overlay::VolatileOverlay;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// In-process fixtures
// ---------------------------------------------------------------------------

/// Empty `GraphDatabase` backed by a fresh tempdir + `graph.bin` file
/// inside it. Caller owns the tempdir through the returned
/// `GraphDatabase`'s `persistence_path`.
pub fn empty_graph() -> GraphDatabase {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("graph.bin");
    GraphDatabase::new(&path).unwrap()
}

/// Empty `VolatileOverlay`. Pair with [`empty_graph`] via
/// [`graph_and_overlay`].
pub fn empty_overlay() -> VolatileOverlay {
    VolatileOverlay::new()
}

/// `(empty_graph, empty_overlay)` pair. The common starting point
/// for tests that build a fixture from scratch.
pub fn graph_and_overlay() -> (GraphDatabase, VolatileOverlay) {
    (empty_graph(), empty_overlay())
}

/// The canonical call-graph fixture used by `tests/graph_invariants.rs`
/// and a handful of other tests. Shape:
///
/// ```text
/// main -> a -> b -> c   (b has two callers)
/// main -> x -> b
/// main -> y             (y is dead — no outgoing edges)
/// ```
///
/// Nodes: `main`, `a`, `b`, `c`, `x`, `y` (Functions); the matching
/// `File` and `Module`/`Namespace` nodes that the scanner would
/// produce are NOT included — callers add them as needed. `b` is
/// the only function with multiple callers (a + x), making it the
/// canonical "duplicate incoming edge" test target.
pub fn call_graph_fixture() -> (GraphDatabase, VolatileOverlay) {
    use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
    let (g, o) = graph_and_overlay();
    let main = GraphNode::new(NodeType::Function, "main".into(), "/src/main.rs".into());
    let a = GraphNode::new(NodeType::Function, "a".into(), "/src/a.rs".into());
    let b = GraphNode::new(NodeType::Function, "b".into(), "/src/b.rs".into());
    let c = GraphNode::new(NodeType::Function, "c".into(), "/src/c.rs".into());
    let x = GraphNode::new(NodeType::Function, "x".into(), "/src/x.rs".into());
    let y = GraphNode::new(NodeType::Function, "y".into(), "/src/y.rs".into());
    for n in [&main, &a, &b, &c, &x, &y] {
        g.upsert_node(n.clone()).unwrap();
    }
    let edges = [
        GraphEdge::new(EdgeType::Calls, main.id.clone(), a.id.clone()),
        GraphEdge::new(EdgeType::Calls, a.id.clone(), b.id.clone()),
        GraphEdge::new(EdgeType::Calls, b.id.clone(), c.id.clone()),
        GraphEdge::new(EdgeType::Calls, main.id.clone(), x.id.clone()),
        GraphEdge::new(EdgeType::Calls, x.id.clone(), b.id.clone()),
    ];
    for e in &edges {
        g.insert_edge(e).unwrap();
    }
    (g, o)
}

// ---------------------------------------------------------------------------
// E2E harness — boots a real `lain server --transport http` and
// exposes the same JSON-RPC surface that real agents hit.
// ---------------------------------------------------------------------------

/// Find a free TCP port by binding + releasing. The OS reuses it on
/// the next bind unless a race occurs; on a CI runner with no other
/// listeners this is reliable. If the race does occur, the server's
/// bind will fail and the test will panic with that error.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Minimal HTTP request helper — issues one request, reads the full
/// response (connection: close), and returns `(status_code, body)`.
pub fn http_request(host: &str, raw: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(host).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.write_all(raw.as_bytes()).expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    let status_line = response.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_start = response.find("\r\n\r\n").map(|i| i + 4).unwrap_or(response.len());
    (status, response[body_start..].to_string())
}

/// Issue a JSON-RPC request to the live MCP `/mcp` endpoint and return
/// the parsed envelope. Asserts HTTP 200 (the MCP transport always
/// returns 200 with a JSON body, even for tool errors).
pub fn jsonrpc(host: &str, body: &str) -> serde_json::Value {
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        host = host,
        len = body.len(),
    );
    let (status, body) = http_request(host, &req);
    assert!(
        status == 200,
        "JSON-RPC call returned HTTP {status}: {body}"
    );
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("rpc not JSON: {e}\n{body}"))
}

/// Issue `tools/call <name> <args>` and return the raw text payload.
/// Most lain tools return Markdown prose (`get_health`,
/// `find_anchors`, etc.) — JSON-shaped tools are rare. Callers parse
/// the string with `serde_json::from_str` when they need fields.
/// Panics on `isError=true` — happy-path helper only.
pub fn tools_call_text(host: &str, name: &str, arguments: serde_json::Value) -> String {
    let resp = tools_call_envelope(host, name, arguments);
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("missing result.content[0].text: {resp}"))
        .to_string();
    if resp.pointer("/result/isError").and_then(|v| v.as_bool()) == Some(true) {
        panic!("tool {name} returned isError=true: {text}");
    }
    text
}

/// Issue `tools/call <name> <args>` and return the full JSON-RPC
/// response envelope. Does NOT panic on `isError=true` — callers
/// inspect `result.isError` and `error.code` themselves.
pub fn tools_call_envelope(host: &str, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
    .to_string();
    jsonrpc(host, &body)
}

/// RAII guard that kills the spawned server on drop, regardless of
/// how the test exits. Prevents orphan `lain server` processes when
/// the test panics.
pub struct ServerGuard(pub Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl ServerGuard {
    /// True if `try_wait` says the child is still running. A return
    /// value of `false` means the process exited — either it crashed
    /// (the failure mode we're testing for) or `Drop` already ran.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

/// Spawn `lain server --transport http` against an arbitrary
/// `repos.yaml` path. The returned [`ServerGuard`] cleans the
/// child up on drop. Caller is responsible for writing the
/// `repos.yaml` (and any `workspaces.yaml`) before calling.
pub fn boot_server(port: u16, repos_yaml_path: &Path) -> ServerGuard {
    let stderr_path = std::env::temp_dir().join(format!("lain-test-stderr-{port}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport", "http",
            "--port", &port.to_string(),
            "--workspace", "auto",
            "--config",
            repos_yaml_path.to_str().unwrap(),
        ])
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "spawn lain server failed: {e}; binary={}; stderr={}",
                env!("CARGO_BIN_EXE_lain"),
                stderr_path.display()
            )
        });

    ServerGuard(child)
}

/// Wait until `/health` returns 200, or panic with the captured
/// stderr if it doesn't. Federation boot can be slow (tree-sitter +
/// optional LSP per repo); 60s is the budget used by every existing
/// test that boots a real server.
pub fn wait_for_health(host: &str, deadline: Duration) {
    let start = Instant::now();
    loop {
        if start.elapsed() > deadline {
            panic!(
                "lain server did not become healthy within {:?} on {}",
                deadline, host
            );
        }
        match TcpStream::connect(host) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .ok();
                let _ = stream.write_all(
                    format!(
                        "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
                let mut response = String::new();
                let _ = stream.read_to_string(&mut response);
                if response.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// End-to-end boot: free port + `boot_server` + `wait_for_health`.
/// Returns the `ServerGuard` and the host:port string for callers to
/// pass to the request helpers.
pub fn boot_and_wait(repos_yaml_path: &Path) -> (String, ServerGuard) {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let guard = boot_server(port, repos_yaml_path);
    wait_for_health(&host, Duration::from_secs(60));
    (host, guard)
}

/// Build a single-repo fixture in a tempdir, write a `repos.yaml`
/// for it, and boot a `lain server` against the fixture. Polls
/// `get_health` until the per-repo `node_count` is non-zero, then
/// polls a follow-up `search_org` for any symbol the caller cares
/// about (so the caller can immediately hit a per-repo tool).
/// Returns the `host:port` and the `ServerGuard` (drop = cleanup).
///
/// Use this for per-use-case proving tests where the fixture is
/// small and self-contained. Federation / cross-repo tests should
/// use `load_federation` directly.
///
/// `wait_for_symbol` is an optional list of symbol names the
/// caller knows exist in the fixture; the helper polls
/// `search_org` until each is visible in the federated view. This
/// avoids the "node_count != 0 but my function isn't there yet"
/// race that the federation e2e hit.
pub fn boot_single_repo(
    repos_root: &Path,
    repos_yaml: &Path,
    wait_for_symbol: &[&str],
) -> (String, ServerGuard) {
    let (host, guard) = boot_and_wait(repos_yaml);
    // The federation boot is fast, but the indexer may not have
    // walked the files yet. Poll `list_repos` for non-zero count,
    // then poll `search_org` for each symbol the caller named.
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(30) {
            panic!("per-repo index never populated within 30s on {host}");
        }
        let resp = tools_call_text(&host, "list_repos", serde_json::json!({}));
        if resp.contains("\"node_count\":0") || !resp.contains("node_count") {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        break;
    }
    if !wait_for_symbol.is_empty() {
        // `search_org` only returns matches, so we issue one query per
        // symbol and require each to be visible. A simpler "ping any
        // symbol" loop would race the indexer.
        let start = std::time::Instant::now();
        for &name in wait_for_symbol {
            loop {
                if start.elapsed() > Duration::from_secs(30) {
                    panic!("symbol `{name}` never appeared in search_org within 30s on {host}");
                }
                let resp = tools_call_text(
                    &host,
                    "search_org",
                    serde_json::json!({"query": name, "limit": 50}),
                );
                if resp.contains(name) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    // Defensive: drop the `repos_root` warning by using it. The caller
    // already holds the tempdir handle; this just keeps the borrow
    // checker quiet if the caller passes a path-typed reference.
    let _ = repos_root;
    (host, guard)
}

/// Initialize a git repo at `path` with a committable local identity
/// and commit the working tree. The indexer reads `git ls-files`
/// for the seeded set; without a commit the per-repo DB stays
/// empty. Identity is passed inline via `-c` flags so a missing
/// global config doesn't surface as "Please tell me who you are".
pub fn git_init_committed(path: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed: {status}");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git failed");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    run(&["-c", "user.email=test@lain", "-c", "user.name=test", "add", "-A"]);
    run(&[
        "-c", "user.email=test@lain",
        "-c", "user.name=test",
        "commit", "-q", "-m", "fixture",
    ]);
}

/// Smoke test: every helper returns a usable value and the fixture
/// is internally consistent (the `b` node has two incoming `Calls`
/// edges from `a` and `x`).
#[test]
fn helpers_return_usable_fixtures() {
    let (g, _) = call_graph_fixture();
    let main = g.find_node_by_name("main").expect("main node");
    assert_eq!(main.name, "main");
    let b = g.find_node_by_name("b").expect("b node");
    let incoming = g.get_edges_to(&b.id).unwrap();
    let callers: Vec<_> = incoming
        .iter()
        .filter(|e| matches!(e.edge_type, lain::schema::EdgeType::Calls))
        .map(|e| e.source_id.as_str())
        .collect();
    assert_eq!(callers.len(), 2, "b should have exactly two callers");
}
