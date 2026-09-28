//! Runtime trace ingest — Tier 3 of the dynamic-dispatch mitigation.
//!
//! The static graph and the heuristic sensor still leave blind spots:
//! some receivers are only resolvable by observing the running
//! application. This module bridges that gap by ingesting
//! OpenTelemetry spans and turning them into `RuntimeCall` edges with
//! provenance = `Runtime { trace_id, last_seen_unix }`.
//!
//! Architecture:
//!
//! - [`store::RuntimeTraceStore`] — in-memory map of runtime edges
//!   keyed by `(source_id, target_id, trace_id)`, with per-edge TTL.
//! - [`spans::SpanRecord`] — the data model produced by an ingest
//!   adapter. OTLP gRPC / HTTP ingestion is the natural next step; the
//!   store accepts `SpanRecord`s directly so tests can drive it
//!   without spinning up an OTLP collector.
//! - [`otlp`] — minimal OTLP HTTP/JSON adapter. Parses the official
//!   OTLP JSON shape (`{"resourceSpans": [...]}`) without pulling in
//!   the `tonic` / `opentelemetry-proto` deps that the gRPC
//!   adapter would. Production deployments that use the OTLP gRPC
//!   wire format can write their own thin adapter; the JSON adapter
//!   here covers the common case (collector → OTLP HTTP exporter
//!   → lain).
//!
//! Activation:
//!
//! Runtime tracing is opt-in via `LAIN_TRACE_RUNTIME=true`. The store
//! is always available via [`RuntimeTraceStore::global`]; no listener
//! starts unless the env var is set when an OTLP adapter ships. This
//! keeps the current default build free of network listeners and lets
//! tool handlers query the store unconditionally.

pub mod otlp;
pub mod server;
pub mod spans;
pub mod store;

pub use spans::{SpanKind, SpanRecord};
pub use store::{RuntimeTraceStore, StoreConfig};
