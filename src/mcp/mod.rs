//! MCP protocol implementation for Lain
//!
//! Thin handlers that delegate to ToolExecutor

mod federation_tools;
mod handler;

pub use federation_tools::{
    get_federation_health, get_repo_info, list_repos, search_org, FederationHealth, RepoInfo,
    SymbolMatch,
};
pub use handler::LainMcpServer;
