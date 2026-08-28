//! D-M2: `lain mcp` must serve a populated graph on the first tool
//! call after `initialize`, bounded by `LAIN_REINDEX_TIMEOUT`.
//!
//! Before the fix, the startup re-index is `tokio::spawn`-ed inside
//! `LainMcpServer::run_stdio`, so the stdio loop comes up while
//! `build_core_memory` is still running. The first `find_anchors`
//! call therefore races against indexing and reads an empty graph.
//!
//! These tests pin the fix: a `find_anchors` call fired immediately
//! after `initialize` returns a non-empty anchor list, AND a too-short
//! `LAIN_REINDEX_TIMEOUT` still produces a running server that
//! reports the timeout via `get_health`.

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
        "/// Anchor: called by one, coordinates three, with a real body.\n\
         pub fn orchestrate() -> u32 {\n\
         \x20   let a = helper_a(1);\n\
         \x20   let b = helper_b(2);\n\
         \x20   let c = helper_c(3);\n\
         \x20   a + b + c\n\
         }\n\
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
        LainChild { child, stdin, stdout }
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
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read stdout");
        serde_json::from_str(&line).expect("parse jsonrpc response")
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let resp = self.send(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        );
        resp
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait_timeout(Duration::from_secs(5));
    }
}

/// Helper to shut a Child down with a timeout. Not in std today.
trait ChildWaitTimeout {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}
impl ChildWaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        dur: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
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

/// D-M2 main test. Boots `lain mcp` against the fixture, fires
/// `initialize`, then immediately calls `find_anchors` with NO
/// sleep or poll loop. Asserts a non-empty anchor list — proving
/// the re-index ran before the stdio loop came up.
///
/// On the current (buggy) source this test fails because the spawn
/// runs in parallel with the stdio loop, and `find_anchors` reads
/// an empty graph.
#[test]
fn find_anchors_works_immediately_after_initialize() {
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
    let init_resp = child.send("initialize", init_params.clone());
    assert!(
        init_resp.get("result").is_some(),
        "initialize must succeed: {init_resp}"
    );

    // No sleep, no poll loop. This is the contract.
    let resp = child.call_tool("find_anchors", serde_json::json!({"limit": 5}));
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    assert!(
        !text.is_empty() && !text.contains("No anchors"),
        "find_anchors returned empty on the first call after initialize — \
         cold-start re-index is not awaited. Response: {text:?}\n\
         Full JSON: {resp}"
    );
    // The fixture's orchestrator must surface — the test is meaningful
    // only if we know the graph had something to find.
    assert!(
        text.contains("orchestrate"),
        "the fixture's `orchestrate` function must appear in the anchor list: {text}"
    );

    child.shutdown();
}
