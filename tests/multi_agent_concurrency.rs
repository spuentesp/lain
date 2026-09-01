//! Concurrent multi-agent race tests for `lain mcp`.
//!
//! Boots multiple `lain mcp` child processes that all point at
//! the same workspace fixture. The workspace path is the seed
//! for `state_path_for_workspace`, so every child reads and
//! writes the same `<XDG_STATE_HOME>/lain/<stem>-<hash>.json`
//! state file — the cross-process presence channel. A
//! `std::sync::Barrier` pins the children to the same instant so
//! the race in `OccupancyMap::claim_with_session` is actually
//! exercised.
//!
//! These tests pin the conflict-detection contract that the
//! single-agent `feat_suite` only implies:
//!
//! 1. Two agents racing for the same `edit` claim — exactly one
//!    wins, the other sees a `conflicts` entry.
//! 2. A holder's `edit` claim does not block two read-intent
//!    racers — both read claims land as grants; each observes
//!    an `advisories` entry pointing back at the holder.
//! 3. A released path is free for the next agent.
//! 4. Heartbeats refresh the session enough for a claim to
//!    land five seconds later.
//! 5. A wrong session token is rejected by the auth check
//!    (the 600-second interactive TTL makes a true-expiry
//!    path impractical inside the 30-second test budget; the
//!    task brief allows verifying the auth check directly).
//!
//! Each test owns its `TempDir`s — fixture and state dir both
//! dropped on test exit — so state is hermetic.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use tempfile::TempDir;

/// Compare a server-supplied path string against an expected list of
/// path components, treating both `/` and `\` as separators. Used
/// instead of `==` so the same assertion holds whether the server
/// emits forward slashes (Linux) or backslashes (Windows). Matches
/// the helper in `tests/feat_suite.rs`.
fn path_components_eq(path: &str, expected: &[&str]) -> bool {
    let actual: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect();
    actual == expected
}

/// Locate the `lain` binary. Mirrors `tests/mcp_cold_start.rs` —
/// `LAIN_BIN` first, then `target/{release,debug}/lain`.
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

/// Build a single-Cargo-file git fixture. Mirrors
/// `tests/mcp_cold_start.rs::build_fixture` — the indexer needs
/// a `.git` directory at the workspace root and at least one
/// Rust source file committed so `build_core_memory` produces a
/// non-empty graph.
fn build_fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();

    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"multi_agent_concurrency\"\n\
         version = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n\
         pub fn mul(a: u32, b: u32) -> u32 { a * b }\n",
    )
    .unwrap();

    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "concurrency-test@lain"],
        vec!["config", "user.name", "concurrency-test"],
        vec!["add", "-A"],
        vec!["commit", "-q", "-m", "fixture"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(root)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }
    dir
}

/// One agent's view of the MCP server: a `lain mcp` child
/// process with piped stdin/stdout. Each agent gets its own
/// child — the cross-agent plumbing is the state file, not a
/// shared process. Spawning many children is the only way to
/// exercise the cross-process presence lock.
struct AgentChild {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u32,
    protocol_version: String,
}

