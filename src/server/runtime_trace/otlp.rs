//! Minimal OTLP HTTP/JSON ingest adapter.
//!
//! Parses the official OTLP HTTP/JSON shape
//! (`{"resourceSpans": [...]}`) without depending on the full
//! `opentelemetry-proto` crate or `tonic` for gRPC. Covers the
//! common case where an OTel collector is configured with the HTTP
//! exporter (`otlphttp`) and POSTs JSON to lain.
//!
//! Wire format reference:
//! <https://opentelemetry.io/docs/specs/otlp/#json-protobuf-encoding>
//!
//! Trace + span IDs are hex-encoded byte arrays in the wire format
//! (16 bytes for trace_id, 8 bytes for span_id, base16 lower-case).
//! Times are UNIX nanoseconds as strings. Attributes are typed
//! key/value pairs nested under `"key"` and `"value": { ... }`.

use super::spans::{AttributeValue, SpanKind, SpanRecord};
use serde::Deserialize;
use std::collections::HashMap;
use thiserror::Error;

/// Top-level OTLP payload: an `ExportTracePartialSuccess` reply is
/// also valid OTLP, but the inbound request shape is what we care
/// about for parsing.
#[derive(Debug, Deserialize)]
pub struct OtlpRequest {
    #[serde(default, rename = "resourceSpans")]
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceSpans {
    #[serde(default)]
    pub resource: Option<Resource>,
    #[serde(default, rename = "scopeSpans")]
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeSpans {
    #[serde(default)]
    pub scope: Option<Scope>,
    #[serde(default)]
    pub spans: Vec<Span>,
}

#[derive(Debug, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// One OTLP span. We only deserialize the fields we care about; the
/// rest are silently dropped, which is exactly what the OTLP
/// spec recommends for forward-compatibility.
#[derive(Debug, Deserialize)]
pub struct Span {
    #[serde(rename = "traceId")]
    pub trace_id: String,
    #[serde(rename = "spanId")]
    pub span_id: String,
    #[serde(default, rename = "parentSpanId")]
    pub parent_span_id: Option<String>,
    pub name: String,
    /// OTLP span-kind integer. Mapped to a [`SpanKind`] via
    /// [`SpanKind::from_otlp`]; unknown or future kinds fall back to
    /// `Internal` rather than being dropped, because the OTLP spec
    /// explicitly allows forward-compatible unknown enum values and
    /// dropping the span would lose the trace.
    #[serde(default)]
    pub kind: i32,
    /// End time as nanoseconds. Stored as `String` in the wire
    /// format (uint64 isn't representable in JSON); we parse as
    /// `u64` and divide to seconds.
    #[serde(default, rename = "endTimeUnixNano")]
    pub end_time_unix_nano: String,
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Deserialize)]
pub struct KeyValue {
    pub key: String,
    /// OTLP wraps every typed value under `"value": { "<type>Value": ... }`.
    /// We accept the four typed variants we preserve.
    #[serde(default)]
    pub value: Option<AnyValue>,
}

#[derive(Debug, Deserialize)]
pub struct AnyValue {
    #[serde(default, rename = "stringValue")]
    pub string_value: Option<String>,
    #[serde(default, rename = "boolValue")]
    pub bool_value: Option<bool>,
    #[serde(default, rename = "intValue")]
    pub int_value: Option<String>,
    #[serde(default, rename = "doubleValue")]
    pub double_value: Option<f64>,
}

#[derive(Debug, Error)]
pub enum OtlpParseError {
    #[error("OTLP payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OTLP trace_id {0:?} is not 16-byte hex")]
    TraceId(String),
    #[error("OTLP span_id {0:?} is not 8-byte hex")]
    SpanId(String),
    #[error("OTLP end_time_unix_nano {0:?} is not a valid uint64")]
    EndTime(String),
}

fn hex_len(s: &str) -> Option<usize> {
    // OTLP IDs are lowercase base16 per the spec. Accepting upper
    // case here would mask producer bugs (or non-conformant
    // collectors) — a `0A1B...` ID should surface as a parse
    // error rather than silently being treated as a valid
    // lowercase ID that happens to alias the same byte sequence.
    if !s.len().is_multiple_of(2)
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some(s.len())
}

/// Parse one OTLP HTTP/JSON payload into a flat `Vec<SpanRecord>`.
///
/// Flattening `resourceSpans → scopeSpans → spans` is the caller's
/// choice — the store doesn't care about the nesting. The
/// translation discards resource + scope attributes for now
/// (resource-level service-name tags would be useful, but pinning
/// the basic ingest path is the priority); a follow-up can add
/// a span attribute for `service.name` if needed.
pub fn parse_otlp_json(payload: &[u8]) -> Result<Vec<SpanRecord>, OtlpParseError> {
    let req: OtlpRequest = serde_json::from_slice(payload)?;

    let mut out = Vec::new();
    for resource in &req.resource_spans {
        for scope in &resource.scope_spans {
            for span in &scope.spans {
                out.push(translate_span(span)?);
            }
        }
    }
    Ok(out)
}

fn translate_span(span: &Span) -> Result<SpanRecord, OtlpParseError> {
    if hex_len(&span.trace_id) != Some(32) {
        return Err(OtlpParseError::TraceId(span.trace_id.clone()));
    }
    if hex_len(&span.span_id) != Some(16) {
        return Err(OtlpParseError::SpanId(span.span_id.clone()));
    }
    // `u64` is sufficient for any realistic Unix-nanosecond timestamp
    // (~584 years from epoch). `try_from` the seconds result so a
    // malformed payload that somehow encodes a far-future or
    // negative timestamp returns `OtlpParseError::EndTime` rather
    // than silently wrapping via `as i64`.
    let end_unix_nanos: u64 = span
        .end_time_unix_nano
        .parse()
        .map_err(|_| OtlpParseError::EndTime(span.end_time_unix_nano.clone()))?;
    let end_unix_secs = end_unix_nanos / 1_000_000_000;
    let end_unix = i64::try_from(end_unix_secs)
        .map_err(|_| OtlpParseError::EndTime(span.end_time_unix_nano.clone()))?;

    let kind = SpanKind::from_otlp(span.kind).unwrap_or(SpanKind::Internal);

    let mut attributes = HashMap::with_capacity(span.attributes.len());
    for kv in &span.attributes {
        if let Some(v) = &kv.value {
            if let Some(attr) = any_value_to_attr(v) {
                attributes.insert(kv.key.clone(), attr);
            }
        }
    }

    Ok(SpanRecord {
        trace_id: span.trace_id.clone(),
        span_id: span.span_id.clone(),
        parent_span_id: span.parent_span_id.clone(),
        name: span.name.clone(),
        kind,
        attributes,
        end_unix,
    })
}

fn any_value_to_attr(v: &AnyValue) -> Option<AttributeValue> {
    if let Some(s) = &v.string_value {
        return Some(AttributeValue::Str(s.clone()));
    }
    if let Some(b) = v.bool_value {
        return Some(AttributeValue::Bool(b));
    }
    if let Some(i) = &v.int_value {
        if let Ok(v) = i.parse::<i64>() {
            return Some(AttributeValue::I64(v));
        }
    }
    if let Some(d) = v.double_value {
        return Some(AttributeValue::F64(d));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TRACE_ID: &str = "00000000000000000000000000000001";
    const SAMPLE_SPAN_ID: &str = "0000000000000001";
    const SAMPLE_PARENT_ID: &str = "0000000000000002";

    /// Minimal valid OTLP payload — one resource, one scope,
    /// one span with `code.namespace` + `code.function`
    /// attributes (the semconv keys the resolver looks for).
    const SAMPLE: &str = r#"{
        "resourceSpans": [{
            "resource": {"attributes": []},
            "scopeSpans": [{
                "scope": {"name": "test", "version": "0"},
                "spans": [{
                    "traceId": "00000000000000000000000000000001",
                    "spanId": "0000000000000001",
                    "parentSpanId": "0000000000000002",
                    "name": "GET /orders",
                    "kind": 1,
                    "startTimeUnixNano": "1700000000000000000",
                    "endTimeUnixNano": "1700000000100000000",
                    "attributes": [
                        {"key": "code.namespace", "value": {"stringValue": "app.orders"}},
                        {"key": "code.function", "value": {"stringValue": "handle_order"}}
                    ]
                }]
            }]
        }]
    }"#;

    #[test]
    fn parse_minimal_payload_produces_one_span_record() {
        let records = parse_otlp_json(SAMPLE.as_bytes()).expect("parse");
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.trace_id, SAMPLE_TRACE_ID);
        assert_eq!(r.span_id, SAMPLE_SPAN_ID);
        assert_eq!(r.parent_span_id.as_deref(), Some(SAMPLE_PARENT_ID));
        assert_eq!(r.name, "GET /orders");
        assert_eq!(r.kind, SpanKind::Internal);
        assert_eq!(
            r.attributes.get("code.namespace").and_then(|a| a.as_str()),
            Some("app.orders"),
        );
        assert_eq!(
            r.attributes.get("code.function").and_then(|a| a.as_str()),
            Some("handle_order"),
        );
        // 1700000000100000000 ns = 1700000000 s + 100 ms.
        assert_eq!(r.end_unix, 1_700_000_000);
    }

