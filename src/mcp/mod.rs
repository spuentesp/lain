//! MCP protocol implementation for Lain
//!
//! Thin handlers that delegate to ToolExecutor

mod federation_tools;
mod handler;

pub use federation_tools::{
    get_cross_repo_blast_radius, get_cross_repo_blast_radius_for_repo, get_federation_health,
    get_repo_info, list_repos, search_org, CrossRepoBlastRadius, FederationHealth, RepoInfo,
    SymbolMatch,
};
pub use handler::LainMcpServer;
