//! D-M2 / AGENT_UX_ROADMAP Milestone 4: `lain mcp` must answer the MCP
//! handshake immediately — never behind the startup re-index — and must
//! eventually serve a populated graph once indexing completes, bounded
//! by `LAIN_REINDEX_TIMEOUT`.
//!
//! This file's contract inverted with Milestone 4's central readiness
//! gate. It originally pinned the *opposite* behavior: the startup
//! re-index was awaited before the stdio loop came up, so a
//! `find_anchors` call fired immediately after `initialize` was
//! guaranteed to see a fully populated graph, never a "warming up"
//! state. That was itself the fix for an earlier bug (the re-index
//! raced a bare `tokio::spawn` and the first call read an empty graph).
//!
//! Milestone 4 backgrounds the re-index on purpose, so the protocol
//! handshake is never gated on repository size — and now the central
//! gate in `dispatch_tool_call` can legitimately answer that same
//! immediate `find_anchors` call with a structured `warming_up` result
//! instead of a populated (or empty) one. `find_anchors_works_after_a_cold_start`
//! below pins the new contract: an immediate call may be `warming_up`,
//! but the *same* call must return the real, populated answer once
//! `get_capabilities` reports `ready` — polled no faster than the
//! envelope's own `retry_after_ms`, never a fixed sleep.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Locate the `lain` binary to drive. Mirror `tests/e2e/lain_test.py:27-38`:
/// `LAIN_BIN` env first, then `target/{release,debug}/lain` next to the
/// repo root. Skip cleanly when nothing is built (developer machines
/// without `cargo build` artifacts shouldn't see a hard failure).
fn lain_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LAIN_BIN") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for sub in ["target/release/lain", "target/debug/lain"] {
        let candidate = repo_root.join(sub);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn protocol_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--print-mcp-protocol-version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Build a one-file Rust fixture with an orchestrator function so
/// `find_anchors` has at least one anchor to rank. Commit it so the
/// indexer picks the file up.
///
/// `anchor_score` is `calls_in * log2(1 + calls_out) * size_factor`.
/// A bare orchestrator that calls helpers but has no callers scores 0
/// (calls_in = 0) — same as the helpers it calls (calls_out = 0). With
/// all four fixtures tied at 0, the relative order comes from the
/// graph's node iteration, which is not stable across reindexes. By
/// giving `orchestrate` a caller (`entrypoint`), it ranks #1 with a
/// non-zero score while the leaves stay at 0, so the second assertion
/// has a deterministic anchor to look for.
fn build_fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();

    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cold_start_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "/// Anchor: called by `entrypoint`, coordinates three helpers.\n\
         pub fn orchestrate() -> u32 {\n\
         \x20   let a = helper_a(1);\n\
         \x20   let b = helper_b(2);\n\
         \x20   let c = helper_c(3);\n\
         \x20   a + b + c\n\
         }\n\
         pub fn entrypoint() -> u32 { orchestrate() }\n\
         pub fn helper_a(x: u32) -> u32 { x + 1 }\n\
         pub fn helper_b(x: u32) -> u32 { x + 2 }\n\
         pub fn helper_c(x: u32) -> u32 { x + 3 }\n",
    )
    .unwrap();

    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init");
    assert!(init.success(), "git init failed");
    for (k, v) in [
        ("user.email", "cold-start-test@lain"),
        ("user.name", "cold-start-test"),
    ] {
        Command::new("git")
            .args(["config", k, v])
            .current_dir(root)
            .status()
            .unwrap();
    }
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = Command::new("git")
        .args(["commit", "-q", "-m", "fixture"])
        .current_dir(root)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    dir
}

/// Spawn `lain mcp` over stdio. Returns the Child handle plus the
/// stdin handle and a line-buffered stdout reader.
struct LainChild {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    /// Server-initiated notifications (no `id` field) seen on stdout
    /// while waiting for a request/response pair in `send`. A pipe
    /// preserves write order, so any notification the server sent before
    /// the next response is guaranteed to surface here rather than being
    /// silently skipped or mistaken for that response.
    notifications: Vec<serde_json::Value>,
}