    #[test]
    fn parse_typed_attributes() {
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "00000000000000000000000000000002",
                        "spanId": "0000000000000003",
                        "name": "x",
                        "kind": 2,
                        "endTimeUnixNano": "1700000000000000000",
                        "attributes": [
                            {"key": "code.function", "value": {"stringValue": "f"}},
                            {"key": "http.status", "value": {"intValue": "200"}},
                            {"key": "retry", "value": {"boolValue": true}},
                            {"key": "latency", "value": {"doubleValue": 1.5}}
                        ]
                    }]
                }]
            }]
        }"#;
        let records = parse_otlp_json(payload.as_bytes()).expect("parse");
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.kind, SpanKind::Server);
        assert!(matches!(
            r.attributes.get("http.status"),
            Some(AttributeValue::I64(200))
        ));
        assert!(matches!(
            r.attributes.get("retry"),
            Some(AttributeValue::Bool(true))
        ));
        assert!(matches!(
            r.attributes.get("latency"),
            Some(AttributeValue::F64(1.5))
        ));
    }

    #[test]
    fn parse_rejects_malformed_trace_id() {
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "tooshort",
                        "spanId": "0000000000000001",
                        "name": "x",
                        "endTimeUnixNano": "1700000000000000000"
                    }]
                }]
            }]
        }"#;
        let err = parse_otlp_json(payload.as_bytes()).expect_err("must reject");
        assert!(
            matches!(err, OtlpParseError::TraceId(_)),
            "expected TraceId error, got: {err:?}"
        );
    }

    #[test]
    fn parse_rejects_malformed_span_id() {
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "00000000000000000000000000000001",
                        "spanId": "bad",
                        "name": "x",
                        "endTimeUnixNano": "1700000000000000000"
                    }]
                }]
            }]
        }"#;
        let err = parse_otlp_json(payload.as_bytes()).expect_err("must reject");
        assert!(matches!(err, OtlpParseError::SpanId(_)));
    }

    /// OTLP IDs are lowercase base16 per the spec. A producer
    /// emitting uppercase is non-conformant and the bug should
    /// surface as a parse error rather than silently being
    /// accepted.
    #[test]
    fn parse_rejects_uppercase_hex_ids() {
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "0A1B2C3D4E5F60718293A4B5C6D7E8F90",
                        "spanId": "0000000000000001",
                        "name": "x",
                        "endTimeUnixNano": "1700000000000000000"
                    }]
                }]
            }]
        }"#;
        let err = parse_otlp_json(payload.as_bytes()).expect_err("must reject");
        assert!(matches!(err, OtlpParseError::TraceId(_)));
    }

    #[test]
    fn parse_rejects_non_integer_end_time() {
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "00000000000000000000000000000001",
                        "spanId": "0000000000000001",
                        "name": "x",
                        "endTimeUnixNano": "notanumber"
                    }]
                }]
            }]
        }"#;
        let err = parse_otlp_json(payload.as_bytes()).expect_err("must reject");
        assert!(matches!(err, OtlpParseError::EndTime(_)));
    }

    #[test]
    fn parse_empty_resource_spans_is_ok() {
        let payload = r#"{"resourceSpans": []}"#;
        let records = parse_otlp_json(payload.as_bytes()).expect("parse");
        assert!(records.is_empty());
    }

    #[test]
    fn parse_unknown_span_kind_falls_back_to_internal() {
        // OTLP kind 99 is reserved for future use; we accept the
        // payload and fall back to Internal rather than dropping it
        // outright (the alternative — silent loss of one span out
        // of many — would surprise operators).
        let payload = r#"{
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "00000000000000000000000000000001",
                        "spanId": "0000000000000001",
                        "name": "x",
                        "kind": 99,
                        "endTimeUnixNano": "1700000000000000000"
                    }]
                }]
            }]
        }"#;
        let records = parse_otlp_json(payload.as_bytes()).expect("parse");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, SpanKind::Internal);
    }
}
