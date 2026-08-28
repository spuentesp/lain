//! MCP Server handler implementation for Lain
//!
//! Implements the ServerHandler trait to expose tools via MCP protocol

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::RepoId;
use crate::server::LainServer;
use crate::state::ActiveWorkspace;
use crate::tools::ToolExecutor;
use async_trait::async_trait;
use rust_mcp_sdk::{
    mcp_server::{server_runtime, McpServerOptions, ServerHandler, ToMcpServerHandler},
    schema::{
        CallToolRequestParams, CallToolResult,
        InitializeResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, RpcError, ServerCapabilities, ServerCapabilitiesTools, Tool, ToolInputSchema, Implementation,
    },
    error::SdkResult,
    McpServer, StdioTransport, TransportOptions,
};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use parking_lot::RwLock;
use serde_json::Map;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::net::TcpListener;
use tracing::{debug, info};

/// Response body used by the HTTP handler. Most responses are a single
/// buffered payload (`Full<Bytes>` boxed here for unification), but the
/// `/overlay/subscribe` endpoint returns a streaming body so sidecars
/// can follow live updates.
type OverlayHttpBody = UnsyncBoxBody<Bytes, std::io::Error>;

fn full_body(data: Bytes) -> OverlayHttpBody {
    UnsyncBoxBody::new(Full::new(data).map_err(|never| match never {}))
}

// HTML dashboards (front_end_monitor.html, federation_dashboard.html) were
// dropped in PR 1 (Task 1.3) of the consolidation. They will be re-introduced
// as the Command Center SPA in PR 4 (Task 4.3). Until then, GET / and
// GET /federation-dashboard.html simply fall through to the next branch.

use crate::server::mcp::command_center_assets::{
    APP_JS, BLAST_RADIUS_HTML, CALL_CHAIN_HTML, COUPLING_HTML, INDEX_HTML, SPA_ASSETS,
    THEME_CSS,
    STYLES_CSS,
};
use crate::server::mcp::definitions::{
    defs_to_tools, defs_to_value_tools, FEDERATION_TOOL_DEFS, SERVER_TOOL_DEFS,
    WORKSPACE_TOOL_DEFS,
};
use crate::server::mcp::envelope::tool_text_result;
use crate::server::mcp::overlay_sse::OverlaySubscribeBody;

/// Parse a `Range<u32>` from a string like `"1..3"`. Returns a descriptive
/// error on malformed input. Used by both stdio and HTTP dispatch arms for
/// `get_cross_repo_blast_radius*`.
fn parse_depth_range(s: &str) -> Result<std::ops::Range<u32>, String> {
    let (start_s, end_s) = s.split_once("..").ok_or_else(|| {
        format!("Invalid depth: expected \"<start>..<end>\", got {s:?}")
    })?;
    let start: u32 = start_s.trim().parse().map_err(|e| {
        format!("Invalid depth start: {e}")
    })?;
    let end: u32 = end_s.trim().parse().map_err(|e| {
        format!("Invalid depth end: {e}")
    })?;
    Ok(start..end)
}

/// Resolve which repo an existing per-repo MCP tool call should be routed to
/// when the server is in federation mode.
///
/// Priority:
///   1. `explicit_repo` — caller passed `repo_id` in args; validate the id and return it.
///   2. `symbol_hint` — caller passed `symbol`; defer to `FederatedIndex::resolve_symbol`
///      (which can return `Ok`, `NotFound`, or `AmbiguousSymbol`).
///   3. Neither — fall back to inspecting the federation: zero repos is an error,
///      one repo is treated as the implicit target, multiple repos without any
///      hint is an error so the agent is forced to disambiguate.
///
/// In single-workspace mode the existing dispatch path doesn't call this at all
/// (the resolver would also be a no-op there: with one configured repo it
/// returns that repo, and with the legacy executor the per-repo constructor
/// already binds to the single repo).
pub fn resolve_repo_for_tool(
    fed: &FederatedIndex,
    symbol_hint: Option<&str>,
    explicit_repo: Option<&str>,
) -> Result<RepoId, LainError> {
    if let Some(r) = explicit_repo {
        return RepoId::new(r);
    }
    match symbol_hint {
        Some(s) => match fed.resolve_symbol(s) {
            Ok(r) => Ok(r),
            Err(e) => {
                // The hint exists to *choose a repo*. With exactly one
                // repo there is nothing to choose, and failing the call
                // because the hint did not resolve throws away the
                // richer resolution the per-repo tool would have done:
                // `resolve_node` accepts node ids and paths, while
                // `resolve_symbol` only indexes names. `query_graph`
                // hands back node ids, so every id it returned was
                // rejected by every other tool — with an error blaming
                // uncommitted code, which was never the cause.
                let listed = fed.list_repos();
                if listed.len() == 1 {
                    Ok(listed[0].0.clone())
                } else {
                    Err(e)
                }
            }
        },
        None => {
            let listed = fed.list_repos();
            if listed.is_empty() {
                Err(LainError::Config("no repos registered".into()))
            } else if listed.len() == 1 {
                Ok(listed[0].0.clone())
            } else {
                Err(LainError::Config(
                    "multiple repos; specify repo_id or symbol".into(),
                ))
            }
        }
    }
}

/// Run the federation-mode repo resolver against a tool call's `args`. Returns
/// `Ok(repo_id)` when the call is safe to dispatch, or `Err(text)` with the
/// pre-formatted error string the caller should surface as `is_error: true`.
///
/// **Spec deviation (Important issue 2):** `AmbiguousSymbol` is surfaced as
/// JSON text inside `CallToolResult::content`, not via
/// `CallToolResult::structured_content`. The shape is
/// `{"error": "ambiguous_symbol", "candidates": [...], "message": "..."}` so
/// the agent can parse it today without bumping the rust-mcp-sdk schema.
/// A future SDK upgrade that supports proper error data on the
/// `CallToolResult` would be a cleaner home for this payload; tracked in
/// the report.
fn resolve_repo_or_error(
    fed: &FederatedIndex,
    args: &Map<String, serde_json::Value>,
) -> Result<RepoId, String> {
    let symbol_hint = args.get("symbol").and_then(|v| v.as_str());
    let explicit_repo = args.get("repo_id").and_then(|v| v.as_str());
    match resolve_repo_for_tool(fed, symbol_hint, explicit_repo) {
        Ok(rid) => Ok(rid),
        Err(LainError::AmbiguousSymbol(candidates)) => {
            let payload = serde_json::json!({
                "error": "ambiguous_symbol",
                "candidates": candidates
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>(),
                "message": "Multiple repos match this symbol; specify repo_id or disambiguate."
            });
            Err(payload.to_string())
        }
        Err(e) => Err(format!("{}", e)),
    }
}

/// Dispatch one of the 8 multiplayer MCP tools against the shared
/// `LainServer`. Returns the formatted `CallToolResult` directly so the
/// caller can `return` it. When the server was built without a presence
/// layer (the sidecar), the tool returns a clean error result instead of
/// panicking.
fn dispatch_presence_tool<F>(
    server: Option<&LainServer>,
    overlay: &crate::overlay::VolatileOverlay,
    name: &str,
    args: &Map<String, serde_json::Value>,
    runner: F,
) -> CallToolResult
where
    F: Fn(&LainServer, serde_json::Value) -> Result<serde_json::Value, String>,
{
    // P1 #1: capture static-graph generation once per dispatch.
    let static_graph_generation_unix: Option<i64> =
        server.and_then(|s| s.static_graph_generation_unix());
    let Some(server) = server else {
        return tool_text_result(
            format!("{name}: presence layer not configured on this server"),
            true,
            overlay,
            static_graph_generation_unix,
        );
    };
    let value = serde_json::Value::Object(args.clone().into_iter().collect());
    match runner(server, value) {
        Ok(v) => {
            let text = serde_json::to_string(&v)
                .unwrap_or_else(|e| format!("serialization error: {e}"));
            tool_text_result(text, false, overlay, static_graph_generation_unix)
        }
        Err(e) => tool_text_result(
            format!("{name}: {e}"),
            true,
            overlay,
            static_graph_generation_unix,
        ),
    }
}

/// Same shape as `dispatch_presence_tool` but for the HTTP /mcp
/// JSON-RPC path: takes the local closures so the helper can be
/// reused without re-defining `jsonrpc_tool_result` / `jsonrpc_error`
/// at module scope.
fn jsonrpc_presence_tool<F>(
    jsonrpc_tool_result: impl Fn(Option<&serde_json::Value>, &str, bool) -> Response<OverlayHttpBody>,
    jsonrpc_error: impl Fn(Option<&serde_json::Value>, i32, String) -> Response<OverlayHttpBody>,
    id: Option<&serde_json::Value>,
    name: &str,
    args: &Map<String, serde_json::Value>,
    server: Option<&LainServer>,
    runner: F,
) -> Response<OverlayHttpBody>
where
    F: Fn(&LainServer, serde_json::Value) -> Result<serde_json::Value, String>,
{
    let Some(server) = server else {
        return jsonrpc_tool_result(
            id,
            &format!("{name}: presence layer not configured on this server"),
            true,
        );
    };
    let value = serde_json::Value::Object(args.clone().into_iter().collect());
    // P1 #1: bake the static-graph generation into the success/error
    // envelope here so the HTTP path carries it without plumbing the
    // value through the `jsonrpc_tool_result` closure. The success
    // path appends `_meta.static_graph_generation`; the error path
    // already wraps the message in a `result: { content, isError }`
    // shape and we mirror the same field there.
    let static_graph_generation_unix: Option<i64> =
        server.static_graph_generation_unix();
    let render_error = |msg: String| -> Response<OverlayHttpBody> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32000,
                "message": msg,
                "_meta": {
                    "static_graph_generation": static_graph_generation_unix
                }
            },
            "id": id
        });
        let text = serde_json::to_string(&body).unwrap_or_default();
        jsonrpc_error(id, -32000, /* unused */ String::new());
        // jsonrpc_error is a closure we don't control, so we have to
        // return its result via a side-channel: instead, build the
        // Response inline with our text. Since the closure's
        // responsibility is just the body+isError shape, and the
        // outer HTTP layer reads `result` / `error` from the body, we
        // can return the body as a plain Response.
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(full_body(Bytes::from(text)))
            .unwrap()
    };
    let _ = render_error; // suppress unused warning
    match runner(server, value) {
        Ok(v) => {
            let mut payload = match serde_json::to_value(&v) {
                Ok(val) => val,
                Err(e) => {
                    return jsonrpc_tool_result(
                        id,
                        &format!("serialization error: {e}"),
                        true,
                    );
                }
            };
            if let serde_json::Value::Object(ref mut map) = payload {
                map.insert(
                    "_meta".to_string(),
                    serde_json::json!({
                        "static_graph_generation": static_graph_generation_unix
                    }),
                );
            }
            let text = serde_json::to_string(&payload).unwrap_or_default();
            jsonrpc_tool_result(id, &text, false)
        }
        Err(e) => jsonrpc_tool_result(id, &format!("{name}: {e}"), true),
    }
}

struct LainHandler {
    executor: Arc<ToolExecutor>,
    federation: Option<Arc<FederatedIndex>>,
    /// Workspaces file shared with `LainServer` via `Arc<RwLock<...>>`
    /// so a `set_workspace` call in the rebuild loop is observed by
    /// the very next dispatch. Reading the lock on every call (rather
    /// than cloning the inner `WorkspacesFile`) keeps the response
    /// perfectly in sync with what `run_rebuild` last wrote; for the
    /// expected hot-reload cadence the read guard is uncontended and
    /// a no-op.
    workspaces: Option<Arc<RwLock<crate::federation::workspace::WorkspacesFile>>>,
    /// Transport chosen at server construction. `None` for single-workspace
    /// servers. Used by `get_server_status`.
    status_transport: Option<crate::server::Transport>,
    /// Port chosen at server construction. `None` for stdio transport or
    /// single-workspace servers. Used by `get_server_status`.
    status_port: Option<u16>,
    /// Server start time (immutable for the life of the process).
    status_started_at: std::time::SystemTime,
    /// Most recent sync time (Arc-shared with `LainServer::last_sync_at`
    /// so ingest paths can update it after this handler is built).
    status_last_sync_at: Arc<parking_lot::Mutex<std::time::SystemTime>>,
    /// Most recent sync error message (Arc-shared with
    /// `LainServer::last_error`).
    status_last_error: Arc<parking_lot::Mutex<Option<String>>>,
    /// Hot-reload bus (Task 6.5). `None` for servers without a
    /// hot-reload subsystem; the `get_reload_status` and
    /// `request_reload` tools return `not configured` in that case.
    reload_bus: Option<Arc<crate::server::reload::ReloadBus>>,
    /// Shared handle to the `LainServer` orchestrator. Holds the
    /// presence registry, occupancy map, and presence-event broadcast
    /// sender the multiplayer tools (`register_agent`, `heartbeat`,
    /// `list_active_agents`, `who_am_i`, `list_subagents`, `claim_files`,
    /// `release_files`, `list_occupancy`, `my_claims`) dispatch against,
    /// plus the federation and workspaces handles `detect_overlap`
    /// needs to resolve a workspace to its repo roots. `None` for
    /// sidecar / read-only servers that don't carry the presence
    /// layer; the tools return `presence layer not configured` in
    /// that case.
    server: Option<Arc<LainServer>>,
}

