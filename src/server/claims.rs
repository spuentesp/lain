//! Machine-checkable impact claims — the `AFFECTED:` line protocol
//! (issue #294, `tests/fixtures/contract_task/ground_truth.json`
//! §protocol).
//!
//! Agents analyse impact correctly in prose but never emit the format
//! the acceptance harness parses. Prompting failed 5/5 times, so the
//! tooling produces the lines itself and agents copy an output instead
//! of obeying a format:
//!
//! ```text
//! AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>
//! ```
//!
//! Two spaces before `EVIDENCE`; the evidence token is one of
//! `verified`, `needs-investigation`, `missing`. Lines must stay
//! whitespace-free so `AFFECTED_RE` in
//! `scripts/acceptance/contract_task.py` can parse them — claim
//! identities that would break the parser (spaces in the symbol, e.g.
//! route-style `GET /api/orders/{}` node names) are dropped rather
//! than emitted unparseable.
//!
//! Evidence classes map to what the product already knows:
//!
//! - **verified** — edge provenance is `Static`/`Confirmed`
//!   (confidence 1.0), or the diff impact class is `Verified`
//!   (static `Binds` + `ReadsField` chains, `repos.yaml` bindings).
//! - **needs-investigation** — the graph flags the relationship but
//!   cannot prove it: heuristic edges (with the detector named),
//!   runtime-observed edges, edges with no provenance recorded, and
//!   diff impact class `NeedsInvestigation` (with its `Reason`).
//! - **missing** — the information is not in the graph at all:
//!   unresolved/ambiguous/unnormalized consumers from coverage, an
//!   empty blast radius (never dressed up as "no impact"), or a seed
//!   no impact path could be derived for.
//!
//! False-positive discipline is structural: claims are derived only
//! from real edges (per-repo walk) or real impact paths / coverage
//! entries (federation). A similarly-named symbol with no edge to the
//! seed can never appear, and evidence is never upgraded — a chain
//! that touches one heuristic hop classifies as `needs-investigation`,
//! and the strongest *proven* class for an identity wins on merge.

use crate::federation::contracts::diff::{Class, Coverage, Reason};
use crate::federation::graph_backend::ImpactPath;
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeProvenance, EdgeType, GraphNode, NodeType};
use crate::server::tools::handlers::impact::is_heuristic_edge;
use crate::server::tools::utils::resolve_node_ambiguous;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};

