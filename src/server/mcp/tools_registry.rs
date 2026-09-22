//! Inventory-registered MCP tool wrappers and the `McpToolEntry` they
//! submit. Extracted from `handler.rs` so the LainMcpServer + HTTP
//! router file stays focused on its own concerns (ServerHandler impl,
//! JSON-RPC envelope, `/health` and `/hook` routes, the SSE /overlay
//! bridge, and the SPA assets).
//!
//! Pattern: every wrapper is a free function with no captures, so
//! `inventory::submit!` can carry it at static-init time. `McpContext`
//! supplies the per-call state (server, federation, workspaces,
//! presence layer, etc.) — wrappers dispatch on the context, not the
//! module. The `declare_presence_tool!`, `declare_audit_tool!`, and the
//! federation/workspace macros collapse the per-tool boilerplate
//! from 4 lines to a single `declare_*_tool!(name, "tool", runner)`
//! invocation.

use crate::federation::federated_index::FederatedIndex;
use parking_lot::RwLock;
use serde_json::Map;
use std::sync::Arc;

use super::handler::McpContext;

/// Inventory entry: a (name, handler) pair. The handler is a free
/// `fn` pointer (no captures) so `inventory::collect!` can carry it
/// at static-init time and the dispatcher (`dispatch_tool_call`) can
/// iterate the collection without owning anything.
pub struct McpToolEntry {
    pub name: &'static str,
    pub handler: fn(&McpContext, serde_json::Value) -> Result<serde_json::Value, String>,
}
inventory::collect!(McpToolEntry);

/// Wrap a handler result into the `(text, is_error)` shape every
/// `dispatch_tool_call` arm returns. Centralizes the serialization
/// fallback so the per-tool wrapper functions stay short.
///
/// `is_error` is `true` whenever the result text could not be derived
/// from the handler's intent — including a `serde_json` serialization
/// failure of an otherwise-`Ok` payload. A tool that returned
/// `Ok(value)` but failed to serialize that value still hasn't produced
/// its result, and an agent that ignores `is_error` and reads the
/// text would otherwise see a misleading "serialization error" string
/// presented as if it were a successful payload.
pub(crate) fn tool_result(name: &str, result: Result<serde_json::Value, String>) -> (String, bool) {
    match result {
        Ok(v) => match serde_json::to_string(&v) {
            Ok(s) => (s, false),
            Err(e) => (format!("{name}: serialization error: {e}"), true),
        },
        Err(e) => (format!("{name}: {e}"), true),
    }
}

/// Walk the inventory collection looking for `name` and dispatch.
///
/// Returns `None` if no entry matches — the dispatcher then falls
/// through to its match ladder for the federation / workspace tools
/// that haven't migrated to the inventory pattern yet.
pub(crate) fn invoke_inventory(
    ctx: &McpContext,
    name: &str,
    args_map: Map<String, serde_json::Value>,
) -> Option<Result<serde_json::Value, String>> {
    let value = serde_json::Value::Object(args_map.into_iter().collect());
    for entry in inventory::iter::<McpToolEntry>() {
        if entry.name == name {
            return Some((entry.handler)(ctx, value));
        }
    }
    None
}


fn server_status_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(ctx.status.render()).map_err(|e| e.to_string())
}

fn list_recent_projects_handler(
    _ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let list =
        crate::server::mcp::federation_tools::list_recent_projects().map_err(|e| e.to_string())?;
    serde_json::to_value(list).map_err(|e| e.to_string())
}

fn get_reload_status_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bus = ctx
        .reload_bus
        .ok_or_else(|| "reload bus not configured on this server".to_string())?;
    let payload = crate::server::mcp::federation_tools::get_reload_status(bus);
    serde_json::to_value(payload).map_err(|e| e.to_string())
}

fn request_reload_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bus = ctx
        .reload_bus
        .ok_or_else(|| "reload bus not configured on this server".to_string())?;
    let payload =
        crate::server::mcp::federation_tools::request_reload(bus).map_err(|e| e.to_string())?;
    serde_json::to_value(payload).map_err(|e| e.to_string())
}

