//! `SpanRecord` — the in-memory representation of one OTLP span.
//!
//! An OTLP span describes a unit of work in the running application.
//! The parts we care about for code-graph mapping are:
//!
//! - `name` — usually `ClassName::method_name` or a fully-qualified
//!   function path. Used as a fallback when `code.namespace` /
//!   `code.function` aren't set.
//! - `kind` — server / client / producer / consumer / internal. Helps
//!   decide whether the span should produce a `RuntimeCall` edge in
//!   the inbound or outbound direction.
//! - `attributes` — semconv attributes (`code.namespace`,
//!   `code.function`, `code.filepath`, `code.lineno`,
//!   `code.column`, plus RPC/HTTP/database-specific ones).
//! - `trace_id` / `span_id` / `parent_span_id` — identity and parent
//!   linkage. `parent_span_id == None` means a root span.
//!
//! We deliberately model these as plain data, not as an OTLP protobuf
//! type. The OTLP adapter (a future `otlp.rs`) is responsible for
//! converting the wire format into `SpanRecord`; everything downstream
//! of that boundary is independent of the OTLP encoding.

use std::collections::HashMap;

/// OTLP `SpanKind` reduced to the variants we care about. The full
/// OTLP enum has five values; "internal" and the four RPC-style kinds
/// are the ones that map to code-graph edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    /// Map an OTLP span-kind integer to the reduced enum. Returns
    /// `None` for unknown variants so callers can decide whether to
    /// drop or store the span.
    pub fn from_otlp(v: i32) -> Option<Self> {
        match v {
            0 => None, // unspecified — drop
            1 => Some(Self::Internal),
            2 => Some(Self::Server),
            3 => Some(Self::Client),
            4 => Some(Self::Producer),
            5 => Some(Self::Consumer),
            _ => None,
        }
    }
}

/// One span in the application's runtime trace. Cheap to construct
/// and clone; the store keeps many of these in memory.
#[derive(Debug, Clone)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    /// Free-form attributes. The span-to-node resolver looks up the
    /// `code.*` semconv keys here; the rest are preserved so the OTLP
    /// adapter can round-trip them later if needed.
    pub attributes: HashMap<String, AttributeValue>,
    /// Unix timestamp (seconds since epoch) when the span ended.
    /// Drives the TTL purge and the `last_seen_unix` field on
    /// `EdgeProvenance::Runtime`.
    pub end_unix: i64,
}

/// OTLP attribute values are typed; we keep the relevant variants. The
/// full enum has 7; we collapse to the four that carry semantic value
/// for graph mapping.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    Str(String),
    Bool(bool),
    I64(i64),
    F64(f64),
}

impl AttributeValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otlp_kind_mapping_is_exhaustive_for_relevant_variants() {
        assert_eq!(SpanKind::from_otlp(0), None);
        assert_eq!(SpanKind::from_otlp(1), Some(SpanKind::Internal));
        assert_eq!(SpanKind::from_otlp(2), Some(SpanKind::Server));
        assert_eq!(SpanKind::from_otlp(3), Some(SpanKind::Client));
        assert_eq!(SpanKind::from_otlp(4), Some(SpanKind::Producer));
        assert_eq!(SpanKind::from_otlp(5), Some(SpanKind::Consumer));
        assert_eq!(SpanKind::from_otlp(99), None);
    }

    #[test]
    fn attribute_value_str_accessor_only_matches_strings() {
        let s = AttributeValue::Str("hello".into());
        assert_eq!(s.as_str(), Some("hello"));
        assert_eq!(AttributeValue::Bool(true).as_str(), None);
        assert_eq!(AttributeValue::I64(42).as_str(), None);
        assert_eq!(AttributeValue::F64(1.5).as_str(), None);
    }
}