impl LainChild {
    fn spawn(bin: &Path, workspace: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(bin);
        // `mcp` is a clap subcommand; `--workspace` belongs *under* it.
        // Passing `--workspace` before `mcp` makes clap reject the argv
        // and the child exits before any stdio handshake, masking the
        // cold-start race we want to pin.
        cmd.arg("mcp")
            .args(["--workspace"])
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("LAIN_REINDEX_TIMEOUT");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn lain mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        LainChild {
            child,
            stdin,
            stdout,
            notifications: Vec::new(),
        }
    }

    fn send(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{}", msg).expect("write stdin");
        self.stdin.flush().expect("flush");
        // Milestone 4 added a server-initiated notification
        // (`notifications/lain/capabilities_changed`) that can land on
        // stdout at any time, not just as a reply to something this test
        // sent. A notification has no `id`; keep reading past any until
        // the actual response to *this* request arrives, instead of
        // risking every existing poll loop here misreading a
        // notification as the response it was waiting for.
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("read stdout");
            let value: serde_json::Value =
                serde_json::from_str(&line).expect("parse jsonrpc message");
            if value.get("id").is_none() {
                self.notifications.push(value);
                continue;
            }
            return value;
        }
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.send(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        )
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        // Closing stdin does not reliably make the stdio transport exit on
        // its own — observed leaking `lain mcp` processes across repeated
        // local test runs (each one holding its own LSP pool / model)
        // until this fixture's tempdir-backed workspace was cleaned up by
        // something else entirely. Explicitly kill it once the grace
        // period elapses instead of leaving an orphaned process behind.
        if self
            .child
            .wait_timeout(Duration::from_secs(5))
            .ok()
            .flatten()
            .is_none()
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Helper to shut a Child down with a timeout. Not in std today.
trait ChildWaitTimeout {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}
impl ChildWaitTimeout for std::process::Child {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = Instant::now();
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if start.elapsed() >= dur {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

/// Milestone 4 main test. Boots `lain mcp` against the fixture and
/// proves two things:
///
/// 1. `initialize` never waits on the startup re-index — it must return
///    well within the roadmap's 2-second budget regardless of repository
///    size, because the re-index now runs on a background task instead
///    of gating the protocol handshake.
/// 2. An immediate `find_anchors` call is allowed to come back as a
///    structured `warming_up` result (the central gate in
///    `dispatch_tool_call` denying a `graph_required` tool before the
///    index is ready), but the *same* call must return the real,
///    populated answer once `get_capabilities` reports the structural
///    capability `ready` — polled by respecting the envelope's own
///    `retry_after_ms`, never a fixed sleep.
#[test]
fn find_anchors_works_after_a_cold_start() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary (set LAIN_BIN or run `cargo build`)");
        return;
    };
    let Some(version) = protocol_version(&bin) else {
        eprintln!("skipping: could not determine MCP protocol version");
        return;
    };
    let fixture = build_fixture();

    let mut child = LainChild::spawn(&bin, fixture.path(), &[]);
    let init_params = serde_json::json!({
        "protocolVersion": version,
        "capabilities": {},
        "clientInfo": {"name": "cold-start-test", "version": "1"},
    });

    let handshake_budget = Duration::from_secs(2);
    let handshake_start = Instant::now();
    let init_resp = child.send("initialize", init_params.clone());
    let initialize_elapsed = handshake_start.elapsed();
    assert!(
        init_resp.get("result").is_some(),
        "initialize must succeed: {init_resp}"
    );
    assert!(
        initialize_elapsed < handshake_budget,
        "initialize must never wait on the startup re-index — took {initialize_elapsed:?}, \
         budget is {handshake_budget:?}"
    );

    let list_start = Instant::now();
    let list_resp = child.send("tools/list", serde_json::json!({}));
    assert!(
        list_resp.get("result").is_some(),
        "tools/list must succeed: {list_resp}"
    );
    assert!(
        list_start.elapsed() < handshake_budget,
        "tools/list must never wait on the startup re-index — took {:?}",
        list_start.elapsed()
    );

