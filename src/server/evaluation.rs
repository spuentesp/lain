//! Pre-edit evaluation engine (PR 3 of
//! `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
//!
//! The agent asks "may I edit this?" before every Edit. The answer
//! is one of three levels:
//!
//! - **GREEN** — declared scope matches, no peer intent overlaps, no
//!   live exclusive claim on the target.
//! - **YELLOW** — proceed with caution; the reason is surfaced
//!   alongside related activity so the agent can decide.
//! - **RED** — another agent holds an exclusive lease (claim) on
//!   this path/symbol. The agent must not proceed without releasing
//!   or expiring that lease.
//!
//! The function lives here, the wiring lives in
//! `src/server/mcp/intent_tools.rs` (for the `lain_intent` baseline
//! response) and `src/server/mcp/handler.rs::handle_request` (for the
//! pre-edit hook endpoint, planned for a follow-up PR).
//!
//! ## Two-tier consistency
//!
//! Per the plan: presence may be advisory; ownership must not be.
//! RED is the only level that's authoritative — a peer claim is
//! enforced by the existing file-lock primitive, so a RED here means
//! "the in-memory registry says someone holds it; the file lock
//! agrees". YELLOW and GREEN are advisory; the agent can still
//! proceed under caution.

use crate::server::intent::Intent;
use crate::server::presence::{AgentId, Claim, ClaimIntent};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Three-level evaluation result. Serialized to JSON for the
/// `coordination` field in the `lain_intent` response and for the
/// (planned) pre-edit hook endpoint.
///
/// The `related` field is a per-variant field so the JSON shape is
/// uniform across GREEN / YELLOW / RED. The wire format docs
/// promise `coordination: {level, related[]}` regardless of level —
/// a GREEN response carries an empty `related` (no peers flagged)
/// so the agent's UI renders "you're clear" the same way it renders
/// "caution" or "blocked".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum CoordinationLevel {
    /// Target inside declared scope, no peer overlap, no live
    /// claim. Proceed.
    Green {
        #[serde(default)]
        related: Vec<RelatedActivity>,
    },
    /// Proceed with caution. The reason explains why.
    Yellow {
        reason: YellowReason,
        /// Related activity the agent should consult before
        /// proceeding. Empty when the reason is structural rather
        /// than peer-driven (e.g. `OutsideDeclaredScope`).
        #[serde(default)]
        related: Vec<RelatedActivity>,
    },
    /// Stop. Another agent holds an exclusive lease.
    Red {
        reason: RedReason,
        #[serde(default)]
        related: Vec<RelatedActivity>,
    },
}

impl CoordinationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            CoordinationLevel::Green { .. } => "green",
            CoordinationLevel::Yellow { .. } => "yellow",
            CoordinationLevel::Red { .. } => "red",
        }
    }
}

/// Why a yellow. Carries enough context for the agent to render a
/// human-readable reason in its UI / log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum YellowReason {
    /// Agent hasn't declared an intent. The hook evaluator nudges
    /// the agent toward declaring rather than editing blind.
    NoIntentDeclared,
    /// Target (path or symbol) is outside the agent's declared
    /// scope. Proceeding is allowed but flagged.
    OutsideDeclaredScope {
        declared: Vec<String>,
        attempted: String,
    },
    /// A peer intent is structurally close to the target — within
    /// `distance` graph hops — but the peer hasn't claimed it.
    PeerIntentNearby { distance: u32, peer: AgentId },
    /// A peer is actively reading the same file right now. The
    /// agent may want to coordinate to avoid a mid-edit Read.
    PeerIsReading { peer: AgentId, file: String },
}

/// Why a red. Distinct from yellow so the agent's UI can render a
/// stop-state differently from a caution-state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RedReason {
    /// Another agent holds an exclusive lease on this exact
    /// path/symbol. The agent must release the lease or wait for
    /// expiry before editing.
    ExclusiveClaimHeld {
        holder: AgentId,
        /// The intent of the holder's claim (read vs edit). Reads
        /// never block, so a holder with `Read` would have made
        /// this a yellow instead.
        intent: ClaimIntent,
    },
}

