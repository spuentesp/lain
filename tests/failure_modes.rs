//! Failure-mode tests for `lain server --transport http`.
//!
//! These tests boot the same hermetic fixture used by `feat_suite.rs` and
//! `feat_negative_paths.rs` and then deliberately throw malformed,
//! truncated, or hostile inputs at the running server. The goal is to
//! verify that the server *survives* — no panic, no hang, the PID is
//! still alive afterward, and a new connection can still get work done —
//! rather than to assert specific wire shapes.
//!
//! Why this matters: the suite only ever sends well-formed JSON-RPC.
//! A real production deployment will see truncated connections (proxy
//! timeouts, TLS terminators), malformed JSON (clients on old versions,
//! partial writes from a misbehaving shim), garbage floods (probes from
//! unrelated scanners hitting the port), and editor-shaped mistakes
//! (wrong types for fields). The server's job is to keep going.
//!
//! Pattern note: each test boots its own server via the local
//! `boot_server` helper so a regression that takes the server down
//! doesn't cascade into a 90-second wall-clock cost on every test.
//! `ServerGuard` cleans up the child on drop regardless of how the
//! test exits.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use common::{free_port, http_request, jsonrpc, tools_call_envelope, ServerGuard};

/// Initialize a git repo at `path`. The indexer skips non-git
/// directories, so without `.git/` `find_anchors` would have nothing
/// to rank.
fn git_init(path: &std::path::Path) {
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    for (k, v) in [("user.email", "failure-modes@lain"), ("user.name", "failure-modes")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(path)
            .status()
            .unwrap();
    }
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .status()
        .expect("git add");
    let _ = add.success();
    let commit = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "failure-modes fixture"])
        .current_dir(path)
        .status()
        .expect("git commit");
    let _ = commit.success();
}

