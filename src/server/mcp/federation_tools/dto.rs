//! Wire DTOs returned by the federation-mode MCP tools. Kept in a
//! single file so adding/renaming a field is one diff instead of
//! six. Re-exported from `mod.rs` so external callers (mcp/handler.rs,
//! tests/federation_integration.rs, the SPA) keep working unchanged.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoInfo {
    pub id: String,
    pub path: String,
    pub health: String,
    pub last_refreshed_unix: i64,
    pub last_indexed_unix: i64,
    pub node_count: usize,
    pub edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederationHealth {
    pub total_repos: usize,
    pub ready: usize,
    pub indexing: usize,
    pub degraded: usize,
    pub unavailable: usize,
    pub missing: usize,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub memory_estimate_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SymbolMatch {
    pub global_id: String,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
}

/// Result of a cross-repo blast radius traversal: every node reachable from
/// the seed via outgoing `Calls` edges in `[min_depth, max_depth)`, grouped
/// by the repo each node came from. `total_count` is the number of nodes we
/// tried to bucket (including any whose global id failed to parse, which
/// silently fall out of `by_repo`). `truncated` is `true` when the result
/// hit the per-call cap of 1000 nodes — additional reachable nodes exist
/// beyond it but were not loaded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrossRepoBlastRadius {
    pub by_repo: BTreeMap<String, Vec<String>>,
    pub total_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceInfo {
    pub name: String,
    pub description: Option<String>,
    /// Source kind as a stable label: "workspace_dir" or "workspace_clone".
    /// None if the workspace was declared without a `source:` block.
    pub source: Option<String>,
    pub member_count: usize,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveWorkspaceInfo {
    pub name: String,
    pub members: Vec<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepoInfo {
    pub repo_id: String,
    pub path: String,
    pub health: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceDetail {
    pub name: String,
    pub description: Option<String>,
    pub source: Option<String>,
    pub members: Vec<WorkspaceRepoInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub repo_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub cross_repo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
}

/// Project metadata enriched with repo/workspace counts from the
/// project's `repos.yaml` / `workspaces.yaml`. Returned as one entry
/// per row in the `list_recent_projects` response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentProjectEntry {
    pub path: PathBuf,
    pub last_used: i64,
    pub workspace_count: usize,
    pub repo_count: usize,
    /// Active workspace name for this project, if set in the global
    /// `~/.config/lain/active_workspace` pointer and the pointer's
    /// `config_path` matches this entry's `path`. `None` otherwise.
    /// Used by the Command Center's recent-projects switcher to copy
    /// the right `lain server --workspace <name>` restart command.
    pub active_workspace: Option<String>,
}
