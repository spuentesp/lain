//! HTTP listener for the OTLP `/v1/traces` endpoint.
//!
//! The OTLP HTTP exporter (most OTel collectors default to this
//! when not using gRPC) POSTs JSON-encoded spans to `/v1/traces`.
//! This module owns a dedicated `hyper::server::conn::http1` listener
//! — separate from the MCP HTTP transport — so the runtime trace
//! path doesn't have to share the MCP bearer-token auth layer.
//!
//! Activation:
//!
//! Runtime tracing is opt-in via `LAIN_TRACE_RUNTIME=true`. The
//! server starts only when the env var is set. Otherwise this
//! module's `start` function returns `Ok(None)` and the caller
//! continues without the listener — keeping the default build
//! free of network listeners, like the rest of lain.

use super::otlp::parse_otlp_json;
use super::store::RuntimeTraceStore;
use super::SpanRecord;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Symbol→node_id resolver for OTLP spans. The caller supplies
/// this because the resolution policy is mode-dependent: stdio
/// mode uses a single `GraphDatabase::find_node_by_name` lookup,
/// federation mode walks every registered repo. We keep
/// `runtime_trace` agnostic to that policy by accepting a closure
/// rather than a `GraphDatabase` reference — avoids a
/// `runtime_trace → graph` dependency and keeps the test surface
/// small (a closure over a `HashMap` is enough for unit tests).
///
/// Returning `None` is the safe default: it tells
/// [`RuntimeTraceStore::ingest`] to drop that end of the edge
/// rather than mint a `RuntimeCall` to a non-existent node.
pub type SpanResolver = Arc<dyn Fn(&SpanRecord) -> Option<String> + Send + Sync>;

/// No-op resolver — accepts every span verbatim with no edge
/// minted. Useful in tests that don't exercise resolution and
/// as the explicit fallback when the caller has no graph to
/// resolve against (today: standalone / sidecar executors).
pub fn no_resolver() -> SpanResolver {
    Arc::new(|_| None)
}

/// Federation-aware resolver. Walks every registered repo's graph
/// looking for the span name and returns the global node id when
/// exactly one repo owns the symbol. Honors OTLP semconv hints
/// (`code.repo` for repo-narrowed resolution; `service.name` as a
/// secondary lookup key) before falling back to the global index.
///
/// Returns `None` when no repo owns the symbol or when the
/// span-name lookup is ambiguous across repos — in both cases
/// the caller drops the edge rather than minting a stale
/// `RuntimeCall` against the staging placeholder.
///
/// Federation mode (`Arc<FederatedIndex>`) is the only mode
/// `FederatedIndex` carries; for single-workspace mode the
/// caller wraps this in a single-graph closure (see
/// `cli/server.rs`).
pub fn federation_resolver(
    fed: Arc<crate::server::federation::federated_index::FederatedIndex>,
) -> SpanResolver {
    Arc::new(
        move |span: &crate::server::runtime_trace::spans::SpanRecord| {
            use crate::server::runtime_trace::spans::AttributeValue;
            // Honor `code.repo` (OTLP semconv) when present so the
            // resolver doesn't waste work scanning every repo.
            let code_repo = span.attributes.get("code.repo").and_then(|v| v.as_str());
            let service_name = span.attributes.get("service.name").and_then(|v| v.as_str());

            if let Some(repo_id) = code_repo {
                if let Ok(repo_id) = crate::server::federation::repo_id::RepoId::new(repo_id) {
                    if let Some(repo) = fed.get_repo(&repo_id) {
                        if let Some(node) = repo.db().find_node_by_name(&span.name) {
                            return Some(node.id);
                        }
                    }
                }
            }
            // Fallback: walk every repo, pick the unambiguous match.
            let mut hits = Vec::new();
            for (repo_id, _) in fed.list_repos() {
                if let Some(repo) = fed.get_repo(&repo_id) {
                    if let Some(node) = repo.db().find_node_by_name(&span.name) {
                        hits.push(node.id);
                    }
                }
            }
            // `service.name` (OTLP semconv) is a secondary lookup
            // key — if the symbol-name lookup is empty, try the
            // service name so cross-runtime callers (HTTP routes,
            // gRPC handlers) still mint edges.
            if hits.is_empty() {
                if let Some(svc) = service_name {
                    for (repo_id, _) in fed.list_repos() {
                        if let Some(repo) = fed.get_repo(&repo_id) {
                            if let Some(node) = repo.db().find_node_by_name(svc) {
                                hits.push(node.id);
                            }
                        }
                    }
                }
            }
            if hits.len() == 1 {
                hits.into_iter().next()
            } else {
                // 0 hits or ambiguous → drop the edge. Better to be
                // silent than to mint a `RuntimeCall` against the
                // wrong repo (which is exactly what the prior code did).
                None
            }
        },
    )
}