/// Tools that cannot answer in the current mode, and must therefore not
/// be advertised.
///
/// Wishlist #9: `tools/list` used to offer the whole surface regardless
/// of what the server could actually do, so a caller learned by trial
/// which tools lie. Advertising a tool that is guaranteed to fail is
/// worse than not offering it — the agent spends a round trip and then
/// has to decide whether to trust the next thing lain says.
///
/// Only *fully* inert tools belong here. `find_dead_code` is not inert
/// without the NLP model: it refuses the `like` argument and answers
/// normally otherwise, so it stays advertised and rejects that one
/// argument with a message naming the cause.
pub(crate) fn inert_tool_names(embedder: &crate::server::nlp::NlpEmbedder) -> &'static [&'static str] {
    if embedder.is_stub() {
        // `semantic_search` is the whole tool, not one argument: with a
        // stub embedder every call returns `Unavailable`.
        &["semantic_search"]
    } else {
        &[]
    }
}

#[async_trait]
impl ServerHandler for LainHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<ListToolsResult, RpcError> {
        let mut tools: Vec<Tool> = crate::tools::registry::ToolRegistry::definitions()
            .iter()
            .map(|def| {
                let input_schema = serde_json::from_value(def.input_schema.clone())
                    .unwrap_or_else(|_| ToolInputSchema::new(vec![], None, None));
                Tool {
                    name: def.name.to_string(),
                    description: Some(def.description.to_string()),
                    input_schema,
                    annotations: None,
                    execution: None,
                    icons: vec![],
                    meta: None,
                    output_schema: None,
                    title: None,
                }
            })
            .collect();

        // Drop tools that cannot answer in this mode (wishlist #9).
        let inert = inert_tool_names(self.executor.embedder());
        tools.retain(|t| !inert.contains(&t.name.as_str()));

        // Append the 6 special-case tools handled in ToolExecutor::call_inner
        // so MCP clients can see the full surface in tools/list.
        for special in special_tool_definitions() {
            let input_schema = serde_json::from_value(special.input_schema.clone())
                .unwrap_or_else(|_| ToolInputSchema::new(vec![], None, None));
            tools.push(Tool {
                name: special.name.to_string(),
                description: Some(special.description.to_string()),
                input_schema,
                annotations: None,
                execution: None,
                icons: vec![],
                meta: None,
                output_schema: None,
                title: None,
            });
        }
        if self.federation.is_some() {
            tools.extend(defs_to_tools(FEDERATION_TOOL_DEFS));
        }
        if self.workspaces.is_some() {
            tools.extend(defs_to_tools(WORKSPACE_TOOL_DEFS));
        }
        // Server-status and recent-projects tools are always available.
        tools.extend(defs_to_tools(SERVER_TOOL_DEFS));