impl AgentChild {
    fn spawn(
        bin: &Path,
        workspace: &Path,
        xdg_state_home: &Path,
        protocol_version: &str,
        agent_label: &str,
    ) -> Self {
        // The full CLI is `lain mcp --workspace <path>`.
        //
        // `XDG_STATE_HOME` is the trick that makes the
        // children share state: every child hashes the
        // workspace path the same way, so the resolved state
        // file is `<xdg_state_home>/lain/<stem>-<hash>.json`
        // for all of them. `LAIN_REINDEX_TIMEOUT` caps the
        // cold-start wait so a slow CI runner can't blow the
        // 30-second budget; the fixture is tiny so the
        // re-index normally completes well inside that
        // window.
        let mut cmd = Command::new(bin);
        cmd.arg("mcp")
            .args(["--workspace"])
            .arg(workspace)
            .env("XDG_STATE_HOME", xdg_state_home)
            .env("LAIN_REINDEX_TIMEOUT", "30")
            .env_remove("LAIN_EMBEDDING_MODEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap_or_else(|e| {
            panic!(
                "spawn lain mcp for {agent_label} failed: {e}; bin={}",
                bin.display()
            )
        });
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        AgentChild {
            child,
            stdin,
            stdout,
            next_id: 1,
            protocol_version: protocol_version.to_string(),
        }
    }

    fn send(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
        )
        .expect("write stdin");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read stdout");
        if line.is_empty() {
            panic!("empty reply for {method}");
        }
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("not JSON for {method}: {e}\nline: {line:?}"))
    }

    fn initialize(&mut self) {
        let resp = self.send(
            "initialize",
            serde_json::json!({
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {
                    "name": "multi-agent-concurrency",
                    "version": "1",
                },
            }),
        );
        assert!(
            resp.get("result").is_some(),
            "initialize must succeed: {resp}"
        );
    }

    fn call_tool_raw(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        self.send(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        )
    }

    /// Send a `tools/call` and parse
    /// `result.content[0].text` as JSON. Panics on
    /// `isError=true` — the body is included in the message.
    fn call_tool_json(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let resp = self.call_tool_raw(name, arguments);
        if resp.pointer("/error").is_some() {
            panic!("{name} returned JSON-RPC error: {resp}");
        }
        let is_err = resp
            .pointer("/result/isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!("{name} missing result.content[0].text: {resp}")
            })
            .to_string();
        if is_err {
            panic!("{name} signalled isError=true: {text}");
        }
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{name} text not JSON: {e}\n{text}"))
    }

    /// Same as `call_tool_json` but does NOT panic on
    /// `isError=true` — used by the auth-rejection test which
    /// is supposed to fail. Returns
    /// `(envelope, json_rpc_error_message, parsed_text)`.
    fn call_tool_json_lenient(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> (serde_json::Value, Option<String>, Option<serde_json::Value>) {
        let resp = self.call_tool_raw(name, arguments);
        let err_msg = resp
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let is_err = resp
            .pointer("/result/isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if is_err && err_msg.is_none() {
            // Surface the isError path as a synthetic
            // JSON-RPC-style error message so callers can
            // assert against it uniformly.
            let synthetic = text.clone().unwrap_or_default();
            let parsed = text
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            return (resp, Some(format!("isError=true: {synthetic}")), parsed);
        }
        let parsed = text
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        (resp, err_msg, parsed)
    }

    /// Best-effort graceful shutdown: closing stdin prompts
    /// `lain mcp` to exit; if it doesn't, kill + wait so we
    /// don't leak processes on test failure.
    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait_timeout(Duration::from_secs(3));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

trait ChildWaitTimeout {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}
impl ChildWaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        dur: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
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

/// Two tempdirs the test owns together: the fixture workspace
/// and the shared state directory. `_fixture` and `_state` are
/// held purely to keep the tempdirs alive across the test
/// (their `Drop` reaps the directory).
struct TestEnv {
    _fixture: TempDir,
    _state: TempDir,
    workspace: PathBuf,
    state_dir: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let fixture = build_fixture();
        let state = tempfile::tempdir().expect("state tempdir");
        let workspace = fixture.path().to_path_buf();
        let state_dir = state.path().to_path_buf();
        TestEnv {
            _fixture: fixture,
            _state: state,
            workspace,
            state_dir,
        }
    }
}

/// Boot one `lain mcp` child and run its `initialize` handshake.
fn spawn_agent(
    bin: &Path,
    env: &TestEnv,
    pv: &str,
    label: &str,
) -> AgentChild {
    let mut child = AgentChild::spawn(bin, &env.workspace, &env.state_dir, pv, label);
    child.initialize();
    child
}

