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
