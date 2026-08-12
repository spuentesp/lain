//! MCP protocol implementation for Lain
//!
//! Thin handlers that delegate to ToolExecutor

pub mod federation_tools;
mod handler;

pub use federation_tools::{
    get_active_workspace, get_cross_repo_blast_radius, get_cross_repo_blast_radius_for_repo,
    get_federation_health, get_repo_info, get_workspace, get_workspace_graph, list_repos,
    list_workspaces, search_org, ActiveWorkspaceInfo, CrossRepoBlastRadius, FederationHealth,
    GraphEdge, GraphNode, RepoInfo, SymbolMatch, WorkspaceDetail, WorkspaceGraph, WorkspaceInfo,
    WorkspaceRepoInfo,
};
pub use handler::LainMcpServer;
