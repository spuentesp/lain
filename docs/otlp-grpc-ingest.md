# Plan — OTLP gRPC Ingest Adapter

## Goal

Wire real OpenTelemetry ingest into `RuntimeTraceStore` so the
Tier 3 mitigation stops being a forward-looking promise and becomes
an end-to-end pipeline. After this lands, an application exporting
spans to `localhost:4317` causes `explain_dispatch` to surface
`runtime_callers` with `verdict = runtime_confirmed` whenever the
spans cross a function the static graph already knows about.

## Why deferred, why now

Tier 3 shipped the data model (`SpanRecord`, `RuntimeTraceStore`,
`EdgeProvenance::Runtime`, `EdgeType::RuntimeCall`) and the consumer
(`explain_dispatch` reads from `RuntimeTraceStore::global()`). What
is missing is the producer: the bytes-to-`SpanRecord` adapter that
takes a real OTLP stream off the wire and feeds the store. Without
it, runtime callers only appear via test fixtures.

The reason it was deferred is dep weight: `tonic` + `opentelemetry`
+ `opentelemetry-otlp` + `opentelemetry_sdk` add roughly 6 MB of
release binary and a non-trivial build-time cost. This plan
isolates them behind a Cargo feature so the default build stays
lean.

## Scope

In scope:

- OTLP/gRPC listener on `127.0.0.1:4317` (configurable via
  `LAIN_TRACE_OTLP_ADDR`).
- OTLP/HTTP/protobuf listener on `127.0.0.1:4318` as a fallback.
- Conversion `ExportTraceServiceRequest` → `Vec<SpanRecord>`.
- Span-to-node-id resolution via the existing
  `GraphNode::generate_id` UUID v5 scheme.
- `RuntimeTraceStore::ingest` batching with mpsc back-pressure.
- Lifecycle wiring: start on `LainServer::run_http` /
  `run_stdio`, stop on `LifecycleInfo::Drop`.
- Federation fan-out: runtime edges ingested in one repo do not
  leak into another repo's graph (UUID namespace is per-repo).
- Tests with a real OTLP exporter talking to the listener.

Out of scope:

- OTLP metrics / logs ingest (separate signal; not the goal here).
- Trace sampling / tail-based sampling (the producer decides what
  to send; lain consumes everything it receives).
- Persistent storage of runtime edges (TTL store only; persistence
  would change blast-radius semantics and is its own epic).
- TLS / mTLS for OTLP. Plaintext on `127.0.0.1` is the v1; documented
  as a follow-up in `docs/security-notes.md` style.

## Cargo changes

`Cargo.toml` adds a feature-gated dependency block:

```toml
[features]
default = []
runtime-tracing = ["dep:tonic", "dep:prost", "dep:opentelemetry",
                   "dep:opentelemetry-otlp", "dep:opentelemetry_sdk"]

[dependencies]
tonic = { version = "0.12", optional = true }
prost = { version = "0.13", optional = true }
opentelemetry = { version = "0.24", optional = true }
opentelemetry-otlp = { version = "0.17", optional = true, features = ["grpc-tonic", "trace"] }
opentelemetry_sdk = { version = "0.24", optional = true, features = ["rt-tokio"] }
```

Pin versions to whatever matches `tokio` 1.35 and `rust-mcp-sdk`
1.1 already in the lockfile; verify with `cargo update -p
opentelemetry` before committing the lockfile change.

`scripts/check-no-deps-on-runtime-tracing.py` (new, small) rejects
`use opentelemetry` or `use tonic` from outside
`#[cfg(feature = "runtime-tracing")]` modules. This is the
defensive guard that keeps the default build clean.

## Module structure

New module: `src/server/runtime_trace/otlp.rs` (gated by the
feature). It exposes one entry point:

```rust
#[cfg(feature = "runtime-tracing")]
pub async fn run_otlp_listener(
    addr: SocketAddr,
    store: Arc<RuntimeTraceStore>,
    graph: GraphDatabase,
    namespace: RepoNamespace,
    cancel: CancellationToken,
) -> Result<(), LainError>
```

`mod.rs` becomes:

```rust
pub mod spans;
pub mod store;
#[cfg(feature = "runtime-tracing")]
pub mod otlp;
```

The listener is started by `LainServer::run_http` after the graph
is ready, gated on `LAIN_TRACE_RUNTIME=true`. The store is
`Arc<RuntimeTraceStore>` so the listener and the tool layer share
the same instance without going through the global accessor — the
global becomes a thin wrapper around an `Arc` that the server
hands out.

## Span resolution

OTLP `ExportTraceServiceRequest` carries a `ResourceSpans` array;
each contains a `ScopeSpans` (the instrumentation library), each
of which contains `Span` records. The conversion is:

| OTLP | SpanRecord field |
|---|---|
| `span.trace_id` (16 bytes → hex) | `trace_id` |
| `span.span_id` (8 bytes → hex) | `span_id` |
| `span.parent_span_id` (8 bytes → 0 means none) | `parent_span_id` |
| `span.name` | `name` |
| `span.kind` (int → enum) | `kind` |
| `span.end_time_unix_nano / 1e9` | `end_unix` |
| `span.attributes` → `KeyValue` → key match | `attributes` |

Resolution to `GraphNode` id is a closure passed into
`RuntimeTraceStore::ingest`:

```rust
let resolver = |s: &SpanRecord| -> Option<String> {
    // 1. code.namespace + code.function (semconv) → direct lookup
    // 2. fallback: span.name parsed as "module::func" or "Class.method"
    // 3. last resort: drop the span
    resolve_span_to_node_id(&graph, &s.attributes, &s.name, &namespace)
};
store.ingest(&span_records, resolver);
```

The resolver is the only piece that knows about the graph schema
details. Keep it pure (no I/O) so tests can drive it directly.

