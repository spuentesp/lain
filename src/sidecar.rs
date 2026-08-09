//! Sidecar runtime: read-only graph, owner overlay subscription, MCP server.
//!
//! A sidecar process is a second Lain instance pointed at the same workspace
//! as an already-running owner. It opens the existing `.lain/graph.bin` as
//! immutable, subscribes to the owner's volatile overlay stream, and serves
//! the same 41 MCP tools that the owner does — but every mutating tool call
//! fails with `graph is read-only` because the static graph database was
//! opened via `GraphDatabase::open_read_only`.
//!
//! The owner publishes the stream over HTTP; the sidecar's client accepts
//! the NDJSON wire used by the owner (and SSE framing for compatibility),
//! retrying with backoff until `/overlay/subscribe` is available.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::mcp::LainMcpServer;
use crate::overlay::VolatileOverlay;

/// Configuration for `sidecar::run`. The caller (typically `main.rs`) is
/// responsible for translating the parsed CLI `Args` into this struct.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
    pub port: u16,
    /// HTTP base URL of the owner instance (e.g. `http://localhost:9999/mcp`).
    /// The sidecar appends `/overlay/subscribe` to subscribe to volatile
    /// overlay events.
    pub owner_url: String,
    #[allow(dead_code)]
    pub embedding_model: Option<PathBuf>,
}

/// Run the sidecar runtime.
///
/// Steps:
/// 1. Open the existing on-disk graph as read-only.
/// 2. Allocate a fresh volatile overlay (initially empty).
/// 3. Spawn the overlay subscription client, which connects to the owner
///    and merges incoming nodes/edges into the local overlay with
///    exponential-backoff retries.
/// 4. Start the MCP HTTP server bound to `127.0.0.1:cfg.port`.
pub async fn run(cfg: SidecarConfig) -> Result<(), LainError> {
    tracing::info!(
        "sidecar: opening graph at {:?} (read-only) and binding 127.0.0.1:{}",
        cfg.memory_path,
        cfg.port
    );
    let graph = GraphDatabase::open_read_only(&cfg.memory_path)?;
    let overlay = VolatileOverlay::new();

    // Subscribe to the owner's overlay stream. The client first hydrates
    // from `/overlay/get_snapshot`, then opens `/overlay/subscribe` and
    // feeds every `OverlayDiff` through the shared `subscribe_apply` task.
    // It accepts the `/mcp` URL convention used by agent configurations and
    // strips that suffix before requesting the root overlay endpoints.
    tokio::spawn(crate::overlay::stream::subscribe(
        cfg.owner_url.clone(),
        overlay.clone(),
    ));

    let server = LainMcpServer::from_read_only_graph(
        graph,
        overlay,
        cfg.workspace.clone(),
    );
    let addr: SocketAddr = ([127, 0, 0, 1], cfg.port).into();
    // `serve` is an infinite loop; surface its MCP error to the caller as
    // `LainError::Mcp` so `main.rs` can map it into anyhow.
    server.serve(addr).await.map_err(|e| LainError::Mcp(format!("{e:?}")))
}

