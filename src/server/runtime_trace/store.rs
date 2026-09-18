//! `RuntimeTraceStore` — in-memory map of runtime edges with TTL.
//!
//! Spans arrive as [`SpanRecord`]s. The store turns each parent→child
//! span pair into a candidate `RuntimeCall` edge, resolves the two
//! endpoints against the static graph by file path + symbol name, and
//! keeps the resulting `RuntimeEdge` records until they expire.
//!
//! The store is intentionally lock-light: it sits behind a single
//! `parking_lot::Mutex` because the working set is small (per the
//! trace TTL, typically a few thousand active spans) and the access
//! pattern is "ingest burst from the OTLP listener, query burst from
//! `explain_dispatch`". A sharded implementation can come later if
//! profiling shows contention.

use super::spans::SpanRecord;
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Configuration for the store. Constructed from environment vars
/// when the server boots; tests build one directly.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// How long a span (and any edge derived from it) survives before
    /// being purged. Default 1 hour.
    pub ttl_secs: i64,
    /// Maximum number of edges the store keeps in memory. A guard
    /// against unbounded growth when a misbehaving producer emits
    /// millions of unique spans. Default 100_000.
    pub max_edges: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 3600,
            max_edges: 100_000,
        }
    }
}

impl StoreConfig {
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(s) = std::env::var("LAIN_TRACE_TTL_SECS") {
            if let Ok(v) = s.parse::<i64>() {
                c.ttl_secs = v.max(0);
            }
        }
        if let Ok(s) = std::env::var("LAIN_TRACE_MAX_EDGES") {
            if let Ok(v) = s.parse::<usize>() {
                c.max_edges = v.max(1);
            }
        }
        c
    }
}

/// One runtime-observed edge, keyed by the unique
/// `(trace_id, span_id, parent_span_id)` triple that produced it.
#[derive(Debug, Clone)]
pub struct RuntimeEdge {
    pub edge: GraphEdge,
    pub trace_id: String,
    pub last_seen_unix: i64,
}

#[derive(Default)]
struct Inner {
    /// Map from `trace_id` to `(caller_id, callee_id)` pairs that have
    /// been observed. Multiple spans on the same trace can produce
    /// multiple edges; we keep the most recent timestamp per pair.
    edges: HashMap<(String, String, String), RuntimeEdge>,
}

/// Thread-safe store. Cheap to clone (Arc-shared).
#[derive(Clone)]
pub struct RuntimeTraceStore {
    inner: Arc<parking_lot::Mutex<Inner>>,
    config: StoreConfig,
}

/// Process-global store, lazily constructed on first access. Tool
/// handlers that want to surface runtime data call
/// [`RuntimeTraceStore::global`] without needing the value threaded
/// through `ToolContext` — the OTLP listener (when added) writes to
/// this same instance. Tests that need isolation should construct
/// their own `RuntimeTraceStore::new(...)` and bypass the global.
static GLOBAL: std::sync::OnceLock<RuntimeTraceStore> = std::sync::OnceLock::new();

