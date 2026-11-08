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
use serde::Serialize;
use std::path::Path;

/// Why an answer is incomplete, or `None` when it is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    matches!(
        graph.freshness(workspace, path),
        Freshness::Dirty { .. } | Freshness::Absent
    )
}

/// Whether `node` dispatches dynamically — it publishes to a bus,
/// resolves through a container, or is routed by a router.
///
/// Such a symbol is reached by calls the static graph cannot see, so an
/// answer that reports no dependents is answering a narrower question
/// than "nothing calls this".
pub fn dispatches_dynamically(graph: &GraphDatabase, node: &GraphNode) -> bool {
    match graph.get_edges_from(&node.id) {
        Ok(edges) => edges.iter().any(|e| {
            matches!(
                e.edge_type,
                EdgeType::DynamicDispatch | EdgeType::BusTopic | EdgeType::RouteMatches
            )
        }),
        Err(_) => false,
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
}