/// Related-activity pointer. Surfaces in the YELLOW `related` array
/// so the agent can render "Codex is also working on this" without
/// a second round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelatedActivity {
    pub agent_id: AgentId,
    /// Best-effort free-form summary. The current implementation
    /// always populates `goal` (from the peer's intent); `scopes`
    /// is included when present.
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// Inputs the evaluator needs. Bundled so the call site is one
/// argument rather than six.
pub struct EvalContext<'a> {
    /// The agent's current intent. `None` when the agent has not
    /// declared. Always produces a non-green level.
    pub intent: Option<&'a Intent>,
    /// Path or symbol the agent is about to edit. Canonicalized
    /// upstream.
    pub target: &'a str,
    /// Every other agent's intent. Excludes the calling agent so
    /// the agent doesn't conflict with itself.
    pub peer_intents: Vec<&'a Intent>,
    /// Active claims from `OccupancyMap.list_all()` (or a
    /// pre-filtered subset). Excludes claims by the calling agent.
    pub peer_claims: Vec<&'a Claim>,
    /// Set of (agent_id, file) pairs the agent is currently
    /// reading — derived from the activity tracker in PR 2. The
    /// evaluator treats a peer "currently reading" the target file
    /// as a yellow signal.
    pub peer_reading: HashSet<(AgentId, String)>,
    /// Optional reference to the static graph for symbol-scope
    /// distance refinement. When `Some`, scope entries that look
    /// like symbol forms (`auth::validate_token`) are resolved
    /// through the graph's name index and distance is computed via
    /// BFS over `Calls` edges. Path scopes still fall back to the
    /// lexical `path_distance` heuristic. When `None`, the original
    /// PR 3 first-pass behavior is used for both.
    pub graph: Option<&'a crate::graph::GraphDatabase>,
}

