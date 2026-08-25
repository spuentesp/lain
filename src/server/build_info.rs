//! Build identity of the *running* process.
//!
//! An MCP stdio server is spawned once by the agent host and then
//! outlives every rebuild: `/proc/<pid>/exe` reads
//! `…/target/release/lain (deleted)` while a newer binary sits at the
//! same path. Nothing in the protocol surface exposed which build was
//! answering, so an agent could read a fix in the source tree, call the
//! tool it fixed, and get the old behavior with no way to tell why.
//! `lain doctor` checks exactly this, but it is a human CLI the agent
//! never sees.
//!
//! This module carries the compile-time identity (`VERSION`,
//! `GIT_SHA`) plus the executable's mtime as observed at startup, so
//! `get_health` and `get_server_status` can report both what this
//! process is and whether the operator has since rebuilt underneath
//! it.

use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git SHA stamped by `build.rs`, with a `-dirty` suffix when the
/// tree had uncommitted changes at build time.
pub const GIT_SHA: &str = env!("LAIN_GIT_SHA");

static STARTUP_EXE_MTIME: OnceLock<Option<u64>> = OnceLock::new();

/// mtime of the file at `current_exe()`, right now. `None` when the
/// path can't be resolved or stat'd — a deleted-and-replaced binary
/// still resolves, since the path is re-read on each call.
fn exe_mtime_now() -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(exe).ok()?;
    let mtime = meta.modified().ok()?;
    mtime.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Record the executable's mtime as of process start. Call once, early
/// in `main`, before anything can rebuild underneath us. Calling it
/// later is harmless but weakens [`binary_is_stale`], which compares
/// against whatever was recorded first.
pub fn record_startup_exe_mtime() {
    let _ = STARTUP_EXE_MTIME.set(exe_mtime_now());
}

/// The recorded startup mtime, if [`record_startup_exe_mtime`] ran.
pub fn binary_mtime_unix() -> Option<u64> {
    *STARTUP_EXE_MTIME.get().unwrap_or(&None)
}

/// True when the binary on disk is newer than the image this process is
/// running — i.e. someone rebuilt while the server stayed up, so the
/// tools answering are older than the source tree.
///
/// False when the mtime was never recorded or can't be read now:
/// absence of evidence is not staleness.
pub fn binary_is_stale() -> bool {
    match (binary_mtime_unix(), exe_mtime_now()) {
        (Some(startup), Some(now)) => now > startup,
        _ => false,
    }
}

/// One-line human summary: `0.6.0 (9756156)`, plus a marker when the
/// on-disk binary has moved ahead of this process.
pub fn summary() -> String {
    let mut s = format!("{VERSION} ({GIT_SHA})");
    if binary_is_stale() {
        s.push_str(" — ⚠ a newer binary exists on disk; restart to pick it up");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_sha_are_populated() {
        assert!(!VERSION.is_empty());
        assert!(!GIT_SHA.is_empty());
    }

    #[test]
    fn summary_contains_version_and_sha() {
        let s = summary();
        assert!(s.contains(VERSION), "summary must name the version: {s}");
        assert!(s.contains(GIT_SHA), "summary must name the sha: {s}");
    }

    #[test]
    fn stale_is_false_without_a_recorded_startup_mtime() {
        // Unit tests never call `record_startup_exe_mtime`, so this
        // pins the "absence of evidence is not staleness" branch.
        assert!(!binary_is_stale());
    }
}