/// Generate a free-function wrapper + `inventory::submit!` for a
/// presence-style tool. The wrapper is a free function (no captures),
/// so it can sit in the inventory static; the runner is captured by
/// the macro's expansion of `$runner` into the wrapper body.
macro_rules! declare_presence_tool {
    ($wrapper:ident, $name:literal, $runner:path) => {
        fn $wrapper(
            ctx: &McpContext,
            args: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            match ctx.server {
                Some(server) => $runner(server, args),
                None => Err("presence layer not configured on this server".to_string()),
            }
        }
        inventory::submit!(McpToolEntry {
            name: $name,
            handler: $wrapper,
        });
    };
}

declare_presence_tool!(
    register_agent_handler,
    "register_agent",
    crate::server::mcp::presence_tools::run_register_agent
);
declare_presence_tool!(
    heartbeat_handler,
    "heartbeat",
    crate::server::mcp::presence_tools::run_heartbeat
);
declare_presence_tool!(
    list_active_agents_handler,
    "list_active_agents",
    crate::server::mcp::presence_tools::run_list_active_agents
);
declare_presence_tool!(
    who_am_i_handler,
    "who_am_i",
    crate::server::mcp::presence_tools::run_who_am_i
);
declare_presence_tool!(
    list_subagents_handler,
    "list_subagents",
    crate::server::mcp::presence_tools::run_list_subagents
);
declare_presence_tool!(
    claim_files_handler,
    "claim_files",
    crate::server::mcp::presence_tools::run_claim_files
);
declare_presence_tool!(
    release_files_handler,
    "release_files",
    crate::server::mcp::presence_tools::run_release_files
);
declare_presence_tool!(
    list_occupancy_handler,
    "list_occupancy",
    crate::server::mcp::presence_tools::run_list_occupancy
);
declare_presence_tool!(
    my_claims_handler,
    "my_claims",
    crate::server::mcp::presence_tools::run_my_claims
);
declare_presence_tool!(
    detect_overlap_handler,
    "detect_overlap",
    crate::server::mcp::presence_tools::run_detect_overlap
);
declare_presence_tool!(
    get_world_state_handler,
    "get_world_state",
    crate::server::mcp::presence_tools::run_get_world_state
);

// The five annotation/handoff tools (M4 §4.3) fit the same
// `&LainServer` + `Value` -> `Result<Value, String>` shape as the
// presence tools above, so they register through the same macro
// rather than a sixth near-identical one.
declare_presence_tool!(
    add_annotation_handler,
    "add_annotation",
    crate::server::mcp::annotation_tools::run_add_annotation
);
declare_presence_tool!(
    list_annotations_handler,
    "list_annotations",
    crate::server::mcp::annotation_tools::run_list_annotations
);
declare_presence_tool!(
    resolve_annotation_handler,
    "resolve_annotation",
    crate::server::mcp::annotation_tools::run_resolve_annotation
);
declare_presence_tool!(
    leave_handoff_note_handler,
    "leave_handoff_note",
    crate::server::mcp::annotation_tools::run_leave_handoff_note
);
declare_presence_tool!(
    get_pending_handoffs_handler,
    "get_pending_handoffs",
    crate::server::mcp::annotation_tools::run_get_pending_handoffs
);

// Intent-layer tools. Same shape as the presence tools — the
// runner signature is `fn(&LainServer, Value) -> Result<Value,
// String>` — so they register through `declare_presence_tool!`
// and reach `tools/call` via the inventory iteration in
// `dispatch_tool_call`. Before this commit the three tools
// `lain_intent`, `list_active_intents`, and `unregister_agent`
// were matched in a direct-dispatch arm because the inventory
// section reportedly didn't reach the production binary. That
// turned the dispatcher back into a stringly-typed match ladder
// and tripped `scripts/check-mcp-dispatch-shape.py`, which is
// the load-bearing guardrail from
// `docs/CONTRIBUTING_AGENTS.md#inventory-pattern`. Routing them
// through the same macro as the other presence tools removes
// the match arms and the guardrail violation in one move.
declare_presence_tool!(
    lain_intent_handler,
    "lain_intent",
    crate::server::mcp::intent_tools::run_lain_intent
);
declare_presence_tool!(
    list_active_intents_handler,
    "list_active_intents",
    crate::server::mcp::intent_tools::run_list_active_intents
);
declare_presence_tool!(
    unregister_agent_handler,
    "unregister_agent",
    crate::server::mcp::presence_tools::run_unregister_agent
);