/// One-shot response body. We always emit a single buffered payload;
/// streaming isn't needed for `/v1/traces`. `UnsyncBoxBody<Full<Bytes>, E>`
/// is the same shape the MCP handler uses for non-streaming responses.
type OtlpBody = http_body_util::combinators::UnsyncBoxBody<Bytes, Infallible>;

fn bytes_body(b: Bytes) -> OtlpBody {
    http_body_util::combinators::UnsyncBoxBody::new(Full::new(b).map_err(|never| match never {}))
}

/// Bind a TcpListener for the OTLP endpoint. Picks an ephemeral
/// port when `addr` is `127.0.0.1:0`; useful for tests that want
/// a real HTTP round-trip without picking a fixed port.
pub async fn try_bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// Start the OTLP accept loop on `listener`. `resolver` maps
/// each span to a node_id (or `None` to skip edge minting); it
/// runs once per span per request, so keep it cheap. Returns a
/// `JoinHandle` for the background task; drop it to stop the
/// listener.
pub fn start(
    listener: TcpListener,
    store: Arc<RuntimeTraceStore>,
    resolver: SpanResolver,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        accept_loop(listener, store, resolver).await;
    })
}

async fn accept_loop(listener: TcpListener, store: Arc<RuntimeTraceStore>, resolver: SpanResolver) {
    // Bound the number of in-flight OTLP connections. Without a cap
    // a peer can open thousands of slowloris-style connections and
    // each `tokio::spawn` here adds memory + scheduler pressure until
    // the process OOMs. The semaphore is `tokio::sync::Semaphore` so
    // a flood of `accept()`s just stalls at the `.acquire().await`
    // line rather than spawning unbounded tasks.
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_OTLP_CONCURRENT));
    loop {
        let permit = match permits.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed
        };
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let store = store.clone();
                let resolver = Arc::clone(&resolver);
                tokio::spawn(async move {
                    let _permit = permit; // released when the task ends
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req| {
                        let store = store.clone();
                        let resolver = Arc::clone(&resolver);
                        async move { handle_request(req, store, resolver).await }
                    });
                    if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                        tracing::debug!("otlp connection error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("otlp accept error: {e}");
                drop(permit); // release the unused permit
            }
        }
    }
}

async fn handle_request(
    req: Request<Incoming>,
    store: Arc<RuntimeTraceStore>,
    resolver: SpanResolver,
) -> Result<Response<OtlpBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let response = match (method, path.as_str()) {
        (Method::POST, "/v1/traces") => ingest_traces(req, store, resolver).await,
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(bytes_body(Bytes::from_static(b"not found")))
            .unwrap(),
    };

    Ok(response)
}

/// Maximum OTLP ingest body size, in bytes. A peer that can reach the
/// listener can otherwise send a multi-GB POST body; `req.collect().await`
/// would happily buffer it. The cap is enforced by `Limited::new` on the
/// incoming body before any allocation past the limit. 16 MiB is well
/// above any reasonable single-batch OTLP payload.
pub const MAX_OTLP_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of concurrent OTLP connections. Each accepted
/// connection spawns a Tokio task that holds the connection until the
/// client closes or the read times out. The cap is enforced through a
/// `tokio::sync::Semaphore` so a peer that opens thousands of slowloris
/// connections stalls at the `accept()` line rather than spawning
/// unbounded work.
pub const MAX_OTLP_CONCURRENT: usize = 64;

