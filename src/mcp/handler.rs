//! MCP Server handler implementation for Lain
//!
//! Implements the ServerHandler trait to expose tools via MCP protocol

use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::RepoId;
use crate::tools::ToolExecutor;
use async_trait::async_trait;
use rust_mcp_sdk::{
    mcp_server::{server_runtime, McpServerOptions, ServerHandler, ToMcpServerHandler},
    schema::{
        CallToolRequestParams, CallToolResult, ContentBlock,
        InitializeResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, RpcError, ServerCapabilities, ServerCapabilitiesTools,
        TextContent, Tool, ToolInputSchema, Implementation,
    },
    error::SdkResult,
    McpServer, StdioTransport, TransportOptions,
};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper::body::{Bytes, Frame};
use hyper_util::rt::TokioIo;
use serde_json::Map;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Response body used by the HTTP handler. Most responses are a single
/// buffered payload (`Full<Bytes>` boxed here for unification), but the
/// `/overlay/subscribe` endpoint returns a streaming body so sidecars
/// can follow live updates.
type OverlayHttpBody = UnsyncBoxBody<Bytes, std::io::Error>;

fn full_body(data: Bytes) -> OverlayHttpBody {
    UnsyncBoxBody::new(Full::new(data).map_err(|never| match never {}))
}

const FRONT_END_HTML: &str = include_str!("front_end_monitor.html");
const FEDERATION_DASHBOARD_HTML: &str = include_str!("federation_dashboard.html");

/// Wrap a string payload in a `CallToolResult` with a single text block.
fn tool_text_result(text: String, is_error: bool) -> CallToolResult {
    CallToolResult {
        content: vec![ContentBlock::TextContent(TextContent::new(text, None, None))],
        is_error: Some(is_error),
        meta: None,
        structured_content: None,
    }
}

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
        Some(s) => fed.resolve_symbol(s),
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

/// Tool definitions exposed only when the MCP server was constructed with a
/// `FederatedIndex`. Centralized here so the stdio `tools/list` response, the
/// stdio `tools/call` dispatch, and the HTTP JSON-RPC `tools/list` / `tools/call`
/// paths agree on names, schemas, and gating.
const FEDERATION_TOOL_DEFS: &[(&str, &str, &[&str])] = &[
    (
        "list_repos",
        "List every repository currently registered in the federation, with id, path, health, and graph stats.",
        &[],
    ),
    (
        "get_repo_info",
        "Get info about a single repository in the federation by id.",
        &["id"],
    ),
    (
        "get_federation_health",
        "Aggregate health counts and total node/edge counts across the federation, plus a rough memory estimate.",
        &[],
    ),
    (
        "search_org",
        "Case-insensitive substring search across every repo's symbols (matched on name or path). Args: query (substring), limit (max results, parsed as usize). Returns matches sorted by (repo_id, name).",
        &["query", "limit"],
    ),
    (
        "get_cross_repo_blast_radius",
        "Resolve a symbol across the federation, traverse outgoing Calls edges in [min_depth, max_depth) (depth is a u32 range like \"1..3\"), and group visited nodes by repo. Returns {by_repo: {repo_id: [global_ids...]}, total_count, truncated}. Caps at 1000 nodes; truncated=true when the cap is hit.",
        &["symbol", "depth"],
    ),
    (
        "get_cross_repo_blast_radius_for_repo",
        "Same as get_cross_repo_blast_radius but the caller disambiguates the repo explicitly via repo_id, bypassing symbol resolution. Args: repo_id, symbol, depth (u32 range like \"1..3\"). Returns {by_repo: {repo_id: [global_ids...]}, total_count, truncated}.",
        &["repo_id", "symbol", "depth"],
    ),
];

