//! Query handler for graph operations
//!
//! Provides query_graph and describe_schema tools.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::{Executor, QuerySpec};
use crate::server::presence::{OccupancyMap, PresenceRegistry};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

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
fn occupancy_payload(presence: &PresenceRegistry, occupancy: &OccupancyMap) -> Value {
    let active = presence.list_active(false);
    let mut agents_json: Vec<Value> = Vec::with_capacity(active.len());
    for sess in &active {
        let claims = occupancy.list_for_agent(&sess.id);
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
/// active-agents summary described in
/// `docs/superpowers/plans/2026-08-15-lain-multiplayer-awareness.md`.
/// When no agents have registered (the default for the sidecar /
/// read-only executor), `active_agents` is an empty list.
#[allow(clippy::too_many_arguments)] // one more than the lint's 7; the
// alternative is a parameter struct used by exactly one call site.
pub fn query_graph(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    presence: &PresenceRegistry,
    occupancy: &OccupancyMap,
    arguments: Option<&Map<String, Value>>,
    default_limit: usize,
) -> Result<String, LainError> {
    let mut executor =
        Executor::with_default_limit(graph, embedder, embedding_cache, workspace, default_limit);

    // Parse query spec from arguments.
    // Accepts: {"spec": {...}} (per docs/query-language.md), {"query": {...}} (legacy),
    // or the spec fields directly as top-level arguments.
    let spec = if let Some(args) = arguments {
        if let Some(spec_val) = args.get("spec").or_else(|| args.get("query")) {
            serde_json::from_value(spec_val.clone()).map_err(|e| LainError::Json(e))?
        } else {
            // User provided unwrapped query spec directly as arguments
            serde_json::from_value(Value::Object(args.clone())).map_err(|e| LainError::Json(e))?
        }
    } else {
        QuerySpec::default()
    };

    let result = executor
        .execute(&spec)
        .map_err(|e| LainError::Graph(e.to_string()))?;

    // Round-trip the executor's `QueryResult` through `serde_json::Value`
    // so we can splice the occupancy summary into the same JSON object
    // (rather than wrapping the whole thing in another envelope).
    let mut value: Value = serde_json::to_value(&result)
        .map_err(|e| LainError::Json(e))?;
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

    serde_json::to_string_pretty(&value).map_err(|e| LainError::Json(e))
}

/// Describe the graph schema for LLM session initialization
pub fn describe_schema() -> Result<String, LainError> {
    let schema = crate::query::schema::describe_schema();
    serde_json::to_string_pretty(&schema).map_err(|e| LainError::Json(e))
}