/// Initial backoff between overlay subscription attempts.
pub const SUBSCRIBE_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// Maximum backoff between overlay subscription attempts.
pub const SUBSCRIBE_BACKOFF_MAX: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    /// The full owner+sidecar smoke test from the brief (which spawns two
    /// real `lain` processes against `LAIN_PORT`) lives in
    /// `tests/dual_instance.rs` (Task 7). Here we just exercise the
    /// lightweight contract: `open_read_only` rejects writes, reads work,
    /// and the constructor plumbs through.
    #[tokio::test]
    async fn run_sidecar_opens_graph_read_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("graph.bin");
        // Build a real graph with one node so open_read_only has something
        // to hydrate.
        let owner = GraphDatabase::new(&path).expect("new");
        let main_id = crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "main".into(),
            "/src/main.rs".into(),
        )
        .id;
        owner
            .upsert_node(crate::schema::GraphNode::new(
                crate::schema::NodeType::Function,
                "main".into(),
                "/src/main.rs".into(),
            ))
            .expect("upsert");
        owner.save_to_disk().await.expect("save");
        drop(owner);

        let cfg = SidecarConfig {
            workspace: tmp.path().to_path_buf(),
            memory_path: path.clone(),
            port: 0,
            owner_url: "http://127.0.0.1:1/mcp".into(),
            embedding_model: None,
        };
        let graph = GraphDatabase::open_read_only(&cfg.memory_path).expect("open ro");
        assert!(graph.is_read_only());
        // Reads succeed; writes fail.
        assert!(graph
            .get_node(&main_id)
            .expect("get")
            .is_some());
        assert!(graph
            .upsert_node(crate::schema::GraphNode::new(
                crate::schema::NodeType::Function,
                "x".into(),
                "/src/x.rs".into(),
            ))
            .is_err());
    }

    async fn health_text(port: u16) -> Option<String> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "get_health", "arguments": {} }
        });
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .json(&request)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let value: serde_json::Value = response.json().await.ok()?;
        value
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    }

    async fn wait_for_health(port: u16, require_overlay: bool, _label: &str) -> String {
        let started = std::time::Instant::now();
        loop {
            if let Some(text) = health_text(port).await {
                if !require_overlay || text.contains("**Volatile Nodes (Overlay):** 1") {
                    return text;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "HTTP server on port {port} did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn reserve_port() -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("reserve port");
        let port = listener.local_addr().expect("port address").port();
        drop(listener);
        port
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sidecar_tool_executor_reads_owner_overlay_and_stays_read_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_path = tmp.path().to_path_buf();
        let path = tmp.path().join("graph.bin");
        let static_node = crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "owner_static".into(),
            "/src/owner.rs".into(),
        );

        let graph = GraphDatabase::new(&path).expect("create owner graph");
        graph.upsert_node(static_node).expect("seed owner graph");
        graph.save_to_disk().await.expect("persist owner graph");
        drop(graph);

        // Use the test executor factory for the owner so its overlay is
        // injectable without loading heavyweight NLP/LSP assets.
        let owner_graph = GraphDatabase::new(&path).expect("reopen owner graph");
        let owner_executor = crate::tools::create_test_executor_with_graph(owner_graph);
        owner_executor.overlay().insert_node(crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "owner_live".into(),
            "/src/live.rs".into(),
        ));

        let owner_port = reserve_port().await;
        let sidecar_port = reserve_port().await;
        assert_ne!(owner_port, sidecar_port, "port probe returned duplicate ports");
        let owner_task = tokio::spawn(async move {
            let _ = LainMcpServer::new(owner_executor)
                .serve(([127, 0, 0, 1], owner_port).into())
                .await;
        });

        // `run` constructs the real read-only ToolExecutor and starts its
        // subscription client, just as the binary sidecar path does.
        let sidecar_workspace = workspace_path.clone();
        let sidecar_memory = path.clone();
        let sidecar_task = tokio::spawn(async move {
            if let Err(e) = run(SidecarConfig {
                workspace: sidecar_workspace,
                memory_path: sidecar_memory,
                port: sidecar_port,
                owner_url: format!("http://127.0.0.1:{owner_port}/mcp"),
                embedding_model: None,
            })
            .await
            {
                eprintln!("[sidecar] run returned error: {e}");
            }
        });

        // Yield once so the spawned tasks get a chance to start binding.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let owner_health = wait_for_health(owner_port, false, "owner").await;
        assert!(owner_health.contains("Operational"));
        let sidecar_health = wait_for_health(sidecar_port, true, "sidecar").await;
        assert!(sidecar_health.contains("Operational"));
        assert!(sidecar_health.contains("**Static Nodes:** 1"));
        assert!(sidecar_health.contains("**Volatile Nodes (Overlay):** 1"));

        // Exercise the same constructor used by `run` directly as well: a
        // read-only executor can answer reads but cannot mutate the graph.
        let read_only_graph = GraphDatabase::open_read_only(&path).expect("open sidecar graph");
        let read_only_executor = crate::tools::ToolExecutor::new_read_only(
            read_only_graph,
            VolatileOverlay::new(),
            workspace_path,
        );
        let direct_health = read_only_executor
            .call("get_health", None)
            .await
            .expect("read-only health call");
        assert!(direct_health.contains("Operational"));
        let write_error = read_only_executor
            .graph()
            .upsert_node(crate::schema::GraphNode::new(
                crate::schema::NodeType::Function,
                "should_fail".into(),
                "/src/should_fail.rs".into(),
            ))
            .expect_err("sidecar graph must reject writes");
        assert_eq!(write_error.to_string(), "graph is read-only");

        sidecar_task.abort();
        owner_task.abort();
    }
}