/// Run the evaluation. The function is total (never panics) and
/// deterministic for given inputs.
pub fn evaluate(ctx: EvalContext<'_>) -> CoordinationLevel {
    // (1) No intent → YELLOW NoIntentDeclared. An agent editing
    // without declaring intent is a coordination regression; the
    // hook must surface that so the agent declares before
    // continuing.
    let Some(intent) = ctx.intent else {
        return CoordinationLevel::Yellow {
            reason: YellowReason::NoIntentDeclared,
            related: Vec::new(),
        };
    };

    // (2) Target outside declared scope → YELLOW OutsideDeclaredScope.
    // The agent may proceed under caution (the declaration isn't a
    // hard boundary) but should know it crossed it.
    let target_in_scope = intent.scopes.iter().any(|s| s == ctx.target)
        || intent.scopes.iter().any(|s| scope_covers(s, ctx.target));
    if !target_in_scope {
        return CoordinationLevel::Yellow {
            reason: YellowReason::OutsideDeclaredScope {
                declared: intent.scopes.clone(),
                attempted: ctx.target.to_string(),
            },
            related: Vec::new(),
        };
    }

    // (3) Peer holds an exclusive claim → RED. The claim primitive
    // is authoritative (file-lock fail-closed per the prior
    // coordination plan); a peer holding it means the in-memory
    // registry and the on-disk lock agree, so a stop is the only
    // honest answer.
    for claim in ctx.peer_claims {
        if claim.path.to_string_lossy() == ctx.target
            || claim.symbols.iter().any(|s| s == ctx.target)
        {
            // Only edit-intent claims block; a peer's read claim
            // never blocks. (Wishlist #5 / presence consistency
            // split: ownership is exclusive, reads are observational.)
            if matches!(claim.intent, ClaimIntent::Edit) {
                return CoordinationLevel::Red {
                    reason: RedReason::ExclusiveClaimHeld {
                        holder: claim.agent_id.clone(),
                        intent: claim.intent.clone(),
                    },
                    related: Vec::new(),
                };
            }
        }
    }

    // (4) Peer intent overlaps structurally — same path / symbol
    // scope, or graph-distance 1–2. Symbol scopes route through
    // the static graph (BFS over Calls edges) when a graph
    // reference is available; path scopes still use the lexical
    // `path_distance` heuristic. Distance 0 = exact match,
    // distance 1 = parent/child path OR direct call-graph edge,
    // distance 2 = sibling path OR two-hop call-graph neighbor.
    let mut nearest_distance: Option<u32> = None;
    let mut nearest_peer: Option<&Intent> = None;
    let mut related: Vec<RelatedActivity> = Vec::new();
    for peer in &ctx.peer_intents {
        let distance = scope_distance(&intent.scopes, &peer.scopes, ctx.graph);
        if let Some(d) = distance {
            if nearest_distance.is_none() || Some(d) < nearest_distance {
                nearest_distance = Some(d);
                nearest_peer = Some(peer);
            }
            related.push(RelatedActivity {
                agent_id: peer.agent_id.clone(),
                goal: peer.goal.clone(),
                scopes: peer.scopes.clone(),
            });
        }
    }
    if let (Some(d), Some(peer)) = (nearest_distance, nearest_peer) {
        if d == 0 {
            // Same scope on both sides but the claim check above
            // already returned (the peer's claim would have hit if
            // held). So a distance-0 here means "the peer has
            // declared the same scope but not claimed it yet" —
            // that's a yellow, not a red.
            return CoordinationLevel::Yellow {
                reason: YellowReason::PeerIntentNearby {
                    distance: d,
                    peer: peer.agent_id.clone(),
                },
                related,
            };
        }
        if d <= 2 {
            return CoordinationLevel::Yellow {
                reason: YellowReason::PeerIntentNearby {
                    distance: d,
                    peer: peer.agent_id.clone(),
                },
                related,
            };
        }
    }

    // (5) Peer is currently reading the same file. This is the
    // last check because peer intent > peer activity in importance
    // — a yellow-nearby is more useful than a peer-reading.
    let target_path = ctx.target.to_string();
    for (peer_id, file) in &ctx.peer_reading {
        if file == &target_path {
            return CoordinationLevel::Yellow {
                reason: YellowReason::PeerIsReading {
                    peer: peer_id.clone(),
                    file: file.clone(),
                },
                related: Vec::new(),
            };
        }
    }

    // (6) Otherwise green.
    CoordinationLevel::Green {
        related: Vec::new(),
    }
}

/// A scope entry can be a path form (`src/auth.rs`) or a symbol form
/// (`auth::validate_token`). For PR 3's path-level evaluation, a
/// symbol scope "covers" a target only on exact match; path
/// scopes cover the same path.
fn scope_covers(scope: &str, target: &str) -> bool {
    scope == target
}

/// Minimum "distance" between two scope lists. Returns `None`
/// when the lists are disjoint (no overlap, no cover). The first
/// pass uses path / symbol equality — distance 0 is "same scope",
/// distance 1 is "scope is a parent path of the other", distance
/// 2 is "scope is a sibling path under the same parent". PR 3's
/// static-graph refinement (added 2026-09-21) routes symbol-scope
/// entries through `GraphDatabase::find_nodes_by_name` and computes
/// BFS distance over `Calls` edges; path-scope entries still fall
/// back to the lexical `path_distance` heuristic.
fn scope_distance(
    a: &[String],
    b: &[String],
    graph: Option<&crate::graph::GraphDatabase>,
) -> Option<u32> {
    let mut best: Option<u32> = None;
    for sa in a {
        for sb in b {
            if sa == sb {
                return Some(0);
            }
            let d = if let Some(g) = graph {
                symbol_distance_via_graph(g, sa, sb).or_else(|| path_distance(sa, sb))
            } else {
                path_distance(sa, sb)
            };
            if let Some(d) = d {
                best = Some(best.map_or(d, |cur| cur.min(d)));
            }
        }
    }
    best
}

