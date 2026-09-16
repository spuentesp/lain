//! Test-only helpers shared across `#[cfg(test)]` modules under `src/`.
//!
//! Distinct from `tests/common/mod.rs`, which is the integration-test
//! harness for binaries under `tests/`. This module compiles into the
//! library crate (only under `cfg(test)`) so unit-test modules inside
//! source files can share fixtures without redefining them.
//!
//! Canonical home for `XdgGuard` — see `docs/CONTRIBUTING_AGENTS.md`.
//! Two source files previously inlined a byte-identical copy of this
//! struct; both now import it from here.

use std::path::Path;

/// RAII guard that points `XDG_CONFIG_HOME` at `dir` for the duration
/// of a test, restoring the previous value on drop.
///
/// Used by tests that exercise the config-file helpers in
/// `crate::state::ActiveWorkspace` and
/// `crate::server::mcp::federation_tools::recent_projects`, so they
/// don't touch the developer's real `~/.config/lain/`.
pub struct XdgGuard {
    prev: Option<String>,
}

impl XdgGuard {
    pub fn new(dir: &Path) -> Self {
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", dir);
        Self { prev }
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}

/// RAII guard that points `XDG_STATE_HOME` at `dir` for the duration
/// of a test. Mirrors `XdgGuard` for the `state_dir()` path. Currently
/// no source file uses it; declared preemptively so the next test
/// that needs it imports from here rather than inlining a third copy.
#[allow(dead_code)]
pub struct XdgStateGuard {
    prev: Option<String>,
}

#[allow(dead_code)]
impl XdgStateGuard {
    pub fn new(dir: &Path) -> Self {
        let prev = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", dir);
        Self { prev }
    }
}

#[allow(dead_code)]
impl Drop for XdgStateGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}