    // The first `find_anchors` call may legitimately be denied with a
    // structured `warming_up` result now — that is the point of the
    // central gate. Poll `get_capabilities` (itself `graph_independent`,
    // so always answered immediately) until the structural capability
    // is `ready`, sleeping only by the amount the envelope itself names.
    let index_budget = Duration::from_secs(30);
    let poll_start = Instant::now();
    loop {
        let caps = child.call_tool("get_capabilities", serde_json::json!({}));
        let text = caps
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let state = value
            .pointer("/indexing/state")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_ne!(
            state, "unavailable_error",
            "indexing failed while polling get_capabilities: {value}"
        );
        if state == "ready" {
            break;
        }
        assert!(
            poll_start.elapsed() < index_budget,
            "indexing never reached ready within {index_budget:?}; last capabilities: {value}"
        );
        let retry_after_ms = value
            .pointer("/indexing/retry_after_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(200);
        std::thread::sleep(Duration::from_millis(retry_after_ms.min(500)));
    }

    let resp = child.call_tool("find_anchors", serde_json::json!({"limit": 5}));
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    assert!(
        !text.is_empty() && !text.contains("No anchors"),
        "find_anchors returned empty once indexing reported ready. \
         Response: {text:?}\nFull JSON: {resp}"
    );
    // The fixture's orchestrator must surface — the test is meaningful
    // only if we know the graph had something to find.
    assert!(
        text.contains("orchestrate"),
        "the fixture's `orchestrate` function must appear in the anchor list: {text}"
    );

    child.shutdown();
}

/// Milestone 4 (roadmap step 9 / issue 13): once the background re-index
/// reaches a terminal state, the server pushes one
/// `notifications/lain/capabilities_changed` notification, unprompted,
/// carrying the same payload `get_capabilities` would return. Polling
/// remains the canonical fallback for a client that doesn't support it —
/// this test just proves the push side actually fires over stdio, not
/// that clients must rely on it.
#[test]
fn capabilities_changed_notification_fires_on_startup_completion() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary (set LAIN_BIN or run `cargo build`)");
        return;
    };
    let Some(version) = protocol_version(&bin) else {
        eprintln!("skipping: could not determine MCP protocol version");
        return;
    };
    let fixture = build_fixture();

    let mut child = LainChild::spawn(&bin, fixture.path(), &[]);
    let init_params = serde_json::json!({
        "protocolVersion": version,
        "capabilities": {},
        "clientInfo": {"name": "cold-start-test-notify", "version": "1"},
    });
    let init_resp = child.send("initialize", init_params);
    assert!(
        init_resp.get("result").is_some(),
        "initialize must succeed: {init_resp}"
    );

    // Poll until indexing reaches `ready` (or fails the test on timeout).
    // Every `send`/`call_tool` call in this loop transparently drains any
    // notification lines into `child.notifications` as a side effect —
    // see `LainChild::send`.
    let budget = Duration::from_secs(30);
    let poll_start = Instant::now();
    loop {
        let caps = child.call_tool("get_capabilities", serde_json::json!({}));
        let text = caps
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let state = value
            .pointer("/indexing/state")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if state == "ready" || state == "unavailable_error" {
            break;
        }
        assert!(
            poll_start.elapsed() < budget,
            "indexing never reached a terminal state within {budget:?}; last capabilities: {value}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let notification = child.notifications.iter().find(|n| {
        n.get("method").and_then(|m| m.as_str()) == Some("notifications/lain/capabilities_changed")
    });
    assert!(
        notification.is_some(),
        "expected a notifications/lain/capabilities_changed notification on stdout \
         after startup completed; saw these notifications instead: {:?}",
        child.notifications
    );
    let params = notification
        .unwrap()
        .get("params")
        .expect("notification must carry params");
    assert!(
        params.get("schema_version").is_some() && params.get("capabilities").is_some(),
        "notification params must match the get_capabilities payload shape: {params}"
    );

    child.shutdown();
}

