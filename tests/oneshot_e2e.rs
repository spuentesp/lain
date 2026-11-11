//! End-to-end regression test for `lain oneshot`.
//!
//! Bug: oneshot closed the child's stdin immediately after writing
//! initialize + tools/call. EOF makes the MCP SDK start shutting down
//! the stdio transport; when a startup re-index is in flight (fresh or
//! stale graph) the single-threaded `lain mcp` runtime is busy and the
//! in-flight tools/call loses the race against the shutdown — the
//! response never arrives. The client's blocking `read()` then waited
//! forever, because the deadline was only checked *after* read
//! returned. Net effect: `lain oneshot` hung for the full timeout
//! (or indefinitely) exactly on the first run against a repo — the
//! worst possible first-run experience.
//!
//! Fix under test: the client keeps stdin open until the tools/call
//! response arrives and enforces the deadline with `recv_timeout` on a
//! reader thread instead of a blocking read.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Build a temp git repo with enough Rust files that the startup
/// re-index takes measurably longer than a tools/call dispatch.
/// Returns the TempDir (kept alive by the caller) and its path.
fn make_repo(files: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..files {
        let mut body = String::new();
        for j in 0..20 {
            body.push_str(&format!(
                "pub fn fn_{i}_{j}(x: u32) -> u32 {{ helper_{i}(x) + {j} }}\n"
            ));
        }
        body.push_str(&format!("fn helper_{i}(x: u32) -> u32 {{ x + {i} }}\n"));
        std::fs::write(root.join("src").join(format!("mod{i}.rs")), body).unwrap();
    }
    std::fs::write(root.join("src/lib.rs"), "// test repo\n").unwrap();

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    (tmp, root)
}

