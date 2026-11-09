//! Why an answer may not cover everything — one vocabulary for every tool
//! that can return an empty, partial, or truncated result.
//!
//! The reasons here are the ones the analyzer can *observe* from the data
//! it holds. There is deliberately no numeric confidence: a score without a
//! calibrated method behind it is decoration, not evidence, and a reader
//! cannot act on it. What a reader can act on is the distinction that
//! matters most — an answer that says "no callers" because the graph
//! looked and found none, versus one that says "no callers" because the
//! graph could not look.
//!
//! Every tool that can return an incomplete answer uses these labels, so
//! the same word means the same thing wherever it appears.

use crate::graph::{Freshness, GraphDatabase};
use crate::schema::{EdgeType, GraphNode};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Why an answer is incomplete, or `None` when it is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    /// The name did not resolve to a node in the graph.
    UnresolvedName,
    /// The name resolves to more than one definition, so the answer
    /// covers one of several candidates.
    AmbiguousTarget,
    /// The target is chosen at runtime — dynamic dispatch, a bus, a
    /// router, or a URL/path assembled at call time — so no static edge
    /// can exist for it.
    DynamicTarget,
    /// The file was modified after its last scan, or was never scanned,
    /// so the index may predate the code it is being asked about.
    StaleOrUnindexed,
    /// The traversal hit its cap or its depth limit, so the result is a
    /// prefix of the full answer.
    Truncated,
}

impl IncompleteReason {
    /// The wire label used in structured tool output.
    pub fn wire_label(&self) -> &'static str {
        match self {
            IncompleteReason::UnresolvedName => "unresolved_name",
            IncompleteReason::AmbiguousTarget => "ambiguous_target",
            IncompleteReason::DynamicTarget => "dynamic_target",
            IncompleteReason::StaleOrUnindexed => "stale_or_unindexed",
            IncompleteReason::Truncated => "truncated",
        }
    }

    /// One sentence explaining the gap, for the text responses. The
    /// wording says what the answer cannot tell you, not what went wrong
    /// internally.
    pub fn explanation(&self) -> &'static str {
        match self {
            IncompleteReason::UnresolvedName => {
                "the name did not resolve to a symbol in the graph"
            }
            IncompleteReason::AmbiguousTarget => {
                "the name matches more than one definition, so this answer covers one of them"
            }
            IncompleteReason::DynamicTarget => {
                "the target is chosen at runtime, so callers the static graph cannot see are not counted"
            }
            IncompleteReason::StaleOrUnindexed => {
                "the file changed after it was last indexed (or was never indexed), so recent changes are missing"
            }
            IncompleteReason::Truncated => {
                "the traversal hit its cap, so this is a prefix of the full answer"
            }
        }
    }
}

/// The reason an answer cannot be trusted to be complete, or `None` when
/// it can.
///
/// The checks run in the order a reader cares about: a name that matches
/// several definitions is answered "about one of them" whatever else is
/// true; a file that moved on after its scan makes the whole answer
/// provisional; and a symbol reached through a dispatcher has callers the
/// static graph cannot see.
///
/// The caller supplies the three signals because only the caller knows
/// them: `ambiguous` is whether the *name* the caller used resolves to
/// more than one node, `stale` is whether the index may predate the file
/// on disk (a snapshot has no disk to compare against, so its caller
/// passes `false`), and `dynamic` is whether the symbol dispatches
/// dynamically — it publishes to a bus, resolves through a container, or
/// is routed by a router — which is what makes its own incoming callers
/// invisible.
pub fn reason_for(ambiguous: bool, stale: bool, dynamic: bool) -> Option<IncompleteReason> {
    if ambiguous {
        return Some(IncompleteReason::AmbiguousTarget);
    }
    if stale {
        return Some(IncompleteReason::StaleOrUnindexed);
    }
    if dynamic {
        return Some(IncompleteReason::DynamicTarget);
    }
    None
}

/// Whether the graph's copy of `path` may predate the file on disk.
///
/// `Freshness::Absent` covers both "the file has no nodes" and "no node
/// carries a scan timestamp" — in both cases the index cannot prove it
/// saw the current file, so an answer about that file is provisional.
pub fn is_stale(graph: &GraphDatabase, workspace: &Path, path: &str) -> bool {
    let freshness = graph.freshness(workspace, path);
    freshness_says_stale(Some(&freshness))
}