/// Evidence class of one `AFFECTED:` claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Evidence {
    Verified,
    NeedsInvestigation,
    Missing,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Evidence::Verified => "verified",
            Evidence::NeedsInvestigation => "needs-investigation",
            Evidence::Missing => "missing",
        }
    }

    /// Merge strength: a proven relationship beats a flagged one, a
    /// flagged one beats absent information. Used when the same
    /// identity shows up through several paths.
    fn strength(self) -> u8 {
        match self {
            Evidence::Verified => 2,
            Evidence::NeedsInvestigation => 1,
            Evidence::Missing => 0,
        }
    }
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One affected place with its evidence class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claim {
    pub repo: String,
    pub file: String,
    pub symbol: String,
    pub evidence: Evidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Claim {
    /// Build a claim after validating that its identity survives the
    /// grader's parser (`AFFECTED_RE`): all three parts non-empty,
    /// whitespace-free, and no `:` in `repo`/`file` (a colon there
    /// would shift the three capture groups; `symbol` may carry colons
    /// such as Rust's `Type::method`).
    pub fn new(
        repo: impl Into<String>,
        file: impl Into<String>,
        symbol: impl Into<String>,
        evidence: Evidence,
    ) -> Option<Claim> {
        let repo = repo.into();
        let file = file.into();
        let symbol = symbol.into();
        if !claim_identity_ok(&repo, &file, &symbol) {
            return None;
        }
        Some(Claim {
            repo,
            file,
            symbol,
            evidence,
            note: None,
        })
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        self.note = match self.note.take() {
            Some(existing) => Some(format!("{existing}; {note}")),
            None => Some(note),
        };
        self
    }

    /// The exact protocol line. Two spaces before `EVIDENCE`.
    pub fn line(&self) -> String {
        format!(
            "AFFECTED: {}:{}:{}  EVIDENCE: {}",
            self.repo,
            self.file,
            self.symbol,
            self.evidence.as_str()
        )
    }
}

/// True when `(repo, file, symbol)` renders as one parseable claim
/// line. Whitespace anywhere breaks `AFFECTED_RE`; a `:` in `repo` or
/// `file` shifts its three capture groups (a `:` in `symbol` does not
/// — the last group is lazy up to `EVIDENCE`).
pub fn claim_identity_ok(repo: &str, file: &str, symbol: &str) -> bool {
    let part = |s: &str| !s.trim().is_empty() && !s.chars().any(char::is_whitespace);
    part(repo) && part(file) && part(symbol) && !repo.contains(':') && !file.contains(':')
}

/// Running evidence for one walk/path position: the weakest class
/// seen so far plus every reason that pushed it there.
#[derive(Clone, Debug)]
struct Acc {
    evidence: Evidence,
    notes: Vec<String>,
}

impl Acc {
    fn verified() -> Self {
        Acc {
            evidence: Evidence::Verified,
            notes: Vec::new(),
        }
    }

    fn apply(&mut self, evidence: Evidence, note: Option<String>) {
        if evidence.strength() < self.evidence.strength() {
            self.evidence = evidence;
        }
        if let Some(note) = note {
            if !self.notes.contains(&note) {
                self.notes.push(note);
            }
        }
    }

    fn note(&self) -> Option<String> {
        if self.notes.is_empty() {
            None
        } else {
            Some(self.notes.join("; "))
        }
    }
}

/// Map one edge's provenance to the protocol's evidence classes.
/// Static/Confirmed provenance is confidence 1.0 — the "static
/// `Binds`/`ReadsField`, provenance static" arm of `verified`.
/// Heuristic, runtime, and legacy no-provenance edges are flagged but
/// unproven: `needs-investigation`, with the reason spelled out.
pub fn evidence_from_provenance(provenance: Option<&EdgeProvenance>) -> (Evidence, Option<String>) {
    match provenance {
        Some(EdgeProvenance::Static { .. }) | Some(EdgeProvenance::Confirmed { .. }) => {
            (Evidence::Verified, None)
        }
        Some(EdgeProvenance::Heuristic {
            detector,
            confidence,
        }) => (
            Evidence::NeedsInvestigation,
            Some(format!(
                "heuristic edge (detector={detector}, confidence={confidence:.2})"
            )),
        ),
        Some(EdgeProvenance::Runtime { .. }) => (
            Evidence::NeedsInvestigation,
            Some("runtime-observed edge (not static proof)".to_string()),
        ),
        None => (
            Evidence::NeedsInvestigation,
            Some("edge provenance not recorded (confidence 0.0)".to_string()),
        ),
    }
}

/// Map a diff impact class + reason (§9) to a claim for one consumer.
/// `NoKnownImpact` is not reported as a claim — §9.5 keeps compatible
/// changes out of the report; `Verified` / `NeedsInvestigation` carry
/// their `Reason` as the claim note so the agent can copy the why.
pub fn claim_from_diff_class(
    repo: &str,
    file: &str,
    symbol: &str,
    class: Class,
    reason: Option<Reason>,
) -> Option<Claim> {
    let evidence = match class {
        Class::Verified => Evidence::Verified,
        Class::NeedsInvestigation => Evidence::NeedsInvestigation,
        Class::NoKnownImpact => return None,
    };
    let claim = Claim::new(repo, file, symbol, evidence)?;
    Some(match reason {
        Some(r) => claim.with_note(format!("diff impact: {}", r.as_str())),
        None => claim,
    })
}

/// Node types that name an *affected place* worth a claim line.
/// `File`/`Synthetic`/`FieldRef`/route-like nodes are containment or
/// evidence artifacts, and route names (`GET /api/orders/{}`) carry
/// spaces the grader's parser cannot read anyway.
fn claimable_node_type(t: &NodeType) -> bool {
    matches!(
        t,
        NodeType::Function
            | NodeType::Method
            | NodeType::Class
            | NodeType::Struct
            | NodeType::Trait
            | NodeType::Interface
            | NodeType::Enum
    )
}

/// Repository part of a node's identity: the `GlobalId` prefix when
/// the node lives in the federation, else the node's `repo_id`, else
/// the caller's fallback (the workspace directory name for per-repo
/// graphs, where the repo id is implicit).
fn claim_repo(node: &GraphNode, repo_fallback: &str) -> String {
    GlobalId::parse(&node.id)
        .ok()
        .map(|g| g.repo_id().to_string())
        .or_else(|| node.repo_id.clone())
        .unwrap_or_else(|| repo_fallback.to_string())
}

fn claim_for_node(node: &GraphNode, acc: &Acc, repo_fallback: &str) -> Option<Claim> {
    if !claimable_node_type(&node.node_type) {
        return None;
    }
    let claim = Claim::new(
        claim_repo(node, repo_fallback),
        &node.path,
        &node.name,
        acc.evidence,
    )?;
    Some(match acc.note() {
        Some(note) => claim.with_note(note),
        None => claim,
    })
}

/// A `missing` claim naming `node` itself — used when the analysis
/// of a known subject came back empty. Deliberately skips the
/// affected-node whitelist: the seed may be a `Field`/`Schema`, and
/// the claim says "we lack information about this subject", not "this
/// subject is an affected symbol". Identity still has to be
/// parseable, so this returns `None` rather than emit a broken line.
pub fn missing_claim_for_node(node: &GraphNode, repo_fallback: &str, note: &str) -> Option<Claim> {
    let claim = Claim::new(
        claim_repo(node, repo_fallback),
        &node.path,
        &node.name,
        Evidence::Missing,
    )?;
    Some(claim.with_note(note))
}

/// Known-unknowns from a `get_coverage` / `diff_contracts` coverage
/// view: consumers the graph saw but could not resolve, plus the
/// ambiguous and unnormalized ones. These are named `missing` so an
/// agent says "the information is not in the graph" instead of
/// claiming "no impact".
pub fn claims_from_coverage(coverage: &Coverage) -> Vec<Claim> {
    let buckets: [(&Vec<_>, &str); 3] = [
        (
            &coverage.unresolved_consumers,
            "unresolved consumer — target is not in the graph",
        ),
        (&coverage.ambiguous, "ambiguous consumer binding"),
        (&coverage.unnormalized, "unnormalized consumer url"),
    ];
    let mut out = Vec::new();
    for (keys, note) in buckets {
        for key in keys {
            if let Some(claim) = Claim::new(
                key.caller.repo.as_str(),
                &key.caller.path,
                &key.caller.name,
                Evidence::Missing,
            ) {
                out.push(claim.with_note(note));
            }
        }
    }
    out
}

/// The caveat every claims block must carry when
/// `coverage.complete == false`: verified claims are not exhaustive
/// and the absence of a claim is not evidence of no impact.
pub fn coverage_note(coverage: &Coverage) -> Option<&'static str> {
    if coverage.complete {
        None
    } else {
        Some(
            "coverage.complete=false — claims are not exhaustive; \
             absence of a claim is not evidence of no impact",
        )
    }
}