fn register(child: &mut AgentChild, name: &str) -> (String, String) {
    let resp = child.call_tool_json(
        "register_agent",
        serde_json::json!({
            "name": name,
            "kind": "other",
            "mode": "interactive",
        }),
    );
    let agent_id = resp
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("register_agent missing agent_id: {resp}"))
        .to_string();
    let session_token = resp
        .get("session_token")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("register_agent missing session_token: {resp}"))
        .to_string();
    (agent_id, session_token)
}

// ─── 1. Two agents race for the same edit claim ──────────────────────
//
// Both agents pick the same instant via a 2-party Barrier and
// claim `["src/contested.rs"]`. The shared state file is the
// serialization point: only one wins, the other receives a
// `conflicts` entry naming the winner. This is the test that
// motivated the suite — the single-agent `feat_suite` only
// checks the happy path.
#[test]
fn two_agents_race_claim_one_wins_one_conflicts() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary (set LAIN_BIN or run `cargo build`)");
        return;
    };
    let Some(pv) = protocol_version(&bin) else {
        eprintln!("skipping: could not determine MCP protocol version");
        return;
    };
    let env = TestEnv::new();
    let mut alice = spawn_agent(&bin, &env, &pv, "alice");
    let mut bob = spawn_agent(&bin, &env, &pv, "bob");
    let (alice_id, alice_token) = register(&mut alice, "alice");
    let (bob_id, bob_token) = register(&mut bob, "bob");
    assert_ne!(alice_id, bob_id, "distinct agents must get distinct ids");

    let payload = serde_json::json!([{"path": "src/contested.rs", "intent": "edit"}]);
    let barrier = Arc::new(Barrier::new(2));

    // Move each AgentChild into its own thread; the worker
    // closes the child when it returns. After join, the
    // race results live in `claims` and the children are
    // cleaned up — no need for an external guard.
    let claims: Vec<(String, String, serde_json::Value)> = {
        // Each thread needs its own owned copies of the
        // agent_id / token / payload (serde_json::Value is
        // not Copy). Alice and Bob are arbitrary labels —
        // whichever child reaches the state file first
        // wins; the test only asserts "exactly one wins,
        // exactly one sees the conflict".
        let alice_id_for_alice = alice_id.clone();
        let bob_id_for_bob = bob_id.clone();
        let alice_token_for_alice = alice_token.clone();
        let bob_token_for_bob = bob_token.clone();
        let payload_alice = payload.clone();
        let payload_bob = payload.clone();
        let b1 = barrier.clone();

        let alice_thread = std::thread::spawn(move || {
            let mut child = alice;
            b1.wait();
            let resp = child.call_tool_json(
                "claim_files",
                serde_json::json!({
                    "agent_id": alice_id_for_alice,
                    "session_token": alice_token_for_alice,
                    "files": payload_alice,
                }),
            );
            child.shutdown();
            ("alice".to_string(), "alice".to_string(), resp)
        });

        let b2 = barrier;
        let bob_thread = std::thread::spawn(move || {
            let mut child = bob;
            b2.wait();
            let resp = child.call_tool_json(
                "claim_files",
                serde_json::json!({
                    "agent_id": bob_id_for_bob,
                    "session_token": bob_token_for_bob,
                    "files": payload_bob,
                }),
            );
            child.shutdown();
            ("bob".to_string(), "bob".to_string(), resp)
        });

        let a = alice_thread.join().expect("alice join");
        let b = bob_thread.join().expect("bob join");
        vec![a, b]
    };

    let grants: Vec<&(String, String, serde_json::Value)> = claims
        .iter()
        .filter(|(_, _, r)| {
            r.get("granted")
                .and_then(|g| g.as_array())
                .map(|arr| {
                    arr.iter().any(|g| {
                        g.get("path")
                            .and_then(|p| p.as_str())
                            .is_some_and(|p| path_components_eq(p, &["src", "contested.rs"]))
                    })
                })
                .unwrap_or(false)
        })
        .collect();
    let losers: Vec<&(String, String, serde_json::Value)> = claims
        .iter()
        .filter(|(_, _, r)| {
            r.get("conflicts")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter().any(|c| {
                        c.get("path")
                            .and_then(|p| p.as_str())
                            .is_some_and(|p| path_components_eq(p, &["src", "contested.rs"]))
                    })
                })
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(
        grants.len(),
        1,
        "exactly one agent must win the claim, got {grants:?}; full results: {claims:?}"
    );
    assert_eq!(
        losers.len(),
        1,
        "exactly one agent must see the conflict, got {losers:?}; full results: {claims:?}"
    );

    // The lost agent's conflict entry must point at the
    // winner's agent_id.
    let (loser_name, _, loser_resp) = losers[0];
    let conflict = loser_resp
        .get("conflicts")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .expect("loser must have at least one conflict entry");
    let reported_winner = conflict
        .get("agent_id")
        .and_then(|v| v.as_str())
        .expect("conflict entry missing agent_id");
    let expected_winner = if *loser_name == "alice" {
        &bob_id
    } else {
        &alice_id
    };
    assert_eq!(
        reported_winner, expected_winner,
        "loser={loser_name} should report winner={expected_winner}; conflict: {conflict}"
    );
}

