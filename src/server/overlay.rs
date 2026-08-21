//! Volatile overlay using petgraph
//!
//! In-memory graph that mirrors uncommitted Git diffs for real-time synchronization.
//!
//! Also exposes a thin `subscribe` helper used by the sidecar runtime to
//! mirror the owner's volatile overlay across processes.

use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};

pub mod stream;
pub use stream::{
    broadcast_overlay_diff, subscribe_apply, subscribe_channel, OverlayDiff, RevisionId,
};
use crate::server::revision_log::{LookupResult, RevisionLog};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// Volatile overlay graph using petgraph
#[derive(Clone)]
pub struct VolatileOverlay {
    graph: Arc<RwLock<DiGraph<GraphNode, EdgeType>>>,
    node_index_map: Arc<RwLock<HashMap<String, NodeIndex>>>,
    bloom_filter: Arc<RwLock<Vec<u8>>>, // Simple Bloom Filter for fast existence checks
    /// Last time the overlay was modified
    last_updated: Arc<RwLock<Instant>>,
    /// Bounded history of overlay mutations, fed by `insert_node` /
    /// `insert_edge`. Wrapped in its own `Mutex` so enqueue does not widen
    /// the surface of the existing RwLocks; the lock is held only for the
    /// `VecDeque` push. See Task 1.2 of the coordination staleness/audit
    /// design for the lock-decision rationale.
    log: Arc<Mutex<RevisionLog>>,
}

/// Cheap clone (already provided by the `#[derive(Clone)]` on the
/// struct — every field is an `Arc`, so duplicating the overlay just
/// bumps the reference counts). Used for sharing one overlay between
/// `FederatedIndex` (so index() can touch it) and `LainServer`
/// (which the tool executor dispatches against).

impl VolatileOverlay {
    /// Create a new volatile overlay
    pub fn new() -> Self {
        Self {
            graph: Arc::new(RwLock::new(DiGraph::new())),
            node_index_map: Arc::new(RwLock::new(HashMap::new())),
            bloom_filter: Arc::new(RwLock::new(vec![0u8; 1024])), // 8192 bits
            last_updated: Arc::new(RwLock::new(Instant::now())),
            log: Arc::new(Mutex::new(RevisionLog::new())),
        }
    }

    /// Returns how long ago the overlay was last updated
    pub fn last_update_age_secs(&self) -> f64 {
        let last = *self.last_updated.read();
        last.elapsed().as_secs_f64()
    }

    /// Bump the overlay's last-updated timestamp without changing
    /// nodes. Used after a successful indexing pass so the freshness
    /// indicator reflects "we just indexed" rather than "no edits
    /// ever". The index path doesn't insert nodes through the
    /// overlay (it writes the static graph), so without this the
    /// freshness banner stays "stale" forever on a freshly-indexed
    /// server.
    pub fn touch(&self) {
        *self.last_updated.write() = Instant::now();
    }

    /// Highest revision id assigned by this overlay's internal `RevisionLog`.
    /// Returns 0 when nothing has been inserted yet. See Task 1.2 of the
    /// coordination staleness/audit design.
    pub fn current_revision(&self) -> RevisionId {
        self.log.lock().current_revision()
    }

    /// All retained diffs strictly newer than `rev`. See
    /// `RevisionLog::diffs_since` for the meaning of each `LookupResult`
    /// arm.
    pub fn diffs_since(&self, rev: RevisionId) -> Result<Vec<OverlayDiff>, LookupResult> {
        self.log.lock().diffs_since(rev)
    }

    fn update_bloom(&self, id: &str) {
        let mut filter = self.bloom_filter.write();
        let h1 = self.hash_str(id, 0) % 8192;
        let h2 = self.hash_str(id, 1) % 8192;
        filter[(h1 / 8) as usize] |= 1 << (h1 % 8);
        filter[(h2 / 8) as usize] |= 1 << (h2 % 8);
    }

    fn check_bloom(&self, id: &str) -> bool {
        let filter = self.bloom_filter.read();
        let h1 = self.hash_str(id, 0) % 8192;
        let h2 = self.hash_str(id, 1) % 8192;
        let b1 = filter[(h1 / 8) as usize] & (1 << (h1 % 8)) != 0;
        let b2 = filter[(h2 / 8) as usize] & (1 << (h2 % 8)) != 0;
        b1 && b2
    }

