//! Zero-config multi-agent shared daemon (Phase A/B).
//!
//! `lain mcp` on stdio no longer builds a private index per agent: it
//! attaches to (or starts) one resident daemon keyed by the resolved
//! workspace set and bridges stdin↔socket JSON-RPC to it. A late client
//! whose workspaces are NOT covered by a live daemon hands it the
//! missing paths via `add_workspaces`, which hot-adds them to the served
//! federation — the "give the running daemon your config" story.
//!
//! Three contracts are pinned here (all `#[cfg(unix)]`, like the
//! feature itself):
//!
//! 1. `two_clients_same_set_share_one_daemon` — concurrent clients with
//!    the same resolved set end on ONE socket and ONE daemon pid; the
//!    second client attaches instead of building a second index.
//! 2. `late_client_hands_config_and_it_is_added` — client A serves
//!    {ws1}; client B wants {ws1, ws2}; B attaches to A's daemon and
//!    adds ws2; afterwards tools/call over BOTH bridges sees ws2's
//!    content. The user story, end to end.
//! 3. `plain_mcp_is_zero_config` — bare `lain mcp` (walk-up resolution,
//!    no flags, no feature env) starts or joins a daemon and serves
//!    tools.
//!
//! 4–6. The no-share family (`share: false` / `--no-share` /
//!    `LAIN_MCP_NO_SHARE=1`): a no-share daemon is not discoverable
//!    and refuses the `add_workspaces` handoff (`not_shared`), and a
//!    no-share client serves privately even when a covering shared
//!    daemon exists. The DEFAULT stays the shared attach-or-start
//!    daemon — tests 1–3 above pin it unchanged.
//!
//! Isolation: every test pins `XDG_RUNTIME_DIR` / `HOME` / `XDG_*` to
//! its own tempdirs so sockets and state cannot collide with a real
//! user daemon or another test. `LAIN_SHARED_IDLE_SECS=30` keeps the
//! daemon alive through the assertions; a Drop guard reaps every daemon
//! pid recorded under the run dir (a still-running daemon corrupts
//! `cargo llvm-cov`'s profile merge).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Isolated XDG roots for one test: run dir (sockets), home (config /
/// state). Kept alive by the caller for the test's duration.
struct Isolation {
    _run: TempDir,
    home: TempDir,
    run: PathBuf,
}

impl Isolation {
    fn new() -> Self {
        // Short paths: `sun_path` is ~104 bytes and the socket lives at
        // `<run>/lain/lain-mcp-<16hex>.sock`.
        let run = tempfile::Builder::new()
            .prefix("lr")
            .tempdir_in("/tmp")
            .unwrap();
        let home = tempfile::Builder::new()
            .prefix("lh")
            .tempdir_in("/tmp")
            .unwrap();
        let run_path = run.path().to_path_buf();
        Isolation {
            run: run_path,
            home,
            _run: run,
        }
    }

    fn socket_dir(&self) -> PathBuf {
        self.run.join("lain")
    }

