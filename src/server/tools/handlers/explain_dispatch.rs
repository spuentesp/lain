//! `explain_dispatch` — synthesise every signal that bears on the
//! "does this symbol have any callers" question.
//!
//! Where `get_blast_radius` answers "what breaks if I change X",
//! `explain_dispatch` answers the upstream question: "do I actually
//! *know* everything that touches X?". It collects four orthogonal
//! signals:
//!
//! - **static** — incoming `Calls`/`Uses` edges from the static graph.
//! - **heuristic** — incoming `DynamicDispatch` / `BusTopic` /
//!   `RouteMatches` edges laid down by the Tier 2 sensor, with the
//!   detector name and confidence that produced each one.
//! - **runtime** — incoming `RuntimeCall` edges from the OTLP ingest
//!   store, with the trace id and last-seen timestamp.
//! - **co_change** — files that move together with the target's file
//!   in git history.
//!
//! From these it emits a single `verdict` field so the agent can
//! route on a string instead of repeating the synthesis logic on
//! every call:
//!
//! | static | heuristic | runtime | verdict |
//! |---|---|---|---|
//! | yes | – | – | `static_only` |
//! | yes | yes | – | `static_and_heuristic` |
//! | – | yes | – | `heuristic_only` |
//! | – | – | yes | `runtime_only` |
//! | yes | yes | yes | `runtime_confirmed` |
//! | – | – | – | `insufficient_evidence` |
//!
//! `insufficient_evidence` is the value Tier 1 teaches the agent to
//! treat as "do not assume safety" — the gap the dynamic-dispatch
//! mitigation exists to close.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode};
use crate::server::runtime_trace::RuntimeTraceStore;
use crate::server::tools::utils::resolve_node;
use serde::Serialize;
use std::collections::HashSet;

/// One heuristic caller, paired with the detector that flagged it.
#[derive(Debug, Serialize)]
pub struct HeuristicCaller {
    pub target_id: String,
    pub detector: String,
    pub confidence: f32,
}

/// One runtime-observed caller, with the trace that produced it.
#[derive(Debug, Serialize)]
pub struct RuntimeCaller {
    pub source_id: String,
    pub trace_id: String,
    pub last_seen_unix: i64,
}

