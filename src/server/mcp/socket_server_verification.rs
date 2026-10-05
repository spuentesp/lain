//! Safety properties of the shared Unix-socket server (see the module docs).
use super::*;
use std::io::{BufRead, BufReader as StdBufReader, Write};
use std::os::unix::net::{UnixListener as StdListener, UnixStream};

fn sock(dir: &Path) -> PathBuf {
    dir.join("s.sock")
}

#[test]
fn probe_is_false_for_missing_and_for_a_dead_leftover_and_true_for_a_live_listener() {
    let dir = tempfile::tempdir().unwrap();
    let p = sock(dir.path());
    assert!(!probe_alive(&p), "no file");
    let l = StdListener::bind(&p).unwrap();
    assert!(probe_alive(&p), "live listener");
    drop(l); // the socket FILE stays behind, as after a crash
    assert!(p.exists());
    assert!(
        !probe_alive(&p),
        "a leftover socket file is not a live server"
    );
}

#[test]
fn path_limit_is_enforced() {
    assert!(path_fits(Path::new(&format!("/{}", "a".repeat(100)))));
    assert!(!path_fits(Path::new(&format!("/{}", "a".repeat(103)))));
}

#[tokio::test]
async fn bind_clears_a_stale_socket_but_refuses_a_live_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = sock(dir.path());
    drop(StdListener::bind(&p).unwrap()); // stale leftover
    let first = bind_exclusive(&p).expect("a stale socket must be replaced");
    let err = bind_exclusive(&p)
        .err()
        .expect("a live server must not be displaced");
    assert!(err.to_string().contains("already serving"), "{err}");
    drop(first);
}

#[tokio::test]
async fn socket_is_owner_only_and_a_created_parent_is_0700() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("run").join("lain").join("s.sock");
    let _l = bind_exclusive(&p).unwrap();
    let mode = |q: &Path| std::fs::metadata(q).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&p), 0o600, "socket must be owner-only");
    assert_eq!(
        mode(p.parent().unwrap()),
        0o700,
        "created run dir must be private"
    );
}

/// Several daemons start together on a stale socket: exactly one may win, and
/// none may delete the winner's live socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starters_on_a_stale_socket_yield_exactly_one_server() {
    for round in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let p = sock(dir.path());
        drop(StdListener::bind(&p).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (p, b) = (p.clone(), barrier.clone());
                tokio::task::spawn_blocking(move || {
                    b.wait();
                    bind_exclusive(&p).ok()
                })
            })
            .collect();
        let mut winners = Vec::new();
        for h in handles {
            if let Some(l) = h.await.unwrap() {
                winners.push(l);
            }
        }
        assert_eq!(
            winners.len(),
            1,
            "round {round}: {} servers bound",
            winners.len()
        );
        assert!(
            probe_alive(&p),
            "round {round}: the winner's socket was deleted"
        );
    }
}

#[tokio::test]
async fn an_oversized_frame_is_rejected_not_buffered() {
    let (mut client, server) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        let _ = client.write_all(&[b'x'; 4096]).await; // no newline, ever
        let _ = client.shutdown().await;
    });
    let mut reader = BufReader::new(server);
    let mut line = String::new();
    let err = read_frame_limited(&mut reader, &mut line, 64).await.err();
    assert!(
        err.is_some(),
        "an unterminated frame past the limit must error"
    );
    assert!(line.len() <= 65, "buffered {} bytes", line.len());
}

#[tokio::test]
async fn frames_within_the_limit_and_eof_are_read_normally() {
    let (mut client, server) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        let _ = client.write_all(b"{\"a\":1}\n{\"b\":2}\n").await;
        let _ = client.shutdown().await;
    });
    let mut reader = BufReader::new(server);
    let mut line = String::new();
    assert!(read_frame_limited(&mut reader, &mut line, 64)
        .await
        .unwrap()
        .is_some());
    assert_eq!(line.trim(), "{\"a\":1}");
    assert!(read_frame_limited(&mut reader, &mut line, 64)
        .await
        .unwrap()
        .is_some());
    assert_eq!(line.trim(), "{\"b\":2}");
    assert!(read_frame_limited(&mut reader, &mut line, 64)
        .await
        .unwrap()
        .is_none());
}

