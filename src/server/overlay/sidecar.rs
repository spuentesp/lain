//! Sidecar subscription: connect to an owner `lain` server's overlay
//! endpoints and merge its volatile-overlay diffs into a local
//! `VolatileOverlay`.
//!
//! Extracted from `src/server/overlay.rs` so the data-type concerns
//! (`VolatileOverlay`, `OverlayStats`, the revision log) stay in the
//! parent module and the transport concerns (HTTP, NDJSON/SSE
//! parsing, retry/backoff) live here.
//!
//! Wire protocol (Task 4):
//!   * `GET <owner_url>/overlay/get_snapshot` → JSON array of every
//!     `GraphNode` currently in the owner's volatile overlay. Called
//!     once per (re)connect to hydrate the local cache before streaming
//!     begins.
//!   * `GET <owner_url>/overlay/subscribe` → `application/x-ndjson`,
//!     one `OverlayDiff` per line. Stays open until the owner shuts
//!     down or the sidecar drops the connection.

use super::stream::{subscribe_apply, OverlayDiff};
use super::VolatileOverlay;
use crate::schema::GraphNode;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Subscribe to the owner's overlay stream and merge incoming `OverlayDiff`s
/// into `overlay`.
///
/// This function spawns the shared `stream::subscribe_apply` apply loop
/// exactly once and feeds it from a local broadcast channel; the
/// streaming body parser below pushes every parsed diff into that
/// channel. On any stream failure the function sleeps with exponential
/// backoff, re-hydrates from the snapshot endpoint, and reconnects.
pub async fn subscribe(owner_url: String, overlay: VolatileOverlay) -> ! {
    const SUBSCRIBE_CHANNEL_CAPACITY: usize = 1024;
    let (tx, rx) = broadcast::channel::<OverlayDiff>(SUBSCRIBE_CHANNEL_CAPACITY);

    // Spawn the apply loop once; it runs for the lifetime of the
    // sidecar and only exits when the broadcast sender is dropped
    // (process shutdown). The overlay is `Arc`-backed so the clone here
    // shares the same data the snapshot hydration step writes to.
    let overlay_for_apply = overlay.clone();
    tokio::spawn(subscribe_apply(overlay_for_apply, rx));

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);
    loop {
        // 1) Hydrate from the snapshot endpoint. Failure here is
        //    non-fatal: if the owner is up but only the streaming
        //    endpoint is live (or vice-versa), we still try to stream.
        if let Err(e) = hydrate_snapshot(&owner_url, &overlay).await {
            warn!("overlay snapshot hydrate from {} failed: {}", owner_url, e);
        }

        // 2) Drain the NDJSON stream until the connection closes or
        //    errors out. Each parsed diff is pushed into the local
        //    broadcast channel; subscribe_apply applies it.
        match stream_diffs(&owner_url, &tx).await {
            Ok(()) => {
                // Clean EOF (owner closed the stream). Reset backoff.
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!(
                    "overlay subscribe to {} failed: {}; retrying in {:?}",
                    owner_url, e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

/// Normalize the configured owner URL to the HTTP singleton root.
/// Agent configurations conventionally use `http://localhost:9999` (bare
/// server URL) or `http://localhost:9999/mcp` (full MCP endpoint); the
/// `lain hooks` CLI accepts either shape and appends `/mcp` when given
/// the bare form. The overlay endpoints are rooted at `/overlay`, so we
/// strip any optional MCP suffix and any trailing slash before
/// appending an endpoint.
fn owner_base_url(owner_url: &str) -> String {
    let trimmed = owner_url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/mcp")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

/// Fetch the owner's current overlay snapshot and apply it to `overlay`.
/// `GET <owner_url>/overlay/get_snapshot` returns `Vec<GraphNode>` as a
/// JSON array. We upsert each node by id, so the merge is idempotent —
/// re-running on reconnect converges to the owner's current state even
/// if a few nodes were already present from the streaming session.
async fn hydrate_snapshot(owner_url: &str, overlay: &VolatileOverlay) -> Result<(), String> {
    let url = format!("{}/overlay/get_snapshot", owner_base_url(owner_url));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read snapshot body: {e}"))?;
    let nodes: Vec<GraphNode> =
        serde_json::from_slice(&bytes).map_err(|e| format!("decode snapshot: {e}"))?;
    debug!(
        "overlay snapshot hydrated {} node(s) from {}",
        nodes.len(),
        url
    );
    for node in nodes {
        overlay.insert_node(node);
    }
    Ok(())
}

/// Open `<owner_url>/overlay/subscribe` and parse the owner's stream until
/// the connection closes. The owner currently emits NDJSON; accepting the
/// equivalent SSE `data:` framing as well keeps the client compatible with
/// HTTP singleton implementations that use an event-stream response. Each
/// decoded `OverlayDiff` is pushed into `tx`, which is consumed by the
/// `subscribe_apply` task spawned in `subscribe`.
/// The function accumulates bytes across chunks because HTTP framing does not
/// align with line boundaries — a single chunk may contain several lines or a
/// partial line. Malformed payloads are logged at debug level and dropped.
async fn stream_diffs(
    owner_url: &str,
    tx: &tokio::sync::broadcast::Sender<OverlayDiff>,
) -> Result<(), String> {
    let url = format!("{}/overlay/subscribe", owner_base_url(owner_url));
    let client = reqwest::Client::builder()
        // No overall timeout: this connection is meant to stay open
        // for the sidecar's lifetime. Chunk-level timeouts would
        // surface as a "stuck" stream instead of an idle stream.
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut sse_data = String::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read chunk: {e}"))? {
        buf.extend_from_slice(&chunk);
        // Split out every complete line and parse it. The remainder stays in
        // `buf` for the next chunk.
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=nl).collect();
            // Strip the trailing `\n`; `process_stream_line` handles an
            // optional `\r` from CRLF responses.
            line.pop();
            process_stream_line(&line, &mut sse_data, tx);
        }
    }

    // A server may close immediately after a final payload without a newline.
    if !buf.is_empty() {
        process_stream_line(&buf, &mut sse_data, tx);
    }
    flush_sse_data(&mut sse_data, tx);
    Ok(())
}

fn process_stream_line(
    line: &[u8],
    sse_data: &mut String,
    tx: &tokio::sync::broadcast::Sender<OverlayDiff>,
) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches('\r');

    if text.is_empty() {
        // An empty SSE line terminates the current event. NDJSON lines are
        // unaffected because they are dispatched as soon as they arrive.
        flush_sse_data(sse_data, tx);
        return;
    }

    if let Some(data) = text.strip_prefix("data:") {
        let data = data.strip_prefix(' ').unwrap_or(data);
        if !sse_data.is_empty() {
            sse_data.push('\n');
        }
        sse_data.push_str(data);
        return;
    }

    // These SSE metadata/comment fields do not carry an OverlayDiff.
    if text.starts_with(':')
        || text.starts_with("event:")
        || text.starts_with("id:")
        || text.starts_with("retry:")
    {
        return;
    }

    // A plain line is the Task 5 NDJSON wire format. Flush any pending SSE
    // event first so mixed framing cannot reorder diffs.
    flush_sse_data(sse_data, tx);
    send_diff_payload(text, tx);
}

fn flush_sse_data(sse_data: &mut String, tx: &tokio::sync::broadcast::Sender<OverlayDiff>) {
    if !sse_data.is_empty() {
        send_diff_payload(sse_data, tx);
        sse_data.clear();
    }
}

fn send_diff_payload(payload: &str, tx: &tokio::sync::broadcast::Sender<OverlayDiff>) {
    let payload = payload.trim();
    if payload.is_empty() {
        return;
    }
    match serde_json::from_str::<OverlayDiff>(payload) {
        Ok(diff) => {
            // Best-effort send: if the apply task has fallen behind more
            // than the channel capacity, the dropped gap is logged by its
            // `Lagged` branch when it resumes.
            let _ = tx.send(diff);
        }
        Err(e) => {
            debug!(
                "overlay subscribe: dropped malformed stream payload ({} bytes): {}",
                payload.len(),
                e
            );
        }
    }
}