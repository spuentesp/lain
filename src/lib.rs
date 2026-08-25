//! Lain — local MCP server for cross-repo and per-repo code analysis.

pub mod server;
pub mod cli;
pub mod config;
pub mod state;

// Re-export the top-level clap `Command` factory at the crate root so
// tests and external callers can inspect the rendered help without
// going through the binary in `src/main.rs`.
pub use cli::dispatch::main_command_factory;

// Re-export error, lsp, and the analytical modules at the crate root so
// call sites that pre-date the `server/` migration keep working. The
// canonical home for these modules is `crate::server::*`; new code should
// import from there.
pub mod error {
    pub use crate::server::error::*;
}
pub mod lsp {
    pub use crate::server::lsp::*;
}
pub mod federation {
    //! Re-export the federation engine from `server::federation`.
    pub use crate::server::federation::*;
}
pub mod mcp {
    pub use crate::server::mcp::*;
}
pub mod graph {
    pub use crate::server::graph::*;
}
pub mod git {
    pub use crate::server::git::*;
}
pub mod schema {
    pub use crate::server::schema::*;
}
pub mod tuning {
    pub use crate::server::tuning::*;
}

// Crate-root re-exports for modules that pre-date the `server/` move.
// `crate::tools`, `crate::overlay`, etc. all live under `crate::server::`
// now; these aliases keep the older import paths working until callers
// migrate.
pub mod tools {
    pub use crate::server::tools::*;
}
pub mod overlay {
    pub use crate::server::overlay::*;
}
pub mod nlp {
    pub use crate::server::nlp::*;
}
pub mod query {
    pub use crate::server::query::*;
}
pub mod toolchains {
    pub use crate::server::toolchains::*;
}
pub mod sensors {
    pub use crate::server::sensors::*;
}
pub mod watcher {
    pub use crate::server::watcher::*;
}
pub mod treesitter {
    pub use crate::server::treesitter::*;
}

pub use server::LainServer;

/// Resolve a `repos.yaml` path passed to a subcommand.
///
/// When the caller passes `--config PATH`, use that path verbatim.
/// When the default `./repos.yaml` is used and the file exists,
/// use it. When the default doesn't exist, fall back to the
/// discoverable candidates in this order:
///
/// 1. `./.lain/repos.yaml` — local override (gitignored in teams
///    that prefer per-developer federation configs).
/// 2. `$XDG_CONFIG_HOME/lain/repos.yaml` (or
///    `~/.config/lain/repos.yaml`) — the per-user default.
///
/// If none of these exist, return the original path (which the
/// server will fail on with a clear "not found" error). This keeps
/// the current failure mode unchanged for typos while removing the
/// `--config ./repos.yaml` boilerplate for the common case where the
/// operator already has a federation file at one of these locations.
pub fn resolve_repos_config(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    // If the user passed an explicit --config (not the default),
    // respect it. We can't tell "default" from "explicit" here — the
    // helper is only invoked when the caller wants discovery, which
    // is exactly the default path. Callers who want to force a path
    // pass it through unchanged.
    if path != Path::new("./repos.yaml") {
        return path.to_path_buf();
    }
    if path.exists() {
        return path.to_path_buf();
    }
    let candidates = [
        PathBuf::from("./.lain/repos.yaml"),
        user_config_dir().join("lain/repos.yaml"),
    ];
    for c in candidates {
        if c.exists() {
            return c;
        }
    }
    path.to_path_buf()
}

fn user_config_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
}
