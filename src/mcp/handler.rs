//! MCP Server handler implementation for Lain
//!
//! Implements the ServerHandler trait to expose tools via MCP protocol

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

struct LainHandler {
    executor: Arc<ToolExecutor>,
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

        Ok(ListToolsResult { tools, meta: None, next_cursor: None })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> std::result::Result<CallToolResult, rust_mcp_sdk::schema::schema_utils::CallToolError> {
        let empty: Map<String, serde_json::Value> = Map::new();
        let args = params.arguments.as_ref().unwrap_or(&empty);

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
}

impl LainMcpServer {
    pub fn new(executor: ToolExecutor) -> Self {
        Self { executor }
    }

    /// Build a sidecar-flavored server. The graph inside `executor` should
    /// already be opened read-only (see `GraphDatabase::open_read_only`),
    /// so mutating tool calls fail at the database layer with a clean
    /// `graph is read-only` error.
    pub fn new_read_only(executor: ToolExecutor) -> Self {
        Self { executor }
    }

    /// Convenience: build a sidecar server directly from a read-only graph
    /// and a freshly-allocated overlay.
    pub fn from_read_only_graph(
        graph: crate::graph::GraphDatabase,
        overlay: crate::overlay::VolatileOverlay,
        workspace: std::path::PathBuf,
    ) -> Self {
        let executor = crate::tools::ToolExecutor::new_read_only(graph, overlay, workspace);
        Self { executor }
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
                            handle_request(req, executor)
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
        let handler = LainHandler { executor: Arc::new(self.executor) };

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
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let executor = executor.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| {
                            let executor = executor.clone();
                            handle_request(req, executor)
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

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    executor: Arc<ToolExecutor>,
) -> Result<Response<OverlayHttpBody>, hyper::Error> {
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
                        let tools: Vec<serde_json::Value> = tools_vec
                            .iter()
                            .map(|def| {
                                serde_json::json!({
                                    "name": def.name,
                                    "description": def.description,
                                    "inputSchema": def.input_schema
                                })
                            })
                            .collect();
                        serde_json::json!({"jsonrpc": "2.0", "result": {"tools": tools}, "id": id})
                    }
                    "tools/call" => {
                        let name = params
                            .and_then(|p| p.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("");
                        let args: Option<&serde_json::Map<String, serde_json::Value>> = params
                            .and_then(|p| p.get("arguments"))
                            .and_then(|v| v.as_object());

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
                .body(full_body(Bytes::from("Invalid path"))).unwrap()),
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