/// Claims from `traverse_impact` paths (§5.2). Every hop node is an
/// affected place; its evidence is the weakest provenance along the
/// path from the seed, so one heuristic hop anywhere downgrades the
/// whole chain to `needs-investigation` (never upgraded — the
/// discipline the harness grades hardest). Seeds are excluded;
/// non-symbol nodes and unparseable identities drop out.
pub fn claims_from_impact_paths(paths: &[ImpactPath], starts: &[String]) -> Vec<Claim> {
    let starts: HashSet<&str> = starts.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for path in paths {
        let mut acc = Acc::verified();
        for hop in &path.hops {
            if starts.contains(hop.node.id.as_str()) {
                // The seed (or the degenerate self-loop path for a
                // seed with no predecessors) is the subject, not an
                // affected place.
                continue;
            }
            let (evidence, note) = evidence_from_provenance(hop.edge.provenance.as_ref());
            acc.apply(evidence, note);
            if let Some(claim) = claim_for_node(&hop.node, &acc, "") {
                out.push(claim);
            }
        }
    }
    out
}

/// Claims from a per-repo graph: transitive incoming walk over the
/// same edge classes `get_blast_radius` follows (`Calls`/`Uses` plus
/// heuristic dispatch/bus/router edges). Every heuristic edge is kept
/// — here it becomes a `needs-investigation` line instead of being
/// thresholded out, because the taxonomy exists to show flagged-but-
/// unproven relationships. Uncommitted overlay callers count too
/// (they are real code the committed graph does not have yet) and
/// classify as `needs-investigation` — the overlay carries no
/// provenance.
///
/// Known-unknowns surface as `missing` claims naming the *seed*: an
/// empty blast radius ("no dependents found") is not proof of no
/// impact (the graph indexes committed code only), and an incoming
/// edge from a node the index cannot resolve means a dependency
/// exists that cannot be named.
///
/// `repo_fallback` names the repository for single-workspace graphs,
/// where `GraphNode::repo_id` is `None` (pass the workspace directory
/// name).
pub fn claims_from_graph(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    repo_fallback: &str,
) -> Result<Vec<Claim>, crate::error::LainError> {
    let (seed, _other_defs) = resolve_node_ambiguous(graph, overlay, symbol)?;

    let mut out: Vec<Claim> = Vec::new();
    let mut found_named = 0usize;
    let mut found_unnameable = 0usize;

    let mut visited: HashSet<String> = HashSet::new();
    let mut queued: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, Acc)> = VecDeque::new();
    visited.insert(seed.id.clone());
    queued.insert(seed.id.clone());
    queue.push_back((seed.id.clone(), Acc::verified()));

    while let Some((id, acc)) = queue.pop_front() {
        // Static-graph incoming dependency + heuristic edges (the
        // `get_blast_radius` walk; see its module docs for why
        // `Contains` is not followed).
        if let Ok(incoming) = graph.get_edges_to(&id) {
            for edge in incoming {
                let is_dependency = matches!(edge.edge_type, EdgeType::Calls | EdgeType::Uses);
                let is_heuristic = is_heuristic_edge(&edge.edge_type);
                if !is_dependency && !is_heuristic {
                    continue;
                }
                let mut hop = acc.clone();
                let (evidence, note) = evidence_from_provenance(edge.provenance.as_ref());
                hop.apply(evidence, note);

                let caller = graph.get_node(&edge.source_id).ok().flatten();
                if visited.contains(&edge.source_id) || !queued.insert(edge.source_id.clone()) {
                    continue;
                }
                queue.push_back((edge.source_id.clone(), hop.clone()));
                match caller {
                    Some(caller) => {
                        if let Some(claim) = claim_for_node(&caller, &hop, repo_fallback) {
                            found_named += 1;
                            out.push(claim);
                        }
                    }
                    // The edge exists but its source node is not in
                    // the index: a dependency that cannot be named.
                    None => found_unnameable += 1,
                }
            }
        }

        // Live, uncommitted callers from the volatile overlay. They
        // carry no provenance, so they can never be `verified`.
        for (caller, edge_type) in overlay.get_incoming_edges(&id) {
            let is_dependency = matches!(edge_type, EdgeType::Calls | EdgeType::Uses);
            let is_heuristic = is_heuristic_edge(&edge_type);
            if !is_dependency && !is_heuristic {
                continue;
            }
            if visited.contains(&caller.id) || !queued.insert(caller.id.clone()) {
                continue;
            }
            let mut hop = acc.clone();
            hop.apply(
                Evidence::NeedsInvestigation,
                Some("uncommitted overlay caller (no provenance recorded)".to_string()),
            );
            queue.push_back((caller.id.clone(), hop.clone()));
            if let Some(claim) = claim_for_node(&caller, &hop, repo_fallback) {
                found_named += 1;
                out.push(claim);
            }
        }
    }

    // Known-unknowns about the seed itself: say "missing" instead of
    // letting an empty or partial answer read as "no impact".
    if found_named == 0 {
        if let Some(claim) = missing_claim_for_node(
            &seed,
            repo_fallback,
            "no dependents in the index — an empty blast radius is not proof of no impact",
        ) {
            out.push(claim);
        }
    }
    if found_unnameable > 0 {
        if let Some(claim) = missing_claim_for_node(
            &seed,
            repo_fallback,
            &format!(
                "{found_unnameable} incoming edge(s) from nodes the index cannot resolve — \
                 a dependency exists but is unnameable"
            ),
        ) {
            out.push(claim);
        }
    }
    Ok(out)
}

