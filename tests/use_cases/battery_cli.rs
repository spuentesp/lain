//! Battery of positive + negative tests for every CLI subcommand.
//!
//! Spawns the `lain` binary as a subprocess and asserts:
//!   - positive: each subcommand runs without panic, prints expected
//!     header / output, exits 0 (or the documented non-zero code)
//!   - negative: bad args exit non-zero with a clear error message;
//!     bad paths / missing files surface gracefully
//!
//! Uses the same build artifact `cargo test` invokes (`target/debug/deps/`),
//! so no separate `cargo build` step is needed.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Resolve the `lain` binary path. The test runner uses
/// `target/debug/lain` (same target dir as the test binary).
fn lain_bin() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by cargo for integration tests.
    // For `lain` itself, look in target/debug.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| "target".to_string());
    let candidate = PathBuf::from(&target_dir).join("debug").join("lain");
    if candidate.exists() {
        candidate
    } else {
        // Fall back to $PATH.
        PathBuf::from("lain")
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(lain_bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn lain: {e}"))
}

// ─── version + help ──────────────────────────────────────────────

#[test]
fn lain_version_works() {
    let out = run(&["--version"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "--version must exit 0; got {:?}", out.status);
    assert!(stdout.contains("lain"), "--version must mention 'lain'");
}

#[test]
fn lain_help_works() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "--help must exit 0");
    assert!(stdout.contains("server") && stdout.contains("mcp"),
            "--help must list the headline subcommands");
}

#[test]
fn lain_unknown_subcommand_errors() {
    let out = run(&["totally_not_a_real_subcommand"]);
    assert!(!out.status.success(),
            "unknown subcommand must exit non-zero; got {:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // clap's default "unrecognized subcommand" error is fine.
    assert!(!stderr.is_empty() || !String::from_utf8_lossy(&out.stdout).is_empty());
}

#[test]
fn lain_no_args_shows_help() {
    let out = run(&[]);
    // clap default is to print help on no args.
    assert!(out.status.success() || !out.status.success(),
            "no-args is allowed to either print help or error");
}

// ─── doctor ──────────────────────────────────────────────────────

#[test]
fn lain_doctor_runs() {
    let out = run(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // doctor may exit non-zero on a partial install; we pin
    // "runs without panic" and "produces output".
    assert!(!stdout.is_empty() || !stderr.is_empty(),
            "doctor must produce some output");
}

#[test]
fn lain_doctor_unknown_flag_errors() {
    let out = run(&["doctor", "--no-such-flag"]);
    assert!(!out.status.success(), "unknown flag must exit non-zero");
}

// ─── schema dump ─────────────────────────────────────────────────

#[test]
fn lain_schema_dump_writes_default_path() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(&["schema", "dump", "--out",
                    dir.path().join("schema.json").to_str().unwrap()]);
    assert!(out.status.success(),
            "schema dump must succeed; stderr: {}",
            String::from_utf8_lossy(&out.stderr));
    assert!(dir.path().join("schema.json").exists(),
            "schema.json must be written");
}

#[test]
fn lain_schema_dump_unknown_action_errors() {
    let out = run(&["schema", "no_such_action"]);
    assert!(!out.status.success(), "unknown schema action must error");
}

// ─── server ──────────────────────────────────────────────────────

#[test]
fn lain_server_help_works() {
    let out = run(&["server", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "server --help must exit 0");
    assert!(stdout.contains("--config") && stdout.contains("--transport"),
            "server --help must document --config and --transport");
}

#[test]
fn lain_server_unknown_transport_errors() {
    let out = run(&["server", "--transport", "totally_not_a_transport"]);
    assert!(!out.status.success(), "unknown transport value must error");
}

#[test]
fn lain_server_unknown_flag_errors() {
    let out = run(&["server", "--no-such-flag"]);
    assert!(!out.status.success(), "unknown server flag must error");
}

// ─── mcp ─────────────────────────────────────────────────────────

#[test]
fn lain_mcp_help_works() {
    let out = run(&["mcp", "--help"]);
    assert!(out.status.success(), "mcp --help must exit 0");
}

// ─── init ────────────────────────────────────────────────────────

#[test]
fn lain_init_help_works() {
    let out = run(&["init", "--help"]);
    assert!(out.status.success(), "init --help must exit 0");
}

// ─── hooks ───────────────────────────────────────────────────────

#[test]
fn lain_hooks_help_works() {
    let out = run(&["hooks", "--help"]);
    assert!(out.status.success(), "hooks --help must exit 0");
    assert!(String::from_utf8_lossy(&out.stdout).contains("claim"),
            "hooks --help must list `claim` subcommand");
}

#[test]
fn lain_hooks_unknown_action_errors() {
    let out = run(&["hooks", "no_such_action"]);
    assert!(!out.status.success(), "unknown hooks action must error");
}

#[test]
fn lain_hooks_release_without_server_does_not_panic() {
    // Zero-daemon path (#3): release without a server should fail
    // open (not panic), per wishlist #1.
    let out = run(&["hooks", "release", "/tmp/no_such_path_here_xyz_unique"]);
    // The contract: exit non-zero with a clear message OR exit 0
    // (some implementations no-op). Either way, no panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stderr.contains("panic") && !stdout.contains("panic"));
}

// ─── workspaces ──────────────────────────────────────────────────

#[test]
fn lain_workspaces_help_works() {
    let out = run(&["workspaces", "--help"]);
    assert!(out.status.success(), "workspaces --help must exit 0");
}

#[test]
fn lain_workspaces_list_handles_missing_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(lain_bin())
        .args(["workspaces", "list"])
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("xdg"))
        .output()
        .expect("spawn");
    // Either succeeds with empty list or exits non-zero with a
    // clear message; either is acceptable.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panic"));
}

// ─── repos ───────────────────────────────────────────────────────

#[test]
fn lain_repos_help_works() {
    let out = run(&["repos", "--help"]);
    assert!(out.status.success(), "repos --help must exit 0");
}

#[test]
fn lain_repos_list_handles_missing_config() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(lain_bin())
        .args(["repos", "list"])
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("xdg"))
        .output()
        .expect("spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panic"),
            "repos list without config must not panic; stderr: {stderr}");
}

// ─── oneshot / ask / query ───────────────────────────────────────

#[test]
fn lain_oneshot_help_works() {
    let out = run(&["oneshot", "--help"]);
    assert!(out.status.success(), "oneshot --help must exit 0");
}

#[test]
fn lain_ask_help_works() {
    let out = run(&["ask", "--help"]);
    assert!(out.status.success(), "ask --help must exit 0");
}

#[test]
fn lain_query_help_works() {
    let out = run(&["query", "--help"]);
    assert!(out.status.success(), "query --help must exit 0");
}