/// BFS distance between two symbol-form scopes over `Calls`
/// edges. Returns `Some(d)` with the shortest hop count when both
/// scopes resolve to graph nodes and at least one path exists,
/// `None` otherwise (so the caller can fall back to lexical
/// `path_distance`).
fn symbol_distance_via_graph(graph: &crate::graph::GraphDatabase, a: &str, b: &str) -> Option<u32> {
    let a_nodes = graph.find_all_nodes_by_name(a);
    let b_nodes = graph.find_all_nodes_by_name(b);
    if a_nodes.is_empty() || b_nodes.is_empty() {
        return None;
    }
    // BFS from each `a` node, stopping when we hit any `b` node or
    // exhaust the frontier. We bound the search at 32 hops so a
    // pathological dense graph can't run forever.
    let b_set: std::collections::HashSet<String> = b_nodes.into_iter().map(|n| n.name).collect();
    let mut visited: std::collections::HashSet<String> =
        a_nodes.iter().map(|n| n.name.clone()).collect();
    let mut frontier: std::collections::VecDeque<(usize, String)> =
        a_nodes.into_iter().map(|n| (0usize, n.name)).collect();
    while let Some((d, node)) = frontier.pop_front() {
        if b_set.contains(&node) {
            return Some(d as u32);
        }
        if d >= 32 {
            return None;
        }
        for edge in graph.all_edges() {
            if edge.source_id == node && visited.insert(edge.target_id.clone()) {
                frontier.push_back((d + 1, edge.target_id));
            }
        }
    }
    None
}

