//! MCP protocol implementation for Lain
//!
//! Thin handlers that delegate to ToolExecutor

mod federation_tools;
mod handler;

pub use federation_tools::{get_repo_info, list_repos, RepoInfo};
pub use handler::LainMcpServer;