/// Same shape for the audit tools; the runner signature differs only
/// in the domain module.
macro_rules! declare_audit_tool {
    ($wrapper:ident, $name:literal, $runner:path) => {
        fn $wrapper(
            ctx: &McpContext,
            args: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            match ctx.server {
                Some(server) => $runner(server, args),
                None => Err("audit layer not configured on this server".to_string()),
            }
        }
        inventory::submit!(McpToolEntry {
            name: $name,
            handler: $wrapper,
        });
    };
}

declare_audit_tool!(
    get_audit_log_handler,
    "get_audit_log",
    crate::server::mcp::audit_tools::run_get_audit_log
);
declare_audit_tool!(
    get_recent_activity_handler,
    "get_recent_activity",
    crate::server::mcp::audit_tools::run_get_recent_activity
);

inventory::submit!(McpToolEntry {
    name: "get_server_status",
    handler: server_status_handler
});
inventory::submit!(McpToolEntry {
    name: "list_recent_projects",
    handler: list_recent_projects_handler
});
inventory::submit!(McpToolEntry {
    name: "get_reload_status",
    handler: get_reload_status_handler
});
inventory::submit!(McpToolEntry {
    name: "request_reload",
    handler: request_reload_handler
});

// -------------------------------------------------------------------------
// Federation + workspace tool wrappers (Phase 3.2 followup).
//
// Like the presence/audit macros above, these declare a free-function
// wrapper + `inventory::submit!` block. Federation tools need
// `ctx.federation` (a `FederatedIndex`); workspace tools need
// `ctx.workspaces` (an `Arc<RwLock<WorkspacesFile>>`); some need both
// (workspace tools that resolve to a repo set).
// -------------------------------------------------------------------------

fn fed_required<'a>(ctx: &'a McpContext<'a>) -> Result<&'a FederatedIndex, String> {
    ctx.federation
        .ok_or_else(|| "federation not configured on this server".to_string())
}

fn workspaces_required<'a>(
    ctx: &'a McpContext<'a>,
) -> Result<&'a Arc<RwLock<crate::federation::workspace::WorkspacesFile>>, String> {
    ctx.workspaces
        .ok_or_else(|| "workspaces not configured on this server".to_string())
}

fn args_map(
    args: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, String> {
    args.as_object()
        .ok_or_else(|| "args must be a JSON object".to_string())
}

fn list_repos_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let repos = crate::server::mcp::federation_tools::list_repos(fed);
    serde_json::to_value(repos).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "list_repos",
    handler: list_repos_handler
});

fn get_repo_info_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let map = args_map(&args)?;
    let repo_id_str = map
        .get("repo_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required argument: repo_id".to_string())?;
    let rid = crate::federation::repo_id::RepoId::new(repo_id_str).map_err(|e| e.to_string())?;
    let info = crate::server::mcp::federation_tools::get_repo_info(fed, &rid)
        .map_err(|e| e.to_string())?;

    // Enrich with the count of agents currently editing this repo, so
    // the SPA can render the badge without a second round-trip.
    let mut value = serde_json::to_value(&info).map_err(|e| e.to_string())?;
    let active_edits = ctx
        .server
        .map(|s| {
            let occupancy = s.occupancy();
            let active = s.presence().list_active(false);
            active
                .iter()
                .filter(|sess| !occupancy.list_for_agent(&sess.id).is_empty())
                .count()
        })
        .unwrap_or(0);
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "active_edits".to_string(),
            serde_json::Value::Number(active_edits.into()),
        );
    }
    Ok(value)
}
inventory::submit!(McpToolEntry {
    name: "get_repo_info",
    handler: get_repo_info_handler
});

fn get_federation_health_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let health = crate::server::mcp::federation_tools::get_federation_health(fed);
    serde_json::to_value(health).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_federation_health",
    handler: get_federation_health_handler
});