/// Whether a freshness reading means the answer may predate the code.
///
/// One policy, stated once: a snapshot (`None` — no disk to compare
/// against) and a current file both mean "the index is current";
/// anything else means the answer is provisional. Callers that already
/// hold a reading use this rather than re-deriving the policy.
pub fn freshness_says_stale(freshness: Option<&Freshness>) -> bool {
    !matches!(freshness, None | Some(Freshness::Fresh))
}

/// Whether the static graph may be missing callers of `node` because
/// of dynamic dispatch.
///
/// Two shapes cover what the graph actually holds:
///
/// - A heuristic edge (`DynamicDispatch` / `BusTopic` / `RouteMatches`)
///   pointing **at** the symbol. The dispatch sensor emits these when
///   it can resolve the receiver, and then the symbol is reached by
///   calls the static graph cannot follow.
/// - The symbol's **file** carrying a dispatch site. The sensor cannot
///   resolve a receiver it cannot type, so it emits `File -> Hub:<detector>`
///   instead: the file publishes to a bus or resolves through a
///   container, and whatever is reached that way is invisible to the
///   call graph. A symbol in such a file may be one of those targets,
///   so an answer that reports no callers is answering a narrower
///   question than "nothing calls this".
///
/// The direction matters: a symbol that *emits* a heuristic edge is a
/// dispatcher, not a dynamically-reached target. Only the incoming
/// edge and the file's dispatch site say "callers of this may be
/// invisible".
pub fn dispatches_dynamically(graph: &GraphDatabase, node: &GraphNode) -> bool {
    let is_heuristic = |e: &crate::schema::GraphEdge| {
        matches!(
            e.edge_type,
            EdgeType::DynamicDispatch | EdgeType::BusTopic | EdgeType::RouteMatches
        )
    };
    // The resolved-receiver form: a heuristic edge points at the symbol.
    if let Ok(edges) = graph.get_edges_to(&node.id) {
        if edges.iter().any(is_heuristic) {
            return true;
        }
    }
    // The unresolved-receiver form: the symbol's file dispatches
    // dynamically, so the graph cannot see what that dispatch reaches.
    match graph.get_file_node(&node.path) {
        Some(file) => match graph.get_edges_from(&file.id) {
            Ok(edges) => edges.iter().any(is_heuristic),
            Err(_) => false,
        },
        None => false,
    }
}