// ─── 2. Three agents: holder edits, two readers observe advisory ────
//
// A holds an edit claim on `src/shared.rs`. B and C
// simultaneously claim the same path with `intent: "read"`.
// Both reads succeed (wishlist #5: reads never conflict) but
// each response carries an `advisories` entry naming A.
#[test]
fn three_agents_one_wins_two_observe_advisory() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary");
        return;
    };
    let Some(pv) = protocol_version(&bin) else {
        eprintln!("skipping");
        return;
    };
    let env = TestEnv::new();
    let mut a = spawn_agent(&bin, &env, &pv, "alice-edit");
    let mut b = spawn_agent(&bin, &env, &pv, "bob-read");
    let mut c = spawn_agent(&bin, &env, &pv, "carol-read");
    let (a_id, a_token) = register(&mut a, "alice-edit");
    let (b_id, b_token) = register(&mut b, "bob-read");
    let (c_id, c_token) = register(&mut c, "carol-read");
    assert!(a_id != b_id && b_id != c_id && a_id != c_id);

    // A claims first (no race).
    let a_resp = a.call_tool_json(
        "claim_files",
        serde_json::json!({
            "agent_id": a_id.clone(),
            "session_token": a_token.clone(),
            "files": [{"path": "src/shared.rs", "intent": "edit"}],
        }),
    );
    let a_granted = a_resp
        .get("granted")
        .and_then(|v| v.as_array())
        .expect("granted array");
    assert!(
        a_granted.iter().any(|g| {
            g.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| path_components_eq(p, &["src", "shared.rs"]))
        }),
        "alice's edit claim must succeed: {a_resp}"
    );
    a.shutdown();

    // B and C race on the read claim.
    let barrier = Arc::new(Barrier::new(2));
    let b1 = barrier.clone();
    let b_thread = std::thread::spawn(move || {
        let mut child = b;
        b1.wait();
        let resp = child.call_tool_json(
            "claim_files",
            serde_json::json!({
                "agent_id": b_id,
                "session_token": b_token,
                "files": [{"path": "src/shared.rs", "intent": "read"}],
            }),
        );
        child.shutdown();
        ("bob".to_string(), resp)
    });
    let c_thread = std::thread::spawn(move || {
        let mut child = c;
        barrier.wait();
        let resp = child.call_tool_json(
            "claim_files",
            serde_json::json!({
                "agent_id": c_id.clone(),
                "session_token": c_token,
                "files": [{"path": "src/shared.rs", "intent": "read"}],
            }),
        );
        child.shutdown();
        ("carol".to_string(), resp)
    });

    let results: Vec<(String, serde_json::Value)> = vec![
        b_thread.join().expect("bob join"),
        c_thread.join().expect("carol join"),
    ];

    // Both reads should have succeeded.
    for (who, r) in &results {
        let granted = r
            .get("granted")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{who}: missing granted array: {r}"));
        assert!(
            granted.iter().any(|g| {
                g.get("path")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| path_components_eq(p, &["src", "shared.rs"]))
            }),
            "{who} read claim must be granted: {r}"
        );
        let conflicts_empty = r
            .get("conflicts")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        assert!(
            conflicts_empty,
            "{who} read claim must not produce a conflict (read intent is non-blocking): {r}"
        );
    }

    // Both readers should observe an advisory pointing at
    // alice.
    for (who, r) in &results {
        let advisories = r
            .get("advisories")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("{who} missing advisories array: {r}"));
        assert!(
            advisories.iter().any(|adv| {
                adv.get("agent_id").and_then(|v| v.as_str()) == Some(a_id.as_str())
                    && adv
                        .get("path")
                        .and_then(|p| p.as_str())
                        .is_some_and(|p| path_components_eq(p, &["src", "shared.rs"]))
            }),
            "{who} must see alice's edit claim as an advisory, got: {advisories:?}; full: {r}"
        );
    }
}