    fn hash_str(&self, s: &str, seed: u32) -> u32 {
        let mut hash = seed;
        for b in s.as_bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(*b as u32);
        }
        hash
    }

    /// Insert a node into the overlay.
    /// If a node with the same ID already exists, it is replaced (upsert).
    pub fn insert_node(&self, node: GraphNode) -> NodeIndex {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();

        // Upsert: if node already exists, remove the old one first to avoid orphans
        if let Some(&old_idx) = index_map.get(&node.id) {
            graph.remove_node(old_idx);
        }

        self.update_bloom(&node.id);
        let index = graph.add_node(node.clone());
        index_map.insert(node.id.clone(), index);

        // Update freshness timestamp
        *self.last_updated.write() = Instant::now();

        // Record this mutation in the revision log. The lock is held only
        // for the `VecDeque::push_back` inside `enqueue`; nothing else is
        // touched here. The `added` vector carries the node that was just
        // upserted so subscribers replaying the diffs reconstruct the same
        // state we have in the graph above.
        {
            let mut log = self.log.lock();
            log.enqueue(OverlayDiff {
                revision: 0, // overwritten by `enqueue`; the log owns numbering
                added: vec![node.clone()],
                removed: vec![],
                updated: vec![],
            });
        }

        debug!("Upserted node into volatile overlay: {}", node.name);
        index
    }

    /// Upsert a node. Identical to `insert_node`; exists so the
    /// `subscribe_apply` apply loop can mirror the `OverlayDiff`
    /// vocabulary (`added` / `updated` both call insert, `removed`
    /// calls `remove_node`) without inventing a second upsert path.
    pub fn upsert_node(&self, node: GraphNode) {
        self.insert_node(node);
    }

    /// Remove a node by id. Returns `true` if the node existed and was
    /// removed, `false` if no node with that id was present. Used by
    /// `subscribe_apply` to honour the `removed` field of
    /// `OverlayDiff`. Edges incident to the removed node are dropped
    /// implicitly by petgraph's `remove_node`.
    pub fn remove_node(&self, id: &str) -> bool {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();

        match index_map.remove(id) {
            Some(idx) => {
                if graph.remove_node(idx).is_some() {
                    *self.last_updated.write() = Instant::now();
                    debug!("Removed node from volatile overlay: {}", id);
                    true
                } else {
                    // index_map claimed the node was present but the
                    // graph had already dropped it (e.g. via `clear`).
                    false
                }
            }
            None => false,
        }
    }

    /// Insert an edge into the overlay
    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), String> {
        let index_map = self.node_index_map.read();

        // Copy indices out since they borrow from index_map
        let source_idx = *index_map.get(&edge.source_id)
            .ok_or_else(|| format!("Source node not found: {}", edge.source_id))?;
        let target_idx = *index_map.get(&edge.target_id)
            .ok_or_else(|| format!("Target node not found: {}", edge.target_id))?;

        // Release index_map lock before acquiring graph lock
        drop(index_map);

        let mut graph = self.graph.write();

        // Check if edge already exists under write lock
        for e in graph.edges(source_idx) {
            if e.target() == target_idx && *e.weight() == edge.edge_type {
                return Ok(()); // Edge already exists
            }
        }

        graph.add_edge(source_idx, target_idx, edge.edge_type.clone());
        *self.last_updated.write() = Instant::now();

        // Record this mutation in the revision log. `OverlayDiff` has no
        // dedicated edge field today (the broadcast bus carries node
        // diffs only), so we enqueue an empty diff purely so the revision
        // counter advances in lockstep with the petgraph mutation. This
        // keeps `current_revision` monotonic across both insert paths;
        // once `OverlayDiff` grows an edges field this site should carry
        // it. Same lock-decision story as `insert_node`: only the log is
        // touched, briefly.
        {
            let mut log = self.log.lock();
            log.enqueue(OverlayDiff {
                revision: 0,
                added: vec![],
                removed: vec![],
                updated: vec![],
            });
        }

        debug!("Inserted edge into volatile overlay: {} -> {}", edge.source_id, edge.target_id);
        Ok(())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<GraphNode> {
        if !self.check_bloom(id) { return None; }
        
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();
        
        index_map.get(id).and_then(|idx| graph.node_weight(*idx).cloned())
    }

    /// Get all nodes
    pub fn get_all_nodes(&self) -> Vec<GraphNode> {
        let graph = self.graph.read();
        graph.node_indices()
            .filter_map(|idx| graph.node_weight(idx).cloned())
            .collect()
    }

    /// Get all edges
    pub fn get_all_edges(&self) -> Vec<(GraphNode, GraphNode, EdgeType)> {
        let graph = self.graph.read();

        graph.edge_indices()
            .filter_map(|idx| {
                let (source, target) = graph.edge_endpoints(idx)?;
                let source_node = graph.node_weight(source)?.clone();
                let target_node = graph.node_weight(target)?.clone();
                let edge_type = graph.edge_weight(idx)?.clone();
                Some((source_node, target_node, edge_type))
            })
            .collect()
    }

    /// Find nodes by name (fuzzy match)
    pub fn find_nodes_by_name(&self, name: &str) -> Vec<GraphNode> {
        let graph = self.graph.read();
        
        graph.node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if node.name.to_lowercase().contains(&name.to_lowercase()) {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find nodes by type
    pub fn find_nodes_by_type(&self, node_type: &NodeType) -> Vec<GraphNode> {
        let graph = self.graph.read();
        
        graph.node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if &node.node_type == node_type {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn find_nodes_by_path(&self, path: &str) -> Vec<GraphNode> {
        let graph = self.graph.read();
        
        graph.node_indices()
            .filter_map(|idx| {
                let node = graph.node_weight(idx)?;
                if node.path == path {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get outgoing edges from a node
    pub fn get_outgoing_edges(&self, node_id: &str) -> Vec<(GraphNode, EdgeType)> {
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();
        
        let idx = match index_map.get(node_id) {
            Some(idx) => *idx,
            None => return vec![],
        };
        
        graph.edges(idx)
            .filter_map(|e| {
                let target_node = graph.node_weight(e.target())?.clone();
                Some((target_node, e.weight().clone()))
            })
            .collect()
    }

    /// Get incoming edges to a node
    pub fn get_incoming_edges(&self, node_id: &str) -> Vec<(GraphNode, EdgeType)> {
        let graph = self.graph.read();
        let index_map = self.node_index_map.read();
        
        let idx = match index_map.get(node_id) {
            Some(idx) => *idx,
            None => return vec![],
        };
        
        // Need to iterate all edges to find incoming
        graph.edge_indices()
            .filter_map(|eid| {
                let (source, target) = graph.edge_endpoints(eid)?;
                if target == idx {
                    let source_node = graph.node_weight(source)?.clone();
                    let edge_type = graph.edge_weight(eid)?.clone();
                    Some((source_node, edge_type))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Clear the overlay
    pub fn clear(&self) {
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();
        let mut bloom = self.bloom_filter.write();

        *graph = DiGraph::new();
        index_map.clear();
        *bloom = vec![0u8; 1024];
        *self.last_updated.write() = Instant::now();

        info!("Volatile overlay cleared");
    }

    /// Get statistics
    pub fn stats(&self) -> OverlayStats {
        let graph = self.graph.read();
        
        OverlayStats {
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
        }
    }

    /// Merge another overlay into this one
    pub fn merge(&self, other: &VolatileOverlay) {
        let other_graph = other.graph.read();
        let mut graph = self.graph.write();
        let mut index_map = self.node_index_map.write();
        
        // Copy nodes
        for idx in other_graph.node_indices() {
            if let Some(node) = other_graph.node_weight(idx) {
                let new_idx = graph.add_node(node.clone());
                index_map.insert(node.id.clone(), new_idx);
                self.update_bloom(&node.id);
            }
        }
        
        // Copy edges
        for idx in other_graph.edge_indices() {
            if let Some((source, target)) = other_graph.edge_endpoints(idx) {
                if let Some(edge_type) = other_graph.edge_weight(idx) {
                    let source_node = other_graph.node_weight(source).unwrap();
                    let target_node = other_graph.node_weight(target).unwrap();

                    if let (Some(&new_source), Some(&new_target)) = (
                        index_map.get(&source_node.id),
                        index_map.get(&target_node.id),
                    ) {
                        graph.add_edge(new_source, new_target, edge_type.clone());
                    }
                }
            }
        }
    }
}

impl Default for VolatileOverlay {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the overlay
#[derive(Debug, Clone)]
pub struct OverlayStats {
    pub node_count: usize,
    pub edge_count: usize,
}

// ─── Overlay stream subscription (sidecar) ──────────────────────────────────

/// Subscribe to the owner's overlay stream and merge incoming `OverlayDiff`s
/// into `overlay`.
///
/// Wire format (Task 4):
///   * `GET <owner_url>/overlay/get_snapshot` → JSON array of every
///     `GraphNode` currently in the owner's volatile overlay. Called
///     once per (re)connect to hydrate the local cache before streaming
///     begins.
///   * `GET <owner_url>/overlay/subscribe` → `application/x-ndjson`,
///     one `OverlayDiff` per line. Stays open until the owner shuts
///     down or the sidecar drops the connection.
///
/// This function spawns the shared `stream::subscribe_apply` apply loop
/// exactly once and feeds it from a local broadcast channel; the
/// streaming body parser below pushes every parsed diff into that
/// channel. On any stream failure the function sleeps with exponential
/// backoff, re-hydrates from the snapshot endpoint, and reconnects.
pub async fn subscribe(owner_url: String, overlay: VolatileOverlay) -> ! {
    use tokio::sync::broadcast;

    const SUBSCRIBE_CHANNEL_CAPACITY: usize = 1024;
    let (tx, rx) = broadcast::channel::<stream::OverlayDiff>(SUBSCRIBE_CHANNEL_CAPACITY);

    // Spawn the apply loop once; it runs for the lifetime of the
    // sidecar and only exits when the broadcast sender is dropped
    // (process shutdown). The overlay is `Arc`-backed so the clone here
    // shares the same data the snapshot hydration step writes to.
    let overlay_for_apply = overlay.clone();
    tokio::spawn(stream::subscribe_apply(overlay_for_apply, rx));

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);
    loop {
        // 1) Hydrate from the snapshot endpoint. Failure here is
        //    non-fatal: if the owner is up but only the streaming
        //    endpoint is live (or vice-versa), we still try to stream.
        if let Err(e) = hydrate_snapshot(&owner_url, &overlay).await {
            warn!(
                "overlay snapshot hydrate from {} failed: {}",
                owner_url, e
            );
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
///
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
///
/// `GET <owner_url>/overlay/get_snapshot` returns `Vec<GraphNode>` as a
/// JSON array. We upsert each node by id, so the merge is idempotent —
/// re-running on reconnect converges to the owner's current state even
/// if a few nodes were already present from the streaming session.
async fn hydrate_snapshot(
    owner_url: &str,
    overlay: &VolatileOverlay,
) -> Result<(), String> {
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
    let nodes: Vec<GraphNode> = serde_json::from_slice(&bytes)
        .map_err(|e| format!("decode snapshot: {e}"))?;
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
///
/// The function accumulates bytes across chunks because HTTP framing does not
/// align with line boundaries — a single chunk may contain several lines or a
/// partial line. Malformed payloads are logged at debug level and dropped.
async fn stream_diffs(
    owner_url: &str,
    tx: &tokio::sync::broadcast::Sender<stream::OverlayDiff>,
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
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("read chunk: {e}"))?
    {
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
    tx: &tokio::sync::broadcast::Sender<stream::OverlayDiff>,
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

fn flush_sse_data(
    sse_data: &mut String,
    tx: &tokio::sync::broadcast::Sender<stream::OverlayDiff>,
) {
    if !sse_data.is_empty() {
        send_diff_payload(sse_data, tx);
        sse_data.clear();
    }
}

fn send_diff_payload(
    payload: &str,
    tx: &tokio::sync::broadcast::Sender<stream::OverlayDiff>,
) {
    let payload = payload.trim();
    if payload.is_empty() {
        return;
    }
    match serde_json::from_str::<stream::OverlayDiff>(payload) {
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

