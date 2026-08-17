//! Smoke test for `lain doctor` — runs the binary, asserts exit 0
//! and that the diagnostic carries binary / git / hooks info.
//!
//! This is the wishlist item #6 ("one version of truth") verification:
//! the operator runs `lain doctor` and gets a single page that
//! confirms the binary version, the git sha it was built from,
//! and the on-disk state of the hook scripts + config + hooks dirs.

use std::process::Command;

fn lain() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn lain_doctor_runs_and_exits_zero() {
    let out = lain().args(["doctor"]).output().expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "lain doctor failed (status {:?}): {stdout}",
        out.status.code()
    );
    // Header should appear at the top.
    assert!(
        stdout.contains("lain doctor"),
        "missing header: {stdout}"
    );
    // Check 1: binary version + git sha.
    assert!(
        stdout.contains("binary") && stdout.contains("version"),
        "missing binary version line: {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("git") || stdout.contains("commit"),
        "missing git-sha info: {stdout}"
    );
}

#[test]
fn lain_doctor_mentions_hook_and_config_dirs() {
    let out = lain().args(["doctor"]).output().expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "lain doctor failed: {stdout}");
    // The four filesystem-facing checks should appear in the output.
    assert!(
        stdout.contains("hook"),
        "missing hook-script check: {stdout}"
    );
    assert!(
        stdout.contains("config"),
        "missing config-dir check: {stdout}"
    );
    assert!(
        stdout.contains("hooks dir"),
        "missing hooks-dir check: {stdout}"
    );
}