/// D-M2 second contract: when the re-index budget elapses, the server
/// still comes up — degraded, but alive — and `get_health` reports the
/// timeout once the background indexing task actually reaches it. This
/// pins the `RefreshResult::Timeout` branch of `await_startup_reindex`,
/// now running on its own task instead of being awaited before the stdio
/// loop starts.
///
/// We force the timeout with `LAIN_REINDEX_TIMEOUT=1` (one second).
/// Since indexing is backgrounded, `get_health` immediately after
/// `initialize` would just see the untouched `Skipped` default and
/// trivially report "Operational" without having exercised anything —
/// so this polls `get_capabilities` until the structural capability
/// reaches a terminal state (`ready` or `unavailable_error`) before
/// checking `get_health`. On a slow CI runner the fixture's first index
/// will exceed the 1s budget, which is what we want; on a fast machine
/// the index may finish first — the contract is "the server must come
/// up AND answer queries", which holds either way.
#[test]
fn startup_degrades_when_reindex_times_out() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary (set LAIN_BIN or run `cargo build`)");
        return;
    };
    let Some(version) = protocol_version(&bin) else {
        eprintln!("skipping: could not determine MCP protocol version");
        return;
    };
    let fixture = build_fixture();

    let mut child = LainChild::spawn(&bin, fixture.path(), &[("LAIN_REINDEX_TIMEOUT", "1")]);

    let init_params = serde_json::json!({
        "protocolVersion": version,
        "capabilities": {},
        "clientInfo": {"name": "cold-start-test-timeout", "version": "1"},
    });
    let init_resp = child.send("initialize", init_params);
    assert!(
        init_resp.get("result").is_some(),
        "initialize must succeed even when re-index times out: {init_resp}"
    );

    // Wait for the background attempt to reach a terminal state (bounded
    // well past the 1s budget so a slow CI runner still converges).
    let terminal_budget = Duration::from_secs(15);
    let poll_start = Instant::now();
    loop {
        let caps = child.call_tool("get_capabilities", serde_json::json!({}));
        let text = caps
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let state = value
            .pointer("/indexing/state")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if state == "ready" || state == "unavailable_error" {
            break;
        }
        assert!(
            poll_start.elapsed() < terminal_budget,
            "indexing never reached a terminal state within {terminal_budget:?} \
             (LAIN_REINDEX_TIMEOUT=1 should have forced one well before this); \
             last capabilities: {value}"
        );
        let retry_after_ms = value
            .pointer("/indexing/retry_after_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(200);
        std::thread::sleep(Duration::from_millis(retry_after_ms.min(500)));
    }

    // `get_health` — must report either:
    //   (a) `Degraded ⚠ ... timed out`  — the budget elapsed, OR
    //   (b) `Operational ✅`             — the index finished under 1s.
    // In both cases the server is alive and the background attempt ran
    // to completion (one way or the other).
    let health = child.call_tool("get_health", serde_json::json!({}));
    let health_text = health
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let timed_out = health_text.contains("timed out");
    let operational = health_text.contains("Operational");

    assert!(
        timed_out || operational,
        "get_health must report either a timeout or Operational after a \
         cold start with LAIN_REINDEX_TIMEOUT=1: {health_text}"
    );

    // Either way, the served graph must answer with a normal result
    // envelope — `warming_up` and populated are both fine here, an
    // MCP-level error is not.
    let anchors = child.call_tool("find_anchors", serde_json::json!({"limit": 5}));

    // The server came up (initialize succeeded, get_health answered).
    // Whether the index finished or timed out, `find_anchors` MUST respond
    // with a normal JSON-RPC result envelope (not an error). The anchor
    // list itself can legitimately be empty when the timeout fired before
    // indexing completed — that's "No anchors found in Merged Brain.",
    // which is a correct answer, not a server failure. The previous assertion
    // treated "No anchors" as a failure mode, which made the test flaky
    // on slow CI runners where indexing exceeded the 1s budget.
    assert!(
        anchors.get("result").is_some(),
        "find_anchors must respond with a result envelope — the server \
         must come up even when re-index times out: {anchors}"
    );

    child.shutdown();
}
