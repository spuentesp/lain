//! Query handler for graph operations
//!
//! Provides query_graph and describe_schema tools.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::{Executor, QuerySpec};
use crate::server::coordination::CoordinationProxy;
use crate::server::presence::PresenceRegistry;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::sync::Arc;

/// Query federation-wide so consumer -> provider bindings are visible even
/// when the tool context is bound to a single repository's local graph.
pub fn query_graph_context(
    ctx: &crate::tools::registry::ToolContext,
    args: &Map<String, Value>,
) -> Result<String, LainError> {
    let graph;
    let mut services = Vec::new();
    let mut roots = std::collections::HashMap::new();
    let selected = if let Some(federation) = &ctx.federation {
        let rejoin = if ctx.snapshot.is_none() {
            federation.rejoin_contracts_if_dirty().map(|_| ())
        } else {
            Ok(())
        };
        let backend = federation.backend();
        let view = if rejoin.is_err() {
            refresh_query_view(backend.as_ref(), rejoin, 0)?
        } else {
            // Hold the projection lock while pairing the snapshot generation
            // with its materialized graph, including cache hits.
            federation
                .with_contract_snapshot(|snapshot, backend| {
                    backend.query_graph_view_at(snapshot as *const _ as usize)
                })
                .unwrap_or_else(|| backend.query_graph_view())?
        };
        graph = if let Some(repo) = args.get("repo_id").and_then(Value::as_str) {
            scoped_query_view(&view, &ctx.graph, repo)?
        } else {
            view
        };
        for (id, _) in federation.list_repos() {
            if let Some(repo) = federation.get_repo(&id) {
                roots.insert(id.to_string(), repo.source().local_path().to_path_buf());
            }
        }
        if let Some(config) = federation.contract_config() {
            services = config.services.clone();
        }
        &graph
    } else {
        &ctx.graph
    };
    query_graph_with_services(
        &ctx.workspace,
        selected,
        &ctx.embedder,
        &ctx.embedding_cache,
        &ctx.presence,
        &ctx.occupancy,
        Some(args),
        ctx.tuning.ingestion.default_query_limit,
        ctx.snapshot.is_some(),
        services,
        roots,
    )
}

/// Build the `occupancy` JSON summary attached to `query_graph` and
/// `explain_symbol` results. Lists every interactive agent and the set
/// of files they've claimed (any intent). Agents with no claims show
/// up with an empty `files` list rather than being omitted — agents
/// are the unit of "who's around", so dropping one would make a quiet
/// agent disappear from `list_active_agents` as soon as they released
/// their last claim.
///
/// This is intentionally local-only: federation-wide occupancy would
/// require a separate per-repo mapping we don't track yet, and the
/// tool doc describes the result as "active agents in this workspace".
fn occupancy_payload(presence: &PresenceRegistry, occupancy: &CoordinationProxy) -> Value {
    let active = presence.list_active(false);
    let mut agents_json: Vec<Value> = Vec::with_capacity(active.len());
    for sess in &active {
        let claims = occupancy.list_for_agent(sess.name.as_str());
        let mut files: Vec<String> = claims
            .iter()
            .map(|c| c.path.to_string_lossy().to_string())
            .collect();
        files.sort();
        agents_json.push(serde_json::json!({
            "agent_id": sess.id.as_str(),
            "name": sess.name,
            "files": files,
        }));
    }
    serde_json::json!({ "active_agents": agents_json })
}

/// Execute a query against the graph using the ops array interface.
///
/// The returned JSON has a top-level `occupancy` key carrying the
/// active-agents summary.
/// When no agents have registered (the default for the sidecar /
/// read-only executor), `active_agents` is an empty list.
#[allow(clippy::too_many_arguments)] // one more than the lint's 7; the
                                     // alternative is a parameter struct used by exactly one call site.
pub fn query_graph(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<lru::LruCache<String, Vec<f32>>>>,
    presence: &PresenceRegistry,
    occupancy: &CoordinationProxy,
    arguments: Option<&Map<String, Value>>,
    default_limit: usize,
) -> Result<String, LainError> {
    query_graph_scoped(
        workspace,
        graph,
        embedder,
        embedding_cache,
        presence,
        occupancy,
        arguments,
        default_limit,
        false,
    )
}