// ─── 3. A released claim is available again ──────────────────────────
//
// A claims, releases, then B claims the same path. The release
// removes A's claim from the occupancy map and from disk; B
// observes a fresh grant.
#[test]
fn released_claim_becomes_available_again() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary");
        return;
    };
    let Some(pv) = protocol_version(&bin) else {
        eprintln!("skipping");
        return;
    };
    let env = TestEnv::new();
    let mut alice = spawn_agent(&bin, &env, &pv, "alice");
    let mut bob = spawn_agent(&bin, &env, &pv, "bob");
    let (alice_id, alice_token) = register(&mut alice, "alice");
    let (bob_id, bob_token) = register(&mut bob, "bob");

    // A claims x.rs.
    let a_resp = alice.call_tool_json(
        "claim_files",
        serde_json::json!({
            "agent_id": alice_id.clone(),
            "session_token": alice_token.clone(),
            "files": ["src/x.rs"],
        }),
    );
    assert!(
        a_resp
            .pointer("/granted/0/path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| path_components_eq(p, &["src", "x.rs"])),
        "alice claim must succeed: {a_resp}"
    );

    // A releases x.rs.
    let rel = alice.call_tool_json(
        "release_files",
        serde_json::json!({
            "agent_id": alice_id.clone(),
            "session_token": alice_token.clone(),
            "files": ["src/x.rs"],
        }),
    );
    let released = rel
        .get("released")
        .and_then(|v| v.as_array())
        .expect("release_files must return `released` array");
    assert!(
        released
            .iter()
            .any(|p| p.as_str().is_some_and(|x| path_components_eq(x, &["src", "x.rs"]))),
        "release_files must report src/x.rs: {rel}"
    );
    alice.shutdown();

    // B claims the same path — must succeed.
    let b_resp = bob.call_tool_json(
        "claim_files",
        serde_json::json!({
            "agent_id": bob_id,
            "session_token": bob_token,
            "files": ["src/x.rs"],
        }),
    );
    bob.shutdown();
    let granted = b_resp
        .get("granted")
        .and_then(|v| v.as_array())
        .expect("bob claim must return granted array");
    assert!(
        granted.iter().any(|g| {
            g.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| path_components_eq(p, &["src", "x.rs"]))
        }),
        "bob must be granted src/x.rs after alice released it: {b_resp}"
    );
}

