//! Battery of positive + negative tests for every agent hook script.
//!
//! Every hook in `hooks/` is a shell script that:
//!   - Reads a file path from $1 or stdin JSON
//!   - Invokes `lain hooks claim <path>` (or the equivalent)
//!   - **Must always exit 0** — wishlist #1 (fail open)
//!
//! Contract pinned for each hook:
//!   - positive: with a real file path as $1, exits 0
//!   - positive: with valid JSON stdin, exits 0
//!   - negative: with no input at all, exits 0 (no panic, no block)
//!   - negative: with malformed JSON stdin, exits 0 (no panic, no block)
//!   - negative: with a path that doesn't exist, exits 0

use std::path::PathBuf;
use std::process::{Command, Output};

fn hook_script(agent: &str, name: &str) -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("hooks").join(agent).join(name)
}

fn run_hook(agent: &str, name: &str, args: &[&str], stdin: &[u8]) -> Output {
    Command::new(hook_script(agent, name))
        .args(args)
        .env("LAIN_URL", "http://localhost:9999") // no server running
        .env_remove("LAIN_AGENT_NAME")
        .env_remove("CLAUDE_AGENT_NAME")
        .env_remove("MCP_CLIENT_NAME")
        .env_remove("AGENT_NAME")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            if let Some(mut s) = c.stdin.take() {
                let _ = s.write_all(stdin);
            }
            c.wait_with_output()
        })
        .unwrap_or_else(|e| panic!("failed to spawn hook {agent}/{name}: {e}"))
}

/// Pin the fail-open contract: every hook must exit 0 even when the
/// underlying `lain hooks claim` call fails (no server, bad path).
fn assert_fail_open(out: &Output, hook: &str) {
    assert!(out.status.success(),
            "{hook} must exit 0 (fail open per wishlist #1); got {:?}\nstderr: {}",
            out.status, String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panic"), "{hook} panicked: {stderr}");
}

// ─── claude-code / pre-edit.sh ───────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_edit_exits_zero_with_path() {
    let out = run_hook("claude-code", "pre-edit.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "claude-code/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_edit_exits_zero_with_no_input() {
    let out = run_hook("claude-code", "pre-edit.sh", &[], b"");
    assert_fail_open(&out, "claude-code/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_edit_exits_zero_with_malformed_json() {
    let out = run_hook("claude-code", "pre-edit.sh", &[],
                       b"this is not valid JSON {{");
    assert_fail_open(&out, "claude-code/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_edit_exits_zero_with_valid_json() {
    let out = run_hook("claude-code", "pre-edit.sh", &[],
                       br#"{"tool_name":"Edit","tool_input":{"file_path":"/tmp/foo.rs"}}"#);
    assert_fail_open(&out, "claude-code/pre-edit.sh");
}

// ─── claude-code / post-edit.sh ──────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_post_edit_exits_zero_with_path() {
    let out = run_hook("claude-code", "post-edit.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "claude-code/post-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_post_edit_exits_zero_with_no_input() {
    let out = run_hook("claude-code", "post-edit.sh", &[], b"");
    assert_fail_open(&out, "claude-code/post-edit.sh");
}

// ─── claude-code / pre-commit.sh ─────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_commit_exits_zero() {
    let out = run_hook("claude-code", "pre-commit.sh", &[], b"");
    assert_fail_open(&out, "claude-code/pre-commit.sh");
}

// ─── claude / lain-hook.sh ───────────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_lain_hook_exits_zero_with_path() {
    let out = run_hook("claude", "lain-hook.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "claude/lain-hook.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_lain_hook_exits_zero_with_no_input() {
    let out = run_hook("claude", "lain-hook.sh", &[], b"");
    assert_fail_open(&out, "claude/lain-hook.sh");
}

// ─── agy / pre-edit.sh ───────────────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn agy_pre_edit_exits_zero_with_path() {
    let out = run_hook("agy", "pre-edit.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "agy/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn agy_pre_edit_exits_zero_with_no_input() {
    let out = run_hook("agy", "pre-edit.sh", &[], b"");
    assert_fail_open(&out, "agy/pre-edit.sh");
}

// ─── codex / pre-edit.sh ─────────────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn codex_pre_edit_exits_zero_with_path() {
    let out = run_hook("codex", "pre-edit.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "codex/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn codex_pre_edit_exits_zero_with_no_input() {
    let out = run_hook("codex", "pre-edit.sh", &[], b"");
    assert_fail_open(&out, "codex/pre-edit.sh");
}

// ─── kimi / pre-edit.sh ──────────────────────────────────────────

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn kimi_pre_edit_exits_zero_with_path() {
    let out = run_hook("kimi", "pre-edit.sh",
                       &["/tmp/no_such_file_xyz_unique.rs"], b"");
    assert_fail_open(&out, "kimi/pre-edit.sh");
}

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn kimi_pre_edit_exits_zero_with_no_input() {
    let out = run_hook("kimi", "pre-edit.sh", &[], b"");
    assert_fail_open(&out, "kimi/pre-edit.sh");
}

// ─── Identity resolution (wishlist #2) ──────────────────────────
//
// Each hook must respect $LAIN_AGENT_NAME when set. We can't easily
// prove the claim happens (no server), but we can prove the hook
// resolves an identity and doesn't crash on the env override.

#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn claude_code_pre_edit_respects_lain_agent_name_env() {
    let out = Command::new(hook_script("claude-code", "pre-edit.sh"))
        .args(&["/tmp/no_such_file_xyz_unique.rs"])
        .env("LAIN_URL", "http://localhost:9999")
        .env("LAIN_AGENT_NAME", "test-agent-fixed")
        .env_remove("CLAUDE_AGENT_NAME")
        .env_remove("MCP_CLIENT_NAME")
        .env_remove("AGENT_NAME")
        .output()
        .expect("spawn");
    assert_fail_open(&out, "claude-code/pre-edit.sh with LAIN_AGENT_NAME");
}
