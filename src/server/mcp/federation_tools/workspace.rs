//! Workspace-aware MCP tools: `list_workspaces`, `get_active_workspace`,
//! `get_workspace`, `get_workspace_graph`. Read-only; depend on the
//! `WorkspacesFile` the server was constructed with and the live
//! `FederatedIndex`.

use super::dto::{
    ActiveWorkspaceInfo, WorkspaceDetail, WorkspaceGraph, WorkspaceInfo, WorkspaceRepoInfo,
};
use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::workspace::{WorkspaceSourceConfig, WorkspacesFile};
use crate::schema::{GraphEdge, GraphNode};
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
    workspaces
        .workspaces
        .iter()
        .map(|ws| {
            let is_active = active.as_ref().map(|a| a.name == ws.name).unwrap_or(false);
            WorkspaceInfo {
                name: ws.name.clone(),
                description: ws.description.clone(),
                source: source_label(&ws.source),
                member_count: ws.members.len(),
                is_active,
            }
        })
        .collect()
}

/// Identify the workspace whose member set exactly matches the loaded repo
/// ids in the federation. Returns `LainError::Workspace(...)` if no
/// workspace matches (no workspace was active, or the federation was
/// loaded without workspace filtering).
pub fn get_active_workspace(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
) -> Result<ActiveWorkspaceInfo, LainError> {
    let loaded: std::collections::HashSet<String> = fed
        .list_repos()
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    if loaded.is_empty() {
        return Err(LainError::Workspace(
            "no repos loaded; no active workspace".into(),
        ));
    }
    let active = workspaces
        .workspaces
        .iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| {
            LainError::Workspace(
                "federation loaded but no workspace matches the loaded repos".into(),
            )
        })?;
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
    let ws = workspaces
        .workspaces
        .iter()
        .find(|w| w.name == name)
        .ok_or_else(|| LainError::NotFound(format!("workspace {name}")))?;
    // Resolve path + health for each member from the federation, if loaded.
    let loaded = fed.list_repos();
    let mut members = Vec::with_capacity(ws.members.len());
    for m in &ws.members {
        let info = loaded.iter().find(|(id, _)| id.as_str() == m);
        let (path, health) = match info {
            Some((id, h)) => {
                let repo = fed.get_repo(id);
                let path = repo
                    .map(|r| r.source().local_path().display().to_string())
                    .unwrap_or_default();
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

/// Node types the dashboard's [`get_workspace_graph`] admits, as their
/// schema/`Debug` names (the same spelling `NodeType`'s `Display`
/// prints and `query_graph`'s `type:` selector takes). Named — not
/// inlined into the filter — so the protocol-coverage matrix can
/// reference the shipped set instead of re-describing it as string
/// literals that can drift.
///
/// `Schema` rides along so a hub's `PayloadSchema` link has a visible
/// endpoint: the edge filter keeps an edge only when both endpoints
/// are admitted (see `WORKSPACE_GRAPH_EDGE_TYPES`).
pub const WORKSPACE_GRAPH_NODE_TYPES: &[&str] = &["Function", "Method", "Class", "Topic", "Schema"];

/// Edge types [`get_workspace_graph`] admits — the call/import spine
/// plus the topic-hub legs (`Produces` / `Consumes`) and the hub's
/// `PayloadSchema` link to its payload definition. Same naming rule
/// as [`WORKSPACE_GRAPH_NODE_TYPES`]; the protocol-coverage matrix
/// references this set for its `view_edges` rows.
pub const WORKSPACE_GRAPH_EDGE_TYPES: &[&str] = &[
    "Calls",
    "Imports",
    "CrossRepoSameSymbol",
    "Produces",
    "Consumes",
    "PayloadSchema",
];

fn node_kind_str(s: &str) -> bool {
    WORKSPACE_GRAPH_NODE_TYPES.contains(&s)
}

fn edge_kind_str(s: &str) -> bool {
    WORKSPACE_GRAPH_EDGE_TYPES.contains(&s)
}

/// Per-workspace graph data for the dashboard's D3 force-directed view.
///
/// Filters to [`WORKSPACE_GRAPH_NODE_TYPES`] /
/// [`WORKSPACE_GRAPH_EDGE_TYPES`] — today `Function` / `Method` /
/// `Class` / `Topic` / `Schema` nodes and `Calls` / `Imports` /
/// `CrossRepoSameSymbol` / `Produces` / `Consumes` / `PayloadSchema`
/// edges. `CrossRepoSameSymbol` is the cross-repo peer-symbol edge
/// type and was missing from this allowlist, so it was silently
/// dropped here even once the backend produced it; `Topic` hubs and
/// their `Produces` / `Consumes` legs were dropped the same way until
/// the hub model landed. Hubs keep per-repo identity in this view: the
/// same `broker/topic` in two repos is two `Topic` nodes, linked at
/// the contract layer, not merged here. Marks edges as
/// `cross_repo: true` when source's repo_id differs from target's.
/// Caps at 5000 nodes / 10000 edges.
pub fn get_workspace_graph(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    filter: Option<&str>,
) -> Result<WorkspaceGraph, LainError> {
    // Identify the active workspace by intersecting loaded repos with
    // each workspace's member set. Errors if no match.
    let loaded: std::collections::HashSet<String> = fed
        .list_repos()
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    let active = workspaces
        .workspaces
        .iter()
        .find(|ws| {
            let ws_set: std::collections::HashSet<&String> = ws.members.iter().collect();
            ws_set.len() == loaded.len()
                && ws_set.iter().all(|m| loaded.contains(*m))
                && loaded.iter().all(|l| ws_set.contains(l))
        })
        .ok_or_else(|| {
            LainError::Workspace(
                "federation loaded but no workspace matches the loaded repos".into(),
            )
        })?;
    let members: std::collections::HashSet<String> = active.members.iter().cloned().collect();

    let all_nodes = fed.backend().list_nodes()?;
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut truncated = false;
    for n in all_nodes {
        let kind = format!("{:?}", n.node_type);
        if !node_kind_str(&kind) {
            continue;
        }
        let gid = crate::federation::repo_id::GlobalId::parse(&n.id).ok();
        let repo_id = gid
            .as_ref()
            .map(|g| g.repo_id().to_string())
            .unwrap_or_default();
        if !members.contains(&repo_id) {
            continue;
        }
        if let Some(f) = filter {
            if !n.name.contains(f) && !n.path.contains(f) {
                continue;
            }
        }
        if nodes.len() >= GRAPH_NODE_CAP {
            truncated = true;
            break;
        }
        let mut wire = GraphNode::new(n.node_type, n.name.clone(), n.path.clone());
        wire.id = n.id.clone();
        wire.repo_id = if repo_id.is_empty() {
            None
        } else {
            Some(repo_id)
        };
        nodes.push(wire);
    }
    let node_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let mut edges: Vec<GraphEdge> = Vec::new();
    let all_edges = fed.backend().all_edges()?;
    for e in all_edges {
        if !node_ids.contains(e.source_id.as_str()) || !node_ids.contains(e.target_id.as_str()) {
            continue;
        }
        let kind = format!("{:?}", e.edge_type);
        if !edge_kind_str(&kind) {
            continue;
        }
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
        let mut wire = GraphEdge::new(e.edge_type, e.source_id, e.target_id);
        wire.weight = e.weight;
        wire.cross_repo = cross_repo;
        edges.push(wire);
    }

    Ok(WorkspaceGraph {
        nodes,
        edges,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::{WORKSPACE_GRAPH_EDGE_TYPES, WORKSPACE_GRAPH_NODE_TYPES};

    /// The allowlist names are compared against `Debug`/`Display`
    /// strings at filter time, so a typo — or a renamed variant —
    /// would silently drop every node/edge of that kind instead of
    /// failing. The protocol-coverage matrix references these consts
    /// by name, which makes a silent rename twice as costly; this is
    /// the guard that turns it into a test failure.
    #[test]
    fn filter_set_names_are_real_schema_variants() {
        for name in WORKSPACE_GRAPH_NODE_TYPES {
            assert!(
                crate::schema::NodeType::all()
                    .iter()
                    .any(|k| format!("{k:?}") == *name),
                "WORKSPACE_GRAPH_NODE_TYPES names `{name}`, which is not a NodeType variant"
            );
        }
        for name in WORKSPACE_GRAPH_EDGE_TYPES {
            assert!(
                crate::schema::EdgeType::all()
                    .iter()
                    .any(|k| format!("{k:?}") == *name),
                "WORKSPACE_GRAPH_EDGE_TYPES names `{name}`, which is not an EdgeType variant"
            );
        }
    }

    /// The `get_workspace_graph` tool description hardcodes the two
    /// filter lists in prose ("Filters to A/B/C + D/E/F") while the
    /// allowlist lives in the two exported sets — same drift risk the
    /// coverage matrix's copied literals had, with no compile error to
    /// catch it. Pin the prose lists to the sets in BOTH directions
    /// (a new allowlist entry must be advertised; a reworded
    /// description must not invent or drop one) and keep every
    /// advertised name a real schema variant, so the two guards cannot
    /// each cover only half the picture.
    #[test]
    fn workspace_graph_description_advertises_the_real_filter_sets() {
        use crate::server::mcp::definitions::WORKSPACE_TOOL_DEFS;

        let desc = WORKSPACE_TOOL_DEFS
            .iter()
            .find(|d| d.name == "get_workspace_graph")
            .expect("get_workspace_graph is advertised")
            .description;
        let lists = desc
            .split_once("Filters to ")
            .expect("description advertises its type filters")
            .1
            .split_once('.')
            .expect("the filter sentence ends at the first period")
            .0;
        let (nodes_part, edges_part) = lists.split_once(" + ").expect("node list ` + ` edge list");
        let prose_nodes: Vec<&str> = nodes_part.split('/').collect();
        let prose_edges: Vec<&str> = edges_part.split('/').collect();

        assert_eq!(
            prose_nodes, WORKSPACE_GRAPH_NODE_TYPES,
            "the description's node list drifted from WORKSPACE_GRAPH_NODE_TYPES"
        );
        assert_eq!(
            prose_edges, WORKSPACE_GRAPH_EDGE_TYPES,
            "the description's edge list drifted from WORKSPACE_GRAPH_EDGE_TYPES"
        );
        for name in &prose_nodes {
            assert!(
                crate::schema::NodeType::all()
                    .iter()
                    .any(|k| format!("{k:?}") == **name),
                "the description advertises node type `{name}`, which is not a NodeType variant"
            );
        }
        for name in &prose_edges {
            assert!(
                crate::schema::EdgeType::all()
                    .iter()
                    .any(|k| format!("{k:?}") == **name),
                "the description advertises edge type `{name}`, which is not an EdgeType variant"
            );
        }
    }
}