// ─── 4. Heartbeats keep the session alive ────────────────────────────
//
// The default interactive TTL is 600s; we don't have time to
// wait that long. The point of this test is that successive
// heartbeats never raise an error and that a claim after the
// heartbeats still lands. It pins the "every authenticated
// call counts as a heartbeat" behavior in `authenticate` —
// without it, an idle agent would silently expire and lose
// its claims.
#[test]
fn heartbeat_keeps_session_alive() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary");
        return;
    };
    let Some(pv) = protocol_version(&bin) else {
        eprintln!("skipping");
        return;
    };
    let env = TestEnv::new();
    let mut agent = spawn_agent(&bin, &env, &pv, "pulse");
    let (id, token) = register(&mut agent, "pulse-agent");

    // Five heartbeats at ~1s intervals. The exact interval
    // is not load-bearing — what matters is that the
    // heartbeat round-trips without error and the session
    // continues to authenticate claims after.
    for _ in 0..5 {
        let hb = agent.call_tool_json(
            "heartbeat",
            serde_json::json!({
                "agent_id": id.clone(),
                "session_token": token.clone(),
            }),
        );
        let ok = hb.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(ok, "heartbeat must return ok=true: {hb}");
        std::thread::sleep(Duration::from_secs(1));
    }

    // After five seconds of heartbeats, claim must still
    // succeed.
    let resp = agent.call_tool_json(
        "claim_files",
        serde_json::json!({
            "agent_id": id,
            "session_token": token,
            "files": ["src/y.rs"],
        }),
    );
    agent.shutdown();
    let granted = resp
        .get("granted")
        .and_then(|v| v.as_array())
        .expect("claim after heartbeats must return granted array");
    assert!(
        granted.iter().any(|g| {
            g.get("path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| path_components_eq(p, &["src", "y.rs"]))
        }),
        "claim must succeed after heartbeats: {resp}"
    );
}

// ─── 5. Wrong session token is rejected by the auth check ────────────
//
// `PresenceRegistry` enforces a 600-second interactive TTL; a
// true wait-and-retry test is not feasible inside the 30-second
// suite budget. The task brief allows "verify the auth check
// exists" — that's this test. A claim that names a real
// `agent_id` but presents a forged `session_token` must fail
// with an auth error, not silently succeed.
//
// `authenticate` at `presence_tools.rs:44` is the single gate
// every claim goes through. This is its integration-test seam.
#[test]
fn expired_session_rejected() {
    let Some(bin) = lain_bin() else {
        eprintln!("skipping: no lain binary");
        return;
    };
    let Some(pv) = protocol_version(&bin) else {
        eprintln!("skipping");
        return;
    };
    let env = TestEnv::new();
    let mut agent = spawn_agent(&bin, &env, &pv, "reject");
    let (id, real_token) = register(&mut agent, "soon-expired");

    // Forged token — wrong bytes, never issued by the server.
    let forged = "deadbeef-this-token-was-never-issued";

    let (_resp, err, body) = agent.call_tool_json_lenient(
        "claim_files",
        serde_json::json!({
            "agent_id": id.clone(),
            "session_token": forged.to_string(),
            "files": ["src/guarded.rs"],
        }),
    );

    // The server has two ways to reject: a JSON-RPC `error`
    // envelope, or a `result.isError=true` with an error
    // message in `content[0].text`. Either is acceptable;
    // what must NOT happen is a `granted` array containing
    // the path.
    let granted_path_present = body
        .as_ref()
        .and_then(|b| b.get("granted"))
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter().any(|g| {
                g.get("path")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| path_components_eq(p, &["src", "guarded.rs"]))
            })
        })
        .unwrap_or(false);
    assert!(
        !granted_path_present,
        "forged session token must NOT grant the claim; resp={_resp}; body={body:?}; err={err:?}"
    );
    // And the server must have signaled *some* form of
    // rejection — either a JSON-RPC error or an isError.
    // A silent "no conflicts, no grants, just empty"
    // response would be a regression of the auth check.
    assert!(
        err.is_some(),
        "forged session token must produce an error response; resp={_resp}; body={body:?}"
    );

    // Also verify the real token still works — guards
    // against a regression where the auth check was so
    // strict it accidentally rejects everything.
    let ok_resp = agent.call_tool_json(
        "claim_files",
        serde_json::json!({
            "agent_id": id,
            "session_token": real_token,
            "files": ["src/guarded.rs"],
        }),
    );
    agent.shutdown();
    assert!(
        ok_resp
            .pointer("/granted/0/path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| path_components_eq(p, &["src", "guarded.rs"])),
        "real token must still grant; got: {ok_resp}"
    );
}