fn search_org_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let map = args_map(&args)?;
    let query =
        crate::server::tools::utils::required_str_arg(map, "query").map_err(|e| e.to_string())?;
    let limit: usize = match map.get("limit") {
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(u) => u as usize,
            None => {
                return Err("Invalid argument: limit must be a non-negative integer".to_string());
            }
        },
        _ => 10,
    };
    let matches = crate::server::mcp::federation_tools::search_org(fed, &query, limit);
    serde_json::to_value(matches).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "search_org",
    handler: search_org_handler
});

fn cross_repo_blast_radius_common(
    fed: &FederatedIndex,
    map: &serde_json::Map<String, serde_json::Value>,
    depth_range: std::ops::Range<u32>,
) -> Result<crate::server::mcp::federation_tools::CrossRepoBlastRadius, String> {
    let symbol =
        crate::server::tools::utils::required_str_arg(map, "symbol").map_err(|e| e.to_string())?;
    crate::server::mcp::federation_tools::get_cross_repo_blast_radius(fed, &symbol, depth_range)
        .map_err(|e| e.to_string())
}

fn get_cross_repo_blast_radius_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let map = args_map(&args)?;
    let depth_str =
        crate::server::tools::utils::required_str_arg(map, "depth").map_err(|e| e.to_string())?;
    let depth =
        crate::server::mcp::handler::parse_depth_range(&depth_str).map_err(|e| e)?;
    let result = cross_repo_blast_radius_common(fed, map, depth)?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_cross_repo_blast_radius",
    handler: get_cross_repo_blast_radius_handler
});

fn get_cross_repo_blast_radius_for_repo_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let map = args_map(&args)?;
    let depth_str =
        crate::server::tools::utils::required_str_arg(map, "depth").map_err(|e| e.to_string())?;
    let depth =
        crate::server::mcp::handler::parse_depth_range(&depth_str).map_err(|e| e)?;
    let symbol =
        crate::server::tools::utils::required_str_arg(map, "symbol").map_err(|e| e.to_string())?;
    let repo_id_str = map
        .get("repo_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required argument: repo_id".to_string())?;
    let result = crate::server::mcp::federation_tools::get_cross_repo_blast_radius_for_repo(
        fed,
        repo_id_str,
        &symbol,
        depth,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_cross_repo_blast_radius_for_repo",
    handler: get_cross_repo_blast_radius_for_repo_handler
});

fn list_workspaces_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let workspaces_lock = workspaces_required(ctx)?;
    let active = crate::state::ActiveWorkspace::load().ok().flatten();
    let list = crate::server::mcp::federation_tools::list_workspaces(
        &workspaces_lock.read(),
        active.as_ref(),
    );
    serde_json::to_value(list).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "list_workspaces",
    handler: list_workspaces_handler
});

fn get_active_workspace_handler(
    ctx: &McpContext,
    _args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let workspaces_lock = workspaces_required(ctx)?;
    let info =
        crate::server::mcp::federation_tools::get_active_workspace(fed, &workspaces_lock.read())
            .map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_active_workspace",
    handler: get_active_workspace_handler
});

fn get_workspace_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let workspaces_lock = workspaces_required(ctx)?;
    let map = args_map(&args)?;
    let name =
        crate::server::tools::utils::required_str_arg(map, "name").map_err(|e| e.to_string())?;
    let detail =
        crate::server::mcp::federation_tools::get_workspace(fed, &workspaces_lock.read(), &name)
            .map_err(|e| e.to_string())?;
    serde_json::to_value(detail).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_workspace",
    handler: get_workspace_handler
});

fn get_workspace_graph_handler(
    ctx: &McpContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let fed = fed_required(ctx)?;
    let workspaces_lock = workspaces_required(ctx)?;
    let map = args_map(&args)?;
    let filter_str = map.get("filter").and_then(|v| v.as_str());
    let graph = crate::server::mcp::federation_tools::get_workspace_graph(
        fed,
        &workspaces_lock.read(),
        filter_str,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_value(graph).map_err(|e| e.to_string())
}
inventory::submit!(McpToolEntry {
    name: "get_workspace_graph",
    handler: get_workspace_graph_handler
});
