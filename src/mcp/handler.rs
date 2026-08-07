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
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::Map;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

const FRONT_END_HTML: &str = include_str!("front_end_monitor.html");

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
/// `AmbiguousSymbol` is surfaced as a structured JSON payload so the agent can
/// see the candidate repos and disambiguate.
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
        let args = params.arguments.as_ref().unwrap_or(&empty);

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
                    let id_str = match args.get("id").and_then(|v| v.as_str()) {
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
                    let query = match args.get("query").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: query".to_string(),
                                true,
                            ));
                        }
                    };
                    let limit: usize = match args.get("limit") {
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
                    let symbol = match args.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth_str = match args.get("depth").and_then(|v| v.as_str()) {
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
                    let repo_id = match args.get("repo_id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: repo_id".to_string(),
                                true,
                            ));
                        }
                    };
                    let symbol = match args.get("symbol").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(tool_text_result(
                                "Missing required argument: symbol".to_string(),
                                true,
                            ));
                        }
                    };
                    let depth_str = match args.get("depth").and_then(|v| v.as_str()) {
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
            if let Err(text) = resolve_repo_or_error(fed, args) {
                return Ok(tool_text_result(text, true));
            }
        }

        match self.executor.call(&params.name, Some(args)).await {
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
            protocol_version: ProtocolVersion::V2024_11_05.into(),
        }
    }
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    executor: Arc<ToolExecutor>,
    federation: Option<Arc<FederatedIndex>>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let jsonrpc_response = |value: serde_json::Value| -> Response<Full<Bytes>> {
        let body = serde_json::to_string(&value).unwrap_or_default();
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body)))
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
            .body(Full::new(Bytes::from(FRONT_END_HTML)))
            .unwrap());
    }

    // GET /health -> health check with graph stats
    if method == Method::GET && path == "/health" {
        let (nodes, edges) = executor.graph().get_stats();
        let health = serde_json::json!({
            "status": "ok",
            "server": "lain",
            "version": env!("CARGO_PKG_VERSION"),
            "graph_nodes": nodes,
            "graph_edges": edges,
            "tools_count": crate::tools::registry::ToolRegistry::definitions().len()
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(health.to_string())))
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
                        let tools_vec = crate::tools::registry::ToolRegistry::definitions();
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
                        let args_map: serde_json::Map<String, serde_json::Value> = params
                            .and_then(|p| p.get("arguments"))
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();
                        let args: Option<&serde_json::Map<String, serde_json::Value>> = if args_map.is_empty() { None } else { Some(&args_map) };

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
                            if let Err(text) = resolve_repo_or_error(fed, &args_map) {
                                return Ok(jsonrpc_tool_result(id, &text, true));
                            }
                        }

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
            .body(Full::new(Bytes::from(response_str)))
            .unwrap());
    }

    // GET /ui/blast-radius/{id} -> interactive blast radius graph
    if method == Method::GET && path.starts_with("/ui/blast-radius/") {
        let session_id = match path.strip_prefix("/ui/blast-radius/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("Invalid path"))).unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (symbol, nodes) = match &session.data {
                crate::tools::UiSessionData::BlastRadius { symbol, nodes } => (symbol, nodes),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(Full::new(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = include_str!("../ui/blast-radius.html").to_string();
            html = html.replace("SYMBOL_PLACEHOLDER", &symbol);
            html = html.replace("NODES_PLACEHOLDER", &serde_json::to_string(&nodes).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(Full::new(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(Full::new(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // GET /ui/coupling/{id} -> interactive coupling heatmap
    if method == Method::GET && path.starts_with("/ui/coupling/") {
        let session_id = match path.strip_prefix("/ui/coupling/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("Invalid path"))).unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (symbol, files, _) = match &session.data {
                crate::tools::UiSessionData::Coupling { symbol, files, .. } => (symbol, files, &()),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(Full::new(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = include_str!("../ui/coupling.html").to_string();
            html = html.replace("SYMBOL_PLACEHOLDER", symbol);
            html = html.replace("FILES_PLACEHOLDER", &serde_json::to_string(files).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(Full::new(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(Full::new(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // GET /ui/call-chain/{id} -> interactive call chain diagram
    if method == Method::GET && path.starts_with("/ui/call-chain/") {
        let session_id = match path.strip_prefix("/ui/call-chain/") {
            Some(s) => s,
            None => return Ok(Response::builder().status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("Invalid path"))).unwrap()),
        };
        let sessions = executor.ui_sessions().lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (from, to, path) = match &session.data {
                crate::tools::UiSessionData::CallChain { from, to, path } => (from, to, path),
                _ => return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(Full::new(Bytes::from("Invalid session type"))).unwrap()),
            };
            let mut html = include_str!("../ui/call-chain.html").to_string();
            html = html.replace("FROM_PLACEHOLDER", from);
            html = html.replace("TO_PLACEHOLDER", to);
            html = html.replace("PATH_PLACEHOLDER", &serde_json::to_string(path).unwrap_or_else(|_| "[]".to_string()));
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(Full::new(Bytes::from(html)))
                .unwrap());
        }
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html")
            .body(Full::new(Bytes::from("Session not found or expired")))
            .unwrap());
    }

    // 404 for everything else
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::from("Not Found")))
        .unwrap())
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
}