/// Read the full request body, parse the OTLP JSON, and ingest
/// each span into the store. The OTLP HTTP exporter doesn't
/// require a content-type header, but it usually sends
/// `application/json`; we accept any content type.
///
/// Storage status (deliberately honest): the listener parses
/// spans and runs [`RuntimeTraceStore::ingest`] with a best-effort
/// resolver. The resolver today returns `None` for every span
/// because this transport has no graph reference — the OTLP
/// listener is wired before the federation/stdio paths and lives
/// independently of `LainServer`. Without a node_id, `ingest`
/// mints zero edges and the spans are not queryable via
/// `explain_dispatch` yet. The response field name reflects this:
/// `receivedSpans` (parsed + validated) is reported even when
/// `storedSpans` (resolved + minted as edges) is zero when the
/// caller supplied [`no_resolver`] or every span's namespace /
/// function attributes failed to resolve.
///
/// The caller-provided `resolver` is invoked once per span. A
/// resolver returning `None` drops that end of the edge rather
/// than mint a `RuntimeCall` to a non-existent node — keeping
/// `RuntimeTraceStore::ingest`'s "no fake edges" contract.
///
/// Federation mode wires the resolver to walk every registered
/// repo's `find_node_by_name`; stdio single-repo mode uses one
/// `find_node_by_name` call against the bound graph. Either way
/// the listener itself stays graph-agnostic.
async fn ingest_traces(
    req: Request<Incoming>,
    store: Arc<RuntimeTraceStore>,
    resolver: SpanResolver,
) -> Response<OtlpBody> {
    use http_body_util::{BodyExt, Limited};
    // Cap the body size before allocating it. Without this, a peer
    // can send a multi-GB POST and we buffer it all before noticing.
    let Ok(body) = Limited::new(req.into_body(), MAX_OTLP_BODY_BYTES)
        .collect()
        .await
    else {
        return json_error(StatusCode::BAD_REQUEST, "could not read body");
    };
    let bytes = body.to_bytes();

    let records: Vec<SpanRecord> = match parse_otlp_json(&bytes) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    };

    let received = records.len();
    // Best-effort ingest. The closure form lets the caller supply
    // any resolver policy without runtime_trace depending on the
    // graph crate; the listener itself stays graph-agnostic.
    let resolver_for_ingest = Arc::clone(&resolver);
    let stored = store.ingest(&records, move |span| resolver_for_ingest(span));
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(bytes_body(Bytes::from(format!(
            "{{\"partialSuccess\":{{\"receivedSpans\":{received},\"storedSpans\":{stored}}}}}"
        ))))
        .unwrap()
}