/// Spawn `lain oneshot` against `workspace` with an isolated state dir
/// and wait up to `budget` for it to exit, killing it past the budget.
/// Returns (exit_success, stdout, elapsed).
fn run_oneshot(workspace: &std::path::Path, budget: Duration) -> (bool, String, Duration) {
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            workspace.to_str().unwrap(),
            "find_anchors",
        ])
        .env("XDG_STATE_HOME", state.path())
        // These tests pin the private-server path; without this a oneshot
        // would start a detached shared daemon that outlives the test.
        .env("LAIN_ONESHOT_NO_SHARE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > budget {
            let _ = child.kill();
            let _ = child.wait();
            return (
                false,
                String::from("<killed: exceeded budget>"),
                start.elapsed(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    (status.success(), out, start.elapsed())
}

/// First-ever run against a repo (cold graph, re-index in flight) must
/// still return within the oneshot deadline — the pre-fix code hung
/// here because the tools/call raced the EOF-triggered shutdown.
///
/// Budget is generous (75s > default LAIN_ONESHOT_TIMEOUT=60s) so a
/// genuine-but-slow success still passes; the pre-fix hang either
/// exceeded the internal 60s deadline (error exit) or blocked forever
/// (killed at 75s).
#[test]
fn oneshot_returns_on_cold_graph() {
    let (_tmp, root) = make_repo(30);
    // 2026-10-04 fix: LAIN_ONESHOT_TIMEOUT default bumped to 600s to
    // match LAIN_REINDEX_TIMEOUT so a cold reindex on a non-trivial
    // repo (e.g. Lain on Lain, ~5 min) doesn't time out at 60s and
    // leave the user with a "no tools/call response" error that
    // doesn't distinguish "server is busy" from "server is hung".
    // The wrapper here still passes a 75s budget — internal default
    // is now well above this, so the assertion "oneshot returns
    // within 75s of cold start" is still meaningful as a hang probe.
    let (ok, out, elapsed) = run_oneshot(&root, Duration::from_secs(75));
    assert!(
        ok,
        "oneshot failed on cold graph after {:?}; stdout:\n{}",
        elapsed, out
    );
    assert!(
        out.contains("anchors") || out.contains("No anchors"),
        "unexpected oneshot output:\n{}",
        out
    );
}

/// Second run with a warm graph must be fast (<20s even on slow CI).
#[test]
fn oneshot_warm_graph_is_fast() {
    let (_tmp, root) = make_repo(5);
    let (ok, _, _) = run_oneshot(&root, Duration::from_secs(75));
    assert!(ok, "warm-up run failed");
    let (ok, out, elapsed) = run_oneshot(&root, Duration::from_secs(30));
    assert!(ok, "warm run failed: {}", out);
    assert!(
        elapsed < Duration::from_secs(20),
        "warm oneshot took {:?}, expected <20s",
        elapsed
    );
}

/// `lain oneshot` from inside the repo (no --workspace) resolves the
/// workspace by walking up to `.git`.
#[test]
fn oneshot_discovers_workspace_from_cwd() {
    let (_tmp, root) = make_repo(3);
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args(["oneshot", "find_anchors"])
        .current_dir(root.join("src"))
        .env("XDG_STATE_HOME", state.path())
        // These tests pin the private-server path; without this a oneshot
        // would start a detached shared daemon that outlives the test.
        .env("LAIN_ONESHOT_NO_SHARE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(75) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("oneshot (cwd discovery) exceeded 75s budget — hung");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "exit: {:?}", status);
}

/// PR #63 review finding: `lain oneshot` only checked the top-level
/// JSON-RPC `error` field, never `result.isError` — the field MCP tools
/// (including the central readiness gate's terminal `unavailable_error`)
/// use for a genuine failure. It printed the error payload and still
/// exited 0, so a script or agent checking the exit code alone saw
/// success. `explain_symbol` with no `symbol` arg is a cheap way to
/// force `isError: true` from a healthy server without needing to
/// manufacture a degraded repo.
#[test]
fn oneshot_exits_nonzero_on_tool_error() {
    let (_tmp, root) = make_repo(3);
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            root.to_str().unwrap(),
            "explain_symbol",
        ])
        .env("XDG_STATE_HOME", state.path())
        // These tests pin the private-server path; without this a oneshot
        // would start a detached shared daemon that outlives the test.
        .env("LAIN_ONESHOT_NO_SHARE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(75) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("oneshot (missing arg) exceeded 75s budget — hung");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert!(
        !status.success(),
        "explain_symbol with a missing required arg must exit non-zero; stdout:\n{out}\nstderr:\n{err}"
    );
    assert!(
        out.contains("symbol") || err.contains("symbol"),
        "the isError payload naming the missing arg should still be printed; stdout:\n{out}\nstderr:\n{err}"
    );
}

/// B1: the first `oneshot` leaves a shared daemon behind, the second reuses
/// it, and the daemon removes itself after the idle window. Isolated run dir
/// (short path: `sun_path` is ~104 bytes) so nothing leaks into the user's.
///
/// Timing is deterministic, not wall-clock luck: a holder connection is
/// opened the moment the daemon's socket appears and kept open through
/// both calls — the daemon's idle shutdown requires zero open
/// connections, so it cannot exit mid-assertions no matter how the test
/// thread is scheduled. Dropping the holder is what *starts* the idle
/// window; socket removal and process death are then observed by
/// polling with generous bounded deadlines (no fixed sleeps).
/// `LAIN_SHARED_IDLE_SECS=30` so the daemon cannot idle-exit in the gap
/// between the first call's connection closing and the holder
/// connecting, even if the test thread is starved under load.
#[cfg(unix)]
#[test]
fn shared_daemon_outlives_the_first_call_and_exits_when_idle() {
    use std::os::unix::net::UnixStream;
    let run = tempfile::Builder::new()
        .prefix("lr")
        .tempdir_in("/tmp")
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_repo, root) = make_repo(3);
    let sock_dir = run.path().join("lain");

    let oneshot = || {
        let out = Command::new(env!("CARGO_BIN_EXE_lain"))
            .args([
                "oneshot",
                "--workspace",
                root.to_str().unwrap(),
                "find_anchors",
            ])
            .env("XDG_RUNTIME_DIR", run.path())
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path())
            .env("LAIN_SHARED_IDLE_SECS", "30")
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "oneshot failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // The daemon's MCP socket is `lain-mcp-<16hex>.sock`. The run dir
    // also holds the daemon's hot-reload signal socket
    // (`lain-daemon-…`), so "first .sock" is not good enough — the
    // holder must pin (and ping) the MCP socket specifically.
    fn is_mcp_sock(p: &std::path::Path) -> bool {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("lain-mcp-"))
            .and_then(|s| s.strip_suffix(".sock"))
            .is_some_and(|stem| stem.len() == 16 && stem.chars().all(|c| c.is_ascii_alphanumeric()))
    }
    let socket = || {
        std::fs::read_dir(&sock_dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| is_mcp_sock(p))
    };

    // The daemon must not outlive this test: under `cargo llvm-cov` a process
    // still running (or killed) when profile data is merged corrupts the merge
    // ("file header is corrupt"). Track its pid and always reap it.
    struct ReapDaemon(Option<String>);
    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    impl Drop for ReapDaemon {
        fn drop(&mut self) {
            if let Some(pid) = self.0.take() {
                if alive(&pid) {
                    let _ = Command::new("kill").args(["-TERM", &pid]).status();
                    for _ in 0..50 {
                        if !alive(&pid) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    }
    let mut reaper = ReapDaemon(None);

    // Pre-armed holder: connect to the daemon's socket the moment it
    // exists (it is created at bind time, while the first call is still
    // in flight) and keep the connection open. An open connection keeps
    // the daemon's activity counter non-zero, which pins the idle
    // shutdown off until we deliberately drop it below.
    let holder = {
        let sock_dir = sock_dir.clone();
        std::thread::spawn(move || -> Option<UnixStream> {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let found = std::fs::read_dir(&sock_dir).ok().and_then(|entries| {
                    entries.flatten().map(|e| e.path()).find(|p| is_mcp_sock(p))
                });
                if let Some(s) = found {
                    if let Ok(stream) = UnixStream::connect(&s) {
                        return Some(stream);
                    }
                }
                if Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };

    let first = oneshot();
    let held = holder
        .join()
        .expect("holder thread must not panic")
        .expect("first call must leave a shared server's socket behind");
    let sock = socket().expect("the socket file must exist while the holder is connected");
    let pid_file = std::path::PathBuf::from(format!("{}.pid", sock.display()));
    reaper.0 = std::fs::read_to_string(&pid_file)
        .ok()
        .map(|p| p.trim().to_string());
    assert!(
        reaper.0.is_some(),
        "the daemon must record its pid at {pid_file:?}"
    );

    // The shared server must still be alive after the first call — proven
    // by a round-trip on the held connection (the daemon must answer a
    // ping), not by a bare connect that only shows the kernel queued us.
    {
        use std::io::{BufRead, BufReader, Write};
        let mut writer = held.try_clone().expect("clone held stream");
        let mut reader = BufReader::new(held.try_clone().expect("clone held stream"));
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .expect("write ping");
        writer.flush().expect("flush ping");
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set read timeout");
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("shared daemon must answer the ping");
        assert!(
            line.contains("\"result\""),
            "shared daemon must answer ping with a result; got: {line:?}"
        );
    }

    let second = oneshot();
    assert_eq!(first, second, "the shared server must answer the same");

    // Drop the held connection: this is the deterministic start of the
    // idle window (LAIN_SHARED_IDLE_SECS=30). Do NOT probe the socket
    // after this point: every connection counts as activity and would
    // keep the daemon alive.
    drop(held);

    // Idle shutdown, observed by polling for the socket's removal with a
    // generous bounded deadline (file checks only — no connects).
    let deadline = Instant::now() + Duration::from_secs(90);
    while sock.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        !sock.exists(),
        "the idle shared server never removed its socket"
    );
    assert!(
        UnixStream::connect(&sock).is_err(),
        "something still accepts on the socket"
    );
    // Removing the socket is not exiting: wait for the process itself to be
    // gone, so its coverage profile is fully written before the harness moves on.
    let pid = reaper.0.clone().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !alive(&pid),
        "the idle daemon (pid {pid}) removed its socket but never exited"
    );
}

// ---------------------------------------------------------------------------
// Shared-daemon identity: a daemon from another build must not answer.
// ---------------------------------------------------------------------------

/// The socket a `lain oneshot` client uses for `workspace`, inside the
/// test's own run dir. The file name is the workspace-set hash, which
/// does not depend on the run dir — so the client (which computes it
/// from its own `XDG_RUNTIME_DIR`) and this test agree on the path.
#[cfg(unix)]
fn oneshot_socket_path(
    workspace: &std::path::Path,
    run_dir: &std::path::Path,
) -> std::path::PathBuf {
    let name = lain::config::oneshot_socket_path(workspace)
        .file_name()
        .expect("the oneshot socket must have a file name")
        .to_string_lossy()
        .into_owned();
    run_dir.join("lain").join(name)
}

/// The text the fake daemon answers a `tools/call` with.
#[cfg(unix)]
const FAKE_TOOL_TEXT: &str = "fake-anchor-payload";

/// A fake shared daemon behind the oneshot socket: it answers
/// `lain/daemon_info` with the given build identity, and
/// `initialize` / `tools/call` with canned replies. It runs no indexer.
///
/// Every method it is asked for is recorded in `methods`, so a test can
/// assert that the client only PROBED it and never used it.
#[cfg(unix)]
fn spawn_fake_daemon(
    socket: &std::path::Path,
    identity: (&str, &str, &str),
    stop: Arc<AtomicBool>,
    methods: Arc<Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (version, build_commit, index_build_id) = (
        identity.0.to_string(),
        identity.1.to_string(),
        identity.2.to_string(),
    );
    let tool_text = FAKE_TOOL_TEXT.to_string();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let Ok(request) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                            continue;
                        };
                        let method = request
                            .get("method")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        methods.lock().unwrap().push(method.clone());
                        let id = request
                            .get("id")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let result = match method.as_str() {
                            "lain/daemon_info" => serde_json::json!({
                                "pid": 999,
                                "version": version,
                                "build_commit": build_commit,
                                "index_build_id": index_build_id,
                                "workspaces": [],
                                "open_connections": 0,
                                "shared": true,
                            }),
                            "initialize" => serde_json::json!({
                                "protocolVersion": "2024-11-05",
                                "capabilities": {},
                                "serverInfo": {"name": "lain", "version": version},
                            }),
                            "tools/call" => serde_json::json!({
                                "content": [{"type": "text", "text": tool_text}],
                            }),
                            _ => serde_json::json!({}),
                        };
                        let response =
                            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                        if writeln!(writer, "{response}").is_err() {
                            break;
                        }
                        let _ = writer.flush();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("fake daemon accept failed: {e}"),
            }
        }
    })
}