/// Distance between two file paths: 1 when one is a parent of the
/// other, 2 when they share a parent directory. `None` otherwise.
/// PR 3's first pass treats file paths lexically; a follow-up
/// could index against the workspace root.
fn path_distance(a: &str, b: &str) -> Option<u32> {
    if a == b {
        return Some(0);
    }
    let a_parts: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let b_parts: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
    let common = a_parts
        .iter()
        .zip(b_parts.iter())
        .take_while(|(x, y)| x == y)
        .count();
    if common == 0 {
        // Sibling paths with no common ancestor — too far for the
        // PR 3 first pass to flag.
        return None;
    }
    if common == a_parts.len() && common < b_parts.len() {
        // `a` is a parent of `b`.
        return Some(1);
    }
    if common == b_parts.len() && common < a_parts.len() {
        // `b` is a parent of `a`.
        return Some(1);
    }
    if common < a_parts.len() && common < b_parts.len() {
        // Common ancestor directory; both descend from it.
        return Some(2);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::intent::{Intent, IntentId, IntentStatus};
    use crate::server::presence::AgentId;
    use std::path::PathBuf;

    fn intent_of(agent: &str, scopes: Vec<&str>) -> Intent {
        let scopes: Vec<String> = scopes.into_iter().map(String::from).collect();
        Intent {
            id: IntentId("I-test".into()),
            agent_id: AgentId(agent.into()),
            goal: format!("{agent}'s task"),
            scopes,
            status: IntentStatus::Editing,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        }
    }

    fn claim_by(agent: &str, path: &str, intent: ClaimIntent) -> Claim {
        Claim {
            agent_id: AgentId(agent.into()),
            path: PathBuf::from(path),
            symbols: vec![],
            content_hash: None,
            intent,
            claimed_at: SystemTime::now(),
            last_touched_unix: SystemTime::now(),
            expires_at: None,
            plan_revision: None,
            inferred: false,
        }
    }

    use std::time::SystemTime;

    /// No intent declared → YELLOW NoIntentDeclared, regardless of
    /// other inputs. This is the "agent editing blind" signal the
    /// hook uses to nudge toward declaring.
    #[test]
    fn no_intent_yields_yellow_no_intent_declared() {
        let ctx = EvalContext {
            intent: None,
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::NoIntentDeclared,
                ..
            }
        ));
    }

    /// Target inside declared scope, no peer activity → GREEN.
    /// The happy path; the test pins that an agent with a clean
    /// declaration gets the proceed-without-caution signal.
    #[test]
    fn declared_target_no_peers_yields_green() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert_eq!(
            evaluate(ctx),
            CoordinationLevel::Green {
                related: Vec::new()
            }
        );
    }

    /// Target outside declared scope → YELLOW OutsideDeclaredScope.
    /// The agent may proceed but is told.
    #[test]
    fn target_outside_declared_scope_yields_yellow() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/session.rs",
            peer_intents: vec![],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::OutsideDeclaredScope { .. },
                ..
            }
        ));
    }

    /// Peer holds an exclusive edit claim on the target → RED.
    /// This is the linearizability invariant from the prior
    /// coordination plan: at most one live exclusive lease per
    /// overlapping scope.
    #[test]
    fn peer_holds_exclusive_edit_claim_yields_red() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let claim = claim_by("codex", "src/auth.rs", ClaimIntent::Edit);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![&claim],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Red {
                reason: RedReason::ExclusiveClaimHeld { .. },
                ..
            }
        ));
    }

    /// Peer holds a read claim → not RED. Reads are observational;
    /// only edit claims block. (Wishlist #5 split.)
    #[test]
    fn peer_holds_read_claim_yields_green() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let claim = claim_by("codex", "src/auth.rs", ClaimIntent::Read);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![&claim],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert_eq!(
            evaluate(ctx),
            CoordinationLevel::Green {
                related: Vec::new()
            }
        );
    }

    /// Peer intent overlaps (distance 0) without claim → YELLOW
    /// PeerIntentNearby distance 0. Both agents declared the same
    /// scope; neither has claimed yet. The hook nudges both toward
    /// coordination before one of them edits.
    #[test]
    fn peer_intent_distance_zero_yields_yellow() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let peer = intent_of("codex", vec!["src/auth.rs"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::PeerIntentNearby { distance: 0, .. },
                ..
            }
        ));
    }

    /// Peer intent is structurally close (distance 2, sibling path
    /// under the same parent directory) → YELLOW PeerIntentNearby
    /// distance 2. The agent sees "codex is editing the same
    /// parent directory" and can choose to wait or proceed under
    /// caution. Distance 1 is reserved for parent/child paths
    /// (one scope is a prefix of the other), tested separately
    /// below.
    #[test]
    fn peer_intent_sibling_path_yields_yellow_distance_two() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let peer = intent_of("codex", vec!["src/session.rs"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::PeerIntentNearby { distance: 2, .. },
                ..
            }
        ));
    }

    /// PR 3 graph-distance refinement: when a graph reference is
    /// passed and the two scopes resolve to connected nodes, the
    /// BFS distance wins over the lexical path heuristic. Without
    /// a graph, the lexical result is used (covered by the existing
    /// parent-directory / sibling tests above).
    #[test]
    fn graph_distance_wins_over_lexical_when_graph_supplied() {
        use crate::graph::GraphDatabase;
        use crate::schema::{EdgeType, GraphEdge, GraphNode};
        let dir = tempfile::tempdir().unwrap();
        let db = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        // Build a tiny call graph: a -> b -> c. Each node's id is the
        // graph key (used by `upsert_edge` for source/target lookup),
        // so we set id = name to match.
        let mk = |id: &str, path: &str, line: u32| -> GraphNode {
            let mut n = GraphNode::new(crate::schema::NodeType::Function, id.into(), path.into());
            n.id = id.into();
            n.line_start = Some(line);
            n
        };
        let na = mk("auth::a", "src/auth.rs", 1);
        let nb = mk("auth::b", "src/auth.rs", 10);
        let nc = mk("other::c", "src/other.rs", 1);
        db.upsert_node(na).unwrap();
        db.upsert_node(nb).unwrap();
        db.upsert_node(nc).unwrap();
        db.upsert_edge(GraphEdge::new(
            EdgeType::Calls,
            "auth::a".into(),
            "auth::b".into(),
        ))
        .unwrap();
        db.upsert_edge(GraphEdge::new(
            EdgeType::Calls,
            "auth::b".into(),
            "other::c".into(),
        ))
        .unwrap();
        let intent = intent_of("alice", vec!["auth::a".into()]);
        let peer = intent_of("codex", vec!["other::c".into()]);
        let ctx_no_graph = EvalContext {
            intent: Some(&intent),
            target: "auth::a",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        let ctx_with_graph = EvalContext {
            intent: Some(&intent),
            target: "auth::a",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: Some(&db),
        };
        // Without a graph, scope_distance returns None for the two
        // different symbol scopes, so the peer is not flagged.
        let level_no_graph = evaluate(ctx_no_graph);
        assert!(
            matches!(level_no_graph, CoordinationLevel::Green { .. }),
            "no graph: scope_distance returns None for disjoint symbols; \
             expected Green, got {level_no_graph:?}"
        );
        // With a graph, the BFS finds a 2-hop path a->b->c, so the
        // peer is flagged as YELLOW PeerIntentNearby distance 2.
        let level_with_graph = evaluate(ctx_with_graph);
        assert!(
            matches!(
                level_with_graph,
                CoordinationLevel::Yellow {
                    reason: YellowReason::PeerIntentNearby { distance: 2, .. },
                    ..
                }
            ),
            "with graph: 2-hop BFS path; expected Yellow distance 2, \
             got {level_with_graph:?}"
        );
    }

    /// Peer intent is the parent directory (distance 1, prefix
    /// relation) → YELLOW PeerIntentNearby distance 1. The agent
    /// sees "codex owns the whole directory" — a stronger signal
    /// than a sibling.
    #[test]
    fn peer_intent_parent_directory_yields_yellow_distance_one() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let peer = intent_of("codex", vec!["src"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::PeerIntentNearby { distance: 1, .. },
                ..
            }
        ));
    }

    /// Peer is currently reading the target file → YELLOW
    /// PeerIsReading. Distinct from PeerIntentNearby because the
    /// peer's *activity* is more actionable than their declared
    /// scope (they may be about to edit).
    #[test]
    fn peer_is_reading_target_yields_yellow() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let mut peer_reading = HashSet::new();
        peer_reading.insert((AgentId("codex".into()), "src/auth.rs".into()));
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![],
            peer_reading,
            graph: None,
        };
        assert!(matches!(
            evaluate(ctx),
            CoordinationLevel::Yellow {
                reason: YellowReason::PeerIsReading { .. },
                ..
            }
        ));
    }

    /// Disjoint peer intent and no other signals → GREEN. A peer
    /// working on an unrelated file shouldn't flag this agent's
    /// edit.
    #[test]
    fn disjoint_peer_intent_yields_green() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let peer = intent_of("codex", vec!["docs/readme.md"]);
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![&peer],
            peer_claims: vec![],
            peer_reading: HashSet::new(),
            graph: None,
        };
        assert_eq!(
            evaluate(ctx),
            CoordinationLevel::Green {
                related: Vec::new()
            }
        );
    }

    /// RED takes precedence over peer reading. If a peer holds an
    /// exclusive claim AND is reading, the answer is RED — the
    /// claim is the authoritative signal.
    #[test]
    fn red_takes_precedence_over_peer_reading() {
        let intent = intent_of("alice", vec!["src/auth.rs"]);
        let claim = claim_by("codex", "src/auth.rs", ClaimIntent::Edit);
        let mut peer_reading = HashSet::new();
        peer_reading.insert((AgentId("codex".into()), "src/auth.rs".into()));
        let ctx = EvalContext {
            intent: Some(&intent),
            target: "src/auth.rs",
            peer_intents: vec![],
            peer_claims: vec![&claim],
            peer_reading,
            graph: None,
        };
        assert!(matches!(evaluate(ctx), CoordinationLevel::Red { .. }));
    }
}
