//! Federation-mode MCP tools.
//!
//! Six read-only tools for inspecting the live state of a `FederatedIndex`:
//! `list_repos`, `get_repo_info`, `get_federation_health`, `search_org`,
//! `get_cross_repo_blast_radius`, `get_cross_repo_blast_radius_for_repo`,
//! plus workspace tools (`list_workspaces`, `get_active_workspace`,
//! `get_workspace`, `get_workspace_graph`), server-status tools
//! (`get_server_status`, `get_reload_status`, `request_reload`), and
//! `list_recent_projects`. All gated on the MCP server having been
//! constructed with a `FederatedIndex` (see `LainMcpServer::with_federation`).
//! When the server runs in single-workspace mode the federation / workspace
//! tools are not registered; the server-status tools are always available.
//!
//! Split into four siblings so adding a new tool category doesn't mean
//! editing a 1287-line file:
//!
//! - [`dto`] — every wire DTO (`RepoInfo`, `WorkspaceGraph`, etc.).
//! - [`federation`] — `list_repos`, `get_repo_info`,
//!   `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`,
//!   `get_cross_repo_blast_radius_for_repo` (+ tests).
//! - [`workspace`] — `list_workspaces`, `get_active_workspace`,
//!   `get_workspace`, `get_workspace_graph`.
//! - [`server_status`] — `get_server_status`, `get_reload_status`,
//!   `request_reload` (+ tests).
//! - [`recent_projects`] — `list_recent_projects` (+ tests).

pub mod dto;
pub mod federation;
pub mod recent_projects;
pub mod server_status;
pub mod workspace;

pub use dto::{
    ActiveWorkspaceInfo, CrossRepoBlastRadius, FederationHealth, GraphEdge, GraphNode,
    RecentProjectEntry, RepoInfo, SymbolMatch, WorkspaceDetail, WorkspaceGraph,
    WorkspaceInfo, WorkspaceRepoInfo,
};
pub use federation::{
    get_cross_repo_blast_radius, get_cross_repo_blast_radius_for_repo, get_federation_health,
    get_repo_info, list_repos, search_org,
};
pub use recent_projects::list_recent_projects;
pub use server_status::{get_reload_status, get_server_status, request_reload};
pub use workspace::{
    get_active_workspace, get_workspace, get_workspace_graph, list_workspaces,
};
