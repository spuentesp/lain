//! Lain — local MCP server for cross-repo and per-repo code analysis.

pub mod server;
pub mod cli;
pub mod config;

pub mod error;
pub mod federation {
    //! Re-export the federation engine from `server::federation`.
    pub use crate::server::federation::*;
}
pub mod graph {
    pub use crate::server::graph::*;
}
pub mod git {
    pub use crate::server::git::*;
}
pub mod lsp {
    pub use crate::server::lsp::*;
}
pub mod schema {
    pub use crate::server::schema::*;
}
pub mod tuning {
    pub use crate::server::tuning::*;
}

pub use server::LainServer;
