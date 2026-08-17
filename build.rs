//! Build script — captures the current git short SHA into the
//! `LAIN_GIT_SHA` env var so `lain doctor` and friends can show
//! "which commit is this binary?" without shelling out at runtime.
//!
//! Falls back to the literal string `"unknown"` when:
//! - the build is not happening inside a git checkout (e.g. a
//!   vendored source tarball), OR
//! - `git rev-parse` fails for any reason.
//!
//! Re-emits on every commit so `cargo build` after a commit picks
//! up the new SHA — `cargo:rerun-if-changed=.git/HEAD` covers the
// cheap case, and we also rerun if `git rev-parse` itself changes.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=LAIN_GIT_SHA={sha}");
}