#[derive(Debug, Serialize)]
pub struct StaticLink {
    pub source_id: String,
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct ExplainDispatch {
    pub symbol: String,
    pub target_id: String,
    pub target_path: String,
    pub static_callers: Vec<StaticLink>,
    pub heuristic_callers: Vec<HeuristicCaller>,
    pub runtime_callers: Vec<RuntimeCaller>,
    pub co_change_partners: Vec<String>,
    pub verdict: &'static str,
    /// Why the answer may not cover everything, or `None` when it does.
    /// `None` alongside `insufficient_evidence` means the graph really
    /// holds no signal for this symbol — a confirmed absence of
    /// *evidence*, which is still not proof that nothing calls it. A
    /// reason names what could not be seen (an ambiguous name, a stale
    /// index, a dispatcher the static graph cannot follow).
    pub answer_scope: Option<crate::server::tools::answer_scope::IncompleteReason>,
}

/// Look up everything we know about `symbol` and render the verdict.
pub async fn explain_dispatch(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    symbol: &str,
) -> Result<String, LainError> {
    explain_dispatch_scoped(graph, overlay, workspace, symbol, false).await
}

pub struct ExplainDispatchReport {
    pub text: String,
    pub report: ExplainDispatch,
}

pub async fn explain_dispatch_scoped(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    symbol: &str,
    historical: bool,
) -> Result<String, LainError> {
    explain_dispatch_report_scoped(graph, overlay, workspace, symbol, historical)
        .await
        .map(|report| report.text)
}

pub async fn explain_dispatch_report_scoped(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    symbol: &str,
    historical: bool,
) -> Result<ExplainDispatchReport, LainError> {
    // Resolve the way the sibling tools do: a bare name can name several
    // definitions, and an answer about one of them must say so rather
    // than read as a confirmed absence of evidence.
    let (node, other_defs) =
        crate::server::tools::utils::resolve_node_ambiguous(graph, overlay, symbol)?;
    let empty_store;
    let store = if historical {
        empty_store = RuntimeTraceStore::new(Default::default());
        &empty_store
    } else {
        RuntimeTraceStore::global()
    };
    let mut report = build_with_store(graph, overlay, &node, store);
    // The builder knows whether the symbol is reached dynamically; the
    // ambiguity and the index freshness come from the resolution above
    // and from the file the symbol lives in.
    report.answer_scope = crate::server::tools::answer_scope::reason_for(
        !other_defs.is_empty(),
        !historical && crate::server::tools::answer_scope::is_stale(graph, workspace, &node.path),
        report.answer_scope
            == Some(crate::server::tools::answer_scope::IncompleteReason::DynamicTarget),
    );

    // Human-readable summary at the top, JSON-ish body at the bottom
    // so existing tools that expect prose still get it.
    //
    // Note: phrased as `Dispatch summary` rather than the CLI form so
    // the user-facing-string sweep doesn't misread this as a
    // subcommand reference (`lain dispatch` is not a real command;
    // the tool is named `explain_dispatch`).
    let mut out = String::new();
    out.push_str(&format!(
        "Dispatch summary for '{}' ({}):\n  verdict: {}\n",
        node.name, node.path, report.verdict
    ));
    out.push_str(&format!(
        "  static_callers:    {}\n",
        report.static_callers.len()
    ));
    out.push_str(&format!(
        "  heuristic_callers: {}\n",
        report.heuristic_callers.len()
    ));
    out.push_str(&format!(
        "  runtime_callers:   {}\n",
        report.runtime_callers.len()
    ));
    out.push_str(&format!(
        "  co_change_partners:{}\n",
        report.co_change_partners.len()
    ));
    out.push_str("\n--- JSON ---\n");
    out.push_str(
        &serde_json::to_string_pretty(&report)
            .unwrap_or_else(|e| format!("{{\"serialise_error\": \"{e}\"}}")),
    );
    Ok(ExplainDispatchReport { text: out, report })
}

/// Pure builder. Public so tests can call it without going through the
/// JSON path. The `explain_dispatch` thin wrapper passes the global
/// runtime store; tests should call `build_with_store` with a private
/// `RuntimeTraceStore` so parallel runs don't share state via the
/// process-global `OnceLock`.
pub fn build(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    target: &GraphNode,
) -> ExplainDispatch {
    build_with_store(graph, overlay, target, RuntimeTraceStore::global())
}

/// Same as `build` but with an injected store. Lets tests pin a
/// private, empty store and avoid the cross-test pollution the global
/// `OnceLock` would otherwise introduce.
pub fn build_with_store(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    target: &GraphNode,
    store: &RuntimeTraceStore,
) -> ExplainDispatch {
    let mut static_callers: Vec<StaticLink> = Vec::new();
    let mut heuristic_callers: Vec<HeuristicCaller> = Vec::new();
    let mut runtime_callers: Vec<RuntimeCaller> = Vec::new();
    let mut seen_static: HashSet<String> = HashSet::new();
    let mut seen_heuristic: HashSet<String> = HashSet::new();
    let mut seen_runtime: HashSet<String> = HashSet::new();

    if let Ok(incoming) = graph.get_edges_to(&target.id) {
        for e in incoming {
            classify_static_or_heuristic(
                e,
                &mut static_callers,
                &mut heuristic_callers,
                &mut seen_static,
                &mut seen_heuristic,
            );
        }
    }
    for (caller, edge_type) in overlay.get_incoming_edges(&target.id) {
        match edge_type {
            EdgeType::Calls | EdgeType::Uses => {
                if seen_static.insert(caller.id.clone()) {
                    static_callers.push(StaticLink {
                        source_id: caller.id.clone(),
                        kind: format!("{:?}", edge_type),
                    });
                }
            }
            EdgeType::DynamicDispatch | EdgeType::BusTopic | EdgeType::RouteMatches
                if seen_heuristic.insert(caller.id.clone()) =>
            {
                // Overlay edges don't carry provenance yet; tag with
                // a synthetic detector so the consumer knows the
                // path. Confidence 0.5 = threshold default.
                heuristic_callers.push(HeuristicCaller {
                    target_id: caller.id.clone(),
                    detector: "overlay".to_string(),
                    confidence: 0.5,
                });
            }
            _ => {}
        }
    }

    // Runtime store is supplied by the caller (the public `build`
    // path uses the global). We mirror the static-edge policy: only
    // attach callers, not callees.
    for edge in store.edges_to(&target.id) {
        if seen_runtime.insert(edge.edge.source_id.clone()) {
            if let Some(EdgeProvenance::Runtime {
                trace_id,
                last_seen_unix,
            }) = &edge.edge.provenance
            {
                runtime_callers.push(RuntimeCaller {
                    source_id: edge.edge.source_id.clone(),
                    trace_id: trace_id.clone(),
                    last_seen_unix: *last_seen_unix,
                });
            }
        }
    }

    let co_change_partners: Vec<String> = graph
        .get_co_change_partners(&target.path)
        .unwrap_or_default()
        .into_iter()
        .map(|(path, _count)| path)
        .collect();

    let has_static = !static_callers.is_empty();
    let has_heuristic = !heuristic_callers.is_empty();
    let has_runtime = !runtime_callers.is_empty();
    let verdict = match (has_static, has_heuristic, has_runtime) {
        (_, _, true) if has_static || has_heuristic => "runtime_confirmed",
        (true, _, _) => "static_only",
        (false, true, _) => "heuristic_only",
        (false, false, true) => "runtime_only",
        (false, false, false) => "insufficient_evidence",
    };

    // The verdict says which signals fired; this says why an empty
    // answer might be empty rather than complete. A symbol that
    // dispatches dynamically has callers the static graph cannot see, so
    // "no static callers" is not evidence that none exist.
    let answer_scope = crate::server::tools::answer_scope::reason_for(
        false,
        false,
        crate::server::tools::answer_scope::dispatches_dynamically(graph, target),
    );

    ExplainDispatch {
        symbol: target.name.clone(),
        target_id: target.id.clone(),
        target_path: target.path.clone(),
        static_callers,
        heuristic_callers,
        runtime_callers,
        co_change_partners,
        verdict,
        answer_scope,
    }
}

fn classify_static_or_heuristic(
    e: GraphEdge,
    static_callers: &mut Vec<StaticLink>,
    heuristic_callers: &mut Vec<HeuristicCaller>,
    seen_static: &mut HashSet<String>,
    seen_heuristic: &mut HashSet<String>,
) {
    match e.edge_type {
        EdgeType::Calls | EdgeType::Uses => {
            if seen_static.insert(e.source_id.clone()) {
                static_callers.push(StaticLink {
                    source_id: e.source_id,
                    kind: format!("{:?}", e.edge_type),
                });
            }
        }
        EdgeType::DynamicDispatch | EdgeType::BusTopic | EdgeType::RouteMatches => {
            let (detector, confidence) = match e.provenance {
                Some(EdgeProvenance::Heuristic {
                    detector,
                    confidence,
                }) => (detector, confidence),
                _ => ("unknown".to_string(), e.weight.unwrap_or(0.0)),
            };
            if seen_heuristic.insert(e.source_id.clone()) {
                heuristic_callers.push(HeuristicCaller {
                    target_id: e.source_id,
                    detector,
                    confidence,
                });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EdgeProvenance, GraphEdge, NodeType, RepoNamespace};

    fn fn_node(name: &str, path: &str, ns: &RepoNamespace) -> GraphNode {
        let id = GraphNode::generate_id(&NodeType::Function, path, name, None, ns);
        let mut n = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
        n.id = id;
        n
    }

    /// `explain_dispatch` resolves the symbol up-front via
    /// `resolve_node`, so a missing symbol must surface as the same
    /// `NotFound` an agent would see from any other tool — not a
    /// `verdict: "insufficient_evidence"` report that lies about
    /// what is and isn't in the graph. Without this pin, a future
    /// refactor that replaces the `resolve_node` early-return with
    /// a fall-through to `build_with_store` would emit a confident
    /// "no callers" answer for a symbol that doesn't exist at all.
    #[tokio::test]
    async fn explain_dispatch_returns_not_found_for_unknown_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();

        let result = explain_dispatch(
            &graph,
            &overlay,
            std::path::Path::new(""),
            "definitely_not_a_real_symbol_xyz",
        )
        .await;

        match result {
            Err(LainError::NotFound(msg)) => {
                assert!(
                    msg.contains("Node not found for handle"),
                    "expected the standard NotFound from resolve_node, got: {msg}"
                );
            }
            other => panic!(
                "expected NotFound for an unknown symbol, got {:?}",
                other.map(|s| s.lines().next().unwrap_or("").to_string())
            ),
        }
    }

    /// A name that matches several definitions must not be answered as
    /// "confirmed absence of evidence": the report says which of the
    /// candidates it is about.
    #[tokio::test]
    async fn an_ambiguous_name_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();
        for path in ["src/a.rs", "src/b.rs"] {
            let mut n = GraphNode::new(NodeType::Function, "twin".to_string(), path.to_string());
            n.id = GraphNode::generate_id(&NodeType::Function, path, "twin", None, &ns);
            graph.upsert_node(n).unwrap();
        }
        let report = explain_dispatch_report_scoped(&graph, &overlay, dir.path(), "twin", false)
            .await
            .expect("a known name must resolve");
        assert_eq!(
            report.report.answer_scope,
            Some(crate::server::tools::answer_scope::IncompleteReason::AmbiguousTarget),
            "an ambiguous name must not read as a confirmed absence"
        );
    }

    /// A file edited after its scan makes the answer provisional.
    #[tokio::test]
    async fn a_dirty_file_is_reported_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/t.rs"), "fn target() {}\n").unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new(
            NodeType::Function,
            "target".to_string(),
            "src/t.rs".to_string(),
        );
        n.id = GraphNode::generate_id(&NodeType::Function, "src/t.rs", "target", None, &ns);
        // Scanned an hour ago; the file on disk was written just now.
        n.last_lsp_sync = Some(crate::server::time::unix_secs(std::time::SystemTime::now()) - 3600);
        graph.upsert_node(n).unwrap();
        let report = explain_dispatch_report_scoped(&graph, &overlay, dir.path(), "target", false)
            .await
            .expect("a known name must resolve");
        assert_eq!(
            report.report.answer_scope,
            Some(crate::server::tools::answer_scope::IncompleteReason::StaleOrUnindexed),
            "a file edited after its scan makes the answer provisional"
        );
    }

    #[test]
    fn verdict_is_insufficient_evidence_when_no_signals() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();
        let target = fn_node("tgt", "src/lib.rs", &ns);
        graph.upsert_node(target.clone()).unwrap();

        let report = build(&graph, &overlay, &target);
        assert_eq!(report.verdict, "insufficient_evidence");
        assert!(report.static_callers.is_empty());
        assert!(report.heuristic_callers.is_empty());
        assert!(report.runtime_callers.is_empty());
    }

    #[test]
    fn verdict_is_static_only_when_only_static_caller_exists() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        let caller = fn_node("caller", "src/caller.rs", &ns);
        let target = fn_node("tgt", "src/tgt.rs", &ns);
        graph.upsert_node(caller.clone()).unwrap();
        graph.upsert_node(target.clone()).unwrap();
        graph
            .insert_edges_batch(&[GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                target.id.clone(),
            )])
            .unwrap();

        let report = build_with_store(
            &graph,
            &overlay,
            &target,
            &RuntimeTraceStore::new(Default::default()),
        );
        assert_eq!(report.verdict, "static_only");
        assert_eq!(report.static_callers.len(), 1);
    }

    #[test]
    fn verdict_is_heuristic_only_when_only_heuristic_caller_exists() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        let caller = fn_node("caller", "src/caller.rs", &ns);
        let target = fn_node("tgt", "src/tgt.rs", &ns);
        graph.upsert_node(caller.clone()).unwrap();
        graph.upsert_node(target.clone()).unwrap();
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::BusTopic,
                source_id: caller.id.clone(),
                target_id: target.id.clone(),
                weight: Some(0.7),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Heuristic {
                    detector: "message_bus_publisher".to_string(),
                    confidence: 0.7,
                }),
                site: None,
                detail: None,
            }])
            .unwrap();

        let report = build_with_store(
            &graph,
            &overlay,
            &target,
            &RuntimeTraceStore::new(Default::default()),
        );
        assert_eq!(report.verdict, "heuristic_only");
        assert_eq!(report.heuristic_callers.len(), 1);
        assert_eq!(
            report.heuristic_callers[0].detector,
            "message_bus_publisher"
        );
        assert!((report.heuristic_callers[0].confidence - 0.7).abs() < 1e-6);
    }

    #[test]
    fn verdict_distinguishes_runtime_confirmed_from_runtime_only() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let ns = RepoNamespace::for_test();

        // static_only case ends up promoted to runtime_confirmed once
        // we drop a span on the same target — that's the exact thing
        // Tier 3 is supposed to express.
        let caller = fn_node("caller", "src/caller.rs", &ns);
        let target = fn_node("tgt", "src/tgt.rs", &ns);
        graph.upsert_node(caller.clone()).unwrap();
        graph.upsert_node(target.clone()).unwrap();
        graph
            .insert_edges_batch(&[GraphEdge::new(
                EdgeType::Calls,
                caller.id.clone(),
                target.id.clone(),
            )])
            .unwrap();

        // Use a private store rather than the global. The global
        // persists across tests in the same process (OnceLock), so
        // any other test that ingests a span with the same trace_id
        // would pollute this verdict. The doc comment on
        // `build_with_store` explicitly notes this isolation contract.
        let store = RuntimeTraceStore::new(crate::server::runtime_trace::StoreConfig::default());
        let parent_span = crate::server::runtime_trace::SpanRecord {
            trace_id: "trace-r1".into(),
            span_id: "parent".into(),
            parent_span_id: None,
            name: "caller".into(),
            kind: crate::server::runtime_trace::SpanKind::Internal,
            attributes: Default::default(),
            end_unix: 0,
        };
        let child_span = crate::server::runtime_trace::SpanRecord {
            trace_id: "trace-r1".into(),
            span_id: "child".into(),
            parent_span_id: Some("parent".into()),
            name: "tgt".into(),
            kind: crate::server::runtime_trace::SpanKind::Internal,
            attributes: Default::default(),
            end_unix: 0,
        };
        let mut map = std::collections::HashMap::new();
        map.insert("parent", caller.id.clone());
        map.insert("child", target.id.clone());
        store.ingest(&[parent_span, child_span], |s| {
            map.get(s.span_id.as_str()).cloned()
        });

        let report = build_with_store(&graph, &overlay, &target, &store);
        assert_eq!(report.verdict, "runtime_confirmed");
        assert_eq!(report.runtime_callers.len(), 1);
        assert_eq!(report.runtime_callers[0].trace_id, "trace-r1");
    }
}