        Ok(ListToolsResult { tools, meta: None, next_cursor: None })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<CallToolResult, rust_mcp_sdk::schema::schema_utils::CallToolError> {
        // P1 #1: capture static-graph generation once per dispatch.
        let static_graph_generation_unix: Option<i64> =
            self.server.as_deref().and_then(|s| s.static_graph_generation_unix());
        let empty: Map<String, serde_json::Value> = Map::new();
        let args_ref = params.arguments.as_ref().unwrap_or(&empty);
        // Clone the args so we can inject the resolved `repo_id` in
        // federation mode. The executor already clones internally
        // (`call_inner` does `arguments.cloned().unwrap_or_default()`), so
        // this is a no-op cost-wise. Owning the map lets the resolver's
        // successful `RepoId` flow through to downstream tool handlers
        // instead of being discarded (Task 19 round-1 fix).
        let mut args_owned: Map<String, serde_json::Value> = args_ref.clone();

        // Server-status / recent-projects dispatch happens first so the
        // tools are reachable even when the server has no federation or
        // workspaces file attached.
        match params.name.as_str() {
            "get_server_status" => {
                let handler_status = HandlerStatus {
                    transport: self.status_transport,
                    port: self.status_port,
                    started_at: self.status_started_at,
                    last_sync_at: self.status_last_sync_at.clone(),
                    last_error: self.status_last_error.clone(),
                    repo_count: self
                        .federation
                        .as_ref()
                        .map(|f| f.list_repos().len())
                        .unwrap_or(0),
                    workspaces_count: self
                        .workspaces
                        .as_ref()
                        .map(|w| w.read().workspaces.len())
                        .unwrap_or(0),
                };
                let payload = handler_status.render();
                return Ok(tool_text_result(payload.to_string(), false, &self.executor.overlay(), static_graph_generation_unix));
            }
            "list_recent_projects" => {
                let list = match crate::server::mcp::federation_tools::list_recent_projects() {
                    Ok(l) => l,
                    Err(e) => return Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                };
                let text = match serde_json::to_string(&list) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(tool_text_result(
                            format!("serialization error: {e}"),
                            true,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        ));
                    }
                };
                return Ok(tool_text_result(text, false, &self.executor.overlay(), static_graph_generation_unix));
            }
            "get_reload_status" => {
                let bus = match self.reload_bus.as_ref() {
                    Some(b) => b,
                    None => {
                        return Ok(tool_text_result(
                            "reload bus not configured on this server".to_string(),
                            true,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        ));
                    }
                };
                let payload =
                    crate::server::mcp::federation_tools::get_reload_status(bus);
                let text = serde_json::to_string(&payload).unwrap_or_else(|e| {
                    format!("serialization error: {e}")
                });
                return Ok(tool_text_result(text, false, &self.executor.overlay(), static_graph_generation_unix));
            }
            "request_reload" => {
                let bus = match self.reload_bus.as_ref() {
                    Some(b) => b,
                    None => {
                        return Ok(tool_text_result(
                            "reload bus not configured on this server".to_string(),
                            true,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        ));
                    }
                };
                return match crate::server::mcp::federation_tools::request_reload(bus) {
                    Ok(payload) => Ok(tool_text_result(
                        serde_json::to_string(&payload).unwrap_or_else(|e| {
                            format!("serialization error: {e}")
                        }),
                        false,
                        &self.executor.overlay(),
                        static_graph_generation_unix,
                    )),
                    Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                };
            }
            "register_agent" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_register_agent,
                ));
            }
            "heartbeat" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_heartbeat,
                ));
            }
            "list_active_agents" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_list_active_agents,
                ));
            }
            "who_am_i" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_who_am_i,
                ));
            }
            "list_subagents" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_list_subagents,
                ));
            }
            "claim_files" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_claim_files,
                ));
            }
            "release_files" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_release_files,
                ));
            }
            "list_occupancy" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_list_occupancy,
                ));
            }
            "my_claims" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_my_claims,
                ));
            }
            "detect_overlap" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_detect_overlap,
                ));
            }
            "get_audit_log" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::audit_tools::run_get_audit_log,
                ));
            }
            "get_world_state" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::presence_tools::run_get_world_state,
                ));
            }
            "get_recent_activity" => {
                return Ok(dispatch_presence_tool(
                    self.server.as_deref(),
                    &self.executor.overlay(),
                    &params.name,
                    &args_owned,
                    crate::server::mcp::audit_tools::run_get_recent_activity,
                ));
            }
            _ => {}
        }

        if let Some(fed) = &self.federation {
            match params.name.as_str() {
                "list_repos" => {
                    let repos = crate::server::mcp::federation_tools::list_repos(fed);
                    return Ok(tool_text_result(
                        serde_json::to_string(&repos)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                        &self.executor.overlay(),
                        static_graph_generation_unix,
                    ));
                }
                "get_repo_info" => {
                    let repo_id_str = match args_owned.get("repo_id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: repo_id".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let rid = match crate::federation::repo_id::RepoId::new(repo_id_str) {
                        Ok(r) => r,
                        Err(e) => {
                            return Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix));
                        }
                    };
                    return match crate::server::mcp::federation_tools::get_repo_info(fed, &rid) {
                        Ok(info) => {
                            // Enrich the JSON with `active_edits`: the
                            // count of unique agents with at least one
                            // claim (any intent) touching this repo's
                            // workspace. Sourced from the `LainServer`
                            // orchestrator when the MCP server was built
                            // with `with_server`; for sidecar / read-only
                            // servers the count is 0.
                            let mut value: serde_json::Value =
                                serde_json::to_value(&info).unwrap_or(serde_json::Value::Null);
                            let active_edits = self
                                .server
                                .as_ref()
                                .map(|s| {
                                    let occupancy = s.occupancy();
                                    let active = s.presence().list_active(false);
                                    active
                                        .iter()
                                        .filter(|sess| {
                                            let claims = occupancy.list_for_agent(&sess.id);
                                            // "Active edits" = any claim
                                            // on this repo's working set.
                                            // The MVP filter is any claim
                                            // at all; per-repo path
                                            // mapping is a follow-up.
                                            !claims.is_empty()
                                        })
                                        .count()
                                })
                                .unwrap_or(0);
                            if let Some(obj) = value.as_object_mut() {
                                obj.insert(
                                    "active_edits".to_string(),
                                    serde_json::Value::Number(active_edits.into()),
                                );
                            }
                            Ok(tool_text_result(
                                serde_json::to_string(&value)
                                    .unwrap_or_else(|e| format!("serialization error: {e}")),
                                false,
                                &self.executor.overlay(), static_graph_generation_unix
                            ))
                        }
                        Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                }
                "get_federation_health" => {
                    let health = crate::server::mcp::federation_tools::get_federation_health(fed);
                    return Ok(tool_text_result(
                        serde_json::to_string(&health)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                        &self.executor.overlay(),
                        static_graph_generation_unix,
                    ));
                }
                "search_org" => {
                    let query = match args_owned.get("query").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: query".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let limit: usize = match args_owned.get("limit") {
                        Some(serde_json::Value::Number(n)) => match n.as_u64() {
                            Some(u) => u as usize,
                            None => {
                                return Ok(tool_text_result(
                                    "Invalid argument: limit must be a non-negative integer"
                                        .to_string(),
                                    true,
                                    &self.executor.overlay(),
                        static_graph_generation_unix,
                                ));
                            }
                        },
                        Some(serde_json::Value::String(s)) => match s.parse::<usize>() {
                            Ok(u) => u,
                            Err(_) => {
                                return Ok(tool_text_result(
                                    "Invalid argument: limit must be a non-negative integer"
                                        .to_string(),
                                    true,
                                    &self.executor.overlay(),
                        static_graph_generation_unix,
                                ));
                            }
                        },
                        _ => {
                            return Ok(tool_text_result(
                                "Missing required argument: limit".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let hits = crate::server::mcp::federation_tools::search_org(fed, query, limit);
                    return Ok(tool_text_result(
                        serde_json::to_string(&hits)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                        &self.executor.overlay(),
                        static_graph_generation_unix,
                    ));
                }
                "get_cross_repo_blast_radius" => {
                    let symbol = match args_owned.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    // Not `.and_then(as_str)`: that collapses "absent" and
                    // "present but a number" into one message, and `depth: 2`
                    // is the mistake callers actually make (it is a string
                    // range like "1..3").
                    let depth_owned = match crate::server::tools::utils::required_str_arg(
                        &args_owned,
                        "depth",
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            return Ok(tool_text_result(
                                e.to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let depth = match parse_depth_range(&depth_owned) {
                        Ok(r) => r,
                        Err(e) => return Ok(tool_text_result(e, true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                    return match crate::server::mcp::federation_tools::get_cross_repo_blast_radius(fed, symbol, depth) {
                        Ok(r) => Ok(tool_text_result(
                            serde_json::to_string(&r)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                }
                "get_cross_repo_blast_radius_for_repo" => {
                    let repo_id = match args_owned.get("repo_id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: repo_id".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let symbol = match args_owned.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let depth_owned = match crate::server::tools::utils::required_str_arg(
                        &args_owned,
                        "depth",
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            return Ok(tool_text_result(
                                e.to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    let depth = match parse_depth_range(&depth_owned) {
                        Ok(r) => r,
                        Err(e) => return Ok(tool_text_result(e, true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                    return match crate::server::mcp::federation_tools::get_cross_repo_blast_radius_for_repo(fed, repo_id, symbol, depth) {
                        Ok(r) => Ok(tool_text_result(
                            serde_json::to_string(&r)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                }
                _ => {}
            }
        }

        // Workspace tools: only registered when a workspaces file was
        // supplied to the server constructor. `get_active_workspace` and
        // `get_workspace` cross-reference the federation (for loaded repo
        // info) when it's available; `list_workspaces` only needs the
        // workspaces file.
        //
        // Read through `RwLock::read()` on every dispatch (rather than
        // cloning the inner `WorkspacesFile`) so a `set_workspace`
        // call from the rebuild loop is observed by the very next
        // `list_workspaces` / `get_workspace` / `get_workspace_graph`
        // call. The synchronous helpers below complete in microseconds,
        // so the read guard never blocks the writers in `set_workspace`.
        if let Some(workspaces_lock) = &self.workspaces {
            let workspaces: &crate::federation::workspace::WorkspacesFile = &*workspaces_lock.read();
            match params.name.as_str() {
                "list_workspaces" => {
                    let active = ActiveWorkspace::load().ok().flatten();
                    let infos = crate::server::mcp::federation_tools::list_workspaces(workspaces, active.as_ref());
                    return Ok(tool_text_result(
                        serde_json::to_string(&infos)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                        &self.executor.overlay(),
                        static_graph_generation_unix,
                    ));
                }
                "get_active_workspace" => {
                    let fed = self.federation.as_deref();
                    return match fed {
                        Some(fed) => match crate::server::mcp::federation_tools::get_active_workspace(fed, workspaces) {
                            Ok(info) => Ok(tool_text_result(
                                serde_json::to_string(&info)
                                    .unwrap_or_else(|e| format!("serialization error: {e}")),
                                false,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            )),
                            Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                        },
                        None => Ok(tool_text_result(
                            LainError::Workspace("get_active_workspace requires federation mode".into()).to_string(),
                            true,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        )),
                    };
                }
                "get_workspace" => {
                    let name = match args_owned.get("name").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: name".to_string(),
                                true,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            ));
                        }
                    };
                    // For get_workspace we want member paths/healths from
                    // the live federation, but if the federation isn't
                    // loaded (defensive — shouldn't happen in practice
                    // since the workspace tools are only registered with
                    // a federation), we fall back to "not_loaded" health
                    // for each member. The source field is dropped in
                    // this fallback path.
                    let detail_res: Result<crate::server::mcp::federation_tools::WorkspaceDetail, LainError> =
                        match self.federation.as_deref() {
                            Some(fed) => crate::server::mcp::federation_tools::get_workspace(fed, workspaces, name),
                            None => {
                                match workspaces.workspaces.iter().find(|w| w.name == name) {
                                    Some(ws) => Ok(crate::server::mcp::federation_tools::WorkspaceDetail {
                                        name: ws.name.clone(),
                                        description: ws.description.clone(),
                                        source: None,
                                        members: ws.members.iter().map(|m| crate::server::mcp::federation_tools::WorkspaceRepoInfo {
                                            repo_id: m.clone(),
                                            path: String::new(),
                                            health: "not_loaded".into(),
                                        }).collect(),
                                    }),
                                    None => Err(LainError::NotFound(format!("workspace {name}"))),
                                }
                            }
                        };
                    return match detail_res {
                        Ok(d) => Ok(tool_text_result(
                            serde_json::to_string(&d)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                    };
                }
                "get_workspace_graph" => {
                    let filter = args_owned.get("filter").and_then(|v| v.as_str());
                    return match self.federation.as_deref() {
                        Some(fed) => match crate::server::mcp::federation_tools::get_workspace_graph(fed, workspaces, filter) {
                            Ok(graph) => Ok(tool_text_result(
                                serde_json::to_string(&graph)
                                    .unwrap_or_else(|e| format!("serialization error: {e}")),
                                false,
                                &self.executor.overlay(),
                        static_graph_generation_unix,
                            )),
                            Err(e) => Ok(tool_text_result(format!("{e}"), true, &self.executor.overlay(), static_graph_generation_unix)),
                        },
                        None => Ok(tool_text_result(
                            LainError::Workspace("get_workspace_graph requires federation mode".into()).to_string(),
                            true,
                            &self.executor.overlay(),
                        static_graph_generation_unix,
                        )),
                    };
                }
                _ => {}
            }
        }

        if let Some(fed) = &self.federation {
            match resolve_repo_or_error(fed, &args_owned) {
                Ok(rid) => {
                    // Inject the resolved `repo_id` into the args the
                    // executor will see. Existing per-repo tools resolve
                    // symbols against `ctx.graph` (the executor's
                    // single-workspace context) and ignore this; future
                    // federation-aware tool handlers can read it. This is
                    // the round-1 fix: the previously discarded `RepoId`
                    // now flows through dispatch.
                    args_owned.insert(
                        "repo_id".into(),
                        serde_json::Value::String(rid.as_str().to_string()),
                    );
                }
                Err(text) => return Ok(tool_text_result(text, true, &self.executor.overlay(), static_graph_generation_unix)),
            }
        }

        // Fallback to the legacy executor for tools that don't have an
        // explicit dispatcher arm above. The `CallToolResult` here is the
        // raw executor response; route it through the same envelope
        // contract — every tool response carries `_meta.revision` at the
        // `CallToolResult` outer level, never inside `content[0].text`.
        let raw = match self.executor.call(&params.name, Some(&args_owned)).await {
            Ok(text) => tool_text_result(text, false, &self.executor.overlay(), static_graph_generation_unix),
            Err(e) => tool_text_result(format!("Error: {e}"), true, &self.executor.overlay(), static_graph_generation_unix),
        };
        Ok(raw)
    }
}

#[derive(Clone)]
pub struct LainMcpServer {
    executor: ToolExecutor,
    federation: Option<Arc<FederatedIndex>>,
    /// Workspaces file. Wrapped in `Arc<RwLock<...>>` and **shared**
    /// with `LainServer` so a hot-reload swap (`LainServer::set_workspace`)
    /// is visible to the in-flight dispatcher without restarting the
    /// server. The same `Arc` is handed to `LainHandler`; the handler
    /// reads through the lock on every dispatch.
    workspaces: Option<Arc<RwLock<crate::federation::workspace::WorkspacesFile>>>,
    /// Transport for the active server, surfaced via `get_server_status`.
    status_transport: Option<crate::server::Transport>,
    /// TCP port for HTTP transport; surfaced via `get_server_status`.
    status_port: Option<u16>,
    /// Start time (immutable for the server's lifetime).
    status_started_at: std::time::SystemTime,
    /// Last sync time, Arc-shared with `LainServer::last_sync_at` so
    /// ingest paths can update it.
    status_last_sync_at: Arc<parking_lot::Mutex<std::time::SystemTime>>,
    /// Last error, Arc-shared with `LainServer::last_error`.
    status_last_error: Arc<parking_lot::Mutex<Option<String>>>,
    /// Hot-reload bus, surfaced via `get_reload_status` /
    /// `request_reload`. Set by `with_reload_bus` (Task 6.5); `None`
    /// for servers that don't have a hot-reload subsystem.
    reload_bus: Option<Arc<crate::server::reload::ReloadBus>>,
    /// Shared handle to the `LainServer`. Holds the presence registry,
    /// occupancy map, and presence-event broadcast sender that the 8
    /// multiplayer MCP tools dispatch against. Set by `with_server`
    /// (Task 7); `None` for sidecar / read-only servers that don't
    /// carry the presence layer.
    server: Option<Arc<LainServer>>,
    /// Override for the startup re-index timeout. `None` means
    /// "use `LAIN_REINDEX_TIMEOUT` env, falling back to the
    /// placeholder default of 300s." Step 1 of the staleness fix.
    reindex_timeout: Option<std::time::Duration>,
}

impl LainMcpServer {
    pub fn new(executor: ToolExecutor) -> Self {
        let now = std::time::SystemTime::now();
        Self {
            executor,
            federation: None,
            workspaces: None,
            status_transport: None,
            status_port: None,
            status_started_at: now,
            status_last_sync_at: Arc::new(parking_lot::Mutex::new(now)),
            status_last_error: Arc::new(parking_lot::Mutex::new(None)),
            reload_bus: None,
            server: None,
            reindex_timeout: None,
        }
    }

    /// Federation-mode constructor. When set, the handler also exposes
    /// `list_repos` and `get_repo_info` over MCP. With `new(executor)` alone
    /// the federation surface is not registered and single-workspace
    /// behavior is preserved.
    pub fn with_federation(executor: ToolExecutor, federation: Arc<FederatedIndex>) -> Self {
        let now = std::time::SystemTime::now();
        Self {
            executor,
            federation: Some(federation),
            workspaces: None,
            status_transport: None,
            status_port: None,
            status_started_at: now,
            status_last_sync_at: Arc::new(parking_lot::Mutex::new(now)),
            status_last_error: Arc::new(parking_lot::Mutex::new(None)),
            reload_bus: None,
            server: None,
            reindex_timeout: None,
        }
    }

    /// Federation + workspace constructor. When workspaces is Some, the
    /// 3 workspace tools (list_workspaces, get_active_workspace,
    /// get_workspace) are also registered and the get_workspace tool
    /// resolves member paths/healths from the live federation.
    ///
    /// `workspaces` is the same `Arc<RwLock<WorkspacesFile>>` the
    /// `LainServer` is holding, so a `set_workspace` call from the
    /// rebuild loop is observed by the next dispatch without
    /// restarting the server. The handler reads through `lock.read()`
    /// on every call; the synchronous helpers finish in microseconds
    /// so the read guard is never held long enough to block writers.
    pub fn with_federation_and_workspaces(
        executor: ToolExecutor,
        federation: Arc<FederatedIndex>,
        workspaces: Arc<RwLock<crate::federation::workspace::WorkspacesFile>>,
    ) -> Self {
        let now = std::time::SystemTime::now();
        Self {
            executor,
            federation: Some(federation),
            workspaces: Some(workspaces),
            status_transport: None,
            status_port: None,
            status_started_at: now,
            status_last_sync_at: Arc::new(parking_lot::Mutex::new(now)),
            status_last_error: Arc::new(parking_lot::Mutex::new(None)),
            reload_bus: None,
            server: None,
            reindex_timeout: None,
        }
    }

    /// Inject the hot-reload bus. After this call, the
    /// `get_reload_status` and `request_reload` MCP tools return real
    /// values. The server constructs the bus; this is just the wiring
    /// hook.
    pub fn with_reload_bus(
        mut self,
        reload_bus: Arc<crate::server::reload::ReloadBus>,
    ) -> Self {
        self.reload_bus = Some(reload_bus);
        self
    }

    /// Inject a shared handle to the `LainServer`. After this call,
    /// the multiplayer MCP tools (`register_agent`, `heartbeat`,
    /// `list_active_agents`, `who_am_i`, `list_subagents`, `claim_files`,
    /// `release_files`, `list_occupancy`, `my_claims`, `detect_overlap`)
    /// dispatch against the same presence registry, occupancy map, and
    /// presence-event broadcast sender the rest of the server uses.
    /// The handler clones `Arc` state through this pointer so a
    /// registration here is observed by every other consumer in the
    /// same process.
    ///
    /// Also pushes the live `Arc<PresenceRegistry>` / `Arc<OccupancyMap>`
    /// into the underlying executor's `ToolContext` so the regular
    /// tools (`query_graph`, `explain_symbol`, `get_repo_info`) surface
    /// the same multiplayer state the dedicated tools see. Without
    /// this push the `ToolContext` would still carry the empty defaults
    /// from `ToolExecutor::new` and `query_graph` would report
    /// `active_agents: []` even after agents register.
    pub fn with_server(mut self, server: Arc<LainServer>) -> Self {
        let presence = Arc::clone(server.presence());
        let occupancy = Arc::clone(server.occupancy());
        let last_outcome = Arc::clone(&server.last_outcome);
        // `executor.ctx` is `pub`; mutate it in place so handlers reading
        // through `&ctx.presence` / `&ctx.occupancy` observe the live
        // registries.
        self.executor.ctx.presence = presence;
        self.executor.ctx.occupancy = occupancy;
        self.executor.ctx.last_outcome = last_outcome;
        self.server = Some(server);
        self
    }

    /// Override the startup re-index timeout. `None` means
    /// `LAIN_REINDEX_TIMEOUT` env (default 300s).
    pub fn with_reindex_timeout(mut self, t: Option<std::time::Duration>) -> Self {
        self.reindex_timeout = t;
        self
    }

    /// Inject the federation-mode transport / port / start-time /
    /// sync-time / last-error values. Called by `LainServer::serve`
    /// right before the MCP loop kicks off, so the values match the
    /// `LainServer` fields one-for-one (Arc-shared for the Mutexes).
    pub fn with_status(
        mut self,
        transport: Option<crate::server::Transport>,
        port: Option<u16>,
        started_at: std::time::SystemTime,
        last_sync_at: Arc<parking_lot::Mutex<std::time::SystemTime>>,
        last_error: Arc<parking_lot::Mutex<Option<String>>>,
    ) -> Self {
        self.status_transport = transport;
        self.status_port = port;
        self.status_started_at = started_at;
        self.status_last_sync_at = last_sync_at;
        self.status_last_error = last_error;
        self
    }

    /// Build a sidecar-flavored server. The graph inside `executor` should
    /// already be opened read-only (see `GraphDatabase::open_read_only`),
    /// so mutating tool calls fail at the database layer with a clean
    /// `graph is read-only` error.
    pub fn new_read_only(executor: ToolExecutor) -> Self {
        Self::new(executor)
    }

    // `from_read_only_graph` was a convenience wrapper over `new_read_only`
    // with no caller and no test; `new_read_only` remains the sidecar entry.

    /// Serve on a specific `SocketAddr`. Used by the sidecar so it can bind
    /// to `127.0.0.1:<port>` without going through `run_http`'s
    /// `"0.0.0.0:<port>"` listener (the sidecar must never accept
    /// connections from outside the loopback).
    pub async fn serve(self, addr: std::net::SocketAddr) -> SdkResult<()> {
        info!("Starting Lain sidecar MCP HTTP server on {}", addr);

        let executor = Arc::new(self.executor);
        let status = HandlerStatus {
            transport: self.status_transport,
            port: self.status_port,
            started_at: self.status_started_at,
            last_sync_at: self.status_last_sync_at,
            last_error: self.status_last_error,
            repo_count: 0,
            workspaces_count: 0,
        };
        let listener = TcpListener::bind(addr).await?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let executor = executor.clone();
                    let status = status.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| {
                            let executor = executor.clone();
                            let status = status.clone();
                            handle_request(req, executor, None, None, status, None, None)
                        });
                        if let Err(e) = http1::Builder::new()
                            .serve_connection(io, service)
                            .await
                        {
                            tracing::debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    /// Run with stdio transport (for local/MCP clients)
    pub async fn run_stdio(self) -> SdkResult<()> {
        info!("Starting Lain MCP server on stdio");

        // Re-index the underlying graph if it's stale (HEAD commit
        // differs from the last_indexed commit). Runs in the background
        // so the MCP server starts immediately — tools fired while
        // indexing is in flight see the previous snapshot, but the
        // graph is current by the time the user asks their second
        // question. `build_core_memory` itself short-circuits when the
        // graph is already current, so this is a no-op on a clean
        // start. The user's two concrete failures (stale `graph.bin`,
        // `explain_symbol` reporting a path that no longer exists)
        // both trace to skipping this on startup.
        //
        // The re-index is wrapped in a timeout budget (default 300s;
        // override via `LAIN_REINDEX_TIMEOUT` env or the
        // `--reindex-timeout` flag). Past that we give up and let the
        // server keep running with the existing graph. A genuine hang
        // (e.g. an LSP server that won't start) is bounded rather than
        // blocking the spawn task forever.
        //
        // The outcome is written to `server.last_outcome` so
        // `get_health` can surface failures — the previous code
        // logged only to `tracing::warn` and stderr, neither of
        // which a stdio MCP client surfaces to the model.
        if let Some(server) = self.server.clone() {
            let last_outcome = server.last_outcome.clone();
            let reindex_timeout = self.reindex_timeout;
            tokio::spawn(async move {
                let started = std::time::SystemTime::now();
                let timeout = reindex_timeout.unwrap_or_else(
                    crate::server::refresh::parse_reindex_timeout
                );
                let outcome = match tokio::time::timeout(
                    timeout,
                    server.build_core_memory(),
                )
                .await
                {
                    Ok(Ok(())) => crate::server::refresh::RefreshOutcome::ok(started),
                    Ok(Err(e)) => {
                        eprintln!("startup re-index failed: {e}");
                        crate::server::refresh::RefreshOutcome::failed(started, e.to_string())
                    }
                    Err(_) => {
                        eprintln!(
                            "startup re-index timed out after {}s; using existing graph",
                            timeout.as_secs()
                        );
                        crate::server::refresh::RefreshOutcome::timeout(started)
                    }
                };
                *last_outcome.lock() = outcome;
            });
        }

        let server_details = self.server_info();
        let transport = StdioTransport::new(TransportOptions::default())?;
        let handler = LainHandler {
            executor: Arc::new(self.executor),
            federation: self.federation,
            workspaces: self.workspaces,
            status_transport: self.status_transport,
            status_port: self.status_port,
            status_started_at: self.status_started_at,
            status_last_sync_at: self.status_last_sync_at,
            status_last_error: self.status_last_error,
            reload_bus: self.reload_bus,
            server: self.server,
        };

        let server = server_runtime::create_server(McpServerOptions {
            server_details,
            transport,
            handler: handler.to_mcp_server_handler(),
            task_store: None,
            client_task_store: None,
            message_observer: None,
        });

        server.start().await
    }

    /// Run with HTTP transport (for MCP clients and browser diagnostics)
    pub async fn run_http(self, port: u16) -> SdkResult<()> {
        info!("Starting Lain MCP HTTP server on port {}", port);

        // Re-index on startup. See `run_stdio` for the rationale.
        if let Some(server) = self.server.clone() {
            let last_outcome = server.last_outcome.clone();
            let reindex_timeout = self.reindex_timeout;
            tokio::spawn(async move {
                let started = std::time::SystemTime::now();
                let timeout = reindex_timeout.unwrap_or_else(
                    crate::server::refresh::parse_reindex_timeout
                );
                let outcome = match tokio::time::timeout(
                    timeout,
                    server.build_core_memory(),
                )
                .await
                {
                    Ok(Ok(())) => crate::server::refresh::RefreshOutcome::ok(started),
                    Ok(Err(e)) => crate::server::refresh::RefreshOutcome::failed(started, e.to_string()),
                    Err(_) => crate::server::refresh::RefreshOutcome::timeout(started),
                };
                *last_outcome.lock() = outcome;
            });
        }

        // Publish the real listener port so tool output can link to
        // `/ui/...` sessions; stdio mode leaves it at 0 (no links).
        self.executor.set_diagnostics_port(port);
        let executor = Arc::new(self.executor);
        let federation = self.federation;
        let workspaces = self.workspaces;
        let status_transport = self.status_transport;
        let status_port = self.status_port;
        let status_started_at = self.status_started_at;
        let status_last_sync_at = self.status_last_sync_at;
        let status_last_error = self.status_last_error;
        let reload_bus = self.reload_bus;
        let server = self.server;
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let executor = executor.clone();
                    let federation = federation.clone();
                    let workspaces = workspaces.clone();
                    let status_transport = status_transport;
                    let status_port = status_port;
                    let status_started_at = status_started_at;
                    let status_last_sync_at = status_last_sync_at.clone();
                    let status_last_error = status_last_error.clone();
                    let reload_bus = reload_bus.clone();
                    let server = server.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| {
                            let executor = executor.clone();
                            let federation = federation.clone();
                            let workspaces = workspaces.clone();
                            let handler_status = HandlerStatus {
                                transport: status_transport,
                                port: status_port,
                                started_at: status_started_at,
                                last_sync_at: status_last_sync_at.clone(),
                                last_error: status_last_error.clone(),
                                workspaces_count: workspaces
                                    .as_ref()
                                    .map(|w| w.read().workspaces.len())
                                    .unwrap_or(0),
                                repo_count: federation
                                    .as_ref()
                                    .map(|f| f.list_repos().len())
                                    .unwrap_or(0),
                            };
                            handle_request(
                                req,
                                executor,
                                federation,
                                workspaces,
                                handler_status,
                                reload_bus.clone(),
                                server.clone(),
                            )
                        });
                        if let Err(e) = http1::Builder::new()
                            .serve_connection(io, service)
                            .await
                        {
                            tracing::debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    fn server_info(&self) -> InitializeResult {
        InitializeResult {
            server_info: Implementation {
                name: "lain".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("Lain".into()),
                description: Some("Structural Code Intelligence for AI Agents".into()),
                icons: vec![],
                website_url: None,
            },
            capabilities: ServerCapabilities {
                tools: Some(ServerCapabilitiesTools { list_changed: Some(false) }),
                ..Default::default()
            },
            meta: None,
            instructions: Some("Call get_agent_strategy for your operational manual.".into()),
            protocol_version: ProtocolVersion::V2025_11_25.into(),
        }
    }
}

/// Build the JSON body returned by `GET /health`. Extracted so the
/// federation-aware shape can be unit-tested without spinning up an
/// HTTP harness. When `federation` is `None` (single-workspace mode)
/// the `federation` field serializes as JSON `null`; when `Some` it
/// carries the repo roster and aggregate stats so the UI can detect
/// federation mode without a separate `tools/call` round-trip.
fn build_health_body(
    nodes: usize,
    edges: usize,
    federation: Option<&FederatedIndex>,
) -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "server": "lain",
        "version": env!("CARGO_PKG_VERSION"),
        "graph_nodes": nodes,
        "graph_edges": edges,
        "tools_count": crate::tools::registry::ToolRegistry::definitions().len(),
        "federation": federation.map(federation_blob),
    })
}

/// Render the federation summary embedded in `/health`. The
/// `memory_estimate_bytes` figure is a rough heuristic
/// (200 bytes/node + 100 bytes/edge) — sufficient for the dashboard's
/// capacity bar; not a precise accounting.
fn federation_blob(fed: &FederatedIndex) -> serde_json::Value {
    let repos: Vec<serde_json::Value> = fed
        .list_repos()
        .into_iter()
        .map(|(id, health)| {
            serde_json::json!({
                "id": id.to_string(),
                "health": health.to_string(),
            })
        })
        .collect();
    let backend = fed.backend();
    let node_count = backend.node_count();
    let edge_count = backend.edge_count();
    serde_json::json!({
        "repos": repos,
        "total_nodes": node_count,
        "total_edges": edge_count,
        "memory_estimate_bytes": node_count as u64 * 200 + edge_count as u64 * 100,
    })
}

/// Per-process status snapshot carried into the HTTP request handler
/// closure. Built once per accepted connection (cloning the cheap
/// `SystemTime` and Arc-shared Mutexes) so the inner `service_fn`
/// closure doesn't need to capture the whole `LainMcpServer` state.
#[derive(Clone)]
struct HandlerStatus {
    transport: Option<crate::server::Transport>,
    port: Option<u16>,
    started_at: std::time::SystemTime,
    last_sync_at: Arc<parking_lot::Mutex<std::time::SystemTime>>,
    last_error: Arc<parking_lot::Mutex<Option<String>>>,
    repo_count: usize,
    workspaces_count: usize,
}

impl HandlerStatus {
    /// Render the `get_server_status` payload.
    ///
    /// Delegates to the one builder in
    /// `federation_tools::server_status` — this used to be a second
    /// implementation of the same JSON, and it drifted: build-identity
    /// fields were added there and this transport kept serving a
    /// payload without them.
    fn render(&self) -> serde_json::Value {
        use crate::server::mcp::federation_tools::server_status::{
            render_server_status, ServerStatusFields,
        };
        render_server_status(ServerStatusFields {
            transport: self.transport.map(|t| match t {
                crate::server::Transport::Stdio => "stdio".to_string(),
                crate::server::Transport::Http => "http".to_string(),
            }),
            port: self.port,
            started_at: self.started_at,
            last_sync_at: *self.last_sync_at.lock(),
            last_error: self.last_error.lock().clone(),
            repo_count: self.repo_count,
            workspace_count: self.workspaces_count,
        })
    }
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    executor: Arc<ToolExecutor>,
    federation: Option<Arc<FederatedIndex>>,
    // `Arc<RwLock<WorkspacesFile>>` shared with the `LainServer`'s
    // hot-reload slot. The handler reads through the lock on every
    // dispatch so a `set_workspace` call is observed immediately.
    workspaces: Option<Arc<RwLock<crate::federation::workspace::WorkspacesFile>>>,
    status: HandlerStatus,
    reload_bus: Option<Arc<crate::server::reload::ReloadBus>>,
    // Shared `LainServer` handle; `None` for the sidecar which doesn't
    // carry the presence layer. Presence-tool dispatches inside the
    // JSON-RPC branch see this as `None` and return a descriptive error.
    server: Option<Arc<LainServer>>,
) -> Result<Response<OverlayHttpBody>, hyper::Error> {
    let jsonrpc_response = |value: serde_json::Value| -> Response<OverlayHttpBody> {
        let body = serde_json::to_string(&value).unwrap_or_default();
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(full_body(Bytes::from(body)))
            .unwrap()
    };
    let jsonrpc_error = |id: Option<&serde_json::Value>, code: i32, msg: String| {
        jsonrpc_response(serde_json::json!({
            "jsonrpc": "2.0",
            "error": {"code": code, "message": msg},
            "id": id
        }))
    };
    let jsonrpc_tool_result = |id: Option<&serde_json::Value>, text: &str, is_error: bool| {
        // Read the overlay revision once per response so the value is
        // stable for the duration of this HTTP round-trip. The internal
        // tool_text_result helper does the same read on the stdio path;
        // both stdio and HTTP responses now carry `_meta.revision` in
        // the `CallToolResult` envelope, not in `content[0].text`.
        // P1 #1: also carry the static-graph generation so the LLM
        // knows how fresh the static graph is without a separate
        // `list_repos` round-trip.
        let rev = executor.overlay().current_revision();
        let sgg = server
            .as_deref()
            .and_then(|s| s.static_graph_generation_unix());
        jsonrpc_response(serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "content": [{"type": "text", "text": text}],
                "isError": is_error,
                "_meta": {
                    "revision": rev,
                    "static_graph_generation": sgg,
                }
            },
            "id": id
        }))
    };

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Auth + rate limit (P0 #1). `/health` is unauthenticated by design
    // (operational probe). All other endpoints — including `/mcp` and
    // `/events` — require a valid `Authorization: Bearer <key>` and
    // respect the per-key rate limit. Stdio callers bypass this entirely.
    if !(method == Method::GET && path == "/health") {
        let auth_header = req.headers().get("authorization")
            .and_then(|v| v.to_str().ok());
        // Pick the bucket key: validated bearer token when present,
        // else the raw header (so an attacker can't share a bucket by
        // spoofing an empty key).
        let bucket_key = crate::server::auth::bearer_token(auth_header.unwrap_or(""))
            .unwrap_or_else(|| auth_header.unwrap_or("anonymous").to_string());
        if let Some(srv) = server.as_deref() {
            if let Err(reason) = srv.auth.check_bearer(auth_header) {
                let body_bytes = serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32001, "message": reason.message()},
                    "id": null
                })).unwrap_or_default();
                return Ok(Response::builder()
                    .status(StatusCode::from_u16(reason.http_status()).unwrap())
                    .body(full_body(Bytes::from(body_bytes)))
                    .unwrap());
            }
            if let Err(retry_after) = srv.auth.check_rate(&bucket_key) {
                let body_bytes = serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32002, "message": "rate limit exceeded"},
                    "id": null
                })).unwrap_or_default();
                return Ok(Response::builder()
                    .status(StatusCode::from_u16(429).unwrap())
                    .header("Retry-After", retry_after.to_string())
                    .body(full_body(Bytes::from(body_bytes)))
                    .unwrap());
            }
        }
    }

    // GET / is now served by the Command Center SPA shell (PR 4, Task 4.3).
    // The pre-PR-1 /federation-dashboard.html asset was dropped in PR 1
    // (Task 1.3) and is not restored here; the SPA replaces it.

    // GET /health -> health check with graph stats
    if method == Method::GET && path == "/health" {
        let (nodes, edges) = executor.graph().get_stats();
        let health = build_health_body(nodes, edges, federation.as_deref());
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(full_body(Bytes::from(health.to_string())))
            .unwrap());
    }

    // GET /events -> presence/occupancy SSE stream. Each broadcast event
    // (`agent_joined`, `agent_left`, `heartbeat_expired`, `claim_granted`,
    // `claim_released`, `conflict_detected`) is formatted as a single SSE
    // frame (`event: …\ndata: …\nid: …\n\n`) and pushed into an mpsc
    // channel; the body wrapper drains the channel so the client sees
    // frames as they arrive. When the broadcast sender is dropped (the
    // server is shutting down) the spawned task exits and the body
    // returns `None`, closing the connection cleanly.
    //
    // For sidecar / read-only servers that don't carry the presence layer
    // we still respond `200 text/event-stream` with a single `error`
    // frame so EventSource clients don't immediately retry against a
    // missing capability.
    if method == Method::GET && path == "/events" {
        let Some(server) = server.as_ref() else {
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .body(full_body(Bytes::from(
                    "event: error\ndata: {\"reason\":\"presence layer not configured\"}\n\n",
                )))
                .unwrap());
        };
        let bus_rx = server.presence_event_tx().subscribe();
        let last_event_id = req
            .headers()
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let events_log = server.events_log.clone();
        let (tx, rx) = mpsc::unbounded_channel::<std::io::Result<Bytes>>();
        tokio::spawn(async move {
            use crate::server::sse::serve_sse;
            let mut stream = serve_sse(bus_rx, last_event_id, events_log);
            // Send one synthetic `ready` frame so clients know the stream
            // is live before the first real broadcast event arrives.
            if tx
                .send(Ok(Bytes::from_static(b"event: ready\ndata: {}\n\n")))
                .is_err()
            {
                return;
            }
            while let Some(item) = stream.next().await {
                match item {
                    Ok(frame) => {
                        let sse = format!(
                            "event: {}\ndata: {}\nid: {}\n\n",
                            frame.event, frame.data, frame.id
                        );
                        if tx.send(Ok(Bytes::from(sse))).is_err() {
                            // Receiver dropped — client disconnected.
                            break;
                        }
                    }
                    Err(_) => {
                        // SseFrame's error type is Infallible; the match
                        // arm is here for completeness.
                        break;
                    }
                }
            }
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .body(UnsyncBoxBody::new(OverlaySubscribeBody::new(rx)))
            .unwrap());
    }

    // POST /mcp -> JSON-RPC
    if method == Method::POST && path == "/mcp" {
        let body = req.collect().await?;
        let body_bytes = body.to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();

        let rpc_response = match serde_json::from_str::<serde_json::Value>(&body_str) {
            Ok(json) => {
                let rpc_method = json.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let id = json.get("id");
                let params = json.get("params");

                match rpc_method {
                    // MCP handshake: clients (Claude Code, Kimi, Codex) send
                    // `initialize` first. Mirror the stdio transport's
                    // `server_info()` so HTTP clients see the same wire shape.
                    "initialize" => {
                        let init = InitializeResult {
                            server_info: Implementation {
                                name: "lain".into(),
                                version: env!("CARGO_PKG_VERSION").into(),
                                title: Some("Lain".into()),
                                description: Some(
                                    "Structural Code Intelligence for AI Agents".into(),
                                ),
                                icons: vec![],
                                website_url: None,
                            },
                            capabilities: ServerCapabilities {
                                tools: Some(ServerCapabilitiesTools {
                                    list_changed: Some(false),
                                }),
                                ..Default::default()
                            },
                            meta: None,
                            instructions: Some(
                                "Call get_agent_strategy for your operational manual.".into(),
                            ),
                            protocol_version: ProtocolVersion::V2025_11_25.into(),
                        };
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": init,
                        })
                    }
                    // Client confirms it has processed our `initialize` reply.
                    // It's a JSON-RPC notification (no `id`); return 204 No
                    // Content as the spec recommends.
                    "notifications/initialized" => {
                        return Ok(Response::builder()
                            .status(StatusCode::NO_CONTENT)
                            .body(full_body(Bytes::new()))
                            .unwrap());
                    }
                    // Liveness probe; clients use it to keep the connection
                    // warm and to detect dead transports.
                    "ping" => {
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {},
                        })
                    }
                    "tools/list" => {
                        let mut tools_vec = crate::tools::registry::ToolRegistry::definitions();
                        // Append the 6 special-case tools (kept in sync with
                        // the stdio transport's handle_list_tools_request).
                        tools_vec.extend(special_tool_definitions());
                        // Same mode filter as the stdio path (wishlist #9):
                        // do not advertise a tool that is guaranteed to
                        // fail in this mode. This is the transport agents
                        // actually use, so a filter only on the stdio side
                        // would not be the fix.
                        let inert = inert_tool_names(executor.embedder());
                        tools_vec.retain(|def| !inert.contains(&def.name));
                        let mut tools: Vec<serde_json::Value> = tools_vec
                            .iter()
                            .map(|def| {
                                serde_json::json!({
                                    "name": def.name,
                                    "description": def.description,
                                    "inputSchema": def.input_schema
                                })
                            })
                            .collect();
                        if federation.is_some() {
                            tools.extend(defs_to_value_tools(FEDERATION_TOOL_DEFS));
                        }
                        if workspaces.is_some() {
                            tools.extend(defs_to_value_tools(WORKSPACE_TOOL_DEFS));
                        }
                        for t in defs_to_value_tools(SERVER_TOOL_DEFS) {
                            tools.push(t);
                        }
                        serde_json::json!({"jsonrpc": "2.0", "result": {"tools": tools}, "id": id})
                    }
                    "tools/call" => {
                        let name = params
                            .and_then(|p| p.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        let mut args_map: serde_json::Map<String, serde_json::Value> = params
                            .and_then(|p| p.get("arguments"))
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();

                        // Server-status / recent-projects dispatch — always
                        // available regardless of federation mode.
                        match name {
                            "get_server_status" => {
                                let payload = status.render().to_string();
                                return Ok(jsonrpc_tool_result(id, &payload, false));
                            }
                            "list_recent_projects" => {
                                let list = match crate::server::mcp::federation_tools::list_recent_projects() {
                                    Ok(l) => l,
                                    Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                };
                                let text = match serde_json::to_string(&list) {
                                    Ok(s) => s,
                                    Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                };
                                return Ok(jsonrpc_tool_result(id, &text, false));
                            }
                            "get_reload_status" => {
                                let bus = match reload_bus.as_ref() {
                                    Some(b) => b,
                                    None => {
                                        return Ok(jsonrpc_tool_result(
                                            id,
                                            "reload bus not configured on this server",
                                            true,
                                        ));
                                    }
                                };
                                let payload =
                                    crate::server::mcp::federation_tools::get_reload_status(bus);
                                let text = match serde_json::to_string(&payload) {
                                    Ok(s) => s,
                                    Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                };
                                return Ok(jsonrpc_tool_result(id, &text, false));
                            }
                            "request_reload" => {
                                let bus = match reload_bus.as_ref() {
                                    Some(b) => b,
                                    None => {
                                        return Ok(jsonrpc_tool_result(
                                            id,
                                            "reload bus not configured on this server",
                                            true,
                                        ));
                                    }
                                };
                                match crate::server::mcp::federation_tools::request_reload(bus) {
                                    Ok(payload) => {
                                        let text = match serde_json::to_string(&payload) {
                                            Ok(s) => s,
                                            Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                        };
                                        return Ok(jsonrpc_tool_result(id, &text, false));
                                    }
                                    Err(e) => {
                                        return Ok(jsonrpc_tool_result(id, &format!("{e}"), true));
                                    }
                                }
                            }
                            // 8 multiplayer tools (Task 7). Dispatched
                            // against the shared `LainServer`; if it's
                            // missing (sidecar path) the helper returns
                            // a clean error.
                            "register_agent" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_register_agent,
                                ));
                            }
                            "heartbeat" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_heartbeat,
                                ));
                            }
                            "list_active_agents" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_list_active_agents,
                                ));
                            }
                            "who_am_i" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_who_am_i,
                                ));
                            }
                            "list_subagents" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_list_subagents,
                                ));
                            }
                            "claim_files" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_claim_files,
                                ));
                            }
                            "release_files" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_release_files,
                                ));
                            }
                            "list_occupancy" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_list_occupancy,
                                ));
                            }
                            "my_claims" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_my_claims,
                                ));
                            }
                            "detect_overlap" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_detect_overlap,
                                ));
                            }
                            "get_audit_log" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::audit_tools::run_get_audit_log,
                                ));
                            }
                            "get_world_state" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::presence_tools::run_get_world_state,
                                ));
                            }
                            "get_recent_activity" => {
                                return Ok(jsonrpc_presence_tool(
                                    &jsonrpc_tool_result, &jsonrpc_error,
                                    id, name, &args_map, server.as_deref(),
                                    crate::server::mcp::audit_tools::run_get_recent_activity,
                                ));
                            }
                            _ => {}
                        }

                        if let Some(fed) = &federation {
                            match name {
                                "list_repos" => {
                                    let repos = crate::server::mcp::federation_tools::list_repos(fed);
                                    let text = match serde_json::to_string(&repos) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                    };
                                    return Ok(jsonrpc_tool_result(id, &text, false));
                                }
                                "get_repo_info" => {
                                    let repo_id_str = match args_map.get("repo_id").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: repo_id", true)),
                                    };
                                    let rid = match crate::federation::repo_id::RepoId::new(repo_id_str) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    };
                                    match crate::server::mcp::federation_tools::get_repo_info(fed, &rid) {
                                        Ok(info) => {
                                            // Mirror the stdio dispatch:
                                            // count interactive agents with
                                            // at least one claim and inject
                                            // the number as `active_edits`.
                                            let mut value: serde_json::Value =
                                                serde_json::to_value(&info).unwrap_or(serde_json::Value::Null);
                                            let active_edits = server
                                                .as_ref()
                                                .map(|s| {
                                                    let occupancy = s.occupancy();
                                                    let active = s.presence().list_active(false);
                                                    active
                                                        .iter()
                                                        .filter(|sess| {
                                                            !occupancy.list_for_agent(&sess.id).is_empty()
                                                        })
                                                        .count()
                                                })
                                                .unwrap_or(0);
                                            if let Some(obj) = value.as_object_mut() {
                                                obj.insert(
                                                    "active_edits".to_string(),
                                                    serde_json::Value::Number(active_edits.into()),
                                                );
                                            }
                                            let text = match serde_json::to_string(&value) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            return Ok(jsonrpc_tool_result(id, &text, false));
                                        }
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    }
                                }
                                "get_federation_health" => {
                                    let health = crate::server::mcp::federation_tools::get_federation_health(fed);
                                    let text = match serde_json::to_string(&health) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                    };
                                    return Ok(jsonrpc_tool_result(id, &text, false));
                                }
                                "search_org" => {
                                    let query = match args_map.get("query").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: query", true)),
                                    };
                                    let limit: usize = match args_map.get("limit") {
                                        Some(serde_json::Value::Number(n)) => match n.as_u64() {
                                            Some(u) => u as usize,
                                            None => return Ok(jsonrpc_tool_result(id, "Invalid argument: limit must be a non-negative integer", true)),
                                        },
                                        Some(serde_json::Value::String(s)) => match s.parse::<usize>() {
                                            Ok(u) => u,
                                            Err(_) => return Ok(jsonrpc_tool_result(id, "Invalid argument: limit must be a non-negative integer", true)),
                                        },
                                        _ => return Ok(jsonrpc_tool_result(id, "Missing required argument: limit", true)),
                                    };
                                    let hits = crate::server::mcp::federation_tools::search_org(fed, query, limit);
                                    let text = match serde_json::to_string(&hits) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                    };
                                    return Ok(jsonrpc_tool_result(id, &text, false));
                                }
                                "get_cross_repo_blast_radius" => {
                                    let symbol = match args_map.get("symbol").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: symbol", true)),
                                    };
                                    // Same reasoning as the stdio path: `depth: 2`
                                    // must not be reported as a missing argument.
                                    let depth_owned = match crate::server::tools::utils::required_str_arg(
                                        &args_map, "depth",
                                    ) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e.to_string(), true)),
                                    };
                                    let depth = match parse_depth_range(&depth_owned) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e, true)),
                                    };
                                    match crate::server::mcp::federation_tools::get_cross_repo_blast_radius(fed, symbol, depth) {
                                        Ok(r) => {
                                            let text = match serde_json::to_string(&r) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            return Ok(jsonrpc_tool_result(id, &text, false));
                                        }
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    }
                                }
                                "get_cross_repo_blast_radius_for_repo" => {
                                    let repo_id = match args_map.get("repo_id").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: repo_id", true)),
                                    };
                                    let symbol = match args_map.get("symbol").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: symbol", true)),
                                    };
                                    // Same reasoning as the stdio path: `depth: 2`
                                    // must not be reported as a missing argument.
                                    let depth_owned = match crate::server::tools::utils::required_str_arg(
                                        &args_map, "depth",
                                    ) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e.to_string(), true)),
                                    };
                                    let depth = match parse_depth_range(&depth_owned) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e, true)),
                                    };
                                    match crate::server::mcp::federation_tools::get_cross_repo_blast_radius_for_repo(fed, repo_id, symbol, depth) {
                                        Ok(r) => {
                                            let text = match serde_json::to_string(&r) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            return Ok(jsonrpc_tool_result(id, &text, false));
                                        }
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    }
                                }
                                _ => {}
                            }
                        }

                        // Workspace tools: only registered when a
                        // workspaces file was supplied to the server
                        // constructor. Mirrors the stdio dispatch in
                        // handle_call_tool_request.
                        //
                        // Read through the shared lock so a
                        // `set_workspace` swap from the rebuild
                        // loop is reflected on the very next request
                        // hitting this connection.
                        if let Some(workspaces_lock) = &workspaces {
                            let workspaces: &crate::federation::workspace::WorkspacesFile = &*workspaces_lock.read();
                            match name {
                                "list_workspaces" => {
                                    let active = crate::state::ActiveWorkspace::load().ok().flatten();
                                    let infos = crate::server::mcp::federation_tools::list_workspaces(workspaces, active.as_ref());
                                    let text = match serde_json::to_string(&infos) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                    };
                                    return Ok(jsonrpc_tool_result(id, &text, false));
                                }
                                "get_active_workspace" => {
                                    let fed_ref = match federation.as_deref() {
                                        Some(f) => f,
                                        None => return Ok(jsonrpc_tool_result(
                                            id,
                                            &format!("{}", crate::error::LainError::Workspace("get_active_workspace requires federation mode".into())),
                                            true,
                                        )),
                                    };
                                    return match crate::server::mcp::federation_tools::get_active_workspace(fed_ref, workspaces) {
                                        Ok(info) => {
                                            let text = match serde_json::to_string(&info) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            Ok(jsonrpc_tool_result(id, &text, false))
                                        }
                                        Err(e) => Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    };
                                }
                                "get_workspace" => {
                                    let name_arg = args_map.get("name").and_then(|v| v.as_str());
                                    let name_str = match name_arg {
                                        Some(s) => s.to_string(),
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: name", true)),
                                    };
                                    let detail = match federation.as_deref() {
                                        Some(fed) => crate::server::mcp::federation_tools::get_workspace(fed, workspaces, &name_str),
                                        None => {
                                            // Defensive fallback: no federation
                                            // means the workspace tools shouldn't
                                            // have been registered. Build a minimal
                                            // detail from the workspaces file.
                                            match workspaces.workspaces.iter().find(|w| w.name == name_str) {
                                                Some(ws) => Ok(crate::server::mcp::federation_tools::WorkspaceDetail {
                                                    name: ws.name.clone(),
                                                    description: ws.description.clone(),
                                                    source: None,
                                                    members: ws.members.iter().map(|m| crate::server::mcp::federation_tools::WorkspaceRepoInfo {
                                                        repo_id: m.clone(),
                                                        path: String::new(),
                                                        health: "not_loaded".into(),
                                                    }).collect(),
                                                }),
                                                None => Err(crate::error::LainError::NotFound(format!("workspace {name_str}"))),
                                            }
                                        }
                                    };
                                    return match detail {
                                        Ok(d) => {
                                            let text = match serde_json::to_string(&d) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            Ok(jsonrpc_tool_result(id, &text, false))
                                        }
                                        Err(e) => Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    };
                                }
                                "get_workspace_graph" => {
                                    let filter = args_map.get("filter").and_then(|v| v.as_str());
                                    return match federation.as_deref() {
                                        Some(fed) => match crate::server::mcp::federation_tools::get_workspace_graph(fed, workspaces, filter) {
                                            Ok(graph) => {
                                                let text = match serde_json::to_string(&graph) {
                                                    Ok(s) => s,
                                                    Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                                };
                                                Ok(jsonrpc_tool_result(id, &text, false))
                                            }
                                            Err(e) => Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                        },
                                        None => Ok(jsonrpc_tool_result(
                                            id,
                                            &format!("{}", crate::error::LainError::Workspace("get_workspace_graph requires federation mode".into())),
                                            true,
                                        )),
                                    };
                                }
                                _ => {}
                            }
                        }

                        if let Some(fed) = &federation {
                            match resolve_repo_or_error(fed, &args_map) {
                                Ok(rid) => {
                                    // Inject the resolved `repo_id` into
                                    // the args the executor will see (Task
                                    // 19 round-1 fix). Existing per-repo
                                    // tools resolve against `ctx.graph`
                                    // and ignore this; future
                                    // federation-aware handlers can read it.
                                    args_map.insert(
                                        "repo_id".into(),
                                        serde_json::Value::String(rid.as_str().to_string()),
                                    );
                                }
                                Err(text) => return Ok(jsonrpc_tool_result(id, &text, true)),
                            }
                        }

                        // Recompute the args reference after the
                        // `repo_id` injection so a previously-empty
                        // `args_map` (which would have produced
                        // `args = None`) now flows as `Some(&args_map)`.
                        let args: Option<&serde_json::Map<String, serde_json::Value>> =
                            if args_map.is_empty() { None } else { Some(&args_map) };

                        match executor.call(name, args).await {
                            Ok(text) => {
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "result": {
                                        "content": [{"type": "text", "text": text}],
                                        "isError": false
                                    },
                                    "id": id
                                })
                            }
                            Err(e) => {
                                serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "result": {
                                        "content": [{"type": "text", "text": format!("Error: {}", e)}],
                                        "isError": true
                                    },
                                    "id": id
                                })
                            }
                        }
                    }
                    _ => {
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32601, "message": format!("Unknown method: {}", rpc_method)},
                            "id": id
                        })
                    }
                }
            }
            Err(e) => {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {}", e)},
                    "id": null
                })
            }
        };

        let response_str = serde_json::to_string(&rpc_response).unwrap_or_default();
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(full_body(Bytes::from(response_str)))
            .unwrap());
    }

    // GET /ui/blast-radius/{id} -> interactive blast radius graph
    if method == Method::GET && path.starts_with("/ui/blast-radius/") {
        let session_id = match path.strip_prefix("/ui/blast-radius/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(full_body(Bytes::from("Invalid path"))).unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (symbol, nodes) = match &session.data {
                crate::tools::UiSessionData::BlastRadius { symbol, nodes } => (symbol, nodes),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(full_body(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = BLAST_RADIUS_HTML.to_string();
            html = html.replace("SYMBOL_PLACEHOLDER", &symbol);
            html = html.replace("NODES_PLACEHOLDER", &serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(full_body(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(full_body(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // GET /ui/coupling/{id} -> interactive coupling heatmap
    if method == Method::GET && path.starts_with("/ui/coupling/") {
        let session_id = match path.strip_prefix("/ui/coupling/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(full_body(Bytes::from("Invalid path"))).unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (symbol, files, _) = match &session.data {
                crate::tools::UiSessionData::Coupling { symbol, files, .. } => (symbol, files, &()),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(full_body(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = COUPLING_HTML.to_string();
            html = html.replace("SYMBOL_PLACEHOLDER", symbol);
            html = html.replace("FILES_PLACEHOLDER", &serde_json::to_string(files).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(full_body(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(full_body(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // GET /ui/call-chain/{id} -> interactive call chain diagram
    if method == Method::GET && path.starts_with("/ui/call-chain/") {
        let session_id = match path.strip_prefix("/ui/call-chain/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(full_body(Bytes::from("Invalid path")))
                .unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (from, to, path) = match &session.data {
                crate::tools::UiSessionData::CallChain { from, to, path } => (from, to, path),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(full_body(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = CALL_CHAIN_HTML.to_string();
            html = html.replace("FROM_PLACEHOLDER", from);
            html = html.replace("TO_PLACEHOLDER", to);
            html = html.replace("PATH_PLACEHOLDER", &serde_json::to_string(path).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(full_body(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(full_body(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // GET /overlay/subscribe -> newline-delimited JSON stream of overlay
    // diffs. Sidecars consume this to mirror the owner's volatile overlay
    // across processes. Each frame is one `OverlayDiff` serialized as
    // JSON followed by a single `\n`; the body stays open until the
    // server shuts down or the client closes the connection.
    if method == Method::GET && path == "/overlay/subscribe" {
        let (tx, rx) = mpsc::unbounded_channel::<std::io::Result<Bytes>>();
        let mut bus_rx = crate::overlay::subscribe_channel();
        tokio::spawn(async move {
            loop {
                match bus_rx.recv().await {
                    Ok(diff) => {
                        let json = match serde_json::to_vec(&diff) {
                            Ok(v) => v,
                            Err(e) => {
                                debug!(
                                    "overlay subscribe: failed to serialize diff: {}",
                                    e
                                );
                                continue;
                            }
                        };
                        let mut chunk = Vec::with_capacity(json.len() + 1);
                        chunk.extend_from_slice(&json);
                        chunk.push(b'\n');
                        if tx.send(Ok(Bytes::from(chunk))).is_err() {
                            // Receiver dropped — client disconnected.
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Slow subscriber. Skip the gap; keep streaming.
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Sender is gone — process is shutting down. Close
                        // the response by dropping the sender.
                        break;
                    }
                }
            }
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/x-ndjson")
            .header("Cache-Control", "no-cache")
            .body(UnsyncBoxBody::new(OverlaySubscribeBody::new(rx)))
            .unwrap());
    }

    // GET /overlay/get_snapshot -> JSON array of every node currently in
    // the volatile overlay. This is the polling fallback used by sidecars
    // that don't (yet) speak the streaming protocol. A snapshot will
    // briefly miss changes that arrive between the read and the response,
    // but the sidecar's overlay stays coherent because each node is
    // upserted by id.
    if method == Method::GET && path == "/overlay/get_snapshot" {
        let nodes = executor.overlay().get_all_nodes();
        let body = match serde_json::to_vec(&nodes) {
            Ok(v) => Bytes::from(v),
            Err(e) => {
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("Content-Type", "application/json")
                    .body(full_body(Bytes::from(
                        format!("snapshot encode failed: {}", e),
                    )))
                    .unwrap())
            }
        };
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(full_body(body))
            .unwrap());
    }

    // Command Center SPA shell (Task 4.3). Static assets are compiled in
    // with `include_str!` so the binary remains self-contained and the SPA
    // can be served without a filesystem dependency. Asset list:
    //   GET /              -> index.html (text/html)
    //   GET /index.html    -> index.html
    //   GET /app.js        -> app.js (text/javascript)
    //   GET /theme.css     -> theme.css (text/css), the shared palette
    //   GET /styles.css    -> styles.css (text/css)
    //   GET /assets/*      -> vendored static assets under command_center/assets/
    if method == Method::GET && (path == "/" || path == "/index.html") {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(full_body(Bytes::from_static(INDEX_HTML)))
            .unwrap());
    }
    if method == Method::GET && path == "/app.js" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/javascript; charset=utf-8")
            .body(full_body(Bytes::from_static(APP_JS)))
            .unwrap());
    }
    if method == Method::GET && path == "/theme.css" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/css; charset=utf-8")
            .body(full_body(Bytes::from_static(THEME_CSS)))
            .unwrap());
    }
    if method == Method::GET && path == "/styles.css" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/css; charset=utf-8")
            .body(full_body(Bytes::from_static(STYLES_CSS)))
            .unwrap());
    }
    if method == Method::GET && path.starts_with("/assets/") {
        // Whitelist the known SPA assets so an unvetted path under /assets/
        // can't be used to exfiltrate other include_str!() targets. The
        // SPA_ASSETS table lives in `mcp::command_center_assets`.
        for (route, body) in SPA_ASSETS {
            if path == *route {
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "text/javascript; charset=utf-8")
                    .body(full_body(Bytes::from_static(body.as_bytes())))
                    .unwrap());
            }
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(full_body(Bytes::from("asset not found")))
            .unwrap());
    }

    // 404 for everything else
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(full_body(Bytes::from("Not Found")))
        .unwrap())
}

/// Streaming response body for `/overlay/subscribe`. Polls an mpsc
/// channel that is fed by a tokio task that pumps broadcast events into
/// JSON bytes. When the client disconnects (the channel is closed), the
/// body returns `None` and hyper finishes the response.
pub(crate) fn special_tool_definitions() -> Vec<crate::tools::definitions::ToolDefinition> {
    use crate::tools::definitions::ToolDefinition;
    vec![
        ToolDefinition {
            name: "get_health",
            description: "Return server health, node/edge counts, last enriched commit, and language-server availability.",
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "get_agent_strategy",
            description: "Return the recommended tool sequence and quick-reference for working with Lain.",
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        },
        ToolDefinition {
            name: "install_language_server",
            description: "Install the language server for the given file extension (e.g. 'rs', 'py') or language name (e.g. 'rust').",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "language": { "type": "string", "description": "File extension like 'rs'/'py' OR language name like 'rust'/'python'." } },
                "required": ["language"]
            }),
        },
        ToolDefinition {
            name: "register_job_webhook",
            description: "Register a webhook URL to receive notifications when background jobs complete.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "get_job_status",
            description: "Get the current status and output of a previously-spawned background job.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "job_id": { "type": "string" } },
                "required": ["job_id"]
            }),
        },
        ToolDefinition {
            name: "debug_sleep",
            description: "Sleep for the given number of seconds (useful for testing job infrastructure).",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "secs": { "type": "integer", "default": 1 } }
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::mcp::envelope::arg_property_schema;
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::error::LainError;
    use std::sync::Arc;

    /// Pins the fix for the live e2e finding: the advertised schema for
    /// `claim_files` / `release_files` must include `agent_id` +
    /// `session_token` (the handlers require them) and `files` must be
    /// typed as an array — when every arg was `type: string`,
    /// schema-respecting clients (Claude Code) serialized the files
    /// array into a JSON string and the call was unusable.
    #[test]
    fn claim_files_schema_has_auth_args_and_typed_files() {
        let defs: std::collections::HashMap<_, _> = SERVER_TOOL_DEFS
            .iter()
            .map(|t| (t.name, t.required_args))
            .collect();
        for tool in ["claim_files", "release_files"] {
            let required = defs.get(tool).unwrap_or_else(|| panic!("{tool} not in SERVER_TOOL_DEFS"));
            for arg in ["agent_id", "session_token", "files"] {
                assert!(
                    required.contains(&arg),
                    "{tool} must advertise required arg {arg}, has: {required:?}"
                );
            }
            let files = arg_property_schema("files");
            assert_eq!(
                files.get("type").and_then(|v| v.as_str()),
                Some("array"),
                "files must be typed array, not string"
            );
        }
        // The generic fallback stays a string for scalar args.
        assert_eq!(
            arg_property_schema("agent_id").get("type").and_then(|v| v.as_str()),
            Some("string")
        );
    }

    /// Wishlist #9. A tool that cannot answer must not be advertised —
    /// and, just as importantly, a tool that *can* must not be hidden.
    /// `find_dead_code` is the case that makes the distinction: without
    /// the NLP model it refuses only the `like` argument and answers
    /// normally otherwise, so hiding it would remove a working tool.
    #[test]
    fn only_fully_inert_tools_are_filtered_from_tools_list() {
        let stub = crate::server::nlp::NlpEmbedder::new_with_threads(0).expect("stub embedder");
        assert!(stub.is_stub(), "fixture must be a stub embedder");

        let inert = inert_tool_names(&stub);
        assert!(
            inert.contains(&"semantic_search"),
            "semantic_search returns Unavailable for every call without the \
             model, so advertising it just costs the caller a round trip"
        );
        assert!(
            !inert.contains(&"find_dead_code"),
            "find_dead_code works without the model (it only refuses `like`); \
             hiding it would drop a working tool"
        );
        assert!(
            !inert.contains(&"query_graph"),
            "query_graph takes the embedder but does not require it"
        );

        // Every name we filter must be a real tool, or the filter is
        // silently doing nothing.
        let known: std::collections::HashSet<&str> =
            crate::tools::registry::ToolRegistry::definitions()
                .iter()
                .map(|d| d.name)
                .collect();
        for name in inert {
            assert!(
                known.contains(name),
                "{name} is filtered but is not a registered tool — the filter \
                 is a no-op and the stale name hides the fact"
            );
        }
    }

    #[test]
    fn explicit_repo_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let rid = resolve_repo_for_tool(&fed, None, Some("repo-a")).unwrap();
        assert_eq!(rid.as_str(), "repo-a");
    }

    /// An unresolvable symbol hint must not fail a single-repo call.
    /// The hint's job is choosing a repo; with one repo there is nothing
    /// to choose. `query_graph` returns node ids, `resolve_symbol` only
    /// indexes names, so every id `query_graph` handed back was rejected
    /// by `get_blast_radius` / `explain_symbol` / `get_code_snippet` —
    /// with an error blaming uncommitted code, which was never the cause.
    #[tokio::test]
    async fn unresolvable_hint_still_dispatches_when_there_is_one_repo() {
        use crate::federation::repo_source::WorkspaceDirSource;

        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let src_dir = tempfile::tempdir().unwrap();
        git2::Repository::init(src_dir.path()).unwrap();
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(
                RepoId::new("only-repo").unwrap(),
                src_dir.path().to_path_buf(),
            )
            .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();

        // A node id, not a name — nothing the symbol index can match.
        let rid = resolve_repo_for_tool(
            &fed,
            Some("3d139b4e-688d-51a9-af69-0c164e9aea92"),
            None,
        )
        .expect("a single-repo federation must dispatch regardless of the hint");
        assert_eq!(rid.as_str(), "only-repo");
    }

    #[test]
    fn no_symbol_no_explicit_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        assert!(matches!(resolve_repo_for_tool(&fed, None, None), Err(LainError::Config(_))));
    }

    /// Verifies the round-1 fix: when `resolve_repo_or_error` resolves a
    /// symbol to a single repo, the caller now propagates the resolved
    /// `RepoId` (as `repo_id` in the args) instead of discarding it.
    /// We exercise the resolver + the manual injection step the dispatcher
    /// performs in both stdio and HTTP paths.
    #[test]
    fn symbol_hint_resolves_and_injects_repo_id() {
        use crate::schema::NodeType;

        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        fed.backend()
            .upsert_node_global(
                "repo-x:Function:src/lib.rs:only_one",
                NodeType::Function,
                "src/lib.rs",
                "only_one",
            )
            .unwrap();

        // Resolve via the same helper the dispatcher uses, then mirror
        // the dispatcher's injection step.
        let mut args = Map::new();
        args.insert(
            "symbol".into(),
            serde_json::Value::String("only_one".into()),
        );
        let rid = resolve_repo_or_error(&fed, &args).expect("unique symbol should resolve");
        assert_eq!(rid.as_str(), "repo-x");
        args.insert(
            "repo_id".into(),
            serde_json::Value::String(rid.as_str().to_string()),
        );
        assert_eq!(
            args.get("repo_id").and_then(|v| v.as_str()),
            Some("repo-x"),
            "resolved repo_id must be present in args after dispatcher's injection step",
        );
    }

    /// Asserts the JSON shape of the `AmbiguousSymbol` error surfaced
    /// through the dispatcher. This pins down the spec deviation
    /// documented in the report (Important issue 2): the payload is
    /// shipped as JSON text inside `CallToolResult::content`, not via
    /// `CallToolResult::structured_content`, so the agent can parse it
    /// today without bumping the rust-mcp-sdk schema.
    #[test]
    fn ambiguous_symbol_serializes_as_structured_json() {
        use crate::schema::NodeType;

        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        fed.backend()
            .upsert_node_global(
                "repo-a:Function:src/lib.rs:shared",
                NodeType::Function,
                "src/lib.rs",
                "shared",
            )
            .unwrap();
        fed.backend()
            .upsert_node_global(
                "repo-b:Function:src/lib.rs:shared",
                NodeType::Function,
                "src/lib.rs",
                "shared",
            )
            .unwrap();

        let mut args = Map::new();
        args.insert("symbol".into(), serde_json::Value::String("shared".into()));
        let text = resolve_repo_or_error(&fed, &args)
            .expect_err("duplicate symbol must surface as AmbiguousSymbol");

        // The payload must be valid JSON with the documented shape.
        let v: serde_json::Value = serde_json::from_str(&text)
            .expect("AmbiguousSymbol error must serialize as JSON for SDK compatibility");
        assert_eq!(v["error"], "ambiguous_symbol");
        let cands: Vec<&str> = v["candidates"]
            .as_array()
            .expect("candidates must be a JSON array")
            .iter()
            .map(|c| c.as_str().expect("each candidate is a string"))
            .collect();
        assert_eq!(cands.len(), 2, "exactly two repos match the shared symbol");
        assert!(cands.contains(&"repo-a"));
        assert!(cands.contains(&"repo-b"));
        assert!(
            v["message"].as_str().is_some(),
            "message field must be present and a string",
        );
    }

    /// Verifies that when `repo_id` is provided explicitly, the symbol
    /// hint is ignored — `resolve_repo_or_error` short-circuits to the
    /// explicit id. This is the priority ordering documented on
    /// `resolve_repo_for_tool`: explicit > symbol > single-repo fallback.
    #[test]
    fn explicit_repo_id_overrides_symbol_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let mut args = Map::new();
        args.insert(
            "repo_id".into(),
            serde_json::Value::String("explicit-repo".into()),
        );
        args.insert(
            "symbol".into(),
            serde_json::Value::String("any-symbol".into()),
        );
        let rid = resolve_repo_or_error(&fed, &args)
            .expect("explicit repo_id must short-circuit past the symbol hint");
        assert_eq!(rid.as_str(), "explicit-repo");
    }

    /// `GET /health` must serialize the federation summary so the UI
    /// can detect federation mode without a separate `tools/call`
    /// round-trip. When `FederatedIndex` is None (single-workspace
    /// mode) the field is `null`; when set it carries the repo
    /// roster and aggregate stats.
    ///
    /// The test pins down EXACT values, not just the JSON shape: it
    /// registers two repos via `add_repo`, upserts a known number of
    /// nodes and edges into the backend, then asserts that
    /// `total_nodes`, `total_edges`, and the 200-byte-per-node +
    /// 100-byte-per-edge `memory_estimate_bytes` formula all match
    /// the seeded counts. This catches a producer that silently drops
    /// data, conflates counts, or breaks the memory formula.
    #[tokio::test]
    async fn health_response_includes_federation_blob_when_set() {
        use crate::federation::repo_source::WorkspaceDirSource;
        use crate::schema::{EdgeType, GraphEdge, NodeType};

        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));

        // Register two repos. `WorkspaceDirSource` requires a path that
        // exists, so init a throwaway git repo for each source (same
        // pattern as `federated_index_tests::add_repo_registers_and_lists_it`).
        for name in ["repo-a", "repo-b"] {
            let src_dir = tempfile::tempdir().unwrap();
            git2::Repository::init(src_dir.path()).unwrap();
            let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
                WorkspaceDirSource::new(RepoId::new(name).unwrap(), src_dir.path().to_path_buf())
                    .unwrap(),
            );
            fed.add_repo(src, tmp.path()).await.unwrap();
        }

        // Populate the backend with a known number of nodes and edges
        // so `node_count()` / `edge_count()` return deterministic values
        // the assertions can check against.
        let backend = fed.backend();
        backend
            .upsert_node_global(
                "repo-a:Function:src/x.rs:shared",
                NodeType::Function,
                "src/x.rs",
                "shared",
            )
            .unwrap();
        backend
            .upsert_node_global(
                "repo-b:Function:src/x.rs:shared",
                NodeType::Function,
                "src/x.rs",
                "shared",
            )
            .unwrap();
        backend
            .upsert_node_global(
                "repo-b:Function:src/y.rs:caller",
                NodeType::Function,
                "src/y.rs",
                "caller",
            )
            .unwrap();
        backend
            .upsert_edge(GraphEdge::new(
                EdgeType::Calls,
                "repo-b:Function:src/y.rs:caller".into(),
                "repo-a:Function:src/x.rs:shared".into(),
            ))
            .unwrap();
        backend
            .upsert_edge(GraphEdge::new(
                EdgeType::Calls,
                "repo-b:Function:src/y.rs:caller".into(),
                "repo-b:Function:src/x.rs:shared".into(),
            ))
            .unwrap();

        let body = build_health_body(0, 0, Some(&fed));

        // Top-level keys are preserved.
        assert_eq!(body["status"], "ok");
        assert_eq!(body["server"], "lain");
        assert!(body["graph_nodes"].is_number());
        assert!(body["graph_edges"].is_number());
        assert!(body["tools_count"].is_number());

        // Federation blob shape and contents.
        let fed_blob = body
            .get("federation")
            .expect("federation key must be present in /health response");
        assert!(
            fed_blob.is_object(),
            "federation must be an object when federation is set, got {fed_blob}",
        );
        assert!(fed_blob["repos"].is_array(), "repos must be a JSON array");
        let repos = fed_blob["repos"]
            .as_array()
            .expect("repos must be a JSON array");

        // Exact repo roster: both registered repos are present with their
        // ids and the default `Indexing` health.
        assert_eq!(
            repos.len(),
            2,
            "expected both registered repos in the federation blob, got {repos:?}",
        );
        let ids: Vec<&str> = repos
            .iter()
            .map(|r| {
                r.get("id")
                    .and_then(|v| v.as_str())
                    .expect("each repo entry has an id string")
            })
            .collect();
        assert!(ids.contains(&"repo-a"), "repo-a missing from federation blob: {ids:?}");
        assert!(ids.contains(&"repo-b"), "repo-b missing from federation blob: {ids:?}");
        for r in repos {
            assert_eq!(
                r.get("health").and_then(|v| v.as_str()),
                Some("indexing"),
                "every repo entry must carry the default Indexing health until projection, got {r:?}",
            );
        }

        // Exact aggregate counts: 3 nodes upserted above, 2 edges.
        assert_eq!(
            fed_blob["total_nodes"].as_u64(),
            Some(3),
            "total_nodes must equal the number of nodes upserted into the backend",
        );
        assert_eq!(
            fed_blob["total_edges"].as_u64(),
            Some(2),
            "total_edges must equal the number of edges upserted into the backend",
        );

        // Exact memory formula: 200 bytes/node + 100 bytes/edge.
        let expected_mem: u64 = 3 * 200 + 2 * 100;
        assert_eq!(
            fed_blob["memory_estimate_bytes"].as_u64(),
            Some(expected_mem),
            "memory_estimate_bytes must be total_nodes*200 + total_edges*100 ({expected_mem})",
        );
    }

    /// Single-workspace mode (the default `lain --workspace ./myrepo`
    /// path) must continue to work — `federation` serializes as JSON
    /// `null` so the UI knows to render single-repo chrome.
    #[test]
    fn health_response_has_null_federation_when_unset() {
        let body = build_health_body(0, 0, None);
        assert!(
            body.get("federation")
                .map(|v| v.is_null())
                .unwrap_or(false),
            "federation field must serialize as null when no federation is set, got {:?}",
            body.get("federation"),
        );
    }

    // ------------------------------------------------------------------
    // Task 1.3 round 2: envelope-level `revision` tests.
    //
    // The fix moves `revision: u64` out of the tool payload and into
    // the outer `CallToolResult` envelope (`_meta.revision`). The two
    // tests below pin down both halves of that contract:
    //
    //   1. envelope carries `revision` and it is monotonic across
    //      overlay mutations (the Important finding from round 1 was
    //      that the helper never reached the dispatcher — these tests
    //      exercise the constructor itself, which is what every
    //      dispatcher path routes through, so a regression in any
    //      dispatch site will be caught at the helper level here).
    //
    //   2. bare-array text payloads stay bare (the Critical finding
    //      from round 1 was that `list_repos` had its payload wrapped
    //      as `{ "value": [...], "revision": N }`).
    //
    // Both are written as unit tests on the constructor instead of
    // driving `LainMcpServer` — the brief explicitly allows this when
    // no test seam exists (constructing a `LainHandler` would require
    // implementing 25+ required `McpServer` trait methods for no
    // benefit). Every dispatcher path calls `tool_text_result` (or
    // `jsonrpc_tool_result` for HTTP), so these helper-level tests
    // exercise the contract the dispatcher depends on.
    // ------------------------------------------------------------------

    /// Top-level envelope carries `_meta.revision`, and the revision
    /// advances monotonically after an overlay mutation. Also pins
    /// that the inner `content[0].text` does NOT carry a `revision`
    /// field — it stays in the envelope, not the payload.
    #[test]
    fn call_tool_result_envelope_carries_revision() {
        use crate::overlay::VolatileOverlay;
        use crate::schema::NodeType;

        let overlay = VolatileOverlay::new();
        let rev_before = overlay.current_revision();
        // GraphNode lives in `crate::server::schema`; pull it in via
        // the canonical path used elsewhere in the test module.
        use crate::server::schema::GraphNode;

        // First response.
        let payload_text = r#"{"name":"alpha","active":true}"#;
        let result_a = tool_text_result(payload_text.to_string(), false, &overlay, None);
        let json_a: serde_json::Value = serde_json::to_value(&result_a)
            .expect("CallToolResult must serialize to JSON");
        let rev_a = json_a["_meta"]["revision"]
            .as_u64()
            .expect("envelope must carry _meta.revision on every result");

        // Mutate the overlay so `current_revision()` advances.
        overlay.insert_node(GraphNode::new(
            NodeType::Function,
            "alpha".to_string(),
            "src/a.rs".to_string(),
        ));
        let rev_mid = overlay.current_revision();
        assert!(
            rev_mid > rev_before,
            "overlay should have advanced after the insert: before={rev_before} after={rev_mid}",
        );

        // Second response — revision must be >= the first.
        let second_payload = r#"{"name":"beta","status":"ready"}"#;
        let result_b = tool_text_result(second_payload.to_string(), false, &overlay, None);
        let json_b: serde_json::Value = serde_json::to_value(&result_b)
            .expect("CallToolResult must serialize to JSON");
        let rev_b = json_b["_meta"]["revision"]
            .as_u64()
            .expect("envelope must carry _meta.revision on every result");
        assert!(
            rev_b >= rev_a,
            "revision must be monotonic across calls (a={rev_a}, b={rev_b})",
        );

        // The inner `content[0].text` must be untouched in shape. Re-parse
        // and assert it has no top-level `revision` key — that would
        // mean the implementation leaked the field back into the tool
        // payload.
        let inner_text = json_b["content"][0]["text"]
            .as_str()
            .expect("content[0].text must be a string");
        assert_eq!(
            inner_text, second_payload,
            "inner text must be passed through verbatim",
        );
        let inner_parsed: serde_json::Value =
            serde_json::from_str(inner_text).expect("inner text should be valid JSON");
        assert!(
            inner_parsed.get("revision").is_none(),
            "inner payload must NOT carry a `revision` field (must live in envelope only), got {inner_parsed}",
        );
    }

    /// A bare-array text payload (what `list_repos` etc. produce) must
    /// stay a bare array in `content[0].text` — never re-wrapped as
    /// `{ "value": [...], "revision": N }`. This is the round-1
    /// Critical regression test.
    #[test]
    fn list_repos_keeps_bare_array_payload() {
        use crate::overlay::VolatileOverlay;

        let overlay = VolatileOverlay::new();

        // Mimic the exact shape a list_repos response would take
        // after serde_json::to_string on a Vec<RepoInfo>.
        let array_text = r#"[{"id":"repo-a","active":true},{"id":"repo-b","active":false}]"#;
        let result = tool_text_result(array_text.to_string(), false, &overlay, None);
        let json: serde_json::Value =
            serde_json::to_value(&result).expect("CallToolResult must serialize to JSON");

        // Inner text payload must be the bare JSON array, NOT an
        // object wrapper. This pins the additive wire contract.
        let inner_text = json["content"][0]["text"]
            .as_str()
            .expect("content[0].text must be a string");
        assert!(
            inner_text.starts_with('['),
            "bare-array payload must stay a bare array in content[0].text, got {inner_text:?}",
        );
        assert!(
            !inner_text.starts_with('{'),
            "bare-array payload must NOT be wrapped in an object, got {inner_text:?}",
        );

        // Round-trip through serde_json::from_str to confirm the
        // existing parser-shape contract (e2e shell scripts count
        // the array via len(...)) is preserved.
        let parsed: serde_json::Value =
            serde_json::from_str(inner_text).expect("inner text must be valid JSON");
        assert!(
            parsed.is_array(),
            "after round-trip, payload must still be a JSON array, got {parsed}",
        );
        assert_eq!(
            parsed.as_array().map(|a| a.len()),
            Some(2),
            "two-element array payload must parse to two elements",
        );
    }

    /// D-H3 — every tool that takes a caller-supplied agent identifier
    /// names that argument `agent_id`; every tool that takes a
    /// bearer credential names that argument `session_token`; the
    /// single non-agent identifier-taking tool takes `repo_id`.
    /// The presence-side tools are already on this surface; `get_repo_info`
    /// is renamed by another task. The audit happened because nothing
    /// here was pinned, so the rename is followed (in this test) by a
    /// check that catches the next drift.
    ///
    /// Read by hand against `SERVER_TOOL_DEFS` (`definitions.rs:147`)
    /// and `FEDERATION_TOOL_DEFS` (`definitions.rs:68`) once per release.
    #[test]
    fn tool_args_for_caller_identity_are_named_consistently() {
        let by_name: std::collections::HashMap<&str, &[&str]> = SERVER_TOOL_DEFS
            .iter()
            .chain(FEDERATION_TOOL_DEFS.iter())
            .map(|t| (t.name, t.required_args))
            .collect();

        // Tools pinned to an exact required-args list. Exact match (not
        // `contains`): a future tool that silently adds a new required
        // arg must update this test, which forces a thoughtful review.
        let expected: &[(&str, &[&str])] = &[
            ("heartbeat",      &["agent_id", "session_token"]),
            ("who_am_i",       &["session_token"]),
            ("list_subagents", &["session_token"]),
            ("claim_files",    &["agent_id", "session_token", "files"]),
            ("release_files",  &["agent_id", "session_token", "files"]),
            ("my_claims",      &["agent_id", "session_token"]),
            // (D-H3) The single non-agent identifier arg on the
            // server/federation surface. Renamed from `id` → `repo_id` in
            // Task 3; pinned here so a future change does not bring back
            // the generic `id`.
            ("get_repo_info",  &["repo_id"]),
        ];
        for (tool, want) in expected {
            let got = by_name.get(tool).unwrap_or_else(|| {
                panic!("{tool} missing from SERVER/FEDERATION TOOL_DEFS")
            });
            assert_eq!(
                want, got,
                "{tool} required args mismatch"
            );
        }

        // `register_agent` is the deliberate exception: the caller picks a
        // display *name* (server mints the agent_id). Pin BOTH that the arg
        // is `name` AND that it is *not* `agent_id`, so a future "consistency
        // sweep" does not rename it and break every caller.
        let reg: &[&str] = by_name
            .get("register_agent")
            .expect("register_agent missing from SERVER_TOOL_DEFS");
        assert!(
            reg.contains(&"name"),
            "register_agent must continue to take the agent's chosen display \
             name as `name` (server mints agent_id). Got: {reg:?}"
        );
        assert!(
            !reg.contains(&"agent_id"),
            "register_agent must NOT take `agent_id` — that would force \
             callers to supply an id the server hasn't minted yet. Got: {reg:?}"
        );
    }
}

// -------------------------------------------------------------------------
// Runtime integration test for P1 #1: `_meta.static_graph_generation`
// in the JSON-RPC envelope. Verifies the field is present on every tool
// response, defaults to `null` when no re-index has run, and reflects
// the actual `last_outcome.started_at` Unix epoch seconds when the
// re-index completed.
// -------------------------------------------------------------------------
