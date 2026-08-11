//! Live behavioral tests for the Claude Code agent.
//!
//! These tests verify that Claude actually reaches for the right Lain MCP
//! tool when given a prompt that should elicit it. They are the only tests
//! that close the loop "awareness doc is loaded -> model picks the tool".
//!
//! Caveats (inherent to behavioral testing):
//!  - Model outputs are stochastic. The assertions check for tool-name
//!    substrings in the model's printed output, which is robust as long as
//!    the prompt explicitly asks Claude to show the tool name. Do not
//!    expect identical output across runs.
//!  - Requires a logged-in Claude CLI and the Lain MCP server configured
//!    in the user's `~/.claude/settings.json`. Tests skip cleanly if
//!    either is missing.
//!  - Marked `#[ignore]` so they do not run in normal `cargo test`.
//!    Run with:  `RUN_E2E_BEHAVIOR=1 cargo test --test e2e_behavior -- --ignored`

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn claude_bin() -> Option<PathBuf> {
    let bin = "claude";
    which::which(bin).ok()
}

fn claude_is_authed() -> bool {
    // The Claude CLI stores OAuth credentials under ~/.claude/.credentials.json
    // once the user has run `claude` interactively at least once. We treat the
    // presence of that file (non-empty) as the auth signal; the actual call
    // would still fail if the token is expired, but that surfaces as a real
    // assertion failure with a useful error rather than a silent skip.
    let Some(home) = dirs::home_dir() else { return false };
    let path = home.join(".claude/.credentials.json");
    std::fs::metadata(&path).is_ok_and(|m| m.len() > 0)
}

fn run_claude_prompt(workspace: &std::path::Path, prompt: &str) -> (String, String, bool) {
    let claude = claude_bin().expect("claude binary required for this test");
    let mut child = Command::new(claude)
        // --print: non-interactive, print the final response and exit.
        // --dangerously-skip-permissions: auto-approve tool use so the
        // model can actually call Lain MCP tools without a TTY.
        .args(["--print", "--dangerously-skip-permissions"])
        .arg(prompt)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn claude");

    let mut stdout = String::new();
    let mut stderr = String::new();
    let start = Instant::now();
    let timeout = Duration::from_secs(120);

    use std::io::{BufRead, BufReader};
    let stdout_pipe = child.stdout.take().expect("stdout pipe");
    let stderr_pipe = child.stderr.take().expect("stderr pipe");
    let mut out_reader = BufReader::new(stdout_pipe).lines();
    let mut err_reader = BufReader::new(stderr_pipe).lines();

    loop {
        match out_reader.next() {
            Some(Ok(line)) => {
                stdout.push_str(&line);
                stdout.push('\n');
            }
            Some(Err(_)) | None => {}
        }
        match err_reader.next() {
            Some(Ok(line)) => {
                stderr.push_str(&line);
                stderr.push('\n');
            }
            Some(Err(_)) | None => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain remaining lines.
                for line in out_reader.flatten() {
                    stdout.push_str(&line);
                    stdout.push('\n');
                }
                for line in err_reader.flatten() {
                    stderr.push_str(&line);
                    stderr.push('\n');
                }
                return (stdout, stderr, status.success());
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "claude --print timed out after {timeout:?}\n\
                         --- stdout (so far):\n{stdout}\n\
                         --- stderr (so far):\n{stderr}"
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("wait error: {e}"),
        }
    }
}

fn should_run() -> bool {
    std::env::var("RUN_E2E_BEHAVIOR").is_ok_and(|v| !v.is_empty() && v != "0")
        && claude_bin().is_some()
        && claude_is_authed()
}

/// Workspace for the live behavior test. By default we skip (the test is
/// `#[ignore]`d). To exercise it locally, set LAIN_TEST_WORKSPACE to the
/// absolute path of a git repo you want OpenCode/Copilot/Claude to operate
/// on (the repo must have Lain configured — e.g. `lain init --agent <x>`
/// was run there). CI never sets this env var, so the path is never
/// referenced.
fn test_workspace() -> Option<PathBuf> {
    let s = std::env::var("LAIN_TEST_WORKSPACE").ok()?;
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

// ── Test 1: sanity — Claude can call get_health and get a real response ──

#[test]
#[ignore = "live behavior test; run with RUN_E2E_BEHAVIOR=1"]
fn claude_calls_get_health_when_asked() {
    if !should_run() {
        eprintln!(
            "skipping: RUN_E2E_BEHAVIOR not set, or `claude` not on PATH, \
             or not authed (need ~/.claude/.credentials.json)"
        );
        return;
    }

    // The model must produce the literal "Operational" string from the
    // real get_health body. That string is unique to a successful
    // tool call — a hallucinated "I'd call get_health" response will
    // not contain it. This is the strongest behavior assertion in the
    // suite.
    let prompt = "Call the Lain MCP server's get_health tool (it is listed \
        under the Lain MCP server in your tools). Print the raw response \
        body verbatim. Do not run any other tools. Do not write files.";

    let ws = test_workspace().expect(
        "LAIN_TEST_WORKSPACE not set; this live behavior test requires a workspace",
    );
    let (stdout, stderr, ok) = run_claude_prompt(&ws, prompt);
    assert!(
        ok,
        "claude --print exited non-zero.\n--- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("Operational"),
        "stdout must contain the literal 'Operational' from the real \
         get_health response (proves a real tool call, not a hallucination); \
         got:\n{stdout}"
    );
}

// ── Test 2: trigger phrase — "where do I start?" drives find_anchors ─────
// NOTE on coverage:
// `claude_calls_get_health_when_asked` is the only behavior test that
// can prove a real tool call happened, because the real `get_health`
// response contains the unique string `Operational` that the model
// cannot reasonably fabricate. The other tools (find_anchors,
// get_blast_radius, ...) return structured data without a unique
// smoking-gun string, so a substring assertion like `stdout.contains(
// "find_anchors")` passes when the model merely MENTIONS the tool in
// a refusal — the tests become false positives.
//
// To assert that trigger phrases ("where do I start?", "what breaks?")
// actually steer Claude to the right tool, we would need either:
//   - a unique signature in each tool's output to assert against, or
//   - an MCP-call trace from Claude Code's runtime.
//
// Neither is reliable today. When either becomes available, add the
// trigger-phrase tests back here and assert on the unique signature.