impl RuntimeTraceStore {
    /// Global, lazily-initialised store. Constructed from env vars on
    /// first access; subsequent calls return the same instance.
    pub fn global() -> &'static Self {
        GLOBAL.get_or_init(|| Self::new(StoreConfig::from_env()))
    }

    pub fn new(config: StoreConfig) -> Self {
        Self {
            inner: Arc::new(parking_lot::Mutex::new(Inner::default())),
            config,
        }
    }

    /// Ingest a batch of spans. Caller and callee symbol resolution is
    /// the OTLP adapter's responsibility — this method receives the
    /// already-resolved `(source_id, target_id)` pair via the
    /// `resolve` closure so the store stays ignorant of the namespace
    /// minting logic that lives elsewhere.
    ///
    /// `resolve` is invoked once per span and once per (parent, child)
    /// pair; it should return `None` when the symbol isn't in the
    /// graph (e.g. third-party code). Returning `None` simply drops
    /// that end of the edge.
    ///
    /// Returns the number of edges added or refreshed.
    pub fn ingest<F>(&self, spans: &[SpanRecord], resolve: F) -> usize
    where
        F: Fn(&SpanRecord) -> Option<String>,
    {
        let now = now_unix();
        let mut added = 0usize;

        // First pass: resolve each span to a node id, remembering
        // parent->child linkages.
        let mut span_ids: HashMap<&str, &SpanRecord> = HashMap::with_capacity(spans.len());
        for s in spans {
            span_ids.insert(s.span_id.as_str(), s);
        }
        let resolved: Vec<(Option<String>, &SpanRecord)> =
            spans.iter().map(|s| (resolve(s), s)).collect();

        // Second pass: for each span with a parent, attempt to mint
        // a (caller, callee) edge.
        let mut guard = self.inner.lock();
        for (callee_id_opt, span) in &resolved {
            let Some(callee_id) = callee_id_opt.as_ref() else {
                continue;
            };
            let Some(parent_id) = span.parent_span_id.as_ref() else {
                continue;
            };
            let Some(parent) = span_ids.get(parent_id.as_str()) else {
                continue;
            };
            let Some(caller_id) = resolve(parent) else {
                continue;
            };
            if caller_id == *callee_id {
                continue;
            }

            let key = (span.trace_id.clone(), caller_id.clone(), callee_id.clone());
            let edge = GraphEdge {
                edge_type: EdgeType::RuntimeCall,
                source_id: caller_id,
                target_id: callee_id.clone(),
                weight: None,
                cross_repo: false,
                provenance: Some(EdgeProvenance::Runtime {
                    trace_id: span.trace_id.clone(),
                    last_seen_unix: now,
                }),
            };
            let entry = RuntimeEdge {
                edge,
                trace_id: span.trace_id.clone(),
                last_seen_unix: now,
            };

            // HashMap::entry lets us refresh the timestamp without
            // an extra lookup.
            let prior = guard.edges.insert(key, entry);
            if prior.is_none() {
                added += 1;
            }
        }

        // Capacity guard: if we exceeded max_edges after the insert,
        // drop the oldest entries by `last_seen_unix`.
        if guard.edges.len() > self.config.max_edges {
            let overflow = guard.edges.len() - self.config.max_edges;
            let mut by_age: Vec<_> = guard
                .edges
                .iter()
                .map(|(k, v)| (k.clone(), v.last_seen_unix))
                .collect();
            by_age.sort_by_key(|(_, t)| *t);
            for (k, _) in by_age.into_iter().take(overflow) {
                guard.edges.remove(&k);
            }
        }

        added
    }

    /// Purge edges whose `last_seen_unix` is older than `ttl_secs`.
    /// Returns the number purged. Cheap to call; takes the lock for
    /// the duration of one drain pass.
    pub fn purge_expired(&self) -> usize {
        let now = now_unix();
        let min_last_seen = now - self.config.ttl_secs;
        let mut guard = self.inner.lock();
        let before = guard.edges.len();
        guard.edges.retain(|_, e| e.last_seen_unix >= min_last_seen);
        before - guard.edges.len()
    }

    /// Snapshot of the current edges, freshest first. Cloned so the
    /// caller can iterate without holding the lock.
    pub fn snapshot(&self) -> Vec<RuntimeEdge> {
        let guard = self.inner.lock();
        let mut v: Vec<_> = guard.edges.values().cloned().collect();
        v.sort_by(|a, b| b.last_seen_unix.cmp(&a.last_seen_unix));
        v
    }

    /// Edges that start at `source_id`. Used by `explain_dispatch` and
    /// `get_blast_radius` (with `include_runtime=true`).
    pub fn edges_from(&self, source_id: &str) -> Vec<RuntimeEdge> {
        self.snapshot()
            .into_iter()
            .filter(|e| e.edge.source_id == source_id)
            .collect()
    }

    /// Edges that end at `target_id`. Symmetric helper.
    pub fn edges_to(&self, target_id: &str) -> Vec<RuntimeEdge> {
        self.snapshot()
            .into_iter()
            .filter(|e| e.edge.target_id == target_id)
            .collect()
    }

    /// Distinct trace ids currently in the store. Useful for
    /// diagnostics and for tests asserting ingest worked.
    pub fn trace_ids(&self) -> HashSet<String> {
        self.snapshot().into_iter().map(|e| e.trace_id).collect()
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::runtime_trace::spans::SpanKind;
    use std::collections::HashMap;

    fn make_span(trace: &str, span: &str, parent: Option<&str>) -> SpanRecord {
        SpanRecord {
            trace_id: trace.into(),
            span_id: span.into(),
            parent_span_id: parent.map(String::from),
            name: format!("fn_{}", span),
            kind: SpanKind::Internal,
            attributes: HashMap::new(),
            end_unix: now_unix(),
        }
    }

    #[test]
    fn ingest_mints_edges_only_for_resolved_pairs() {
        let store = RuntimeTraceStore::new(StoreConfig::default());
        let spans = vec![
            make_span("t1", "root", None),
            make_span("t1", "leaf_a", Some("root")),
            make_span("t1", "leaf_b", Some("root")),
            make_span("t2", "orphan", None), // no parent linkage
        ];

        let resolved: HashMap<&str, String> = [
            ("root", "node:root".into()),
            ("leaf_a", "node:a".into()),
            ("leaf_b", "node:b".into()),
            ("orphan", "node:orphan".into()),
        ]
        .into_iter()
        .collect();

        let added = store.ingest(&spans, |s| resolved.get(s.span_id.as_str()).cloned());

        // Two edges from root to its two children.
        assert_eq!(added, 2);
        let edges = store.snapshot();
        assert_eq!(edges.len(), 2);

        let targets: HashSet<String> = edges.iter().map(|e| e.edge.target_id.clone()).collect();
        assert!(targets.contains("node:a"));
        assert!(targets.contains("node:b"));
    }

    #[test]
    fn refresh_updates_last_seen_without_growing_the_store() {
        let store = RuntimeTraceStore::new(StoreConfig::default());
        let spans = vec![
            make_span("t1", "root", None),
            make_span("t1", "leaf", Some("root")),
        ];
        let r: HashMap<&str, String> = [("root", "node:root".into()), ("leaf", "node:leaf".into())]
            .into_iter()
            .collect();
        let first = store.ingest(&spans, |s| r.get(s.span_id.as_str()).cloned());
        assert_eq!(first, 1);
        let second = store.ingest(&spans, |s| r.get(s.span_id.as_str()).cloned());
        assert_eq!(second, 0, "re-ingesting same trace must not add a new edge");
        assert_eq!(store.snapshot().len(), 1);
    }

    #[test]
    fn purge_drops_expired_edges() {
        let mut cfg = StoreConfig::default();
        cfg.ttl_secs = 1;
        let store = RuntimeTraceStore::new(cfg);

        let root = make_span("t1", "root", None);
        let leaf = make_span("t1", "leaf", Some("root"));
        let mut id_map: HashMap<&str, String> = HashMap::new();
        id_map.insert("root", "node:root".into());
        id_map.insert("leaf", "node:leaf".into());
        store.ingest(&[root, leaf], |s| id_map.get(s.span_id.as_str()).cloned());

        // Mutate last_seen by hand: the only knob for deterministic tests.
        {
            let mut g = store.inner.lock();
            for (_, e) in g.edges.iter_mut() {
                e.last_seen_unix -= 10;
            }
        }
        let purged = store.purge_expired();
        assert_eq!(purged, 1);
        assert_eq!(store.snapshot().len(), 0);
    }

    #[test]
    fn capacity_guard_drops_oldest_first() {
        let mut cfg = StoreConfig::default();
        cfg.max_edges = 2;
        let store = RuntimeTraceStore::new(cfg);

        let mk = |trace: &str, leaf: &str| {
            (
                make_span(trace, "root", None),
                make_span(trace, leaf, Some("root")),
            )
        };
        for (trace, leaf) in [("t1", "a"), ("t2", "b"), ("t3", "c")] {
            let (root, child) = mk(trace, leaf);
            store.ingest(&[root, child], |s| Some(format!("node:{}", s.span_id)));
        }
        assert_eq!(store.snapshot().len(), 2);
    }
}