/// A shared daemon from another build must not answer the call: the
/// oneshot fails with an actionable error naming the socket and both
/// identities, it never talks to the stale daemon beyond the identity
/// probe, and it starts no daemon of its own beside it.
#[cfg(unix)]
#[test]
fn oneshot_refuses_an_incompatible_shared_daemon() {
    let (_repo, root) = make_repo(3);
    let run = tempfile::Builder::new()
        .prefix("lr")
        .tempdir_in("/tmp")
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("lain")).unwrap();
    let socket = oneshot_socket_path(&root, run.path());
    let stop = Arc::new(AtomicBool::new(false));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let fake = spawn_fake_daemon(
        &socket,
        (lain::server::build_info::VERSION, "stale", "stale"),
        Arc::clone(&stop),
        Arc::clone(&methods),
    );

    let out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            root.to_str().unwrap(),
            "find_anchors",
        ])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_STATE_HOME", state.path())
        .env_remove("LAIN_ONESHOT_NO_SHARE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    fake.join().unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a daemon from another build must fail the call; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("incompatible shared daemon"),
        "the error must name the incompatibility:\n{stderr}"
    );
    assert!(
        stderr.contains("stale"),
        "the error must name the actual identity:\n{stderr}"
    );
    assert!(
        stderr.contains("LAIN_ONESHOT_NO_SHARE"),
        "the error must offer the explicit private fallback:\n{stderr}"
    );
    // The stale daemon was only PROBED, never used.
    let seen = methods.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec!["lain/daemon_info".to_string()],
        "the stale daemon must only be probed, never called: {seen:?}"
    );
    // And no competing daemon was started beside it.
    let sockets: Vec<std::path::PathBuf> = std::fs::read_dir(run.path().join("lain"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .collect();
    assert_eq!(
        sockets,
        vec![socket.clone()],
        "the client must not start a daemon beside the stale one"
    );
}

/// The matching daemon IS used: the call is served over the socket and
/// the fake's answer comes back on stdout.
#[cfg(unix)]
#[test]
fn oneshot_uses_a_matching_shared_daemon() {
    let (_repo, root) = make_repo(3);
    let run = tempfile::Builder::new()
        .prefix("lr")
        .tempdir_in("/tmp")
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("lain")).unwrap();
    let socket = oneshot_socket_path(&root, run.path());
    let stop = Arc::new(AtomicBool::new(false));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let fake = spawn_fake_daemon(
        &socket,
        (
            lain::server::build_info::VERSION,
            lain::server::build_info::GIT_SHA,
            lain::server::build_info::INDEX_BUILD_ID,
        ),
        Arc::clone(&stop),
        Arc::clone(&methods),
    );

    let out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            root.to_str().unwrap(),
            "find_anchors",
        ])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_STATE_HOME", state.path())
        .env_remove("LAIN_ONESHOT_NO_SHARE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    fake.join().unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a matching daemon must serve the call; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(FAKE_TOOL_TEXT),
        "the shared daemon's answer must reach stdout:\n{stdout}"
    );
    let seen = methods.lock().unwrap().clone();
    assert!(
        seen.contains(&"initialize".to_string()) && seen.contains(&"tools/call".to_string()),
        "the matching daemon must actually have served the call: {seen:?}"
    );
}