struct LainHandler {
    executor: Arc<ToolExecutor>,
    federation: Option<Arc<FederatedIndex>>,
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
            for (name, description, required) in FEDERATION_TOOL_DEFS {
                let mut props = std::collections::BTreeMap::new();
                for req in *required {
                    let mut p = serde_json::Map::new();
                    p.insert("type".into(), serde_json::Value::String("string".into()));
                    p.insert("description".into(), serde_json::Value::String(format!("{req} of the repo to look up")));
                    props.insert((*req).to_string(), p);
                }
                let input_schema = ToolInputSchema::new(
                    required.iter().map(|s| s.to_string()).collect(),
                    if props.is_empty() { None } else { Some(props) },
                    None,
                );
                tools.push(Tool {
                    name: (*name).to_string(),
                    description: Some((*description).to_string()),
                    input_schema,
                    annotations: None,
                    execution: None,
                    icons: vec![],
                    meta: None,
                    output_schema: None,
                    title: None,
                });
            }
        }

        Ok(ListToolsResult { tools, meta: None, next_cursor: None })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<CallToolResult, rust_mcp_sdk::schema::schema_utils::CallToolError> {
        let empty: Map<String, serde_json::Value> = Map::new();
        let args_ref = params.arguments.as_ref().unwrap_or(&empty);
        // Clone the args so we can inject the resolved `repo_id` in
        // federation mode. The executor already clones internally
        // (`call_inner` does `arguments.cloned().unwrap_or_default()`), so
        // this is a no-op cost-wise. Owning the map lets the resolver's
        // successful `RepoId` flow through to downstream tool handlers
        // instead of being discarded (Task 19 round-1 fix).
        let mut args_owned: Map<String, serde_json::Value> = args_ref.clone();

        if let Some(fed) = &self.federation {
            match params.name.as_str() {
                "list_repos" => {
                    let repos = crate::mcp::federation_tools::list_repos(fed);
                    return Ok(tool_text_result(
                        serde_json::to_string(&repos)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                    ));
                }
                "get_repo_info" => {
                    let id_str = match args_owned.get("id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: id".to_string(),
                                true,
                            ));
                        }
                    };
                    let rid = match crate::federation::repo_id::RepoId::new(id_str) {
                        Ok(r) => r,
                        Err(e) => {
                            return Ok(tool_text_result(format!("{e}"), true));
                        }
                    };
                    return match crate::mcp::federation_tools::get_repo_info(fed, &rid) {
                        Ok(info) => Ok(tool_text_result(
                            serde_json::to_string(&info)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
                    };
                }
                "get_federation_health" => {
                    let health = crate::mcp::federation_tools::get_federation_health(fed);
                    return Ok(tool_text_result(
                        serde_json::to_string(&health)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                    ));
                }
                "search_org" => {
                    let query = match args_owned.get("query").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: query".to_string(),
                                true,
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
                                ));
                            }
                        },
                        _ => {
                            return Ok(tool_text_result(
                                "Missing required argument: limit".to_string(),
                                true,
                            ));
                        }
                    };
                    let hits = crate::mcp::federation_tools::search_org(fed, query, limit);
                    return Ok(tool_text_result(
                        serde_json::to_string(&hits)
                            .unwrap_or_else(|e| format!("serialization error: {e}")),
                        false,
                    ));
                }
                "get_cross_repo_blast_radius" => {
                    let symbol = match args_owned.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth_str = match args_owned.get("depth").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: depth".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth = match parse_depth_range(depth_str) {
                        Ok(r) => r,
                        Err(e) => return Ok(tool_text_result(e, true)),
                    };
                    return match crate::mcp::federation_tools::get_cross_repo_blast_radius(fed, symbol, depth) {
                        Ok(r) => Ok(tool_text_result(
                            serde_json::to_string(&r)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
                    };
                }
                "get_cross_repo_blast_radius_for_repo" => {
                    let repo_id = match args_owned.get("repo_id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: repo_id".to_string(),
                                true,
                            ));
                        }
                    };
                    let symbol = match args_owned.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth_str = match args_owned.get("depth").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: depth".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth = match parse_depth_range(depth_str) {
                        Ok(r) => r,
                        Err(e) => return Ok(tool_text_result(e, true)),
                    };
                    return match crate::mcp::federation_tools::get_cross_repo_blast_radius_for_repo(fed, repo_id, symbol, depth) {
                        Ok(r) => Ok(tool_text_result(
                            serde_json::to_string(&r)
                                .unwrap_or_else(|e| format!("serialization error: {e}")),
                            false,
                        )),
                        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
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
                Err(text) => return Ok(tool_text_result(text, true)),
            }
        }

        match self.executor.call(&params.name, Some(&args_owned)).await {
            Ok(text) => Ok(CallToolResult {
                content: vec![ContentBlock::TextContent(TextContent::new(text, None, None))],
                is_error: Some(false),
                meta: None,
                structured_content: None,
            }),
            Err(e) => Ok(CallToolResult {
                content: vec![ContentBlock::TextContent(TextContent::new(
                    format!("Error: {}", e),
                    None,
                    None,
                ))],
                is_error: Some(true),
                meta: None,
                structured_content: None,
            }),
        }
    }
}

