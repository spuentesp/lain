//! Build script — captures the current git short SHA into the
//! `LAIN_GIT_SHA` env var so `lain doctor` and friends can show
//! "which commit is this binary?" without shelling out at runtime.
//!
//! Falls back to the literal string `"unknown"` when:
//! - the build is not happening inside a git checkout (e.g. a
//!   vendored source tarball), OR
//! - `git rev-parse` fails for any reason.
//!
//! Appends a `-dirty` suffix when the working tree has uncommitted
//! changes so `lain doctor` can tell a fresh build from a dev-loop
//! build (the silent skew this prevented is exactly the bug that
//! caused the original wishlist lockout: a binary built from a
//! dirty tree misreported what's in it).
//!
//! Re-emits on every commit so `cargo build` after a commit picks
//! up the new SHA — `cargo:rerun-if-changed=.git/HEAD` covers the
//! cheap case in regular clones. Worktrees have `.git` as a file
//! pointing at the real gitdir, so we also re-emit on any tracked
//! file change so dev-loop rebuilds from worktrees see the dirty
//! marker flip back to clean after a commit.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
    // Worktree case: `.git` is a file, not a dir. Re-evaluate on any
    // source change so the dirty marker stays fresh.
    println!("cargo:rerun-if-changed=src");

    let mut sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());

    // `git diff --quiet` exits 0 when clean, non-zero when dirty.
    // Treating "dirty" as anything-not-clean is the right inverse
    // for a build-time warning — a dirty tree produces a binary that
    // doesn't reflect a single commit.
    let dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    if dirty {
        sha.push_str("-dirty");
    }

    println!("cargo:rustc-env=LAIN_GIT_SHA={sha}");
}