    /// Sockets currently present in the isolated run dir.
    fn sockets(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.socket_dir())
            .map(|it| {
                it.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "sock"))
                    .filter(|p| {
                        // Daemon MCP sockets are `lain-mcp-<16hex>.sock`;
                        // exclude reload-socket side files (`*.start`) etc.
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("lain-mcp-"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    fn pid_files(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.socket_dir())
            .map(|it| {
                it.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "pid"))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Kill every daemon this test started (its pid is recorded next to
    /// the socket). Coverage-tooling requirement: no live process may
    /// outlive the test binary.
    fn reap_daemons(&self) {
        fn alive(pid: &str) -> bool {
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        for pid_file in self.pid_files() {
            let Ok(pid) = std::fs::read_to_string(&pid_file) else {
                continue;
            };
            let pid = pid.trim().to_string();
            if pid.is_empty() || !alive(&pid) {
                continue;
            }
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
            for _ in 0..50 {
                if !alive(&pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// A `lain mcp` stdio client. Same shape as `tests/mcp_cold_start.rs`'s
/// `LainChild`, trimmed to what these tests need. Requests block until
/// their response arrives; server-initiated notifications are skipped.
struct McpClient {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl McpClient {
    /// Spawn `lain mcp` with `workspaces` (empty = walk-up resolution)
    /// under `iso`'s isolated XDG roots. `cwd` overrides the working
    /// directory (walk-up tests).
    fn spawn(iso: &Isolation, workspaces: &[&Path], cwd: Option<&Path>) -> Self {
        Self::spawn_opt(iso, workspaces, cwd, false, false)
    }

    /// Same, with `--no-share` (never attach; private instance).
    fn spawn_no_share_flag(iso: &Isolation, workspaces: &[&Path], cwd: Option<&Path>) -> Self {
        Self::spawn_opt(iso, workspaces, cwd, true, false)
    }

    /// Same, with `LAIN_MCP_NO_SHARE=1` (the hard override).
    fn spawn_no_share_env(iso: &Isolation, workspaces: &[&Path], cwd: Option<&Path>) -> Self {
        Self::spawn_opt(iso, workspaces, cwd, false, true)
    }

    fn spawn_opt(
        iso: &Isolation,
        workspaces: &[&Path],
        cwd: Option<&Path>,
        no_share_flag: bool,
        no_share_env: bool,
    ) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lain"));
        cmd.arg("mcp");
        for ws in workspaces {
            cmd.arg("--workspace").arg(ws);
        }
        if no_share_flag {
            cmd.arg("--no-share");
        }
        if no_share_env {
            cmd.env("LAIN_MCP_NO_SHARE", "1");
        }
        cmd.env("XDG_RUNTIME_DIR", &iso.run)
            .env("HOME", iso.home.path())
            .env("XDG_STATE_HOME", iso.home.path().join("state"))
            .env("XDG_CONFIG_HOME", iso.home.path().join("config"))
            // The daemon must not idle-exit mid-assertion; the reaper
            // cleans it up at the end instead.
            .env("LAIN_SHARED_IDLE_SECS", "30")
            .env_remove("LAIN_WORKSPACE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().expect("spawn lain mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        McpClient {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .expect("write request");
        self.stdin.flush().expect("flush request");
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("read response");
            assert!(!line.is_empty(), "empty reply for {method} (client died)");
            let value: serde_json::Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {e}: {line}"));
            // Skip server-initiated notifications (no `id`).
            if value.get("id").is_none() {
                continue;
            }
            return value;
        }
    }

    fn call_tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        )
    }

    /// Call `name` until `accept` sees a payload it likes — the
    /// readiness gate legitimately answers `warming_up` (or partial
    /// results with an indexing note) while the daemon indexes a
    /// freshly added repo, and a persistent MCP client is expected to
    /// poll exactly like this.
    fn call_until(
        &mut self,
        name: &str,
        args: serde_json::Value,
        accept: &dyn Fn(&serde_json::Value) -> bool,
        budget: Duration,
    ) -> serde_json::Value {
        let deadline = Instant::now() + budget;
        let mut last;
        loop {
            last = self.call_tool(name, args.clone());
            if accept(&last) {
                return last;
            }
            assert!(
                Instant::now() < deadline,
                "tools/call {name} never satisfied the predicate; last reply:\n{last}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn initialize(&mut self) -> serde_json::Value {
        let resp = self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "shared-daemon-test", "version": "0"}
            }),
        );
        resp
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A git workspace with one uniquely named symbol (`<marker>_marker`) so
/// tools can prove which repo's content they can see.
fn make_repo(marker: &str) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub fn {marker}_marker(x: u32) -> u32 {{ x + 1 }}\n"),
    )
    .unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
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

/// Wait until at least one daemon socket is present in the run dir and
/// its `.pid` file is written (bind time). Bounded.
fn wait_for_daemon(iso: &Isolation, budget: Duration) -> PathBuf {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(sock) = iso.sockets().into_iter().next() {
            let pid = PathBuf::from(format!("{}.pid", sock.display()));
            if pid.exists() {
                return sock;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no daemon socket appeared in {:?}",
            iso.socket_dir()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ask the daemon behind `socket` who it is: `lain/daemon_info` (the
/// discovery method the attach-or-start path uses). Raw JSON-RPC on a
/// short-lived connection.
fn daemon_info(socket: &Path) -> serde_json::Value {
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(socket).expect("connect to daemon socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"lain/daemon_info\"}\n")
        .unwrap();
    writer.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).expect("daemon_info reply");
    serde_json::from_str(line.trim()).expect("daemon_info JSON")
}

fn tool_text(resp: &serde_json::Value) -> String {
    resp.pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Accept a tools/call reply once it carries `needle` in its text and
/// is not a gate `warming_up` envelope.
fn contains(needle: &str) -> impl Fn(&serde_json::Value) -> bool {
    let needle = needle.to_string();
    move |resp: &serde_json::Value| {
        let text = tool_text(resp);
        let warming = text.contains("warming_up") || text.contains("still indexing");
        text.contains(&needle) && !warming
    }
}

// ---------------------------------------------------------------------------
// 1. two_clients_same_set_share_one_daemon
// ---------------------------------------------------------------------------

/// Two stdio clients with the same resolved workspace set must share
/// exactly one daemon: one socket file, one recorded pid, two open
/// bridges (`open_connections >= 3` while both clients are alive: two
/// bridges plus the test's own probe). If
/// the second client had built a private server, its calls would not
/// pin the first daemon's connection counter — this is the
/// "second client attaches, no second index build" assertion.
#[test]
fn two_clients_same_set_share_one_daemon() {
    let iso = Isolation::new();
    let (_ws_tmp, ws) = make_repo("alpha");
    let ws = ws.canonicalize().unwrap();

    let mut a = McpClient::spawn(&iso, &[&ws], None);
    let init_a = a.initialize();
    assert_eq!(
        init_a.pointer("/result/serverInfo/name"),
        Some(&serde_json::json!("lain")),
        "serverInfo must be preserved: {init_a}"
    );
    let tools_a = a.request("tools/list", serde_json::json!({}));
    assert!(
        tools_a
            .pointer("/result/tools")
            .and_then(|t| t.as_array())
            .is_some_and(|t| !t.is_empty()),
        "client A must see tools: {tools_a}"
    );

    let sock = wait_for_daemon(&iso, Duration::from_secs(60));

    let mut b = McpClient::spawn(&iso, &[&ws], None);
    let init_b = b.initialize();
    assert_eq!(
        init_b.pointer("/result/serverInfo/name"),
        Some(&serde_json::json!("lain")),
        "serverInfo must be preserved on the second bridge: {init_b}"
    );
    let tools_b = b.request("tools/list", serde_json::json!({}));
    assert!(
        tools_b
            .pointer("/result/tools")
            .and_then(|t| t.as_array())
            .is_some_and(|t| !t.is_empty()),
        "client B must see tools: {tools_b}"
    );

    // Exactly one daemon: one socket, one pid file, one live pid.
    let sockets = iso.sockets();
    assert_eq!(
        sockets.len(),
        1,
        "same workspace set must map to one daemon socket; found {sockets:?}"
    );
    assert_eq!(sockets[0], sock);
    let pid_files = iso.pid_files();
    assert_eq!(
        pid_files.len(),
        1,
        "one daemon pid file expected; found {pid_files:?}"
    );

    // Both bridges are open on that daemon. The probe connection counts
    // itself, so A + B + probe = 3: the old `>= 2` bound also held with
    // client B privately serving (A's bridge + this probe alone).
    let info = daemon_info(&sock);
    let open = info
        .pointer("/result/open_connections")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        open >= 3,
        "both bridges must be attached to the one daemon \
         (A + B + this probe = 3; open_connections={open}); info: {info}"
    );
    let served = info
        .pointer("/result/workspaces")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        serde_json::Value::Array(served),
        serde_json::json!([ws.display().to_string()]),
        "the daemon must report the resolved workspace set"
    );

    // And both bridges really serve queries (through the daemon).
    let ra = a.call_until(
        "find_anchors",
        serde_json::json!({}),
        &contains("anchor"),
        Duration::from_secs(90),
    );
    let rb = b.call_until(
        "find_anchors",
        serde_json::json!({}),
        &contains("anchor"),
        Duration::from_secs(90),
    );

    // Sharing, from the inside: A's OWN bridge asks the daemon for its
    // connection count and must SEE B's bridge on the same daemon.
    // This path has no probe connection — A's bridge IS the connection
    // — so the count is A + B = 2, and would be 1 if B were privately
    // serving. (Identical tool text alone would not prove sharing: a
    // private server answers the same for the same workspace.)
    let via_a = a.request("lain/daemon_info", serde_json::json!({}));
    let open_via_a = via_a
        .pointer("/result/open_connections")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        open_via_a >= 2,
        "A's bridge must observe B's bridge on the same daemon \
         (open_connections={open_via_a}); reply: {via_a}"
    );
    assert_eq!(
        tool_text(&ra),
        tool_text(&rb),
        "both bridges must see the same daemon state"
    );

    iso.reap_daemons();
}

// ---------------------------------------------------------------------------
// 2. late_client_hands_config_and_it_is_added
// ---------------------------------------------------------------------------

/// The user story: client A serves {ws1}; client B wants {ws1, ws2}. B
/// must detect the live daemon, hand it ws2 (`add_workspaces`), and
/// bridge to it — after which a tools/call over BOTH bridges sees ws2's
/// content.
#[test]
fn late_client_hands_config_and_it_is_added() {
    let iso = Isolation::new();
    let parent = tempfile::tempdir().unwrap();
    // Fixed basenames → stable federation repo ids (`ws1`, `ws2`).
    let (_t1, ws1) = make_repo_in(&parent, "ws1", "alphauniq");
    let (_t2, ws2) = make_repo_in(&parent, "ws2", "megaomega");

    let mut a = McpClient::spawn(&iso, &[&ws1], None);
    a.initialize();
    let sock = wait_for_daemon(&iso, Duration::from_secs(60));

    // Late client: wants the UNION. Its bridge setup attaches to the
    // live daemon and hands it the missing workspace.
    let mut b = McpClient::spawn(&iso, &[&ws1, &ws2], None);
    b.initialize();

    // Still one daemon — B added to A's, it did not spawn a second one.
    let sockets = iso.sockets();
    assert_eq!(
        sockets.len(),
        1,
        "the late client must join the live daemon, not spawn another; sockets: {sockets:?}"
    );
    assert_eq!(sockets[0], sock);

    // Both bridges see ws2's repo and its symbol.
    for (label, client) in [("A", &mut a), ("B", &mut b)] {
        let repos = client.call_until(
            "list_repos",
            serde_json::json!({}),
            &contains("ws2"),
            Duration::from_secs(90),
        );
        assert!(
            tool_text(&repos).contains("ws1") && tool_text(&repos).contains("ws2"),
            "client {label} must see both repos; got:\n{}",
            tool_text(&repos)
        );
        let hit = client.call_until(
            "search_org",
            serde_json::json!({"query": "megaomega_marker"}),
            &contains("megaomega_marker"),
            Duration::from_secs(120),
        );
        assert!(
            tool_text(&hit).contains("megaomega_marker"),
            "client {label} must see ws2's content after the hot-add; got:\n{}",
            tool_text(&hit)
        );
    }

    // The daemon now serves the union.
    let info = daemon_info(&sock);
    let served: Vec<String> = info
        .pointer("/result/workspaces")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        served.contains(&ws1.canonicalize().unwrap().display().to_string())
            && served.contains(&ws2.canonicalize().unwrap().display().to_string()),
        "daemon must report both workspaces after add_workspaces; got {served:?}"
    );

    iso.reap_daemons();
}

/// `make_repo` with a fixed basename under `parent` so federation repo
/// ids are deterministic (`ws1`, `ws2`).
fn make_repo_in(parent: &TempDir, name: &str, marker: &str) -> (PathBuf, PathBuf) {
    let root = parent.path().join(name);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub fn {marker}_marker(x: u32) -> u32 {{ x + 1 }}\n"),
    )
    .unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
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
    (root.clone(), root)
}

// ---------------------------------------------------------------------------
// 3. plain_mcp_is_zero_config
// ---------------------------------------------------------------------------

/// Bare `lain mcp` — no flags, no `LAIN_WORKSPACE`, resolved by walking
/// up from cwd — must start (or join) a shared daemon and serve tools.
#[test]
fn plain_mcp_is_zero_config() {
    let iso = Isolation::new();
    let (_ws_tmp, ws) = make_repo("plain");
    // Walk-up target: run from a nested directory.
    let nested = ws.join("src");
    assert!(nested.is_dir());

    let mut client = McpClient::spawn(&iso, &[], Some(&nested));
    let init = client.initialize();
    assert_eq!(
        init.pointer("/result/serverInfo/name"),
        Some(&serde_json::json!("lain")),
        "bare lain mcp must complete the MCP handshake: {init}"
    );
    let tools = client.request("tools/list", serde_json::json!({}));
    let names: Vec<String> = tools
        .pointer("/result/tools")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.is_empty(),
        "bare lain mcp must advertise tools: {tools}"
    );

    // Zero-config means shared: a resident daemon is behind the tools.
    wait_for_daemon(&iso, Duration::from_secs(60));

    // And it answers a real query.
    let hit = client.call_until(
        "find_anchors",
        serde_json::json!({}),
        &contains("anchor"),
        Duration::from_secs(90),
    );
    assert!(
        !tool_text(&hit).is_empty(),
        "find_anchors must answer through the zero-config path"
    );

    iso.reap_daemons();
}

// ---------------------------------------------------------------------------
// no-share: configurable per-instance isolation (share: false / --no-share /
// LAIN_MCP_NO_SHARE=1). The DEFAULT stays the shared ephemeral daemon.
// ---------------------------------------------------------------------------

/// Spawn `lain mcp --daemon` on `socket` under `iso`'s isolated XDG
/// roots (the test holds the `Child` and the reaper kills by pid file).
/// `no_share` adds `--no-share` so the spawned daemon inherits the
/// property.
fn spawn_daemon(iso: &Isolation, workspaces: &[&Path], socket: &Path, no_share: bool) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lain"));
    cmd.arg("mcp");
    for ws in workspaces {
        cmd.arg("--workspace").arg(ws);
    }
    cmd.arg("--socket").arg(socket).arg("--daemon");
    if no_share {
        cmd.arg("--no-share");
    }
    cmd.env("XDG_RUNTIME_DIR", &iso.run)
        .env("HOME", iso.home.path())
        .env("XDG_STATE_HOME", iso.home.path().join("state"))
        .env("XDG_CONFIG_HOME", iso.home.path().join("config"))
        .env("LAIN_SHARED_IDLE_SECS", "30")
        .env_remove("LAIN_WORKSPACE")
        .env_remove("LAIN_MCP_NO_SHARE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().expect("spawn lain mcp --daemon")
}

/// Wait until `sock` answers `lain/daemon_info` (i.e. the daemon is
/// listening). Bounded.
fn wait_listening(sock: &Path, budget: Duration) -> serde_json::Value {
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(sock) {
            drop(stream);
            return daemon_info(sock);
        }
        assert!(
            Instant::now() < deadline,
            "daemon at {} never started listening",
            sock.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One raw `tools/call add_workspaces` round trip against the daemon
/// behind `socket` (initialize first, like any MCP client).
fn raw_add_workspaces(socket: &Path, paths: &[&Path]) -> serde_json::Value {
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(socket).expect("connect to daemon socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let send = |writer: &mut std::os::unix::net::UnixStream, line: &str| {
        writer.write_all(line.as_bytes()).unwrap();
        writer.flush().unwrap();
    };
    send(
        &mut writer,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"no-share-test\",\"version\":\"0\"}}}\n",
    );
    let mut line = String::new();
    reader.read_line(&mut line).expect("initialize reply");
    let path_strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "add_workspaces", "arguments": {"paths": path_strings}},
    });
    send(&mut writer, &format!("{call}\n"));
    line.clear();
    reader.read_line(&mut line).expect("add_workspaces reply");
    serde_json::from_str(line.trim()).expect("add_workspaces JSON")
}

fn served_set(info: &serde_json::Value) -> Vec<String> {
    info.pointer("/result/workspaces")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn open_connections(info: &serde_json::Value) -> u64 {
    info.pointer("/result/open_connections")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 4. no_share_daemon_is_not_discoverable
// ---------------------------------------------------------------------------

/// A daemon marked no-share is invisible to discovery: a normal client
/// whose set it would cover (or partially cover) must NOT attach to it
/// and must NOT hand it `add_workspaces` — it starts its own daemon
/// (distinct socket, distinct pid) and the no-share daemon's served
/// set stays exactly what it booted with.
#[test]
fn no_share_daemon_is_not_discoverable() {
    let iso = Isolation::new();
    let parent = tempfile::tempdir().unwrap();
    let (_t1, ws1) = make_repo_in(&parent, "ws1", "alphauniq");
    let (_t2, ws2) = make_repo_in(&parent, "ws2", "megaomega");

    // A's daemon: no-share, serving {ws1}, on a candidate-shaped
    // socket (`lain-mcp-<16hex>.sock`) so discovery WOULD see it.
    let sock_a = iso.socket_dir().join("lain-mcp-a1b2c3d4e5f60718.sock");
    let mut daemon_a = spawn_daemon(&iso, &[&ws1], &sock_a, true);
    let info_a = wait_listening(&sock_a, Duration::from_secs(60));
    assert_eq!(
        info_a.pointer("/result/shared"),
        Some(&serde_json::json!(false)),
        "a --no-share daemon must report shared: false: {info_a}"
    );
    let pid_a = info_a.pointer("/result/pid").and_then(|v| v.as_u64());

    // B: a NORMAL client wanting {ws1, ws2} — without the feature this
    // is the partial flow: B hands A's daemon ws2 and attaches to it.
    let mut b = McpClient::spawn(&iso, &[&ws1, &ws2], None);
    b.initialize();
    b.call_until(
        "list_repos",
        serde_json::json!({}),
        &contains("ws2"),
        Duration::from_secs(120),
    );

    // The no-share daemon was never attached to and never grew.
    let info_a2 = daemon_info(&sock_a);
    assert_eq!(
        info_a2.pointer("/result/shared"),
        Some(&serde_json::json!(false)),
        "the flag must be stable: {info_a2}"
    );
    assert_eq!(
        served_set(&info_a2),
        vec![ws1.canonicalize().unwrap().display().to_string()],
        "no-share daemon must not grow via the handoff; got {:?}",
        served_set(&info_a2)
    );
    assert_eq!(
        open_connections(&info_a2),
        1,
        "only this probe may be connected to the no-share daemon \
         (B's bridge would be a second connection): {info_a2}"
    );

    // B got its OWN daemon: two sockets, two live pids.
    let sockets = iso.sockets();
    assert_eq!(
        sockets.len(),
        2,
        "B must spawn its own daemon beside A's; sockets: {sockets:?}"
    );
    let sock_b = sockets
        .iter()
        .find(|p| p.as_path() != sock_a.as_path())
        .expect("B's socket");
    let info_b = daemon_info(sock_b);
    assert_eq!(
        info_b.pointer("/result/shared"),
        Some(&serde_json::json!(true)),
        "a normal daemon keeps the default shared: true: {info_b}"
    );
    let served_b = served_set(&info_b);
    assert!(
        served_b.contains(&ws1.canonicalize().unwrap().display().to_string())
            && served_b.contains(&ws2.canonicalize().unwrap().display().to_string()),
        "B's daemon must serve the union; got {served_b:?}"
    );
    assert_ne!(
        info_b.pointer("/result/pid").and_then(|v| v.as_u64()),
        pid_a,
        "A's and B's daemons must be distinct processes"
    );

    iso.reap_daemons();
    let _ = daemon_a.kill();
    let _ = daemon_a.wait();
}

// ---------------------------------------------------------------------------
// 5. no_share_daemon_refuses_add_workspaces
// ---------------------------------------------------------------------------

/// `add_workspaces` against a no-share daemon fails with the typed
/// `not_shared` error BEFORE any validation or mutation — the served
/// set is unchanged. And the property is pinned at boot: a config read
/// error after startup does not re-enable the handoff.
#[test]
fn no_share_daemon_refuses_add_workspaces() {
    let iso = Isolation::new();
    let parent = tempfile::tempdir().unwrap();
    let (_t1, ws1) = make_repo_in(&parent, "ws1", "alphauniq");
    let (_t2, ws2) = make_repo_in(&parent, "ws2", "megaomega");

    let sock = iso.socket_dir().join("lain-mcp-b2c3d4e5f6071829.sock");
    let mut daemon = spawn_daemon(&iso, &[&ws1], &sock, true);
    wait_listening(&sock, Duration::from_secs(60));

    let resp = raw_add_workspaces(&sock, &[&ws2]);
    assert_eq!(
        resp.pointer("/result/isError"),
        Some(&serde_json::json!(true)),
        "add_workspaces must be refused: {resp}"
    );
    let text = tool_text(&resp);
    let payload: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("typed error envelope expected: {e}: {text}"));
    assert_eq!(
        payload.pointer("/error"),
        Some(&serde_json::json!("not_shared")),
        "typed error code must be `not_shared`: {text}"
    );
    assert_eq!(
        payload.pointer("/ok"),
        Some(&serde_json::json!(false)),
        "refusal envelope: {text}"
    );

    // Nothing mutated: the served set is still exactly {ws1}.
    let info = daemon_info(&sock);
    assert_eq!(
        served_set(&info),
        vec![ws1.canonicalize().unwrap().display().to_string()],
        "a refused handoff must leave the served set untouched; got {:?}",
        served_set(&info)
    );

    // The property is pinned at boot: corrupting the daemon's
    // synthesized config after startup must not flip it back to the
    // shared default (which would make it discoverable again).
    let stem = sock.file_stem().unwrap().to_string_lossy().into_owned();
    let config = iso.socket_dir().join(format!("lain-daemon-{stem}.yaml"));
    assert!(config.exists(), "daemon config at {}", config.display());
    std::fs::write(&config, "share: \"yes\"\nnot: valid: [yaml").unwrap();
    let info = daemon_info(&sock);
    assert_eq!(
        info.pointer("/result/shared"),
        Some(&serde_json::json!(false)),
        "the boot-pinned share must survive a later config read error: {info}"
    );
    let resp = raw_add_workspaces(&sock, &[&ws2]);
    assert_eq!(
        resp.pointer("/result/isError"),
        Some(&serde_json::json!(true)),
        "a read error must not re-enable the handoff: {resp}"
    );
    assert!(
        tool_text(&resp).contains("not_shared"),
        "still not_shared after the config went bad: {}",
        tool_text(&resp)
    );

    iso.reap_daemons();
    let _ = daemon.kill();
    let _ = daemon.wait();
}

// ---------------------------------------------------------------------------
// 6. no_share_client_serves_privately
// ---------------------------------------------------------------------------

/// A no-share client (`--no-share` or `LAIN_MCP_NO_SHARE=1`) never
/// attaches — even to a covering shared daemon. It serves privately
/// (the historical fallback path): its own tools answer, and the shared
/// daemon's connection count proves no bridge was built.
#[test]
fn no_share_client_serves_privately() {
    let iso = Isolation::new();
    let (_ws_tmp, ws) = make_repo("iso");
    let ws = ws.canonicalize().unwrap();

    // The covering shared daemon (default surface), held open by A.
    let mut a = McpClient::spawn(&iso, &[&ws], None);
    a.initialize();
    let sock = wait_for_daemon(&iso, Duration::from_secs(60));

    for (label, mut b) in [
        (
            "--no-share",
            McpClient::spawn_no_share_flag(&iso, &[&ws], None),
        ),
        (
            "LAIN_MCP_NO_SHARE=1",
            McpClient::spawn_no_share_env(&iso, &[&ws], None),
        ),
    ] {
        let init = b.initialize();
        assert_eq!(
            init.pointer("/result/serverInfo/name"),
            Some(&serde_json::json!("lain")),
            "no-share client ({label}) must serve its own handshake: {init}"
        );
        let tools = b.request("tools/list", serde_json::json!({}));
        assert!(
            tools
                .pointer("/result/tools")
                .and_then(|t| t.as_array())
                .is_some_and(|t| !t.is_empty()),
            "no-share client ({label}) must advertise tools from its private server: {tools}"
        );
        let hit = b.call_until(
            "find_anchors",
            serde_json::json!({}),
            &contains("anchor"),
            Duration::from_secs(90),
        );
        assert!(
            !tool_text(&hit).is_empty(),
            "no-share client ({label}) must answer queries from its private server"
        );

        // No bridge to the shared daemon: with only A's bridge and this
        // probe, open_connections is 2 — a bridged B (or C) would make 3.
        let info = daemon_info(&sock);
        assert_eq!(
            open_connections(&info),
            2,
            "no-share client ({label}) must not bridge to the covering shared daemon: {info}"
        );
        // And it spawned no daemon of its own (private = in-process fallback).
        assert_eq!(
            iso.sockets().len(),
            1,
            "no-share client ({label}) must serve in-process, not spawn a daemon"
        );
    }

    iso.reap_daemons();
}