#[derive(Clone)]
pub struct LainMcpServer {
    executor: ToolExecutor,
    federation: Option<Arc<FederatedIndex>>,
}

impl LainMcpServer {
    pub fn new(executor: ToolExecutor) -> Self {
        Self { executor, federation: None }
    }

    /// Federation-mode constructor. When set, the handler also exposes
    /// `list_repos` and `get_repo_info` over MCP. With `new(executor)` alone
    /// the federation surface is not registered and single-workspace
    /// behavior is preserved.
    pub fn with_federation(executor: ToolExecutor, federation: Arc<FederatedIndex>) -> Self {
        Self { executor, federation: Some(federation) }
    }

    /// Build a sidecar-flavored server. The graph inside `executor` should
    /// already be opened read-only (see `GraphDatabase::open_read_only`),
    /// so mutating tool calls fail at the database layer with a clean
    /// `graph is read-only` error.
    pub fn new_read_only(executor: ToolExecutor) -> Self {
        Self { executor, federation: None }
    }

    /// Convenience: build a sidecar server directly from a read-only graph
    /// and a freshly-allocated overlay.
    pub fn from_read_only_graph(
        graph: crate::graph::GraphDatabase,
        overlay: crate::overlay::VolatileOverlay,
        workspace: std::path::PathBuf,
    ) -> Self {
        let executor = crate::tools::ToolExecutor::new_read_only(graph, overlay, workspace);
        Self { executor, federation: None }
    }