## Server lifecycle wiring

`src/server/ingest/server.rs::run_http` (and `run_stdio`) gains a
gated block:

```rust
#[cfg(feature = "runtime-tracing")]
if std::env::var("LAIN_TRACE_RUNTIME").is_ok() {
    let cancel = self.lifecycle_handle().cancel_token();
    let store = self.runtime_trace_store.clone();
    let graph = self.ingest().graph().clone();
    let ns = self.namespace().clone();
    tokio::spawn(async move {
        if let Err(e) = runtime_trace::otlp::run_otlp_listener(
            otlp_addr_from_env(), store, graph, ns, cancel,
        ).await {
            warn!("OTLP listener exited: {e}");
        }
    });
}
```

`runtime_trace_store` becomes an `Arc<RuntimeTraceStore>` field on
`LainServer`, replacing the process-global `OnceLock`. The global
accessor stays for tests and for the `explain_dispatch` shallow API
but reads through the `Arc` so production code never mutates it
directly.

## Federation fan-out

Per-repo graphs have distinct `RepoNamespace`s. The resolver carries
the per-server namespace so ingested spans resolve only into the
graph the listener was started for. Federation mode (one server,
many repos) gets one listener per repo with per-repo namespace;
the trade-off is documented (one OTLP port per repo or one port
with `repo_id` injected via `x-lain-repo` resource attribute).

The simpler v1 is one listener per `LainServer` instance; federation
operators who want cross-repo runtime visibility run a Tier 3 OTLP
listener per repo via the per-repo config. This keeps the v1 simple
without blocking the federation story.

## Back-pressure and resource bounds

| Concern | Bound | How |
|---|---|---|
| In-flight OTLP requests | 64 | `tonic::transport::Server::max_concurrent_requests` |
| Spans per `ingest` call | 4096 | `BatchConfig::max_export_batch_size` |
| `RuntimeTraceStore` size | 100k edges | `LAIN_TRACE_MAX_EDGES`, defaults to 100k |
| Edge TTL | 3600 s | `LAIN_TRACE_TTL_SECS`, defaults to 3600 |
| Listener backlog | 8192 messages | bounded mpsc; full → drop with `WARN` log |

The `WARN` log includes trace ID prefix and the count of dropped
spans so operators can tune.

## Testing strategy

| Test | What it proves |
|---|---|
| `otlp_request_to_span_records` | Conversion of a fixture `ExportTraceServiceRequest` produces the expected `SpanRecord` list. |
| `ingest_round_trip_through_listener` | A test process opens the listener, sends a synthetic span via `opentelemetry-otlp`, and asserts that `RuntimeTraceStore::snapshot()` contains the edge. |
| `resolver_finds_graph_node` | Given a graph with one `Function` node, an OTLP span with the right `code.namespace`/`code.function` resolves to it. |
| `resolver_drops_unknown_spans` | Spans for code not in the graph return `None` and do not produce edges. |
| `listener_respects_cancel_token` | Cancelling the `LifecycleInfo` token causes the listener to exit within 1 s. |
| `federation_listener_does_not_leak_across_repos` | Two `LainServer` instances with distinct namespaces each ingest the same span; each produces edges only in its own graph. |

The integration tests live in `tests/otlp_ingest.rs` and require
`--features runtime-tracing` to opt in (otherwise they're compiled
out by `#[cfg(feature = "runtime-tracing")]`).

## Documentation updates

- `docs/dynamic-dispatch.md`: flip the "future OTLP adapter"
  caveat to "the adapter is shipped; see `docs/otlp-grpc-ingest.md`".
- `docs/QUICKSTART.md`: a "runtime tracing" subsection showing
  `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317` and the
  expected agent flow.
- `README.md` Operator knobs table: add `LAIN_TRACE_RUNTIME=true`
  with a one-line description.

## Verification (end-to-end)

1. Run a small Python app with `opentelemetry-instrumentation-flask`
   exporting to `http://localhost:4317`.
2. `cargo run --features runtime-tracing -- mcp` with
   `LAIN_TRACE_RUNTIME=true`.
3. Trigger an HTTP request to the app.
4. Call `explain_dispatch <a-route-handler>` and confirm
   `verdict = runtime_confirmed`, `runtime_callers` non-empty, and
   the trace ID matches the request.
5. Restart the lain server; runtime edges should drain over their
   TTL (1 hour default).
6. `cargo test -p lain --features runtime-tracing` → green.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `tonic` + `opentelemetry` dep weight (~6 MB release binary) | Feature-gated; default `cargo build` and CI fast lane unaffected. |
| `LainServer` field changes touch all 9 handles | Stage the change behind a builder pattern; existing tests that construct `LainServer` from handles accept an `Arc<RuntimeTraceStore>` with a default of `RuntimeTraceStore::new(Default::default())`. |
| OTLP cardinality blow-up (millions of unique span IDs) | TTL + capacity guard + back-pressure `WARN`. |
| Federation cross-leak | Per-listener namespace; integration test pins the contract. |
| `cargo update` on the four new crates destabilizes the lockfile | Pin to `~0.12` / `~0.24` style ranges; verify against `tokio` 1.35 already in lockfile. |

## Estimated effort

One engineer, two to three weeks. Most of the time goes into the
OTLP conversion (semconv key matching has many edge cases) and the
lifecycle wiring (cancelling the listener cleanly is fiddly). The
store side is already done.

## Out-of-plan follow-ups

- TLS / mTLS for OTLP. v1 is plaintext on loopback.
- Trace sampling. lain consumes everything it gets; sampling
  belongs in the producer.
- Persisted runtime edges. Would change blast-radius semantics
  (runtime callers survive a restart) and needs careful design
  around TTL semantics on disk.
