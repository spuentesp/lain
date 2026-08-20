//! Workspace-aware MCP tools: `list_workspaces`, `get_active_workspace`,
//! `get_workspace`, `get_workspace_graph`. Read-only; depend on the
//! `WorkspacesFile` the server was constructed with and the live
//! `FederatedIndex`.

use super::dto::{
    ActiveWorkspaceInfo, GraphEdge, GraphNode, WorkspaceDetail, WorkspaceGraph,
    WorkspaceInfo, WorkspaceRepoInfo,
};
use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::workspace::{WorkspaceSourceConfig, WorkspacesFile};
use crate::state::ActiveWorkspace;

fn source_label(s: &Option<WorkspaceSourceConfig>) -> Option<String> {
    s.as_ref().map(|c| match c {
        WorkspaceSourceConfig::WorkspaceDir { .. } => "workspace_dir".to_string(),
        WorkspaceSourceConfig::WorkspaceClone { .. } => "workspace_clone".to_string(),
    })
}

pub fn list_workspaces(
    workspaces: &WorkspacesFile,
    active: Option<&ActiveWorkspace>,
) -> Vec<WorkspaceInfo> {
    workspaces.workspaces.iter().map(|ws| {
        let is_active = active.as_ref().map(|a| a.name == ws.name).unwrap_or(false);
        WorkspaceInfo {
            name: ws.name.clone(),
            description: ws.description.clone(),
            source: source_label(&ws.source),
            member_count: ws.members.len(),
            is_active,
        }
    }).collect()
}

/// Identify the workspace whose member set exactly matches the loaded repo
/// ids in the federation. Returns `LainError::Workspace(...)` if no
/// workspace matches (no workspace was active, or the federation was
/// loaded without workspace filtering).
pub fn get_active_workspace(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
) -> Result<ActiveWorkspaceInfo, LainError> {
    let loaded: std::collections::HashSet<String> =
        fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    if loaded.is_empty() {
        return Err(LainError::Workspace(
            "no repos loaded; no active workspace".into(),
        ));
    }
    let active = workspaces.workspaces.iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| LainError::Workspace(
            "federation loaded but no workspace matches the loaded repos".into(),
        ))?;
    Ok(ActiveWorkspaceInfo {
        name: active.name.clone(),
        members: active.members.clone(),
        source: source_label(&active.source),
    })
}

pub fn get_workspace(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    name: &str,
) -> Result<WorkspaceDetail, LainError> {
    let ws = workspaces.workspaces.iter().find(|w| w.name == name)
        .ok_or_else(|| LainError::NotFound(format!("workspace {name}")))?;
    // Resolve path + health for each member from the federation, if loaded.
    let loaded = fed.list_repos();
    let mut members = Vec::with_capacity(ws.members.len());
    for m in &ws.members {
        let info = loaded.iter().find(|(id, _)| id.as_str() == m);
        let (path, health) = match info {
            Some((id, h)) => {
                let repo = fed.get_repo(id);
                let path = repo.map(|r| r.source().local_path().display().to_string()).unwrap_or_default();
                (path, h.to_string())
            }
            None => (String::new(), "not_loaded".to_string()),
        };
        members.push(WorkspaceRepoInfo {
            repo_id: m.clone(),
            path,
            health,
        });
    }
    Ok(WorkspaceDetail {
        name: ws.name.clone(),
        description: ws.description.clone(),
        source: source_label(&ws.source),
        members,
    })
}

const GRAPH_NODE_CAP: usize = 5000;
const GRAPH_EDGE_CAP: usize = 10000;

fn node_kind_str(s: &str) -> bool {
    matches!(s, "Function" | "Method" | "Class")
}

fn edge_kind_str(s: &str) -> bool {
    matches!(s, "Calls" | "Imports")
}

/// Per-workspace graph data for the dashboard's D3 force-directed view.
///
/// Filters to `Function` / `Method` / `Class` nodes and `Calls` / `Imports`
/// edges (per the spec's "filtered Functions + Calls + cross-repo" scope).
/// Marks edges as `cross_repo: true` when source's repo_id differs from
/// target's. Caps at 5000 nodes / 10000 edges.
pub fn get_workspace_graph(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    filter: Option<&str>,
) -> Result<WorkspaceGraph, LainError> {
    // Identify the active workspace by intersecting loaded repos with
    // each workspace's member set. Errors if no match.
    let loaded: std::collections::HashSet<String> =
        fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    let active = workspaces.workspaces.iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| LainError::Workspace(
            "federation loaded but no workspace matches the loaded repos".into(),
        ))?;
    let members: std::collections::HashSet<String> = active.members.iter().cloned().collect();

    let all_nodes = fed.backend().list_nodes().map_err(LainError::from)?;
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut truncated = false;
    for n in all_nodes {
        let kind = format!("{:?}", n.node_type);
        if !node_kind_str(&kind) { continue; }
        let gid = crate::federation::repo_id::GlobalId::parse(&n.id).ok();
        let repo_id = gid.as_ref().map(|g| g.repo_id().to_string()).unwrap_or_default();
        if !members.contains(&repo_id) { continue; }
        if let Some(f) = filter {
            if !n.name.contains(f) && !n.path.contains(f) { continue; }
        }
        if nodes.len() >= GRAPH_NODE_CAP {
            truncated = true;
            break;
        }
        nodes.push(GraphNode {
            id: n.id.clone(),
            name: n.name.clone(),
            path: n.path.clone(),
            repo_id,
            kind,
        });
    }
    let node_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let mut edges: Vec<GraphEdge> = Vec::new();
    let all_edges = fed.backend().all_edges().map_err(LainError::from)?;
    for e in all_edges {
        if !node_ids.contains(e.source_id.as_str()) || !node_ids.contains(e.target_id.as_str()) { continue; }
        let kind = format!("{:?}", e.edge_type);
        if !edge_kind_str(&kind) { continue; }
        if edges.len() >= GRAPH_EDGE_CAP {
            truncated = true;
            break;
        }
        let s = crate::federation::repo_id::GlobalId::parse(&e.source_id).ok();
        let t = crate::federation::repo_id::GlobalId::parse(&e.target_id).ok();
        let cross_repo = match (s, t) {
            (Some(a), Some(b)) => a.repo_id() != b.repo_id(),
            _ => false,
        };
        edges.push(GraphEdge {
            source: e.source_id,
            target: e.target_id,
            edge_type: kind,
            cross_repo,
        });
    }

    Ok(WorkspaceGraph { nodes, edges, truncated })
}