fn git_fixture(root: &Path) {
    let run = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(root.join("lib.rs"), "pub fn hi() {}\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
}

fn test_server(root: &Path) -> Arc<LainServer> {
    git_fixture(root);
    Arc::new(LainServer::new(root, &root.join(".lain/graph.bin"), None).unwrap())
}

fn rpc(stream: &mut UnixStream, req: Value) -> Value {
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    stream.write_all(line.as_bytes()).unwrap();
    let mut reply = String::new();
    StdBufReader::new(stream.try_clone().unwrap())
        .read_line(&mut reply)
        .unwrap();
    serde_json::from_str(&reply).unwrap()
}

/// Calls over the socket go through the readiness gate like stdio/HTTP do:
/// while the graph is still warming up a graph-dependent tool is turned back
/// with a structured `warming_up` result instead of answering from a graph
/// that is being built.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_call_over_the_socket_is_gated_while_warming_up() {
    let dir = tempfile::tempdir().unwrap();
    let server = test_server(dir.path());
    let p = sock(dir.path());
    let _task = start(p.clone(), server.clone(), None).unwrap();

    let p2 = p.clone();
    let (warming, ready) = tokio::task::spawn_blocking(move || {
        let mut c = UnixStream::connect(&p2).unwrap();
        let call = |c: &mut UnixStream, id| {
            rpc(
                c,
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                          "params":{"name":"find_anchors","arguments":{}}}),
            )
        };
        let warming = call(&mut c, 1);
        (warming, c)
    })
    .await
    .map(|(w, c)| (w, c))
    .unwrap();
    let text = warming["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("warming_up"),
        "ungated answer while warming up: {text}"
    );
    drop(ready);

    // Once ready, the same call is dispatched normally.
    server.readiness().ready(None);
    let p3 = p.clone();
    let after = tokio::task::spawn_blocking(move || {
        let mut c = UnixStream::connect(&p3).unwrap();
        rpc(
            &mut c,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                           "params":{"name":"find_anchors","arguments":{}}}),
        )
    })
    .await
    .unwrap();
    let text = after["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("warming_up"),
        "still gated after ready: {text}"
    );
}

/// Tools that live outside the executor (presence, audit, status) used to be
/// unreachable over the socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inventory_tools_are_reachable_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let server = test_server(dir.path());
    let p = sock(dir.path());
    let _task = start(p.clone(), server, None).unwrap();
    let reply = tokio::task::spawn_blocking(move || {
        let mut c = UnixStream::connect(&p).unwrap();
        rpc(
            &mut c,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                           "params":{"name":"get_server_status","arguments":{}}}),
        )
    })
    .await
    .unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert_ne!(reply["result"]["isError"], json!(true), "{text}");
    assert!(
        text.contains("transport") || text.contains("version"),
        "unexpected: {text}"
    );
}

/// Daemon mode exits and removes its socket after the idle window, and an
/// open connection keeps it alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_daemon_exits_and_removes_its_socket_but_an_open_connection_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = test_server(dir.path());
    let p = sock(dir.path());
    let task = start(p.clone(), server, Some(Duration::from_millis(300))).unwrap();

    let held = UnixStream::connect(&p).unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!task.is_finished(), "exited while a connection was open");
    drop(held);

    let res = tokio::time::timeout(Duration::from_secs(5), task).await;
    assert!(res.is_ok(), "did not exit after the idle window");
    assert!(!p.exists(), "socket file left behind after idle exit");
}

// ---- the protocol surface: arbitrary requests, arbitrary tool calls ----------------------

mod surface {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    /// Tools that run builds/tests, spawn processes, reach the network, sleep, or
    /// reconfigure the server: not safe to call with random arguments.
    const UNSAFE: &[&str] = &[
        "run_build",
        "run_clippy",
        "run_tests",
        "run_enrichment",
        "install_language_server",
        "register_job_webhook",
        "debug_sleep",
        "request_reload",
        "sync_state",
        "prepare_snapshot",
    ];