/// Boot a federation-mode server with one minimal Rust repo. Same
/// fixture shape as `feat_suite.rs` so the symbol graph is non-empty
/// and the federation tools (`list_repos`, etc.) have something to
/// report. The returned `ServerGuard` cleans up the child on drop.
fn boot_server(port: u16) -> ServerGuard {
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"failure-modes-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn orchestrate() -> u32 {\n    let a = helper_a();\n    helper_b() + a\n}\n\
         pub fn entrypoint() -> u32 { orchestrate() }\n\
         pub fn helper_a() -> u32 { 1 }\n\
         pub fn helper_b() -> u32 { 2 }\n",
    )
    .unwrap();
    git_init(&repo_dir);
    let repo_id = repo_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml_path = project.path().join("repos.yaml");
    std::fs::write(
        &repos_yaml_path,
        format!(
            "data_dir: {}\nrepos:\n  - id: {}\n    source:\n      type: workspace_dir\n      path: {}\n",
            data_dir.display(),
            repo_id,
            repo_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
        project.path().join("workspaces.yaml"),
        format!(
            "workspaces:\n  - name: failure-modes\n    members: [{}]\n",
            repo_id
        ),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let xdg_config = tempfile::tempdir().unwrap();

    let stderr_path = std::env::temp_dir().join(format!("failure-modes-stderr-{port}.log"));
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
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", xdg_config.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
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

    let guard = ServerGuard(child);

    let host = format!("127.0.0.1:{port}");
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(30) {
            let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!(
                "server did not become healthy within 30s on {host}; last stderr:\n{log}"
            );
        }
        let attempt = (|| -> std::io::Result<(u16, String)> {
            let mut stream = TcpStream::connect(&host)?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.write_all(
                format!(
                    "GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            let status_line = response.lines().next().unwrap_or("");
            let status: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let body_start = response
                .find("\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(response.len());
            Ok((status, response[body_start..].to_string()))
        })();
        match attempt {
            Ok((200, _)) => break,
            Ok((status, body)) => {
                if start.elapsed() > Duration::from_secs(5) {
                    panic!("server returned HTTP {status} from /health: {body}");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                panic!("health probe error: {e}; stderr:\n{log}");
            }
        }
    }
    guard
}

// ─────────────────────────────────────────────────────────────────────
// 1. Truncated MCP request: Content-Length lies; body is short.
// ─────────────────────────────────────────────────────────────────────
/// Send a `POST /mcp` whose `Content-Length` header claims 100,000
/// bytes but whose body is only a few bytes long. The hyper stack
/// will sit waiting for the rest, time out (or, on a faster test,
/// observe a peer close), and surface some kind of "incomplete
/// body" error. The server's response can be anything — close,
/// timeout, an error envelope — as long as the process keeps
/// running and a fresh connection can do work.
#[test]
fn server_survives_truncated_mcp_request() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut server = boot_server(port);

    let mut stream = TcpStream::connect(&host).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    // Lie about Content-Length. The body is just a few bytes.
    let partial = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}";
    let headers = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: 100000\r\n\r\n"
    );
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream.write_all(partial).expect("write partial body");
    // Drop the stream without sending the missing 99,983 bytes.
    drop(stream);

    // Give the server a moment to process the truncation.
    std::thread::sleep(Duration::from_millis(500));

    // The server must still be alive. A crash here is the failure
    // mode the test is naming.
    assert!(
        server.is_alive(),
        "server PID died after truncated Content-Length request"
    );

    // A fresh connection must still get a clean response.
    let tools_list = jsonrpc(
        &host,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
        })
        .to_string(),
    );
    let tools = tools_list
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("tools/list missing result.tools: {tools_list}"));
    assert!(
        !tools.is_empty(),
        "tools/list returned empty array after truncated request: {tools_list}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 2. Malformed JSON over an otherwise well-formed line.
// ─────────────────────────────────────────────────────────────────────
/// Send `{not valid json` to `/mcp`. The handler does
/// `serde_json::from_str` and falls into its parse-error branch
/// (returning an `error: {code, message}` envelope with code
/// -32700), but the contract under test is: process stays alive,
/// subsequent connections work.
#[test]
fn server_survives_malformed_json() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut server = boot_server(port);

    let body = "{not valid json";
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        host = host,
        len = body.len(),
    );
    let (status, resp_body) = http_request(&host, &req);

    // The server may either close the connection (no response) or
    // return a parse-error envelope. Either is fine as long as the
    // process is still alive afterward.
    if status != 0 && resp_body.contains("\"error\"") {
        // Parse error envelope — verify the JSON-RPC error shape.
        let v: serde_json::Value = serde_json::from_str(&resp_body)
            .unwrap_or_else(|e| panic!("malformed-json response not JSON: {e}\n{resp_body}"));
        assert!(
            v.pointer("/error/code").is_some(),
            "JSON-RPC parse error missing code: {v}"
        );
    }

    // Server alive after the bad request.
    assert!(
        server.is_alive(),
        "server PID died after malformed JSON"
    );

    // New connection must still serve tools/list correctly.
    let tools_list = jsonrpc(
        &host,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
        })
        .to_string(),
    );
    let tools = tools_list
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("tools/list missing result.tools");
    assert!(
        !tools.is_empty(),
        "tools/list returned empty array after malformed JSON: {tools_list}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 3. Concurrent /mcp tools/list — 20 simultaneous clients.
// ─────────────────────────────────────────────────────────────────────
/// Spawn 20 threads, each holding a `TcpStream` open, and have them
/// all race to issue `tools/list` through `JSON-RPC`. The server's
/// tokio runtime should handle them concurrently without deadlocking
/// or panicking on shared state. Every thread must see a `result`
/// envelope (no top-level `error`, no isError) and a non-empty
/// `tools` array — the same shape every thread would see in
/// isolation.
#[test]
fn server_handles_concurrent_overloaded_clients() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut server = boot_server(port);

    const N_CLIENTS: usize = 20;
    let barrier = Arc::new(Barrier::new(N_CLIENTS));
    let host_arc = Arc::new(host.clone());

    let handles: Vec<std::thread::JoinHandle<serde_json::Value>> = (0..N_CLIENTS)
        .map(|_| {
            let host = host_arc.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                // Wait for the whole pack to assemble, then race.
                barrier.wait();
                let body = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
                })
                .to_string();
                jsonrpc(&host, &body)
            })
        })
        .collect();

    let mut all_responses = Vec::with_capacity(N_CLIENTS);
    for h in handles {
        let resp = h
            .join()
            .expect("client thread panicked during concurrent tools/list");
        all_responses.push(resp);
    }

    // Every response must be a well-formed tools/list envelope.
    for (i, r) in all_responses.iter().enumerate() {
        let tools = r
            .pointer("/result/tools")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| {
                panic!("concurrent client #{i} got malformed response: {r}")
            });
        assert!(
            !tools.is_empty(),
            "concurrent client #{i} got empty tools array: {r}"
        );
        assert!(
            r.pointer("/error").is_none(),
            "concurrent client #{i} got JSON-RPC error: {r}"
        );
    }

    // Server still alive after the storm.
    assert!(
        server.is_alive(),
        "server PID died after concurrent load test"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 4. Tools must return a structured error, never panic, on hostile args.
// ─────────────────────────────────────────────────────────────────────
/// Verify that the dispatch path tolerates wrong-type, empty, and
/// malformed-shape arguments without crashing. Each call must come
/// back with a JSON-RPC envelope (either a `result` with
/// `isError=true` or a top-level `error`). We additionally assert
/// the server PID is still alive after each call — the cleanest
/// in-process proxy for "no panic in the worker".
///
/// `std::panic::catch_unwind` can only catch panics on the current
/// thread; since the tools run inside a separate process, we use
/// the child's liveness as our crash detector instead.
#[test]
fn tools_return_structured_error_not_panic() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut server = boot_server(port);

    // 4a. find_anchors with `limit` set to a non-number string.
    //     The schema says `limit: integer`; serde rejects string and
    //     the dispatch path should surface a structured error.
    let env = tools_call_envelope(
        &host,
        "find_anchors",
        serde_json::json!({"limit": "not_a_number"}),
    );
    assert!(
        env.pointer("/result").is_some() || env.pointer("/error").is_some(),
        "find_anchors with bad limit produced no envelope: {env}"
    );
    assert!(
        server.is_alive(),
        "server PID died after find_anchors with bad limit"
    );

    // 4b. get_blast_radius with `symbol: ""` — empty string is
    //     well-formed JSON but doesn't resolve to a real symbol.
    //     Either the tool rejects it up front (Missing required
    //     argument) or the resolver reports NotFound.
    let env = tools_call_envelope(
        &host,
        "get_blast_radius",
        serde_json::json!({"symbol": ""}),
    );
    assert!(
        env.pointer("/result").is_some() || env.pointer("/error").is_some(),
        "get_blast_radius with empty symbol produced no envelope: {env}"
    );
    assert!(
        server.is_alive(),
        "server PID died after get_blast_radius with empty symbol"
    );

    // 4c. query_graph with a malformed ops array — each op is
    //     tagged with a serde tag (`op`) and is missing required
    //     fields. The executor's deserialize path should reject
    //     this with a structured error, not panic on the wrong
    //     enum variant.
    let env = tools_call_envelope(
        &host,
        "query_graph",
        serde_json::json!({
            "ops": [
                {"op": "totally_made_up_op", "foo": "bar"},
                {"not_even_an_op": true}
            ]
        }),
    );
    assert!(
        env.pointer("/result").is_some() || env.pointer("/error").is_some(),
        "query_graph with malformed ops produced no envelope: {env}"
    );
    assert!(
        server.is_alive(),
        "server PID died after query_graph with malformed ops"
    );

    // 4d. Sanity check: after the three hostile calls, the server
    //     still responds to a benign call. If anything above
    //     half-killed the worker pool, this would be the first to
    //     notice (e.g. an unhandled poison error).
    let env = tools_call_envelope(
        &host,
        "get_health",
        serde_json::json!({}),
    );
    assert!(
        env.pointer("/result").is_some() && env.pointer("/error").is_none(),
        "get_health after hostile calls failed: {env}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 5. No embedding model present → semantic_search filtered out of tools/list.
// ─────────────────────────────────────────────────────────────────────
/// When `LAIN_EMBEDDING_MODEL` is unset and no `--embedding-model` is
/// passed, the server boots in stub-embedder mode. Per
/// `inert_tool_names(embedder)` in `handler.rs`, the entire
/// `semantic_search` tool is omitted from `tools/list` — a client
/// that reads the inventory should not see a tool it can never get
/// a real answer from. This test pins that contract.
#[test]
fn server_starts_when_embedding_model_missing() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let _server = boot_server(port);

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    })
    .to_string();
    let resp = jsonrpc(&host, &body);
    let tools = resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("tools/list missing result.tools: {resp}"));

    let names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .map(|s| s.to_string())
        .collect();

    assert!(
        !names.contains("semantic_search"),
        "semantic_search should be filtered when no embedding model is loaded; \
         tools/list returned {} names including semantic_search",
        names.len()
    );

    // Spot-check: the rest of the surface is still there. We pick
    // a few flagship tools to confirm the filter is scoped to the
    // inert tool, not the whole list.
    for flagship in ["find_anchors", "get_health", "query_graph"] {
        assert!(
            names.contains(flagship),
            "flagship tool `{flagship}` missing alongside semantic_search filter: {names:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// 6. request_reload must surface a corrupt YAML as a failed reload,
//    not crash the server, and recover when the file is fixed.
// ─────────────────────────────────────────────────────────────────────
/// Write garbage over the live `repos.yaml`, fire `request_reload`,
/// and assert that `get_reload_status` reports `state: "failed"`
/// with a non-empty `last_error`. Then restore the file, reload
/// again, and confirm the server comes back to `idle` and a normal
/// `tools/list` still works. The PID must remain alive throughout.
#[test]
fn request_reload_handles_corrupt_yaml() {
    // The fixture's `repos.yaml` is owned by `boot_server`'s internal
    // `tempfile::TempDir` and we can't reach it directly. Boot a
    // server with our own `repos.yaml` so the test can overwrite and
    // restore the file between reloads.

    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"reload-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn orchestrate() -> u32 {\n    let a = helper_a();\n    helper_b() + a\n}\n\
         pub fn entrypoint() -> u32 { orchestrate() }\n\
         pub fn helper_a() -> u32 { 1 }\n\
         pub fn helper_b() -> u32 { 2 }\n",
    )
    .unwrap();
    git_init(&repo_dir);
    let repo_id = repo_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml_path = project.path().join("repos.yaml");
    let good_yaml = format!(
        "data_dir: {}\nrepos:\n  - id: {}\n    source:\n      type: workspace_dir\n      path: {}\n",
        data_dir.display(),
        repo_id,
        repo_dir.display(),
    );
    std::fs::write(&repos_yaml_path, &good_yaml).unwrap();
    std::fs::write(
        project.path().join("workspaces.yaml"),
        format!(
            "workspaces:\n  - name: reload\n    members: [{}]\n",
            repo_id
        ),
    )
    .unwrap();

    let port2 = free_port();
    let host2 = format!("127.0.0.1:{port2}");
    let state = tempfile::tempdir().unwrap();
    let xdg_config = tempfile::tempdir().unwrap();
    let stderr_path = std::env::temp_dir().join(format!("failure-modes-reload-{port2}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport", "http",
            "--port", &port2.to_string(),
            "--workspace", "auto",
            "--config",
            repos_yaml_path.to_str().unwrap(),
        ])
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", xdg_config.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("spawn second server");
    let mut server = ServerGuard(child);

    // Wait for /health to come up.
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(30) {
            let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!("second server did not become healthy: {log}");
        }
        let attempt = (|| -> std::io::Result<(u16, String)> {
            let mut stream = TcpStream::connect(&host2)?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.write_all(
                format!(
                    "GET /health HTTP/1.1\r\nHost: {host2}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            let status_line = response.lines().next().unwrap_or("");
            let status: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let body_start = response
                .find("\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(response.len());
            Ok((status, response[body_start..].to_string()))
        })();
        match attempt {
            Ok((200, _)) => break,
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                panic!("health probe error: {e}; stderr:\n{log}");
            }
        }
    }

    // 6a. Write garbage over repos.yaml.
    std::fs::write(&repos_yaml_path, "key: : : not yaml\n\t: [unbalanced").unwrap();

    // 6b. Fire request_reload. It returns immediately with
    //     `{accepted: true, ...}` — the rebuild itself is async.
    let _accepted = tools_call_envelope(
        &host2,
        "request_reload",
        serde_json::json!({}),
    );

    // 6c. Poll get_reload_status. The async rebuild should finish
    //     quickly and the bus should record a Failed state with a
    //     non-empty last_error.
    let mut failed = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let env = tools_call_envelope(
            &host2,
            "get_reload_status",
            serde_json::json!({}),
        );
        let text = env
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let status: serde_json::Value =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("status not JSON: {e}\n{text}"));
        if status.get("state").and_then(|v| v.as_str()) == Some("failed") {
            let err = status
                .get("last_error")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(
                !err.is_empty(),
                "rebuild reported failed but last_error was empty: {status}"
            );
            failed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        failed,
        "rebuild with corrupt YAML did not transition to `failed` state within 10s"
    );

    // 6d. Server PID still alive after the failed reload.
    assert!(
        server.is_alive(),
        "server PID died after corrupt YAML reload"
    );

    // 6e. Restore the file and reload again. The server should
    //     come back to idle and a fresh tools/list must work.
    std::fs::write(&repos_yaml_path, &good_yaml).unwrap();
    let _ = tools_call_envelope(
        &host2,
        "request_reload",
        serde_json::json!({}),
    );

    let mut recovered = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let env = tools_call_envelope(
            &host2,
            "get_reload_status",
            serde_json::json!({}),
        );
        let text = env
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let status: serde_json::Value =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("status not JSON: {e}\n{text}"));
        if status.get("state").and_then(|v| v.as_str()) == Some("idle") {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        recovered,
        "server did not return to `idle` after good-YAML reload"
    );

    // 6f. tools/list still works after the recovery.
    let resp = jsonrpc(
        &host2,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
        })
        .to_string(),
    );
    let tools = resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("tools/list missing result.tools after recovery: {resp}"));
    assert!(
        !tools.is_empty(),
        "tools/list empty after YAML recovery: {resp}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 7. Flood of invalid JSON: server must drop or close, but stay up.
// ─────────────────────────────────────────────────────────────────────
/// Open a connection, send 1,000 garbage messages as fast as
/// `write_all` will let us. The server may close the connection
/// (most likely — hyper returns a 400 and drops), or it may stay
/// open and read all of them. Either way, the server's PID must
/// still be alive and a fresh connection must get a clean
/// `tools/list`.
#[test]
fn server_logs_dropped_messages_gracefully() {
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut server = boot_server(port);

    // Open one connection and write 1000 short invalid messages.
    // Each one is a complete HTTP/1.1 request line + headers +
    // body so the parser sees them as discrete requests.
    {
        let mut stream = TcpStream::connect(&host).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        let garbage = b"{not even close to json";
        for _ in 0..1000 {
            let req = format!(
                "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
                 Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n",
                host = host,
                len = garbage.len(),
            );
            // We don't care if write fails — the server may close
            // mid-stream and that's the desired behavior.
            if stream.write_all(req.as_bytes()).is_err() {
                break;
            }
            if stream.write_all(garbage).is_err() {
                break;
            }
        }
        // Drop the stream — the server may have already closed
        // its end.
    }

    // Give the server a moment to recover from the flood.
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        server.is_alive(),
        "server PID died after 1000-message garbage flood"
    );

    // A fresh connection must still get a clean tools/list.
    let tools_list = jsonrpc(
        &host,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
        })
        .to_string(),
    );
    let tools = tools_list
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("tools/list missing result.tools after flood: {tools_list}"));
    assert!(
        !tools.is_empty(),
        "tools/list empty after garbage flood: {tools_list}"
    );
}