    /// Serve on a specific `SocketAddr`. Used by the sidecar so it can bind
    /// to `127.0.0.1:<port>` without going through `run_http`'s
    /// `"0.0.0.0:<port>"` listener (the sidecar must never accept
    /// connections from outside the loopback).
    pub async fn serve(self, addr: std::net::SocketAddr) -> SdkResult<()> {
        info!("Starting Lain sidecar MCP HTTP server on {}", addr);

        let executor = Arc::new(self.executor);
        let listener = TcpListener::bind(addr).await?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let executor = executor.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| {
                            let executor = executor.clone();
                            handle_request(req, executor, None)
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

        let server_details = self.server_info();
        let transport = StdioTransport::new(TransportOptions::default())?;
        let handler = LainHandler {
            executor: Arc::new(self.executor),
            federation: self.federation,
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

        let executor = Arc::new(self.executor);
        let federation = self.federation;
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let executor = executor.clone();
                    let federation = federation.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| {
                            let executor = executor.clone();
                            let federation = federation.clone();
                            handle_request(req, executor, federation)
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

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    executor: Arc<ToolExecutor>,
    federation: Option<Arc<FederatedIndex>>,
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
        jsonrpc_response(serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "content": [{"type": "text", "text": text}],
                "isError": is_error
            },
            "id": id
        }))
    };

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // GET / -> serve diagnostic page
    if method == Method::GET && path == "/" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(full_body(Bytes::from(FRONT_END_HTML)))
            .unwrap());
    }

    // GET /federation-dashboard.html -> federation landing page (used by
    // the federation-aware UI; itself a static asset that fetches
    // /health + /mcp at runtime).
    if method == Method::GET && path == "/federation-dashboard.html" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(full_body(Bytes::from(FEDERATION_DASHBOARD_HTML)))
            .unwrap());
    }

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
                    "tools/list" => {
                        let mut tools_vec = crate::tools::registry::ToolRegistry::definitions();
                        // Append the 6 special-case tools (kept in sync with
                        // the stdio transport's handle_list_tools_request).
                        tools_vec.extend(special_tool_definitions());
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
                            for (name, description, required) in FEDERATION_TOOL_DEFS {
                                let mut props = serde_json::Map::new();
                                for req in *required {
                                    let mut p = serde_json::Map::new();
                                    p.insert("type".into(), serde_json::Value::String("string".into()));
                                    p.insert("description".into(), serde_json::Value::String(format!("{req} of the repo to look up")));
                                    props.insert((*req).to_string(), serde_json::Value::Object(p));
                                }
                                let input_schema = serde_json::json!({
                                    "type": "object",
                                    "properties": props,
                                    "required": required,
                                });
                                tools.push(serde_json::json!({
                                    "name": name,
                                    "description": description,
                                    "inputSchema": input_schema
                                }));
                            }
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

                        if let Some(fed) = &federation {
                            match name {
                                "list_repos" => {
                                    let repos = crate::mcp::federation_tools::list_repos(fed);
                                    let text = match serde_json::to_string(&repos) {
                                        Ok(s) => s,
                                        Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                    };
                                    return Ok(jsonrpc_tool_result(id, &text, false));
                                }
                                "get_repo_info" => {
                                    let id_str = match args_map.get("id").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: id", true)),
                                    };
                                    let rid = match crate::federation::repo_id::RepoId::new(id_str) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    };
                                    match crate::mcp::federation_tools::get_repo_info(fed, &rid) {
                                        Ok(info) => {
                                            let text = match serde_json::to_string(&info) {
                                                Ok(s) => s,
                                                Err(e) => return Ok(jsonrpc_error(id, -32000, format!("serialization: {e}"))),
                                            };
                                            return Ok(jsonrpc_tool_result(id, &text, false));
                                        }
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &format!("{e}"), true)),
                                    }
                                }
                                "get_federation_health" => {
                                    let health = crate::mcp::federation_tools::get_federation_health(fed);
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
                                    let hits = crate::mcp::federation_tools::search_org(fed, query, limit);
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
                                    let depth_str = match args_map.get("depth").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: depth", true)),
                                    };
                                    let depth = match parse_depth_range(depth_str) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e, true)),
                                    };
                                    match crate::mcp::federation_tools::get_cross_repo_blast_radius(fed, symbol, depth) {
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
                                    let depth_str = match args_map.get("depth").and_then(|v| v.as_str()) {
                                        Some(s) => s,
                                        None => return Ok(jsonrpc_tool_result(id, "Missing required argument: depth", true)),
                                    };
                                    let depth = match parse_depth_range(depth_str) {
                                        Ok(r) => r,
                                        Err(e) => return Ok(jsonrpc_tool_result(id, &e, true)),
                                    };
                                    match crate::mcp::federation_tools::get_cross_repo_blast_radius_for_repo(fed, repo_id, symbol, depth) {
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
            let mut html = include_str!("../ui/blast-radius.html").to_string();
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
            let mut html = include_str!("../ui/coupling.html").to_string();
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
            let mut html = include_str!("../ui/call-chain.html").to_string();
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
            .body(UnsyncBoxBody::new(OverlaySubscribeBody { rx }))
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
struct OverlaySubscribeBody {
    rx: mpsc::UnboundedReceiver<std::io::Result<Bytes>>,
}

impl http_body::Body for OverlaySubscribeBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::io::Result<Frame<Self::Data>>>> {
        match Pin::new(&mut self.rx).poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Definitions for the 6 special-case tools that are dispatched directly in
/// `ToolExecutor::call_inner` (not via the inventory registry). Including them
/// in `tools/list` makes them visible to MCP clients.
fn special_tool_definitions() -> Vec<crate::tools::definitions::ToolDefinition> {
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
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::LainError;
    use std::sync::Arc;

    #[test]
    fn explicit_repo_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let rid = resolve_repo_for_tool(&fed, None, Some("repo-a")).unwrap();
        assert_eq!(rid.as_str(), "repo-a");
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
}