    fn safe_tool_names() -> Vec<String> {
        crate::server::mcp::definitions::dump_tools_schema(&[])
            .into_iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(str::to_string))
            .filter(|n| !UNSAFE.contains(&n.as_str()))
            .collect()
    }

    fn arb_value(secret: String) -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            any::<f64>()
                .prop_filter("finite", |f| f.is_finite())
                .prop_map(|f| json!(f)),
            "\\PC{0,24}".prop_map(Value::String),
            Just(json!("")),
            Just(json!("../../../../etc/passwd")),
            Just(json!("..\\..\\windows\\system32")),
            Just(json!("lib.rs")),
            Just(json!("hi")),
            Just(json!("x".repeat(20_000))),
            Just(Value::String(secret.clone())),
            Just(Value::String(format!(
                "../{}",
                secret.rsplit('/').next().unwrap_or("")
            ))),
            Just(json!(u64::MAX)),
            Just(json!(-1)),
        ];
        leaf.prop_recursive(2, 16, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::vec(("[a-z_]{1,12}", inner), 0..4)
                    .prop_map(|kv| Value::Object(kv.into_iter().collect())),
            ]
        })
    }

    fn arb_args(secret: String) -> impl Strategy<Value = serde_json::Map<String, Value>> {
        const KEYS: &[&str] = &[
            "symbol",
            "name",
            "query",
            "path",
            "file",
            "from",
            "to",
            "limit",
            "depth",
            "repo_id",
            "id",
            "agent_id",
            "paths",
            "session_token",
            "kind",
            "status",
            "target",
            "ref",
            "a",
            "b",
            "line",
            "code",
            "text",
            "body",
            "intent",
            "scopes",
            "service",
            "snapshot_id",
        ];
        prop::collection::vec(
            (prop::sample::select(KEYS.to_vec()), arb_value(secret)),
            0..6,
        )
        .prop_map(|kv| kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn texts(result: &Value) -> String {
        result["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// Every safe tool, called with arbitrary arguments — including traversal
    /// paths and the absolute path of a secret OUTSIDE the workspace — never
    /// panics, never hangs, always returns a well-formed result, and never
    /// returns the secret's contents.
    #[test]
    fn arbitrary_tool_calls_never_panic_hang_or_leak_a_file_outside_the_workspace() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        const MARKER: &str = "TOP-SECRET-MARKER-9f3a7c1e";
        let secret_path = outside.path().join("secret.txt");
        std::fs::write(&secret_path, format!("{MARKER}\nfn secret() {{}}\n")).unwrap();
        let secret = secret_path.to_string_lossy().into_owned();

        let server = rt.block_on(async { test_server(ws.path()) });
        server.readiness().ready(None); // ungate graph tools so they really run
        let names = safe_tool_names();
        assert!(names.len() > 50, "tool list looks wrong: {}", names.len());

        let tool = prop_oneof![
            4 => prop::sample::select(names.clone()),
            1 => "[a-z_]{1,16}".prop_map(String::from), // unknown names too
        ];
        let strategy = (tool, arb_args(secret.clone()));
        let mut runner = TestRunner::new(Config {
            cases: 600,
            max_shrink_iters: 100,
            ..Config::default()
        });
        let result = runner.run(&strategy, |(name, args)| {
            let server = server.clone();
            let (n2, a2) = (name.clone(), args.clone());
            let call = rt.block_on(async move {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    crate::server::mcp::handler::call_tool_in_process(&server, &n2, a2),
                )
                .await
            });
            let result = call.map_err(|_| {
                TestCaseError::fail(format!("{name} {args:?} did not finish in 20s"))
            })?;
            let v = serde_json::to_value(&result)
                .map_err(|e| TestCaseError::fail(format!("unserialisable result: {e}")))?;
            let body = texts(&v);
            prop_assert!(
                !body.contains(MARKER),
                "{name} {args:?} leaked a file outside the workspace"
            );
            prop_assert!(v.get("content").is_some(), "{name}: result has no content");
            Ok(())
        });
        if let Err(e) = result {
            panic!("{e}");
        }
    }

    /// The JSON-RPC layer: any method/params/id yields exactly one well-formed
    /// response (or none for a notification), echoing the id.
    #[test]
    fn arbitrary_json_rpc_requests_get_one_well_formed_response() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let ws = tempfile::tempdir().unwrap();
        let server = rt.block_on(async { test_server(ws.path()) });
        let strategy = (
            prop_oneof![
                Just("initialize".to_string()),
                Just("ping".to_string()),
                Just("tools/list".to_string()),
                Just("tools/call".to_string()),
                Just("notifications/initialized".to_string()),
                "[a-z/]{0,16}".prop_map(String::from),
            ],
            arb_value(String::new()),
            prop_oneof![
                Just(Value::Null),
                any::<i64>().prop_map(|n| json!(n)),
                "\\PC{0,8}".prop_map(Value::String)
            ],
        );
        let mut runner = TestRunner::new(Config {
            cases: 400,
            ..Config::default()
        });
        runner
            .run(&strategy, |(method, params, id)| {
                let server = server.clone();
                let (m, p, i) = (method.clone(), params, id.clone());
                let resp = rt.block_on(async move { dispatch(&m, p, i, &server).await });
                if method == "notifications/initialized" {
                    prop_assert_eq!(resp, Value::Null);
                    return Ok(());
                }
                prop_assert_eq!(&resp["jsonrpc"], &json!("2.0"));
                prop_assert_eq!(&resp["id"], &id);
                let (has_result, has_error) =
                    (resp.get("result").is_some(), resp.get("error").is_some());
                prop_assert!(
                    has_result ^ has_error,
                    "need exactly one of result/error: {resp}"
                );
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Positive control for the leak test above: tools that read files DO return
/// content for a path inside the workspace. Without this, "the secret never
/// appears" would hold trivially.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_reading_tools_do_return_workspace_content_so_the_leak_test_has_teeth() {
    let ws = tempfile::tempdir().unwrap();
    let server = test_server(ws.path());
    std::fs::write(
        ws.path().join("marked.rs"),
        "// WS-MARKER-77\npub fn marked_fn() {}\n",
    )
    .unwrap();
    server.readiness().ready(None);
    let mut returned = Vec::new();
    for (tool, args) in [
        (
            "get_code_snippet",
            json!({"path": "marked.rs", "file": "marked.rs", "symbol": "marked_fn", "line": 1}),
        ),
        (
            "read_source",
            json!({"path": "marked.rs", "file": "marked.rs"}),
        ),
        ("get_file_diff", json!({"path": "marked.rs"})),
        (
            "get_test_template",
            json!({"symbol": "marked_fn", "path": "marked.rs"}),
        ),
    ] {
        let r = crate::server::mcp::handler::call_tool_in_process(
            &server,
            tool,
            args.as_object().unwrap().clone(),
        )
        .await;
        let v = serde_json::to_value(&r).unwrap();
        let text = v["content"][0]["text"].as_str().unwrap_or("").to_string();
        if text.contains("WS-MARKER-77") || text.contains("marked_fn") {
            returned.push(tool);
        }
    }
    assert!(
        !returned.is_empty(),
        "no tool returned the workspace file's content: the secret-leak property would be vacuous"
    );
}

/// Targeted containment check. A repository is untrusted input: tools that
/// read files must not hand back a file outside the workspace, whether it is
/// named by an absolute path, by `..` traversal, or reached through a symlink
/// that the repository itself contains.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tools_do_not_escape_the_workspace_by_path_traversal_or_symlink() {
    const MARKER: &str = "ESCAPED-SECRET-5d2b";
    let ws = tempfile::tempdir().unwrap();
    let server = test_server(ws.path());
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.rs");
    std::fs::write(&secret, format!("// {MARKER}\npub fn leaked() {{}}\n")).unwrap();
    // A symlink the repository itself contains, pointing outside.
    std::os::unix::fs::symlink(&secret, ws.path().join("link.rs")).unwrap();
    std::os::unix::fs::symlink(outside.path(), ws.path().join("linkdir")).unwrap();
    // Commit the symlinks and index the repository, so the secret's symbols are
    // real nodes in the graph (a tool can only return what a node points at).
    for args in [
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "links",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(&args)
            .current_dir(ws.path())
            .status()
            .unwrap()
            .success());
    }
    server
        .build_core_memory_until_complete()
        .await
        .expect("index the repo");
    let indexed: Vec<String> = server
        .ingest()
        .graph()
        .get_all_nodes()
        .into_iter()
        .map(|n| n.name)
        .collect();
    eprintln!("indexed symbols: {indexed:?}");
    server.readiness().ready(None);

    let rel_up = format!(
        "../{}/secret.rs",
        outside.path().file_name().unwrap().to_string_lossy()
    );
    let spellings = [
        secret.to_string_lossy().into_owned(),
        rel_up,
        "link.rs".to_string(),
        "linkdir/secret.rs".to_string(),
        "./link.rs".to_string(),
    ];
    let mut leaks = Vec::new();
    for tool in [
        "get_code_snippet",
        "read_source",
        "get_file_diff",
        "get_test_template",
        "get_context",
        "explain_symbol",
        "get_call_sites",
        "get_context_for_prompt",
    ] {
        for p in &spellings {
            for key in ["path", "file", "target"] {
                let mut args = serde_json::Map::new();
                args.insert(key.into(), json!(p));
                args.insert("symbol".into(), json!("leaked"));
                args.insert("name".into(), json!("leaked"));
                args.insert("line".into(), json!(1));
                let r =
                    crate::server::mcp::handler::call_tool_in_process(&server, tool, args).await;
                let v = serde_json::to_value(&r).unwrap();
                let text = v["content"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if text.contains(MARKER) {
                    leaks.push(format!("{tool}({key}={p})"));
                }
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "files outside the workspace were returned: {leaks:#?}"
    );
}

/// A file replaced by a symlink that leaves the workspace is retracted and not
/// re-indexed (the watcher path, `process_change`).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_replaced_by_an_escaping_symlink_is_retracted_not_indexed() {
    let ws = tempfile::tempdir().unwrap();
    let server = test_server(ws.path());
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.rs");
    std::fs::write(&secret, "pub fn leaked_symbol() {}\n").unwrap();

    let f = ws.path().join("swap.rs");
    std::fs::write(&f, "pub fn inside_symbol() {}\n").unwrap();
    server.process_change(&f).await.unwrap();
    let names = |s: &Arc<LainServer>| -> Vec<String> {
        let mut v: Vec<String> = s
            .ingest()
            .graph()
            .get_all_nodes()
            .into_iter()
            .map(|n| n.name)
            .collect();
        v.extend(s.overlay().get_all_nodes().into_iter().map(|n| n.name));
        v
    };
    assert!(
        names(&server).contains(&"inside_symbol".to_string()),
        "control: indexed normally"
    );

    std::fs::remove_file(&f).unwrap();
    std::os::unix::fs::symlink(&secret, &f).unwrap();
    server.process_change(&f).await.unwrap();
    let after = names(&server);
    assert!(
        !after.contains(&"leaked_symbol".to_string()),
        "indexed through the symlink: {after:?}"
    );
    assert!(
        !after.contains(&"inside_symbol".to_string()),
        "the old nodes were not retracted: {after:?}"
    );
}

/// The frame cap is part of the documented contract (16 MiB): big enough for
/// the largest legitimate tool call, small enough to bound memory per client.
#[test]
fn frame_cap_is_sixteen_mebibytes() {
    assert_eq!(MAX_FRAME_BYTES, 16 * 1024 * 1024);
    assert_eq!(MAX_SOCKET_PATH_BYTES, 103);
}

/// The diagnostic pid file sits next to the socket, holds this process's pid,
/// and is replaced (not left stale) when a dead leftover is cleared.
#[tokio::test]
async fn pid_file_is_beside_the_socket_and_names_this_process() {
    let dir = tempfile::tempdir().unwrap();
    let p = sock(dir.path());
    let expect = dir
        .path()
        .join(format!("{}.pid", p.file_name().unwrap().to_string_lossy()));
    assert_eq!(pid_path_for(&p), expect);
    let _l = bind_exclusive(&p).unwrap();
    assert_eq!(
        std::fs::read_to_string(&expect).unwrap(),
        std::process::id().to_string()
    );
    // A stale pid file from a previous owner is overwritten by the next bind.
    drop(_l);
    std::fs::remove_file(&p).unwrap();
    std::fs::write(&expect, "1").unwrap();
    let _l2 = bind_exclusive(&p).unwrap();
    assert_eq!(
        std::fs::read_to_string(&expect).unwrap(),
        std::process::id().to_string()
    );
}
