//! Cross-cutting helpers for the lain config directory layout.
//!
//! All on-disk state that lives under `~/.config/lain` (or `$XDG_CONFIG_HOME/lain`)
//! is rooted here. Keeping the path resolver in one place avoids drift between
//! the server's hot-reload pointer (`active_workspace`) and the CLI's
//! `lain workspaces use` writer.

pub mod recent_projects;

use std::path::PathBuf;

/// Return the path to the user config dir (`~/.config/lain`).
pub fn config_dir() -> PathBuf {
    // XDG: $XDG_CONFIG_HOME or ~/.config. lain doesn't have other XDG-aware
    // code yet, so we follow the standard directly.
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("lain");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lain")
}

/// Return the path to the lain runtime dir (`~/.local/lain/run`).
/// Used by the hot-reload Unix socket listener to drop per-project
/// `.sock` files. `$XDG_RUNTIME_DIR/lain` takes precedence when
/// present (per the XDG spec); otherwise we fall back to
/// `~/.local/lain/run`.
pub fn run_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("lain");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local").join("lain").join("run")
}

/// Return the path to the lain hooks dir (`<config_dir>/hooks`).
/// Per-agent pre-edit hook scripts cache their session token JSON
/// here so subsequent hook invocations can heartbeat without a
/// full `register_agent` round-trip.
pub fn hooks_dir() -> PathBuf {
    config_dir().join("hooks")
}
