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

/// Return the path to the lain state dir (`~/.local/lain/state`).
/// `LainServer::save_state` / `load_state` write and read the
/// `PresenceRegistry` + `OccupancyMap` JSON snapshot here, one file
/// per workspace (`<workspace-stem>.json`). `$XDG_STATE_HOME/lain`
/// takes precedence when present (per the XDG spec).
pub fn state_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("lain");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local").join("lain").join("state")
}

/// Resolve the persisted-state file for a given workspace. The
/// filename is `<config-stem>.json` where `config_stem` is the last
/// path component of the workspace, sanitized to only alphanumerics
/// + `-_` (punctuation becomes `-`). This keeps the state filename
/// stable across relaunches with the same workspace, without needing
/// to embed absolute paths.
pub fn state_path_for_workspace(workspace: &std::path::Path) -> PathBuf {
    let stem = workspace
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    state_dir().join(format!("{}.json", cleaned))
}