/// `LAIN_ONESHOT_NO_SHARE=1` is the explicit private fallback: the
/// client must not even look at a shared daemon (here one from another
/// build) and must serve the call privately.
#[cfg(unix)]
#[test]
fn oneshot_no_share_ignores_a_shared_daemon() {
    let (_repo, root) = make_repo(3);
    let run = tempfile::Builder::new()
        .prefix("lr")
        .tempdir_in("/tmp")
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(run.path().join("lain")).unwrap();
    let socket = oneshot_socket_path(&root, run.path());
    let stop = Arc::new(AtomicBool::new(false));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let fake = spawn_fake_daemon(
        &socket,
        (lain::server::build_info::VERSION, "stale", "stale"),
        Arc::clone(&stop),
        Arc::clone(&methods),
    );

    let out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "oneshot",
            "--workspace",
            root.to_str().unwrap(),
            "find_anchors",
        ])
        .env("XDG_RUNTIME_DIR", run.path())
        .env("XDG_STATE_HOME", state.path())
        .env("LAIN_ONESHOT_NO_SHARE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    fake.join().unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the private path must still serve the call; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let seen = methods.lock().unwrap().clone();
    assert!(
        seen.is_empty(),
        "a no-share client must not contact a shared daemon at all: {seen:?}"
    );
}