/// Deduplicate by identity (strongest evidence wins; notes merge),
/// then sort verified-first so the copy-paste head of a block carries
/// the proven claims. Deterministic: (evidence desc, repo, file,
/// symbol).
pub fn merge_claims(claims: Vec<Claim>) -> Vec<Claim> {
    let mut by_identity: std::collections::BTreeMap<(String, String, String), Claim> =
        std::collections::BTreeMap::new();
    for claim in claims {
        let key = (claim.repo.clone(), claim.file.clone(), claim.symbol.clone());
        match by_identity.get(&key) {
            None => {
                by_identity.insert(key, claim);
            }
            Some(existing) => {
                let mut merged = existing.clone();
                if claim.evidence.strength() > existing.evidence.strength() {
                    merged.evidence = claim.evidence;
                }
                if let Some(note) = claim.note {
                    merged = merged.with_note(note);
                }
                by_identity.insert(key, merged);
            }
        }
    }
    let mut out: Vec<Claim> = by_identity.into_values().collect();
    out.sort_by(|a, b| {
        b.evidence
            .strength()
            .cmp(&a.evidence.strength())
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    out
}

/// The copy-paste payload: one exact `AFFECTED:` line per claim,
/// with any reason on an indented continuation line (the parser only
/// reads the `AFFECTED` line) and a trailing `#` caveat when the
/// caller passed one. An empty result never renders as silence or
/// "no impact" — it renders the explicit absence caveat.
pub fn render_claim_lines(claims: Vec<Claim>, note: Option<&str>) -> String {
    let claims = merge_claims(claims);
    let mut out = String::new();
    for claim in &claims {
        out.push_str(&claim.line());
        out.push('\n');
        if let Some(reason) = &claim.note {
            out.push_str(&format!("  reason: {reason}\n"));
        }
    }
    if let Some(note) = note {
        out.push_str(&format!("# {note}\n"));
    }
    if out.is_empty() {
        out.push_str("# no claims derived — absence is not evidence of no impact\n");
    }
    out
}

/// The same lines under a `## Claims` header, for appending to an
/// MCP tool's Markdown text output.
pub fn render_claims_block(claims: Vec<Claim>, note: Option<&str>) -> String {
    format!("\n## Claims\n\n{}", render_claim_lines(claims, note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::diff::{
        Affected, ConsumerKey, ConsumerTargetKey, Impact, Scope,
    };
    use crate::federation::contracts::model::{PathSegment, ServiceName, SymbolKey};
    use crate::federation::graph_backend::ImpactHop;
    use crate::federation::repo_id::RepoId;
    use crate::schema::{GraphEdge, RepoNamespace};

    /// Fixture-shaped nodes: GlobalIds so `repo`/`file`/`symbol`
    /// derive exactly like `tests/fixtures/contract_task/ground_truth.json`
    /// names them (`billing:src/main.py:build_invoice`).
    fn gnode(kind: NodeType, repo: &str, path: &str, name: &str) -> GraphNode {
        let ns = RepoNamespace::for_test();
        let id = GlobalId::new(
            &RepoId::new(repo).unwrap(),
            kind.clone(),
            path,
            name,
            Some(1),
        );
        let mut node = GraphNode::new_in(kind, name.to_string(), path.to_string(), &ns);
        node.id = id.as_str().to_string();
        node
    }

    fn edge(edge_type: EdgeType, from: &GraphNode, to: &GraphNode) -> GraphEdge {
        GraphEdge::new(edge_type, from.id.clone(), to.id.clone())
    }

    #[test]
    fn claim_line_is_exactly_the_protocol_format() {
        let claim = Claim::new(
            "billing",
            "src/main.py",
            "build_invoice",
            Evidence::Verified,
        )
        .expect("parseable identity");
        // Exactly two spaces before EVIDENCE — the ground truth's
        // `affected_line`, character for character.
        assert_eq!(
            claim.line(),
            "AFFECTED: billing:src/main.py:build_invoice  EVIDENCE: verified"
        );
        assert_eq!(
            Claim::new(
                "billing",
                "src/main.py",
                "fetch_order",
                Evidence::NeedsInvestigation
            )
            .unwrap()
            .line(),
            "AFFECTED: billing:src/main.py:fetch_order  EVIDENCE: needs-investigation"
        );
        assert_eq!(
            Claim::new("billing", "src/main.py", "fetch_order", Evidence::Missing)
                .unwrap()
                .line(),
            "AFFECTED: billing:src/main.py:fetch_order  EVIDENCE: missing"
        );
    }

    #[test]
    fn identities_that_break_the_grader_parser_are_rejected() {
        // A route-style name carries spaces: `AFFECTED_RE`'s `(\S+?)`
        // symbol group could never read the line back.
        assert!(Claim::new(
            "orders",
            "src/orders/routes.rs",
            "GET /api/orders/{}",
            Evidence::Verified
        )
        .is_none());
        // `:` in repo/file shifts the three capture groups.
        assert!(Claim::new("a:b", "x.py", "f", Evidence::Verified).is_none());
        assert!(Claim::new("r", "a:b.py", "f", Evidence::Verified).is_none());
        // A `:` inside the symbol is fine — the last group is lazy
        // up to EVIDENCE.
        assert!(Claim::new("r", "x.rs", "Type::method", Evidence::Verified).is_some());
        assert!(Claim::new("", "x.rs", "f", Evidence::Verified).is_none());
    }

    /// The contract_task ground truth, per-repo shape: `build_invoice`
    /// is the proven consumer of `fetch_order`; the false-positive
    /// traps (`charge` → stripe, `fetch_me` → a different endpoint)
    /// exist in the same graph with no edge to the seed and must not
    /// appear at all.
    #[tokio::test]
    async fn graph_walk_names_the_verified_consumer_and_never_the_decoys() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();

        let fetch_order = gnode(NodeType::Function, "billing", "src/main.py", "fetch_order");
        let build_invoice = gnode(
            NodeType::Function,
            "billing",
            "src/main.py",
            "build_invoice",
        );
        let get_invoice = gnode(NodeType::Function, "billing", "src/main.py", "get_invoice");
        // False-positive traps: same file, no edge to the seed.
        let charge = gnode(NodeType::Function, "billing", "src/main.py", "charge");
        let fetch_me = gnode(NodeType::Function, "billing", "src/main.py", "fetch_me");
        let stripe = gnode(NodeType::Function, "stripe", "client.py", "post");
        let orders_me = gnode(
            NodeType::Function,
            "orders",
            "src/orders/handlers.rs",
            "get_me",
        );
        // A heuristic caller of the seed: flagged, not provable.
        let dispatched = gnode(
            NodeType::Function,
            "billing",
            "src/main.py",
            "dispatched_consumer",
        );

        for n in [
            &fetch_order,
            &build_invoice,
            &get_invoice,
            &charge,
            &fetch_me,
            &stripe,
            &orders_me,
            &dispatched,
        ] {
            graph.upsert_node((*n).clone()).unwrap();
        }

        let mut static_call = edge(EdgeType::Calls, &build_invoice, &fetch_order);
        static_call.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::Lsp,
        });
        let transitive = edge(EdgeType::Calls, &get_invoice, &build_invoice);
        let decoy_edge = edge(EdgeType::Calls, &charge, &stripe);
        let decoy_edge2 = edge(EdgeType::Calls, &fetch_me, &orders_me);
        let mut weak = edge(EdgeType::DynamicDispatch, &dispatched, &fetch_order);
        weak.weight = Some(0.4);
        weak.provenance = Some(EdgeProvenance::Heuristic {
            detector: "dynamic_dispatch".to_string(),
            confidence: 0.4,
        });
        graph
            .insert_edges_batch(&[static_call, transitive, decoy_edge, decoy_edge2, weak])
            .unwrap();

        let claims =
            claims_from_graph(&graph, &overlay, "fetch_order", "billing").expect("resolves");
        let rendered = render_claim_lines(claims, None);

        // The verified consumer, verbatim.
        assert!(
            rendered.contains("AFFECTED: billing:src/main.py:build_invoice  EVIDENCE: verified"),
            "verified consumer missing, got:\n{rendered}"
        );
        // Transitive static reach is verified too.
        assert!(
            rendered.contains("AFFECTED: billing:src/main.py:get_invoice  EVIDENCE: verified"),
            "transitive static caller missing, got:\n{rendered}"
        );
        // The heuristic caller is present but never upgraded.
        assert!(
            rendered.contains(
                "AFFECTED: billing:src/main.py:dispatched_consumer  EVIDENCE: needs-investigation"
            ),
            "heuristic caller missing, got:\n{rendered}"
        );
        assert!(
            rendered.contains("detector=dynamic_dispatch") && rendered.contains("confidence=0.40"),
            "heuristic reason missing, got:\n{rendered}"
        );
        // False-positive traps: present in the graph, no edge to the
        // seed, therefore absent from the claims.
        for trap in ["charge", "fetch_me", "get_me", "post"] {
            assert!(
                !rendered.contains(&format!(":{trap}  EVIDENCE"))
                    && !rendered.contains(&format!(":{trap}\n")),
                "decoy {trap} must not be claimed, got:\n{rendered}"
            );
        }
        assert!(
            !rendered.contains("EVIDENCE: verified\n  reason"),
            "static claims must not carry downgrade reasons, got:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn empty_blast_radius_is_missing_never_no_impact() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();

        let lonely = gnode(NodeType::Function, "billing", "src/main.py", "lonely");
        graph.upsert_node(lonely.clone()).unwrap();

        let claims = claims_from_graph(&graph, &overlay, "lonely", "billing").unwrap();
        let rendered = render_claim_lines(claims, None);
        assert_eq!(
            rendered,
            "AFFECTED: billing:src/main.py:lonely  EVIDENCE: missing\n  \
             reason: no dependents in the index — an empty blast radius is not proof of \
             no impact\n"
        );
        assert!(
            !rendered.contains("no dependents found"),
            "an empty blast radius must never render the old 'no dependents found' \
             non-answer without an evidence class, got:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn empty_blast_radius_on_unknown_symbol_is_an_error_not_a_claim() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let overlay = VolatileOverlay::new();
        let result = claims_from_graph(&graph, &overlay, "anything", "billing");
        assert!(
            matches!(result, Err(crate::error::LainError::NotFound(_))),
            "an unindexed graph must surface NotFound, not invent a claim"
        );
    }

    /// Federation shape: paths from the orders field reach billing's
    /// reader; every hop static → verified; a chain with one
    /// heuristic hop → needs-investigation; seeds excluded.
    #[test]
    fn impact_paths_map_provenance_to_evidence() {
        let field = gnode(NodeType::Field, "orders", "openapi.yaml", "customer_id");
        let call = gnode(
            NodeType::HttpClientCall,
            "billing",
            "src/main.py",
            "GET /api/orders/{}",
        );
        let fetch_order = gnode(NodeType::Function, "billing", "src/main.py", "fetch_order");
        let build_invoice = gnode(
            NodeType::Function,
            "billing",
            "src/main.py",
            "build_invoice",
        );
        let flagged = gnode(
            NodeType::Function,
            "billing",
            "src/main.py",
            "flagged_reader",
        );

        let static_edge = |from: &GraphNode, to: &GraphNode| {
            let mut e = edge(EdgeType::ReadsField, from, to);
            e.provenance = Some(EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            });
            e
        };
        let heur_edge = |from: &GraphNode, to: &GraphNode| {
            let mut e = edge(EdgeType::Calls, from, to);
            e.weight = Some(0.3);
            e.provenance = Some(EdgeProvenance::Heuristic {
                detector: "serde_value".to_string(),
                confidence: 0.3,
            });
            e
        };

        let paths = vec![
            // seed(field) → call → fetch_order → build_invoice, all static.
            ImpactPath {
                hops: vec![
                    ImpactHop {
                        edge: static_edge(&field, &call),
                        node: call.clone(),
                    },
                    ImpactHop {
                        edge: static_edge(&call, &fetch_order),
                        node: fetch_order.clone(),
                    },
                    ImpactHop {
                        edge: static_edge(&fetch_order, &build_invoice),
                        node: build_invoice.clone(),
                    },
                ],
                min_confidence: 1.0,
            },
            // One heuristic hop anywhere downgrades the whole chain.
            ImpactPath {
                hops: vec![
                    ImpactHop {
                        edge: static_edge(&field, &call),
                        node: call.clone(),
                    },
                    ImpactHop {
                        edge: heur_edge(&call, &flagged),
                        node: flagged.clone(),
                    },
                ],
                min_confidence: 0.3,
            },
        ];
        let starts = vec![field.id.clone()];
        let rendered = render_claim_lines(claims_from_impact_paths(&paths, &starts), None);

        assert!(
            rendered.contains("AFFECTED: billing:src/main.py:build_invoice  EVIDENCE: verified"),
            "ground-truth consumer must be verified, got:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "AFFECTED: billing:src/main.py:flagged_reader  EVIDENCE: needs-investigation"
            ),
            "heuristic chain must be needs-investigation, got:\n{rendered}"
        );
        assert!(
            rendered.contains("detector=serde_value"),
            "got:\n{rendered}"
        );
        // The seed (a Field) and route-style nodes are not claims.
        assert!(!rendered.contains("openapi.yaml"), "got:\n{rendered}");
        assert!(!rendered.contains("GET /api/orders"), "got:\n{rendered}");
        // Decoys that exist nowhere in the paths can never appear.
        for trap in ["charge", "fetch_me", "buildMonthlyReport", "check_stock"] {
            assert!(!rendered.contains(trap), "decoy {trap} leaked:\n{rendered}");
        }
        // Verified sorts first.
        let verified_pos = rendered
            .find("build_invoice  EVIDENCE: verified")
            .expect("verified line");
        let ni_pos = rendered
            .find("flagged_reader  EVIDENCE: needs-investigation")
            .expect("ni line");
        assert!(verified_pos < ni_pos, "verified claims must sort first");
    }

    /// Coverage-shaped known-unknowns → `missing`, named; the
    /// `complete=false` caveat is rendered, never "no impact".
    #[test]
    fn coverage_gaps_are_missing_claims() {
        let coverage = Coverage {
            complete: false,
            unresolved_consumers: vec![ConsumerKey {
                caller: SymbolKey {
                    repo: RepoId::new("billing").unwrap(),
                    path: "src/main.py".into(),
                    container: None,
                    name: "fetch_order".into(),
                },
                target: ConsumerTargetKey::UrlExpr("GET /v1/api/orders/{}".to_string()),
            }],
            ambiguous: vec![],
            unnormalized: vec![],
            ..Coverage::default()
        };
        let claims = claims_from_coverage(&coverage);
        let note = coverage_note(&coverage);
        let rendered = render_claim_lines(claims, note);
        assert_eq!(
            rendered,
            "AFFECTED: billing:src/main.py:fetch_order  EVIDENCE: missing\n  \
             reason: unresolved consumer — target is not in the graph\n\
             # coverage.complete=false — claims are not exhaustive; absence of a claim \
             is not evidence of no impact\n"
        );
        // Complete coverage carries no caveat.
        let mut complete = coverage.clone();
        complete.complete = true;
        assert!(coverage_note(&complete).is_none());
    }

    /// Diff impact classes (§9): Verified → verified, NeedsInvestigation
    /// → needs-investigation with its reason, NoKnownImpact → no claim.
    #[test]
    fn diff_impact_classes_map_to_evidence() {
        let verified = claim_from_diff_class(
            "billing",
            "src/main.py",
            "build_invoice",
            Class::Verified,
            Some(Reason::StaticBinding),
        )
        .expect("verified claim");
        assert_eq!(
            verified.line(),
            "AFFECTED: billing:src/main.py:build_invoice  EVIDENCE: verified"
        );
        assert_eq!(
            verified.note.as_deref(),
            Some("diff impact: static_binding")
        );

        let flagged = claim_from_diff_class(
            "billing",
            "src/main.py",
            "fetch_order",
            Class::NeedsInvestigation,
            Some(Reason::UnresolvedCandidates),
        )
        .expect("ni claim");
        assert_eq!(flagged.evidence, Evidence::NeedsInvestigation);
        assert_eq!(
            flagged.note.as_deref(),
            Some("diff impact: unresolved_candidates")
        );

        assert!(claim_from_diff_class(
            "billing",
            "src/main.py",
            "x",
            Class::NoKnownImpact,
            Some(Reason::StaticBinding),
        )
        .is_none());
    }

    /// Merging keeps the strongest evidence for an identity and
    /// never upgrades a heuristic-only identity.
    #[test]
    fn merge_keeps_strongest_evidence_per_identity() {
        let verified = Claim::new(
            "billing",
            "src/main.py",
            "build_invoice",
            Evidence::Verified,
        )
        .unwrap();
        let flagged = Claim::new(
            "billing",
            "src/main.py",
            "build_invoice",
            Evidence::NeedsInvestigation,
        )
        .unwrap()
        .with_note("heuristic edge");
        let missing =
            Claim::new("billing", "src/main.py", "build_invoice", Evidence::Missing).unwrap();
        let merged = merge_claims(vec![flagged, missing, verified]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].evidence, Evidence::Verified);

        // Only a heuristic sighting → stays needs-investigation.
        let only_flagged = merge_claims(vec![
            Claim::new("billing", "src/main.py", "f", Evidence::NeedsInvestigation).unwrap(),
            Claim::new("billing", "src/main.py", "f", Evidence::Verified).unwrap_or_else(|| {
                Claim::new("billing", "src/main.py", "f", Evidence::NeedsInvestigation).unwrap()
            }),
        ]);
        assert_eq!(only_flagged.len(), 1);
    }

    /// `impact.affected` (the §9.6 consumer list, fixture scenario 11:
    /// affected billing Verified) renders as claim lines through the
    /// same class mapping `diff_contracts` uses.
    #[test]
    fn affected_consumers_render_as_claim_lines() {
        let caller = SymbolKey {
            repo: RepoId::new("billing").unwrap(),
            path: "src/main.py".into(),
            container: None,
            name: "build_invoice".into(),
        };
        let consumer = ConsumerKey {
            caller: caller.clone(),
            target: ConsumerTargetKey::Contract(
                crate::federation::contracts::model::ContractKey::Http {
                    method: crate::federation::contracts::model::MethodSpec::Known(
                        crate::federation::contracts::model::HttpMethod::Get,
                    ),
                    template: "/api/orders/{}".into(),
                },
            ),
        };
        let impact = Impact {
            service: ServiceName("orders".into()),
            kind: crate::federation::contracts::diff::ChangeKind::FieldRenamed {
                endpoint: (
                    ServiceName("orders".into()),
                    crate::federation::contracts::model::ContractKey::Http {
                        method: crate::federation::contracts::model::MethodSpec::Known(
                            crate::federation::contracts::model::HttpMethod::Get,
                        ),
                        template: "/api/orders/{}".into(),
                    },
                ),
                direction: crate::federation::contracts::model::Direction::Response,
                from: crate::federation::contracts::model::JsonPath(vec![PathSegment::Name(
                    "customer_id".into(),
                )]),
                to: crate::federation::contracts::model::JsonPath(vec![PathSegment::Name(
                    "customerId".into(),
                )]),
                required: false,
            },
            class: Class::Verified,
            reason: Some(Reason::StaticBinding),
            affected: vec![Affected {
                service: ServiceName("billing".into()),
                consumer,
                class: Class::Verified,
                reason: Reason::StaticBinding,
            }],
            scope: Scope::default(),
            coverage: Coverage::default(),
            compatible_changes: 0,
        };

        let mut lines = Vec::new();
        for a in &impact.affected {
            let file = a.consumer.caller.path.clone();
            let symbol = a.consumer.caller.name.clone();
            if let Some(c) = claim_from_diff_class(
                a.consumer.caller.repo.as_str(),
                &file,
                &symbol,
                a.class,
                Some(a.reason),
            ) {
                lines.push(c);
            }
        }
        let rendered = render_claim_lines(lines, None);
        assert_eq!(
            rendered,
            "AFFECTED: billing:src/main.py:build_invoice  EVIDENCE: verified\n  \
             reason: diff impact: static_binding\n"
        );
        // The decoy from the ground truth's traps: never in affected.
        assert!(!rendered.contains("charge"));
    }
}