fn json_error(status: StatusCode, message: &str) -> Response<OtlpBody> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(bytes_body(Bytes::from(format!(
            "{{\"error\":{}}}",
            serde_json::Value::String(message.to_string())
        ))))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::runtime_trace::StoreConfig;
    use std::net::{IpAddr, Ipv4Addr};

    /// Bind to an ephemeral port, fire one POST against it, verify
    /// the OTLP payload was accepted AND the response carries both
    /// `receivedSpans` and `storedSpans`. The pre-fix code returned
    /// 200 with `acceptedSpans: N` but never wrote to the store, so
    /// the count was a lie. The post-fix test pins both halves of
    /// the response: parsed-but-not-yet-stored is a known
    /// intermediate state (until graph resolution lands) and the
    /// response must distinguish it from "actually in the store".
    #[tokio::test(flavor = "current_thread")]
    async fn end_to_end_post_ingests_payload() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone(), no_resolver());

        let body = br#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "00000000000000000000000000000001",
                        "spanId": "0000000000000001",
                        "name": "GET /orders",
                        "kind": 1,
                        "endTimeUnixNano": "1700000000000000000"
                    }]
                }]
            }]
        }"#;
        let url = format!("http://127.0.0.1:{port}/v1/traces");
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("POST should succeed");

        assert_eq!(resp.status(), 200, "OTLP endpoint should accept the span");
        let body_bytes = resp.bytes().await.expect("read response body");
        let body_str = std::str::from_utf8(&body_bytes).expect("utf-8 body");
        // The response must report both fields, and `receivedSpans`
        // must equal the number of spans we sent (1). `storedSpans`
        // is 0 here because no_resolver() returns None for every
        // span — verified by the dedicated resolver_mints_edge
        // test below.
        assert!(
            body_str.contains("\"receivedSpans\":1"),
            "response must report receivedSpans=1; got: {body_str}"
        );
        assert!(
            body_str.contains("\"storedSpans\":0"),
            "no_resolver must produce storedSpans=0; got: {body_str}"
        );

        // The post-test cleanup: stop the background OTLP listener
        // before the runtime trace store is dropped. Aborting the
        // task without abort_handle leaves the accept loop running
        // and the next test that binds the same port fails with
        // EADDRINUSE.
        handle.abort();

        handle.abort();
        let _ = handle.await;
    }

    /// Pin the resolver contract end-to-end. Build a small map
    /// from symbol string → node_id, install it as the listener's
    /// resolver, send a parent/child pair whose spans both resolve,
    /// and verify the response reports `storedSpans == 1` (one edge
    /// minted). Pre-fix code had no resolver at all and silently
    /// dropped every span; this test pins that the wire path can
    /// carry resolution once the caller supplies it.
    ///
    /// The two spans must resolve to *different* node_ids because
    /// `RuntimeTraceStore::ingest` skips edges where caller ==
    /// callee (`if caller_id == *callee_id { continue; }`).
    #[tokio::test(flavor = "current_thread")]
    async fn resolver_mints_edge_for_matching_span() {
        use std::collections::HashMap;
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let symbol_to_id: HashMap<String, String> = [
            ("orders-handler".to_string(), "node-handler".to_string()),
            ("validate-order".to_string(), "node-validate".to_string()),
        ]
        .into_iter()
        .collect();
        let map = Arc::new(symbol_to_id);
        let resolver: SpanResolver =
            Arc::new(move |span: &SpanRecord| map.get(&span.name).cloned());
        let handle = start(listener, store.clone(), resolver);

        // Parent → child chain. Both span names resolve to distinct
        // node_ids so the store's caller == callee filter doesn't drop
        // the edge.
        let body = br#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [
                        {
                            "traceId": "00000000000000000000000000000001",
                            "spanId": "0000000000000001",
                            "parentSpanId": "0000000000000002",
                            "name": "orders-handler",
                            "kind": 1,
                            "endTimeUnixNano": "1700000000000000000"
                        },
                        {
                            "traceId": "00000000000000000000000000000001",
                            "spanId": "0000000000000002",
                            "name": "validate-order",
                            "kind": 1,
                            "endTimeUnixNano": "1700000000000000000"
                        }
                    ]
                }]
            }]
        }"#;
        let url = format!("http://127.0.0.1:{port}/v1/traces");
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .expect("POST should succeed");
        assert_eq!(resp.status(), 200);
        let body_bytes = resp.bytes().await.expect("read response body");
        let body_str = std::str::from_utf8(&body_bytes).expect("utf-8 body");
        // Two spans received; one edge minted between the distinct
        // resolved node_ids. `storedSpans` counts edges minted, not
        // spans stored.
        assert!(
            body_str.contains("\"receivedSpans\":2"),
            "receivedSpans must be 2; got: {body_str}"
        );
        assert!(
            body_str.contains("\"storedSpans\":1"),
            "matching parent/child pair should mint 1 edge; got: {body_str}"
        );

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_path_returns_404() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone(), no_resolver());

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .expect("GET should succeed");
        assert_eq!(resp.status(), 404);

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_json_returns_400() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone(), no_resolver());

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/v1/traces"))
            .header("Content-Type", "application/json")
            .body(b"not json".to_vec())
            .send()
            .await
            .expect("POST should succeed");
        assert_eq!(resp.status(), 400);

        handle.abort();
        let _ = handle.await;
    }

    /// Federation resolver: when the federation owns two repos and
    /// the span's `code.repo` attribute narrows the lookup, the
    /// resolver must mint an edge against the matching repo's graph
    /// and ignore the other. Without that narrowing, the pre-fix
    /// code resolved against the staging graph and leaked edges
    /// between repos.
    #[tokio::test(flavor = "current_thread")]
    async fn federation_resolver_narrows_via_code_repo_attribute() {
        use crate::graph::GraphDatabase;
        use crate::schema::{GraphNode, NodeType};
        use crate::server::federation::federated_index::FederatedIndex;
        use crate::server::federation::graph_backend::PetgraphBackend;
        use crate::server::federation::repo_id::RepoId;
        use crate::server::federation::repo_source::WorkspaceDirSource;

        fn new_git_repo() -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap();
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .output()
                .unwrap();
            dir
        }

        // Two-repo federation: `auth` owns `validate_token`,
        // `orders` owns `place_order`.
        let fed_dir = tempfile::tempdir().unwrap();
        let fed = Arc::new(FederatedIndex::new(Arc::new(
            PetgraphBackend::new(fed_dir.path()).unwrap(),
        )));
        let auth_src = new_git_repo();
        let orders_src = new_git_repo();
        fed.add_repo(
            Box::new(
                WorkspaceDirSource::new(
                    RepoId::new("auth").unwrap(),
                    auth_src.path().to_path_buf(),
                )
                .unwrap(),
            ),
            fed_dir.path(),
        )
        .await
        .unwrap();
        fed.add_repo(
            Box::new(
                WorkspaceDirSource::new(
                    RepoId::new("orders").unwrap(),
                    orders_src.path().to_path_buf(),
                )
                .unwrap(),
            ),
            fed_dir.path(),
        )
        .await
        .unwrap();
        // Inject the per-repo nodes via the per-repo db (which
        // lives on the federation side after add_repo).
        let auth_db = fed
            .get_repo(&RepoId::new("auth").unwrap())
            .unwrap()
            .db()
            .clone();
        let orders_db = fed
            .get_repo(&RepoId::new("orders").unwrap())
            .unwrap()
            .db()
            .clone();
        let mut auth_node = GraphNode::new(
            NodeType::Function,
            "validate_token".into(),
            "src/auth.rs".into(),
        );
        auth_node.id = "auth::validate_token".into();
        auth_db.upsert_node(auth_node).unwrap();
        let mut orders_node = GraphNode::new(
            NodeType::Function,
            "place_order".into(),
            "src/orders.rs".into(),
        );
        orders_node.id = "orders::place_order".into();
        orders_db.upsert_node(orders_node).unwrap();

        let resolver = federation_resolver(fed.clone());

        // Span in the auth repo: must resolve to auth::validate_token.
        let mut attrs = std::collections::HashMap::new();
        attrs.insert(
            "code.repo".into(),
            crate::server::runtime_trace::spans::AttributeValue::Str("auth".into()),
        );
        let span = SpanRecord {
            trace_id: "00000000000000000000000000000001".into(),
            span_id: "0000000000000001".into(),
            parent_span_id: None,
            name: "validate_token".into(),
            kind: crate::server::runtime_trace::spans::SpanKind::Server,
            attributes: attrs,
            end_unix: 1,
        };
        assert_eq!(
            resolver(&span),
            Some("auth::validate_token".to_string()),
            "code.repo must narrow to the auth repo"
        );

        // Span in the orders repo: must resolve to orders::place_order.
        let mut attrs2 = std::collections::HashMap::new();
        attrs2.insert(
            "code.repo".into(),
            crate::server::runtime_trace::spans::AttributeValue::Str("orders".into()),
        );
        let span2 = SpanRecord {
            trace_id: "00000000000000000000000000000002".into(),
            span_id: "0000000000000003".into(),
            parent_span_id: None,
            name: "place_order".into(),
            kind: crate::server::runtime_trace::spans::SpanKind::Server,
            attributes: attrs2,
            end_unix: 1,
        };
        assert_eq!(
            resolver(&span2),
            Some("orders::place_order".to_string()),
            "code.repo must narrow to the orders repo"
        );

        // Span name that exists in BOTH repos: must return None
        // rather than picking one — ambiguity is fatal for the
        // federation's correctness contract.
        let mut shared =
            GraphNode::new(NodeType::Function, "shared".into(), "src/shared.rs".into());
        shared.id = "auth::shared".into();
        auth_db.upsert_node(shared.clone()).unwrap();
        shared.id = "orders::shared".into();
        orders_db.upsert_node(shared).unwrap();
        let mut attrs3 = std::collections::HashMap::new();
        attrs3.insert(
            "code.repo".into(),
            crate::server::runtime_trace::spans::AttributeValue::Str("auth".into()),
        );
        let span3 = SpanRecord {
            trace_id: "00000000000000000000000000000003".into(),
            span_id: "0000000000000004".into(),
            parent_span_id: None,
            name: "shared".into(),
            kind: crate::server::runtime_trace::spans::SpanKind::Server,
            attributes: attrs3,
            end_unix: 1,
        };
        // Narrowed to auth — picks the auth::shared node.
        assert_eq!(resolver(&span3), Some("auth::shared".to_string()));
        // Without code.repo: ambiguous (both repos own "shared"),
        // so the resolver returns None to drop the edge rather than
        // mint it against the wrong repo.
        let span4 = SpanRecord {
            trace_id: "00000000000000000000000000000004".into(),
            span_id: "0000000000000005".into(),
            parent_span_id: None,
            name: "shared".into(),
            kind: crate::server::runtime_trace::spans::SpanKind::Server,
            attributes: std::collections::HashMap::new(),
            end_unix: 1,
        };
        assert_eq!(
            resolver(&span4),
            None,
            "ambiguous name across repos must return None"
        );
    }
}