pub fn query_graph_scoped(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<lru::LruCache<String, Vec<f32>>>>,
    presence: &PresenceRegistry,
    occupancy: &CoordinationProxy,
    arguments: Option<&Map<String, Value>>,
    default_limit: usize,
    historical: bool,
) -> Result<String, LainError> {
    query_graph_with_services(
        workspace,
        graph,
        embedder,
        embedding_cache,
        presence,
        occupancy,
        arguments,
        default_limit,
        historical,
        Vec::new(),
        std::collections::HashMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn query_graph_with_services(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<lru::LruCache<String, Vec<f32>>>>,
    presence: &PresenceRegistry,
    occupancy: &CoordinationProxy,
    arguments: Option<&Map<String, Value>>,
    default_limit: usize,
    historical: bool,
    services: Vec<crate::federation::contracts::config::ServiceDecl>,
    roots: std::collections::HashMap<String, std::path::PathBuf>,
) -> Result<String, LainError> {
    let mut executor =
        Executor::with_default_limit(graph, embedder, embedding_cache, workspace, default_limit)
            .with_services(services)
            .with_repo_workspaces(roots);

    // Parse query spec from arguments.
    // Accepts: {"spec": {...}} (per docs/query-language.md), {"query": {...}} (legacy),
    // or the spec fields directly as top-level arguments.
    let spec = if let Some(args) = arguments {
        if let Some(spec_val) = args.get("spec").or_else(|| args.get("query")) {
            serde_json::from_value(spec_val.clone())
                .map_err(|e| LainError::InvalidArgument(e.to_string()))?
        } else {
            // User provided unwrapped query spec directly as arguments
            serde_json::from_value(Value::Object(args.clone()))
                .map_err(|e| LainError::InvalidArgument(e.to_string()))?
        }
    } else {
        QuerySpec::default()
    };
    if historical
        && spec
            .ops
            .iter()
            .any(|op| matches!(op, crate::query::spec::GraphOp::SemanticFilter(_)))
    {
        return Err(LainError::InvalidArgument(
            "semantic_filter is not supported for named snapshots".to_string(),
        ));
    }

    let result = executor.execute(&spec)?;

    // Round-trip the executor's `QueryResult` through `serde_json::Value`
    // so we can splice the occupancy summary into the same JSON object
    // (rather than wrapping the whole thing in another envelope).
    let mut value: Value = serde_json::to_value(&result).map_err(LainError::Json)?;
    match value.as_object_mut() {
        Some(obj) => {
            obj.insert(
                "occupancy".to_string(),
                occupancy_payload(presence, occupancy),
            );
        }
        None => {
            // Executor should always return an object, but defend
            // against future shape changes by wrapping rather than
            // dropping the multiplayer signal.
            value = serde_json::json!({
                "result": value,
                "occupancy": occupancy_payload(presence, occupancy),
            });
        }
    }

    serde_json::to_string_pretty(&value).map_err(LainError::Json)
}

/// Describe the graph schema for LLM session initialization
pub fn describe_schema() -> Result<String, LainError> {
    let schema = crate::query::schema::describe_schema();
    serde_json::to_string_pretty(&schema).map_err(LainError::Json)
}

/// Keep this repository's symbols under their local IDs, plus the endpoints
/// of bindings touching the repository. Foreign IDs remain globally unique.
fn scoped_query_view(
    graph: &GraphDatabase,
    local: &GraphDatabase,
    repo: &str,
) -> Result<GraphDatabase, LainError> {
    use crate::federation::repo_id::{GlobalId, RepoId};
    use crate::schema::EdgeType;
    use std::collections::{HashMap, HashSet};
    let rid = RepoId::new(repo)?;
    let local_guard = local.graph_ref_for_read();
    let ids: HashMap<_, _> = local_guard
        .node_weights()
        .map(|n| {
            (
                GlobalId::new(&rid, n.node_type.clone(), &n.path, &n.name, n.line_start)
                    .to_string(),
                n.id.clone(),
            )
        })
        .collect();
    let stored = graph.graph_ref_for_read();
    let own: HashSet<_> = stored
        .node_weights()
        .filter(|n| n.repo_id.as_deref() == Some(repo) || ids.contains_key(&n.id))
        .map(|n| n.id.clone())
        .collect();
    let mut retained = own.clone();
    for edge in stored.edge_weights() {
        if edge.edge_type == EdgeType::Binds
            && (own.contains(&edge.source_id) || own.contains(&edge.target_id))
        {
            retained.insert(edge.source_id.clone());
            retained.insert(edge.target_id.clone());
        }
    }
    let rewrite = |id: &String| ids.get(id).cloned().unwrap_or_else(|| id.clone());
    let view = GraphDatabase::empty_writable();
    view.upsert_nodes_batch(
        stored
            .node_weights()
            .filter(|n| retained.contains(&n.id))
            .map(|n| {
                let mut node = crate::federation::graph_backend::query_node(n);
                node.id = rewrite(&node.id);
                node
            })
            .collect(),
    )?;
    let edges: Vec<_> = stored
        .edge_weights()
        .filter(|e| {
            retained.contains(&e.source_id)
                && retained.contains(&e.target_id)
                && ((own.contains(&e.source_id) && own.contains(&e.target_id))
                    || (matches!(
                        e.edge_type,
                        EdgeType::Binds | EdgeType::Consumes | EdgeType::SendsHttp
                    ) && (own.contains(&e.source_id) || own.contains(&e.target_id))))
        })
        .map(|e| {
            let mut edge = e.clone();
            edge.source_id = rewrite(&edge.source_id);
            edge.target_id = rewrite(&edge.target_id);
            edge
        })
        .collect();
    view.insert_edges_batch(&edges)?;
    Ok(view)
}

#[cfg(test)]
mod review_g_tests {
    use super::*;
    use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

    #[test]
    fn review_g_repo_scope_keeps_local_ids_and_binding_neighbours() {
        let local = GraphDatabase::empty_writable();
        let global = GraphDatabase::empty_writable();
        let consumer = GraphNode::new(
            NodeType::Function,
            "graphql_call".into(),
            "client.ts".into(),
        );
        local.upsert_node(consumer.clone()).unwrap();
        let mut projected = consumer.clone();
        projected.id = "storefront:Function:client.ts:graphql_call:0".into();
        projected.repo_id = Some("storefront".into());
        let mut provider = GraphNode::new(NodeType::Function, "saleor".into(), "api.py".into());
        provider.repo_id = Some("saleor".into());
        provider.id = "saleor:Function:api.py:saleor:0".into();
        let unrelated = GraphNode::new(NodeType::Function, "unrelated".into(), "other.py".into());
        global
            .upsert_nodes_batch(vec![projected.clone(), provider.clone(), unrelated])
            .unwrap();
        global
            .insert_edge(&GraphEdge::new(
                EdgeType::Binds,
                projected.id,
                provider.id.clone(),
            ))
            .unwrap();
        let scoped = scoped_query_view(&global, &local, "storefront").unwrap();
        let spec: QuerySpec = serde_json::from_value(serde_json::json!({"ops":[
            {"op":"find","id":consumer.id}, {"op":"connect","edge":"Binds"}
        ]}))
        .unwrap();
        let embedder = NlpEmbedder::new_stub();
        let cache = Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(8).unwrap(),
        )));
        let result = Executor::new(&scoped, &embedder, &cache, std::path::Path::new("."))
            .execute(&spec)
            .unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.nodes[0].id, provider.id);
        assert_eq!(scoped.get_all_nodes().len(), 2);
        assert!(scoped.get_node(&consumer.id).unwrap().is_some());
        assert_eq!(
            scoped.get_neighbors(&consumer.id, petgraph::Direction::Outgoing)[0]
                .0
                .id,
            provider.id
        );
    }
}

fn refresh_query_view(
    backend: &dyn crate::federation::graph_backend::GraphBackend,
    rejoin: Result<(), LainError>,
    generation: usize,
) -> Result<GraphDatabase, LainError> {
    if let Err(error) = rejoin {
        tracing::warn!(%error, "contract rejoin failed; using last query view");
        if let Some(view) = backend.last_query_graph_view() {
            return Ok(view);
        }
    }
    backend.query_graph_view_at(generation)
}

#[cfg(test)]
#[test]
fn review_g_failed_rejoin_keeps_query_available() {
    use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
    let backend = PetgraphBackend::ephemeral(std::path::Path::new("unused"));
    let cached = backend.query_graph_view().unwrap();
    let view = refresh_query_view(
        &backend,
        Err(LainError::InvalidArgument("injected rejoin failure".into())),
        0,
    )
    .unwrap();
    assert!(std::ptr::eq(
        &*cached.graph_ref_for_read(),
        &*view.graph_ref_for_read()
    ));
}