/// The structured form of an answer's completeness: the reason, or
/// `None` when the answer is complete.
///
/// `None` in this field means the answer covers everything the index
/// knows — including an empty answer that is a confirmed absence (a
/// leaf), which is a complete answer about a symbol nothing calls.
pub fn scope_value(reason: Option<IncompleteReason>) -> serde_json::Value {
    match reason {
        Some(r) => serde_json::Value::String(r.wire_label().to_string()),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::GraphEdge;

    #[test]
    fn wire_labels_are_stable() {
        assert_eq!(
            IncompleteReason::UnresolvedName.wire_label(),
            "unresolved_name"
        );
        assert_eq!(
            IncompleteReason::AmbiguousTarget.wire_label(),
            "ambiguous_target"
        );
        assert_eq!(
            IncompleteReason::DynamicTarget.wire_label(),
            "dynamic_target"
        );
        assert_eq!(
            IncompleteReason::StaleOrUnindexed.wire_label(),
            "stale_or_unindexed"
        );
        assert_eq!(IncompleteReason::Truncated.wire_label(), "truncated");
    }

    #[test]
    fn a_complete_answer_carries_no_reason() {
        assert_eq!(scope_value(None), serde_json::Value::Null);
        assert_eq!(
            scope_value(Some(IncompleteReason::Truncated)),
            serde_json::Value::String("truncated".to_string())
        );
    }

    #[test]
    fn ambiguity_outranks_staleness_and_dynamics() {
        // A name that matches several definitions is answered "about one
        // of them" whatever else is true — the reader has to know that
        // first.
        assert_eq!(
            reason_for(true, true, true),
            Some(IncompleteReason::AmbiguousTarget)
        );
        assert_eq!(
            reason_for(false, true, true),
            Some(IncompleteReason::StaleOrUnindexed)
        );
        assert_eq!(
            reason_for(false, false, true),
            Some(IncompleteReason::DynamicTarget)
        );
        assert_eq!(reason_for(false, false, false), None);
    }

    /// Each explanation names the gap it covers. A reader acts on this
    /// text, so a mutant that swaps it for a placeholder must fail.
    #[test]
    fn each_explanation_names_the_gap_it_covers() {
        let (reason, needle) = (IncompleteReason::UnresolvedName, "resolve");
        assert!(
            reason.explanation().contains(needle),
            "{:?} explanation must mention '{}', got {:?}",
            reason,
            needle,
            reason.explanation()
        );
        assert!(
            IncompleteReason::AmbiguousTarget
                .explanation()
                .contains("more than one definition"),
            "ambiguity must say several definitions share the name"
        );
        assert!(
            IncompleteReason::DynamicTarget
                .explanation()
                .contains("runtime"),
            "a runtime-selected target must say the choice happens at runtime"
        );
        assert!(
            IncompleteReason::StaleOrUnindexed
                .explanation()
                .contains("indexed"),
            "a stale index must say the index is behind the code"
        );
        assert!(
            IncompleteReason::Truncated.explanation().contains("cap"),
            "a truncated traversal must say the walk stopped early"
        );
    }

    /// The dynamic signal must come from the graph's real shape. The
    /// dispatch sensor emits `File -> Hub:<detector>` edges (it cannot
    /// resolve the receiver), so a symbol in a file that dispatches
    /// dynamically has callers the static graph cannot see — even though
    /// no edge points at the symbol itself.
    ///
    /// A hand-built `Function -> Function` heuristic edge would not
    /// catch this: the sensor never produces one. This fixture runs the
    /// real sensor.
    #[test]
    fn a_symbol_in_a_file_that_dispatches_dynamically_is_not_a_confirmed_leaf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dispatcher.rs"),
            "trait Handler { fn handle(&self, req: Request); }\n\
             fn run(h: Box<dyn Handler>) { h.handle(req); }\n",
        )
        .unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let ns = crate::schema::RepoNamespace::for_test();
        crate::server::sensors::dynamic_dispatch_sensor::scan_workspace_dispatch(
            &graph,
            dir.path(),
            &ns,
        )
        .unwrap();

        let mut handler = GraphNode::new(
            crate::schema::NodeType::Function,
            "handle".into(),
            "dispatcher.rs".into(),
        );
        handler.id = GraphNode::generate_id(
            &crate::schema::NodeType::Function,
            "dispatcher.rs",
            "handle",
            None,
            &ns,
        );
        graph.upsert_node(handler.clone()).unwrap();

        assert!(
            dispatches_dynamically(&graph, &handler),
            "a symbol in a file with a dynamic dispatch site is reached by calls \
             the static graph cannot see"
        );
    }

    /// The opposite direction must not fire: a symbol in a file with no
    /// dispatch site is a plain leaf, whatever else the graph holds.
    #[test]
    fn a_symbol_in_a_plain_file_is_not_dynamic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.rs"), "fn plain() {}\n").unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let ns = crate::schema::RepoNamespace::for_test();
        let mut plain = GraphNode::new(
            crate::schema::NodeType::Function,
            "plain".into(),
            "plain.rs".into(),
        );
        plain.id = GraphNode::generate_id(
            &crate::schema::NodeType::Function,
            "plain.rs",
            "plain",
            None,
            &ns,
        );
        graph.upsert_node(plain.clone()).unwrap();
        assert!(!dispatches_dynamically(&graph, &plain));
    }

    /// A heuristic edge that points *at* the symbol is the resolved-
    /// receiver form. It must fire on the symbol, not on the file.
    #[test]
    fn an_incoming_heuristic_edge_marks_the_target_dynamic() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let ns = crate::schema::RepoNamespace::for_test();
        let mut target = GraphNode::new(
            crate::schema::NodeType::Function,
            "handler".into(),
            "handler.rs".into(),
        );
        target.id = GraphNode::generate_id(
            &crate::schema::NodeType::Function,
            "handler.rs",
            "handler",
            None,
            &ns,
        );
        let mut publisher = GraphNode::new(
            crate::schema::NodeType::Function,
            "publish".into(),
            "publisher.rs".into(),
        );
        publisher.id = GraphNode::generate_id(
            &crate::schema::NodeType::Function,
            "publisher.rs",
            "publish",
            None,
            &ns,
        );
        graph.upsert_node(target.clone()).unwrap();
        graph.upsert_node(publisher.clone()).unwrap();
        graph
            .upsert_edge(GraphEdge::new(
                EdgeType::DynamicDispatch,
                publisher.id.clone(),
                target.id.clone(),
            ))
            .unwrap();
        assert!(
            dispatches_dynamically(&graph, &target),
            "a heuristic edge pointing at the symbol marks it as dynamically reached"
        );
        assert!(
            !dispatches_dynamically(&graph, &publisher),
            "the publisher is not itself a dynamic target"
        );
    }
}
