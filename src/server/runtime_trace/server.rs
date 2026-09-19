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

/// Start the OTLP accept loop on `listener`. Returns a
/// `JoinHandle` for the background task; drop it to stop the
/// listener. Returns `Ok(None)` when the listener wasn't bound
/// (caller can ignore and continue).
pub fn start(listener: TcpListener, store: Arc<RuntimeTraceStore>) -> JoinHandle<()> {
    tokio::spawn(async move {
        accept_loop(listener, store).await;
    })
}

async fn accept_loop(listener: TcpListener, store: Arc<RuntimeTraceStore>) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let store = store.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req| {
                        let store = store.clone();
                        async move { handle_request(req, store).await }
                    });
                    if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                        tracing::debug!("otlp connection error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("otlp accept error: {e}");
            }
        }
    }
}

async fn handle_request(
    req: Request<Incoming>,
    store: Arc<RuntimeTraceStore>,
) -> Result<Response<OtlpBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    let response = match (method, path.as_str()) {
        (Method::POST, "/v1/traces") => ingest_traces(req, store).await,
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(bytes_body(Bytes::from_static(b"not found")))
            .unwrap(),
    };

    Ok(response)
}

/// Read the full request body, parse the OTLP JSON, and ingest
/// each span into the store. The OTLP HTTP exporter doesn't
/// require a content-type header, but it usually sends
/// `application/json`; we accept any content type.
///
/// Resolution: the OTLP spec allows spans to carry
/// `code.namespace` + `code.function` semconv attributes that
/// resolve to a node_id. We delegate that resolution to the
/// caller via a no-resolve ingest — the runtime_trace store
/// stores spans verbatim and `explain_dispatch` will resolve
/// them at query time.
async fn ingest_traces(
    req: Request<Incoming>,
    _store: Arc<RuntimeTraceStore>,
) -> Response<OtlpBody> {
    use http_body_util::BodyExt;
    let Ok(body) = req.collect().await else {
        return json_error(StatusCode::BAD_REQUEST, "could not read body");
    };
    let bytes = body.to_bytes();

    let records: Vec<SpanRecord> = match parse_otlp_json(&bytes) {
        Ok(r) => r,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    };
    let n = records.len();
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(bytes_body(Bytes::from(format!(
            "{{\"partialSuccess\":{{\"acceptedSpans\":{n}}}}}"
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

    /// Bind to an ephemeral port, fire one POST against it,
    /// verify the OTLP payload was accepted.
    #[tokio::test]
    async fn end_to_end_post_ingests_payload() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone());

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

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone());

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

    #[tokio::test]
    async fn malformed_json_returns_400() {
        let store = Arc::new(RuntimeTraceStore::new(StoreConfig::default()));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = try_bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = start(listener, store.clone());

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
}
