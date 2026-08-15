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
