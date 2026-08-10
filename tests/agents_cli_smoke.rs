//! Smoke test that the `lain agents ...` CLI subcommands dispatch correctly.
//! Runs each subcommand against the compiled `lain` binary (via `CARGO_BIN_EXE_lain`)
//! using a throwaway `HOME` so it doesn't clobber the developer's real config.

use std::process::Command;

fn lain_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn agents_list_invokes_run_list() {
    let out = Command::new(lain_bin())
        .args(["agents", "list"])
        .output()
        .expect("spawn lain agents list");
    assert!(out.status.success(), "lain agents list failed: stderr={}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude"), "list must include claude");
    assert!(stdout.contains("kimi"), "list must include kimi");
}

#[test]
fn agents_install_remove_round_trip_does_not_panic() {
    // Use a throwaway HOME so we don't clobber the developer's real config.
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = Command::new(lain_bin())
        .env("HOME", tmp.path())
        .args(["agents", "install", "kimi"])
        .output()
        .expect("spawn lain agents install");
    assert!(out.status.success(), "install failed: stderr={}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new(lain_bin())
        .env("HOME", tmp.path())
        .args(["agents", "remove", "kimi"])
        .output()
        .expect("spawn lain agents remove");
    assert!(out.status.success(), "remove failed: stderr={}", String::from_utf8_lossy(&out.stderr));
}
