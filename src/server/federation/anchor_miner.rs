//! Cross-repo heuristic edge minting — the anchor miner.
//!
//! An acceptance program (7+ A/B agent runs) showed that careful
//! agents reconstruct cross-repo blast radii from cheap signals the
//! graph did not use: structural NAME ANCHORS (a queue/topic/route/
//! proto/env name appearing literally in 2+ repos), git co-change,
//! and commit messages. The graph went silent on exactly those
//! couplings (an AMQP queue consumer showed ZERO edges; a call with
//! an unparsed host showed nothing).
//!
//! This pass mints HEURISTIC edges from those same signals into the
//! federation backend when the federated index is built
//! ([`FederatedIndex::mint_cross_repo_anchors`](crate::federation::federated_index::FederatedIndex)).
//! Every edge is `EdgeProvenance::Heuristic` with a named detector
//! (`anchor:queue:<name>`, `anchor:topic:<name>`, `anchor:route:<path>`,
//! `anchor:proto:<service>`, `anchor:env:<VAR>`, `cochange`,
//! `commit-ref`) so tools and claims can print where it came from,
//! and the claims pipeline classifies any chain touching one as
//! `needs-investigation` — never `verified`.
//!
//! Confidence follows the joiner's §7.3 rule-6 heuristic ladder:
//! route-like anchor matches are 0.6 ([`ANCHOR_MATCH_CONFIDENCE`]),
//! co-change / commit-ref are 0.3 ([`CROSS_REPO_SIGNAL_CONFIDENCE`]).
//!
//! Noise control is structural, not lexical:
//! - Only the five STRUCTURAL anchor kinds are ever minted (an
//!   allowlist — [`AnchorKind`]). A field/property literal like
//!   `customer_id` is not a member of any kind and can never produce
//!   an edge; short/common words are filtered by shape (route stems,
//!   env-var UPPER_SNAKE, context-keyed queue/topic names).
//! - An anchor shared by more than [`ANCHOR_MAX_REPOS`] repos is a
//!   crowd, not a coupling — skipped entirely.
//! - An anchor edge that would duplicate an existing static/confirmed
//!   edge between the same node pair is skipped (static wins).
//!
//! Endpoints: the node that contains the literal (innermost symbol
//! with a line span covering the sighting); for provider-shaped
//! literals the sensor node is preferred (`HttpRoute` / `Topic` by
//! name match); when nothing in the graph covers the file (config
//! files such as `amqp.toml` are not indexed as source), a `File`
//! node is materialized — the file node is the honest endpoint for a
//! coupling the per-repo sensors cannot see.

use crate::error::LainError;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::git::AnyGitSensor;
use crate::server::sensors::util::host_env_name;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

// The coupling-shape knowledge this miner consults (queue/topic
// context keywords, generated-name suffixes, env-access call shapes,
// scan policy) is data: bundled `patterns/anchors.yaml` layered with
// per-repo `<root>/.lain/patterns/*.yaml` overrides via the pattern
// registry. Re-exported here so miner callers find the injected
// parameter type next to the functions that take it.
pub use crate::server::sensors::patterns::{AnchorShapes, AnchorShapesLoad};

// ─── ladder constants (joiner §7.3 rule 6) ──────────────────────────

/// Route-like single-match rung of the existing heuristic ladder —
/// the same 0.6 the joiner assigns an `unbound_host` bind.
pub const ANCHOR_MATCH_CONFIDENCE: f32 =
    crate::federation::contracts::joiner::UNBOUND_HOST_CONFIDENCE;

/// Co-change / commit-ref rung — the same 0.3 the joiner assigns an
/// `ambiguous` multi-match.
pub const CROSS_REPO_SIGNAL_CONFIDENCE: f32 =
    crate::federation::contracts::joiner::AMBIGUOUS_CONFIDENCE;

/// An anchor needs at least this many distinct repos to be a coupling.
pub const ANCHOR_MIN_REPOS: usize = 2;

/// An anchor present in more repos than this is a crowd, not a
/// coupling — skipped entirely (no pairwise flood).
pub const ANCHOR_MAX_REPOS: usize = 8;

/// How many commits of history the co-change / commit-ref detectors
/// read per repo.
pub const COCHANGE_HISTORY_LIMIT: usize = 200;

/// Two commits belong to one multi-repo changeset when their
/// timestamps are within this window and their subjects match.
const COCHANGE_TIME_WINDOW_SECS: i64 = 120;

/// Per-repo endpoint cap when minting the pairwise clique for one
/// anchor — bounds fan-out on anchors that live in many symbols.
const MAX_ENDPOINTS_PER_REPO: usize = 5;

/// Files per commit considered by the co-change pair expansion.
const MAX_FILES_PER_COCHANGE_COMMIT: usize = 20;

/// Changed files per commit that mint commit-ref edges.
const MAX_COMMIT_REF_FILES: usize = 5;

/// Hard safety cap on the temporal detectors' edge output.
const MAX_TEMPORAL_EDGES: usize = 5_000;

/// Source files larger than this are skipped by the scanner.
const MAX_SCAN_FILE_BYTES: u64 = 1_048_576;

/// Minimum length for a queue/topic name literal to count.
const MIN_NAME_LEN: usize = 4;

/// Minimum length for a route literal before normalization.
const MIN_ROUTE_LEN: usize = 4;

// ─── public data types ───────────────────────────────────────────────

/// The allowlist of structural anchor kinds. Nothing outside this
/// enum is ever minted — a field name or generic word is not a
/// member of any kind, which is the exclusion mechanism (not a
/// denylist of words).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnchorKind {
    Route,
    Topic,
    Queue,
    Proto,
    Env,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AnchorKind::Route => "route",
            AnchorKind::Topic => "topic",
            AnchorKind::Queue => "queue",
            AnchorKind::Proto => "proto",
            AnchorKind::Env => "env",
        }
    }

    /// The edge type an anchor of this kind rides. Queue/topic are
    /// message-bus couplings (`BusTopic`); routes are `RouteMatches`;
    /// proto RPCs are `Calls`; env vars are shared config (`Uses`).
    fn edge_type(self) -> EdgeType {
        match self {
            AnchorKind::Route => EdgeType::RouteMatches,
            AnchorKind::Topic | AnchorKind::Queue => EdgeType::BusTopic,
            AnchorKind::Proto => EdgeType::Calls,
            AnchorKind::Env => EdgeType::Uses,
        }
    }
}

/// One structural-name sighting in one repo: the normalized anchor
/// key plus where it was found (for endpoint resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorOccurrence {
    pub repo: String,
    /// Repo-relative path of the file the literal was found in.
    pub path: String,
    /// 1-based line of the sighting.
    pub line: u32,
    pub kind: AnchorKind,
    /// Normalized key: route stem, topic/queue name, `pkg.Service`
    /// (or `pkg.Service/Method`), a generated-code service segment
    /// (the primary name its type spells — `NewOrders` for
    /// `NewOrdersClient` — joined to its declared `pkg.Service` at
    /// mint time), or env var name.
    pub key: String,
    /// Generated-code proto keys only: the `New`-stripped constructor
    /// alternative (`NewOrdersClient` → key `NewOrders`, fallback
    /// `Orders`). At fold time it is consulted only when `key` matches
    /// no declared service segment — the primary name always wins.
    pub constructor_fallback: Option<String>,
}

impl AnchorOccurrence {
    /// The named detector every minted edge carries, e.g.
    /// `anchor:queue:order-events`.
    pub fn detector(&self) -> String {
        format!("anchor:{}:{}", self.kind.as_str(), self.key)
    }
}

/// One commit as the git sensor already reads it, plus its repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    pub repo: String,
    /// Commit subject (first line).
    pub message: String,
    pub time: i64,
    pub files: Vec<String>,
    /// Author name — empty when the source cannot provide it (the
    /// sidecar wire protocol carries no author, so sidecar-mode
    /// history behaves as if the field did not exist).
    pub author: String,
}

/// What one mint pass did. Logged by the CLI callers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintReport {
    /// Distinct `(kind, key)` anchors seen across the federation.
    pub anchors_indexed: usize,
    /// Heuristic anchor edges written (both directions per pair).
    pub anchor_edges: usize,
    /// `cochange` edges written.
    pub cochange_edges: usize,
    /// `commit-ref` edges written.
    pub commit_ref_edges: usize,
    /// Anchors skipped because too many repos share them.
    pub crowds_skipped: usize,
    /// File nodes the miner had to materialize as endpoints.
    pub synthesized_file_nodes: usize,
    /// Previously-minted edges this pass retracted because the
    /// current observation set no longer reproduces them.
    pub retracted_edges: usize,
}

/// True for edges minted by this pass (recognized by detector name).
/// The projection reconciliation in `project_edges` skips these so a
/// watcher-triggered reproject never retracts them. The mint pass
/// itself owns the full minted set — [`mint_cross_repo_observations_with`]
/// retracts the ones its current observation set no longer reproduces.
pub fn is_minted_edge(edge: &GraphEdge) -> bool {
    match &edge.provenance {
        Some(EdgeProvenance::Heuristic { detector, .. }) => {
            detector.starts_with("anchor:") || detector == "cochange" || detector == "commit-ref"
        }
        _ => false,
    }
}

// ─── mint (pure inner half) ──────────────────────────────────────────

/// `(repo, path)` → `(line_start, line_end, node_id)` spans.
type SymbolSpans = BTreeMap<(String, String), Vec<(u32, u32, String)>>;

/// Index views over the backend snapshot the mint pass works against.
struct NodeIndex {
    symbols: SymbolSpans,
    /// `(repo, path)` → `File` node id.
    files: HashMap<(String, String), String>,
    /// `(repo, normalized_stem)` → `HttpRoute` node ids.
    routes: BTreeMap<(String, String), Vec<String>>,
    /// `(repo, topic name)` → `Topic` node ids.
    topics: BTreeMap<(String, String), Vec<String>>,
}

fn build_node_index(nodes: Vec<GraphNode>) -> NodeIndex {
    let mut idx = NodeIndex {
        symbols: BTreeMap::new(),
        files: HashMap::new(),
        routes: BTreeMap::new(),
        topics: BTreeMap::new(),
    };
    for n in nodes {
        let Ok(gid) = GlobalId::parse(&n.id) else {
            continue;
        };
        let repo = gid.repo_id().to_string();
        match n.node_type {
            NodeType::File => {
                idx.files
                    .entry((repo, n.path.clone()))
                    .or_insert_with(|| n.id.clone());
            }
            NodeType::HttpRoute => {
                // Names are `"<METHOD> <template>"` (see the openapi
                // sensor); the stem key matches scanner-normalized
                // route literals.
                if let Some((_method, template)) = n.name.split_once(' ') {
                    if let Some(stem) = normalize_route_stem(template) {
                        idx.routes
                            .entry((repo.clone(), stem))
                            .or_default()
                            .push(n.id.clone());
                    }
                }
            }
            NodeType::Topic => {
                idx.topics
                    .entry((repo.clone(), n.name.clone()))
                    .or_default()
                    .push(n.id.clone());
            }
            _ => {
                if let (Some(s), Some(e)) = (n.line_start, n.line_end) {
                    idx.symbols
                        .entry((repo, n.path.clone()))
                        .or_default()
                        .push((s, e, n.id.clone()));
                }
            }
        }
    }
    for spans in idx.symbols.values_mut() {
        // Innermost-first so the containing function wins over its file.
        spans.sort_by_key(|(s, e, _)| (e.saturating_sub(*s), *s));
    }
    idx
}

/// Mint heuristic edges from pre-extracted occurrences + commit
/// records into `backend`. Pure of I/O so tests can drive it
/// directly; the [`FederatedIndex`](crate::federation::federated_index::FederatedIndex)
/// wiring collects the inputs from the repos' checkouts and git
/// history.
///
/// Commit-subject anchors classify against the bundled shapes (this
/// fold takes no shapes parameter and stays pure of I/O — no
/// filesystem load); [`mint_cross_repo_observations_with`] is the
/// injected variant the miner drives with its loaded shapes.
pub fn mint_cross_repo_observations(
    backend: &dyn GraphBackend,
    occurrences: &[AnchorOccurrence],
    commits: &[CommitRecord],
) -> Result<MintReport, LainError> {
    mint_cross_repo_observations_with(backend, occurrences, commits, AnchorShapes::bundled())
}

/// [`mint_cross_repo_observations`] with caller-supplied shapes for
/// the commit-subject pass (the occurrence side is already
/// extracted). `shapes` classifies free-text subject mentions —
/// env-access call shapes and name gates; queue/topic context
/// keywords do not apply to subjects.
pub fn mint_cross_repo_observations_with(
    backend: &dyn GraphBackend,
    occurrences: &[AnchorOccurrence],
    commits: &[CommitRecord],
    shapes: &AnchorShapes,
) -> Result<MintReport, LainError> {
    let mut report = MintReport::default();

    // Snapshot the current edge set once: the static-win filter and
    // the temporal detectors' dedupe both read it.
    let existing = backend.all_edges()?;
    let mut blocked: BTreeSet<[String; 2]> = BTreeSet::new();
    for e in &existing {
        if matches!(
            e.provenance,
            Some(EdgeProvenance::Static { .. }) | Some(EdgeProvenance::Confirmed { .. })
        ) {
            blocked.insert(pair_key(&e.source_id, &e.target_id));
        }
    }

    // Second suppression layer (Task 2): the typed bus edge a sensor
    // mints is fn/synthetic → Topic within ONE repo, so its pair key
    // can never equal the cross-repo fn↔fn (or file↔file) pair the
    // anchor mint would create — the exact-pair rule above is blind
    // to it. Map each endpoint to the topic labels its
    // static/confirmed BusTopic/Consumes/Produces edges reach; when
    // BOTH endpoints of a would-be queue/topic anchor pair already
    // carry a label ending in `/{anchor literal}` (the
    // `{broker}/{name}` convention), the typed edges already cover
    // the coupling and the heuristic twin is skipped.
    let nodes = backend.list_nodes()?;
    let topic_labels: BTreeMap<String, String> = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .map(|n| (n.id.clone(), n.name.clone()))
        .collect();
    let mut bus_covered: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Fallback key: `(repo, path)` → topic labels. The typed edge's
    // endpoint may be a synthetic site node (`line_end: None`, never
    // a `symbols` span) while the anchor occurrence resolves to the
    // File node — same file, different node id. Coverage recorded
    // under the shared `(repo, path)` still owns the coupling.
    let mut bus_covered_by_path: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for e in &existing {
        if !matches!(
            e.provenance,
            Some(EdgeProvenance::Static { .. }) | Some(EdgeProvenance::Confirmed { .. })
        ) {
            continue;
        }
        if !matches!(
            e.edge_type,
            EdgeType::BusTopic | EdgeType::Consumes | EdgeType::Produces
        ) {
            continue;
        }
        let mut cover = |participant: &str, topic: &str| {
            bus_covered
                .entry(participant.to_string())
                .or_default()
                .insert(topic.to_string());
            if let Some(path) = GlobalId::parse(participant)
                .ok()
                .and_then(|gid| gid.path().map(|p| (gid.repo_id().to_string(), p)))
            {
                bus_covered_by_path
                    .entry(path)
                    .or_default()
                    .insert(topic.to_string());
            }
        };
        if let Some(topic) = topic_labels.get(&e.source_id) {
            cover(&e.target_id, topic);
        }
        if let Some(topic) = topic_labels.get(&e.target_id) {
            cover(&e.source_id, topic);
        }
    }

    let mut minted: HashSet<(String, String, EdgeType)> = HashSet::new();
    for e in &existing {
        minted.insert((
            e.source_id.clone(),
            e.target_id.clone(),
            e.edge_type.clone(),
        ));
    }
    // Edges this pass believes must exist. The reconcile at the end
    // retracts previously-minted edges missing from this set, so every
    // mint site records here — including the pairs skipped above as
    // already-present (dedupe must not turn into a silent retraction).
    let mut desired: HashSet<(String, String, EdgeType)> = HashSet::new();

    let mut index = build_node_index(nodes);

    // ── group occurrences by (kind, key) ────────────────────────────
    let mut groups: BTreeMap<(AnchorKind, String), Vec<&AnchorOccurrence>> = BTreeMap::new();
    for occ in occurrences {
        groups
            .entry((occ.kind, occ.key.clone()))
            .or_default()
            .push(occ);
    }
    // Generated-code proto keys (bare service segments spelled by
    // client/stub type names — `NewOrders` / `Orders` from
    // `pb.NewOrdersClient`) join the declared `pkg.Service` /
    // `pkg.Service/Method` groups whose service segment matches
    // ASCII-case-insensitively; the `New`-stripped constructor
    // candidate is fallback only. A generated name with no declared
    // service anchors nothing — the group keeps the declared key, so
    // the detector label stays `anchor:proto:<pkg.Service>`.
    fold_generated_proto_keys(&mut groups);
    report.anchors_indexed = groups.len();

    // Resolve endpoints per (group, repo). `anchor_endpoints` is also
    // the commit-ref detector's lookup: a commit subject mentioning
    // an anchor reaches out to the repos where that anchor lives.
    let mut anchor_endpoints: BTreeMap<(AnchorKind, String), BTreeMap<String, Vec<String>>> =
        BTreeMap::new();
    let mut new_file_nodes: Vec<(String, String, String)> = Vec::new(); // (gid, path, name)
    let mut new_edges: Vec<GraphEdge> = Vec::new();
    {
        for ((kind, key), occs) in &groups {
            let mut per_repo: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            let mut repo_set: HashSet<&str> = HashSet::new();
            for occ in occs {
                repo_set.insert(occ.repo.as_str());
                let endpoint = resolve_endpoint(occ, &mut index, &mut new_file_nodes, &mut report);
                if let Some(id) = endpoint {
                    per_repo.entry(occ.repo.clone()).or_default().insert(id);
                }
            }
            if repo_set.len() > ANCHOR_MAX_REPOS {
                report.crowds_skipped += 1;
                continue;
            }
            // Single-repo groups stay in the index too: the
            // commit-ref detector reaches OUT to repos where an
            // anchor lives even when the commit's own repo does not
            // declare it. Pairwise minting still requires two repos.
            let capped: BTreeMap<String, Vec<String>> = per_repo
                .into_iter()
                .map(|(r, ids)| {
                    let mut ids: Vec<String> = ids.into_iter().collect();
                    ids.sort();
                    ids.truncate(MAX_ENDPOINTS_PER_REPO);
                    (r, ids)
                })
                .collect();
            anchor_endpoints.insert((*kind, key.clone()), capped);
        }
    }

    // ── mint the pairwise anchor cliques (both directions) ──────────
    for ((kind, key), per_repo) in &anchor_endpoints {
        let detector = format!("anchor:{}:{}", kind.as_str(), key);
        let repos: Vec<&String> = per_repo.keys().collect();
        if repos.len() < ANCHOR_MIN_REPOS {
            continue;
        }
        let edge_type = kind.edge_type();
        // Only queue/topic anchors share a `{broker}/{name}` label
        // convention with the typed bus edges; route/proto/env
        // anchors have no Topic-node coverage semantics.
        let typed_covered = matches!(kind, AnchorKind::Queue | AnchorKind::Topic);
        let key_suffix = format!("/{key}");
        for i in 0..repos.len() {
            for j in (i + 1)..repos.len() {
                for a in &per_repo[repos[i]] {
                    for b in &per_repo[repos[j]] {
                        if blocked.contains(&pair_key(a, b)) {
                            continue;
                        }
                        if typed_covered
                            && pair_bus_covered(
                                a,
                                b,
                                &key_suffix,
                                &bus_covered,
                                &bus_covered_by_path,
                            )
                        {
                            continue;
                        }
                        for (s, t) in [(a.clone(), b.clone()), (b.clone(), a.clone())] {
                            desired.insert((s.clone(), t.clone(), edge_type.clone()));
                            if !minted.insert((s.clone(), t.clone(), edge_type.clone())) {
                                continue;
                            }
                            new_edges.push(heuristic_edge(
                                edge_type.clone(),
                                s,
                                t,
                                detector.clone(),
                                ANCHOR_MATCH_CONFIDENCE,
                            ));
                            report.anchor_edges += 1;
                        }
                    }
                }
            }
        }
    }

    // ── Detector 2: cross-repo co-change ────────────────────────────
    let mut by_message: BTreeMap<String, Vec<&CommitRecord>> = BTreeMap::new();
    for c in commits {
        by_message
            .entry(normalize_message(&c.message))
            .or_default()
            .push(c);
    }
    for group in by_message.values() {
        // Time-cluster the group: greedy windows of
        // `COCHANGE_TIME_WINDOW_SECS`. A message that recurs across
        // many repos over months ("bump version") is not one
        // changeset; a real multi-repo changeset lands inside the
        // window. A cluster spanning more repos than the crowd limit
        // is a bulk import / fixture-style sweep — skipped rather
        // than flooding the graph (same convention as anchor crowds).
        let mut sorted = group.clone();
        sorted.sort_by_key(|c| c.time);
        let mut cluster: Vec<&CommitRecord> = Vec::new();
        let mut cluster_start: Option<i64> = None;
        for c in sorted {
            let continues = match cluster_start {
                Some(start) => (c.time - start).abs() <= COCHANGE_TIME_WINDOW_SECS,
                None => true,
            };
            if !continues {
                process_cochange_cluster(
                    &mut report,
                    &index,
                    &blocked,
                    &mut minted,
                    &mut desired,
                    &mut new_edges,
                    &cluster,
                );
                cluster.clear();
                cluster_start = None;
            }
            cluster_start.get_or_insert(c.time);
            cluster.push(c);
        }
        if !cluster.is_empty() {
            process_cochange_cluster(
                &mut report,
                &index,
                &blocked,
                &mut minted,
                &mut desired,
                &mut new_edges,
                &cluster,
            );
        }
    }

    // ── Detector 3: commit-message references ───────────────────────
    for c in commits {
        let subject_anchors = extract_subject_anchors(&c.message, shapes);
        if subject_anchors.is_empty() {
            continue;
        }
        for (kind, key) in subject_anchors {
            let Some(per_repo) = anchor_endpoints.get(&(kind, key.clone())) else {
                continue;
            };
            for (other_repo, eps) in per_repo {
                if other_repo == &c.repo {
                    continue;
                }
                let Some(target) = eps.first() else {
                    continue;
                };
                for f in c.files.iter().take(MAX_COMMIT_REF_FILES) {
                    let Some(source) = index.files.get(&(c.repo.clone(), f.clone())).cloned()
                    else {
                        continue;
                    };
                    if blocked.contains(&pair_key(&source, target)) {
                        continue;
                    }
                    desired.insert((source.clone(), target.clone(), EdgeType::CoChangedWith));
                    if !minted.insert((source.clone(), target.clone(), EdgeType::CoChangedWith)) {
                        continue;
                    }
                    if report.commit_ref_edges >= MAX_TEMPORAL_EDGES {
                        break;
                    }
                    new_edges.push(heuristic_edge(
                        EdgeType::CoChangedWith,
                        source.clone(),
                        target.clone(),
                        "commit-ref".to_string(),
                        CROSS_REPO_SIGNAL_CONFIDENCE,
                    ));
                    report.commit_ref_edges += 1;
                }
            }
        }
    }

    // Reconcile: this mint pass owns its full edge set — add and
    // remove symmetric. A previously-minted heuristic edge whose pair
    // the current observation set no longer reproduces (anchor
    // literal gone, commit dropped out of the window, pair now
    // suppressed by a static edge) is retracted here. Without this,
    // the projection reconciliation deliberately leaves minted edges
    // alone and the obsolete coupling survived every reload and
    // restart until the operator ran `lain reindex`.
    let obsolete: Vec<GraphEdge> = existing
        .iter()
        .filter(|e| is_minted_edge(e))
        .filter(|e| {
            !desired.contains(&(
                e.source_id.clone(),
                e.target_id.clone(),
                e.edge_type.clone(),
            ))
        })
        .cloned()
        .collect();
    report.retracted_edges = obsolete.len();

    // Materialize synthesized file endpoints before the edges that
    // point at them — the backend drops edges with missing endpoints.
    if !new_file_nodes.is_empty() {
        let file_nodes: Vec<GraphNode> = new_file_nodes
            .into_iter()
            .map(|(gid, path, name)| {
                let mut node = GraphNode::new(NodeType::File, name, path);
                node.id = gid;
                node
            })
            .collect();
        backend.upsert_nodes_batch(&file_nodes)?;
    }
    if !new_edges.is_empty() {
        backend.upsert_edges_batch(&new_edges)?;
    }
    if !obsolete.is_empty() {
        backend.remove_edges(&obsolete)?;
    }
    Ok(report)
}

/// A generated-code proto key is a bare service segment (`Orders`,
/// minted from a `pb.NewOrdersClient` / `OrdersStub` type name);
/// declared proto keys are always `pkg.Service` or
/// `pkg.Service/Method`.
fn is_generated_proto_key(key: &str) -> bool {
    !key.contains('.') && !key.contains('/')
}

/// The service segment of a proto anchor key: the last dot-segment of
/// the part before any `/Method` — `orders.Orders` and
/// `orders.Orders/GetOrder` both yield `Orders`.
fn proto_service_segment(key: &str) -> &str {
    let svc_part = key.split_once('/').map_or(key, |(s, _)| s);
    svc_part.rsplit('.').next().unwrap_or(svc_part)
}

/// Fold generated-code proto occurrences into the declared proto
/// groups whose service segment equals theirs ASCII-case-insensitively
/// (`Orders` joins `orders.Orders` and `orders.Orders/GetOrder`). The
/// occurrence's PRIMARY name — the one its type spells — is used when
/// any declared service matches it; the `New`-stripped constructor
/// fallback gets its chance only when the primary matches nothing, so
/// `NewOrdersClient` prefers a declared `v2.NewOrders` and can never
/// pair onto `orders.Orders` while its true service exists. A
/// generated name whose candidates match no declared service anchors
/// nothing — only real proto service keys form minting groups, so a
/// `TicketsClient` can never reach `orders.Orders`.
fn fold_generated_proto_keys(groups: &mut BTreeMap<(AnchorKind, String), Vec<&AnchorOccurrence>>) {
    let mut generated: Vec<&AnchorOccurrence> = Vec::new();
    for key in groups
        .keys()
        .filter(|(kind, key)| *kind == AnchorKind::Proto && is_generated_proto_key(key))
        .map(|(_, key)| key.clone())
        .collect::<Vec<_>>()
    {
        if let Some(occs) = groups.remove(&(AnchorKind::Proto, key)) {
            generated.extend(occs);
        }
    }
    if generated.is_empty() {
        return;
    }
    let declared: Vec<String> = groups
        .keys()
        .filter(|(kind, key)| *kind == AnchorKind::Proto && !is_generated_proto_key(key))
        .map(|(_, key)| key.clone())
        .collect();
    let matching = |seg: &str| -> Vec<String> {
        declared
            .iter()
            .filter(|d| proto_service_segment(d).eq_ignore_ascii_case(seg))
            .cloned()
            .collect()
    };
    for occ in generated {
        let primary = matching(&occ.key);
        let targets = if primary.is_empty() {
            occ.constructor_fallback
                .as_deref()
                .map_or(Vec::new(), |fb| matching(fb))
        } else {
            primary
        };
        for dkey in targets {
            groups
                .entry((AnchorKind::Proto, dkey))
                .or_default()
                .push(occ);
        }
    }
}

fn pair_key(a: &str, b: &str) -> [String; 2] {
    if a <= b {
        [a.to_string(), b.to_string()]
    } else {
        [b.to_string(), a.to_string()]
    }
}

/// Both endpoints of a would-be anchor pair already carry a
/// static/confirmed bus edge to a Topic labelled `*/<key>` (the
/// `{broker}/{name}` convention, e.g. `amqp/order-events` for the
/// queue literal `order-events`) — the typed sensor edges already
/// cover this coupling, so the heuristic twin is skipped. The
/// per-endpoint label sets (`bus_covered`) are keyed by node id, so
/// this works whether the two repos' endpoints bind the same Topic
/// node or distinct per-repo Topic nodes sharing one name.
///
/// `covered_by_path` is the fallback for endpoints the id key can
/// never reach: a top-level bus call has no indexed symbol, so its
/// typed edge rides the synthetic `topic-read:<path>:<line>` node
/// (`line_end: None`, never a `symbols` span) while the anchor pair
/// resolves to the File node — same `(repo, path)`, different id. A
/// label set found under either key counts as coverage.
fn pair_bus_covered(
    a: &str,
    b: &str,
    key_suffix: &str,
    covered: &BTreeMap<String, BTreeSet<String>>,
    covered_by_path: &BTreeMap<(String, String), BTreeSet<String>>,
) -> bool {
    let covers = |id: &str| {
        let labeled = |labels: &BTreeSet<String>| labels.iter().any(|l| l.ends_with(key_suffix));
        if covered.get(id).is_some_and(labeled) {
            return true;
        }
        GlobalId::parse(id)
            .ok()
            .and_then(|gid| gid.path().map(|p| (gid.repo_id().to_string(), p)))
            .and_then(|k| covered_by_path.get(&k))
            .is_some_and(labeled)
    };
    covers(a) && covers(b)
}

fn heuristic_edge(
    edge_type: EdgeType,
    source_id: String,
    target_id: String,
    detector: String,
    confidence: f32,
) -> GraphEdge {
    GraphEdge {
        edge_type,
        source_id,
        target_id,
        weight: Some(confidence),
        cross_repo: true,
        provenance: Some(EdgeProvenance::Heuristic {
            detector,
            confidence,
        }),
        site: None,
        detail: None,
    }
}

/// Mint co-change edges for one time-cluster of same-subject commits.
/// A cluster touching more repos than [`ANCHOR_MAX_REPOS`] is a bulk
/// sweep (fixture-style identical commits, mass imports) — a crowd,
/// not a coupling — and is skipped. A pair mints only when both
/// commits' authors are known and equal; author-less history
/// (sidecar mode carries no author on the wire) matches on subject
/// and time alone, as before.
fn process_cochange_cluster(
    report: &mut MintReport,
    index: &NodeIndex,
    blocked: &BTreeSet<[String; 2]>,
    minted: &mut HashSet<(String, String, EdgeType)>,
    desired: &mut HashSet<(String, String, EdgeType)>,
    new_edges: &mut Vec<GraphEdge>,
    cluster: &[&CommitRecord],
) {
    let repos: HashSet<&str> = cluster.iter().map(|c| c.repo.as_str()).collect();
    if repos.len() > ANCHOR_MAX_REPOS {
        report.crowds_skipped += 1;
        return;
    }
    for i in 0..cluster.len() {
        for j in (i + 1)..cluster.len() {
            let (a, b) = (cluster[i], cluster[j]);
            if a.repo == b.repo {
                continue;
            }
            if (a.time - b.time).abs() > COCHANGE_TIME_WINDOW_SECS {
                continue;
            }
            if !a.author.is_empty() && !b.author.is_empty() && a.author != b.author {
                continue;
            }
            for fa in a.files.iter().take(MAX_FILES_PER_COCHANGE_COMMIT) {
                for fb in b.files.iter().take(MAX_FILES_PER_COCHANGE_COMMIT) {
                    let (Some(ida), Some(idb)) = (
                        index.files.get(&(a.repo.clone(), fa.clone())).cloned(),
                        index.files.get(&(b.repo.clone(), fb.clone())).cloned(),
                    ) else {
                        continue;
                    };
                    // One direction per unordered pair, ordered by
                    // identity — same convention as the per-repo
                    // co-change miner.
                    let (s, t) = if (a.repo.as_str(), fa.as_str()) <= (b.repo.as_str(), fb.as_str())
                    {
                        (ida, idb)
                    } else {
                        (idb, ida)
                    };
                    if blocked.contains(&pair_key(&s, &t)) {
                        continue;
                    }
                    desired.insert((s.clone(), t.clone(), EdgeType::CoChangedWith));
                    if !minted.insert((s.clone(), t.clone(), EdgeType::CoChangedWith)) {
                        continue;
                    }
                    if report.cochange_edges >= MAX_TEMPORAL_EDGES {
                        return;
                    }
                    new_edges.push(heuristic_edge(
                        EdgeType::CoChangedWith,
                        s,
                        t,
                        "cochange".to_string(),
                        CROSS_REPO_SIGNAL_CONFIDENCE,
                    ));
                    report.cochange_edges += 1;
                }
            }
        }
    }
}

fn normalize_message(message: &str) -> String {
    message
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the graph node an occurrence anchors to: innermost symbol
/// whose line span covers the sighting; provider-shaped literals
/// prefer the sensor node (`HttpRoute` by stem, `Topic` by name);
/// otherwise the file node — materialized when the graph has none.
fn resolve_endpoint(
    occ: &AnchorOccurrence,
    index: &mut NodeIndex,
    new_file_nodes: &mut Vec<(String, String, String)>,
    report: &mut MintReport,
) -> Option<String> {
    let spec_like = matches!(
        Path::new(&occ.path)
            .extension()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default(),
        "yaml" | "yml" | "json" | "toml" | "proto"
    );
    let by_line = |index: &NodeIndex| -> Option<String> {
        let spans = index.symbols.get(&(occ.repo.clone(), occ.path.clone()))?;
        spans
            .iter()
            .find(|(s, e, _)| *s <= occ.line && occ.line <= *e)
            .map(|(_, _, id)| id.clone())
    };
    let resolved = match occ.kind {
        AnchorKind::Route if spec_like => index
            .routes
            .get(&(occ.repo.clone(), occ.key.clone()))
            .and_then(|ids| ids.first().cloned())
            .or_else(|| by_line(index)),
        AnchorKind::Topic => index
            .topics
            .get(&(occ.repo.clone(), occ.key.clone()))
            .and_then(|ids| ids.first().cloned())
            .or_else(|| by_line(index)),
        _ => by_line(index).or_else(|| {
            index
                .routes
                .get(&(occ.repo.clone(), occ.key.clone()))
                .and_then(|ids| ids.first().cloned())
        }),
    };
    if resolved.is_some() {
        return resolved;
    }
    // Nothing in the graph covers this file — materialize the file
    // node so the coupling has an honest endpoint.
    if let Some(id) = index.files.get(&(occ.repo.clone(), occ.path.clone())) {
        return Some(id.clone());
    }
    let name = Path::new(&occ.path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let rid = RepoId::new(&occ.repo).ok()?;
    let gid = GlobalId::new(&rid, NodeType::File, &occ.path, &name, None)
        .as_str()
        .to_string();
    index
        .files
        .insert((occ.repo.clone(), occ.path.clone()), gid.clone());
    new_file_nodes.push((gid.clone(), occ.path.clone(), name));
    report.synthesized_file_nodes += 1;
    Some(gid)
}

// ─── scan (fs half) ──────────────────────────────────────────────────

/// Scan a repo checkout for structural anchor occurrences. Walks the
/// tree directly (skipping VCS/build/vendor directories), so tests
/// can drive it with a plain tempdir.
/// Scan one repo's checkout for anchor occurrences. Shapes come from
/// the pattern registry for `local_path` — per-repo
/// `<root>/.lain/patterns/*.yaml` overrides layered on the bundled
/// defaults via `Patterns::load_overrides`; a missing or malformed
/// override degrades to the bundled defaults with a logged warning
/// ([`AnchorShapes::for_repo`]). The injected core is
/// [`scan_repo_occurrences_with`].
pub fn scan_repo_occurrences(repo: &RepoId, local_path: &Path) -> Vec<AnchorOccurrence> {
    let load = AnchorShapes::for_repo(local_path);
    scan_repo_occurrences_with(repo, local_path, &load.shapes)
}

/// [`scan_repo_occurrences`] with caller-supplied shapes: the miner
/// injects the per-repo shapes it loaded through the pattern
/// registry; callers with their own `AnchorShapes` inject directly.
pub fn scan_repo_occurrences_with(
    repo: &RepoId,
    local_path: &Path,
    shapes: &AnchorShapes,
) -> Vec<AnchorOccurrence> {
    use rayon::prelude::*;
    let mut candidate_files: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut stack = vec![local_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if shapes.scan_skip_dirs().iter().any(|d| d == &name) || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default();
            if !shapes.scan_exts().iter().any(|e| e == ext) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_SCAN_FILE_BYTES {
                continue;
            }
            let Ok(rel) = path.strip_prefix(local_path) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            candidate_files.push((path, rel));
        }
    }
    let repo_str = repo.as_str();
    let mut out: Vec<AnchorOccurrence> = candidate_files
        .par_iter()
        .filter_map(|(path, rel)| {
            let content = std::fs::read_to_string(path).ok()?;
            Some(extract_from_text(repo_str, rel, &content, shapes))
        })
        .flatten()
        .collect();
    out.sort_by(|a, b| (a.kind, &a.key, &a.path, a.line).cmp(&(b.kind, &b.key, &b.path, b.line)));
    out.dedup_by(|a, b| a.kind == b.kind && a.key == b.key && a.path == b.path && a.line == b.line);
    out
}

/// Read `limit` commits of history through the git sensor.
pub fn collect_commit_records(
    repo: &RepoId,
    git: &AnyGitSensor,
    limit: usize,
) -> Vec<CommitRecord> {
    match git.get_commit_history(limit) {
        Ok(commits) => commits
            .into_iter()
            .map(|c| CommitRecord {
                repo: repo.as_str().to_string(),
                message: c.message,
                time: c.time,
                files: c.files,
                author: c.author,
            })
            .collect(),
        Err(e) => {
            tracing::warn!(
                "anchor miner: reading git history of '{}' failed: {e}",
                repo.as_str()
            );
            Vec::new()
        }
    }
}

// ─── extraction ──────────────────────────────────────────────────────

/// One file's worth of extraction. `path` is repo-relative; the
/// extension decides whether the `.proto` structure lines are parsed
/// in addition to the generic literal pass.
fn extract_from_text(
    repo: &str,
    path: &str,
    content: &str,
    shapes: &AnchorShapes,
) -> Vec<AnchorOccurrence> {
    let mut out = Vec::new();
    let is_proto = path.ends_with(".proto");
    let mut proto_package: Option<String> = None;
    let mut proto_service: Option<String> = None;
    for (i, line) in content.lines().enumerate() {
        let line_no = (i + 1) as u32;
        if is_proto {
            if let Some(pkg) = proto_line_package(line) {
                proto_package = Some(pkg);
                continue;
            }
            if let Some(svc) = proto_line_service(line) {
                proto_service = Some(svc.clone());
                if let Some(pkg) = &proto_package {
                    out.push(anchor(
                        repo,
                        path,
                        line_no,
                        AnchorKind::Proto,
                        format!("{pkg}.{svc}"),
                    ));
                }
                continue;
            }
            if let Some(method) = proto_line_rpc(line) {
                if let (Some(pkg), Some(svc)) = (&proto_package, &proto_service) {
                    out.push(anchor(
                        repo,
                        path,
                        line_no,
                        AnchorKind::Proto,
                        format!("{pkg}.{svc}/{method}"),
                    ));
                }
                continue;
            }
        }
        out.extend(extract_from_line(repo, path, line_no, line, shapes));
    }
    out.sort_by(|a, b| (a.kind, &a.key, a.line).cmp(&(b.kind, &b.key, b.line)));
    out.dedup_by(|a, b| a.kind == b.kind && a.key == b.key && a.line == b.line);
    out
}

fn anchor(repo: &str, path: &str, line: u32, kind: AnchorKind, key: String) -> AnchorOccurrence {
    AnchorOccurrence {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        kind,
        key,
        constructor_fallback: None,
    }
}

/// Extract every anchor from one line of any supported language.
fn extract_from_line(
    repo: &str,
    path: &str,
    line_no: u32,
    line: &str,
    shapes: &AnchorShapes,
) -> Vec<AnchorOccurrence> {
    let mut out = Vec::new();
    let mut seen: HashSet<(AnchorKind, String)> = HashSet::new();
    let mut push = |kind: AnchorKind, key: String, out: &mut Vec<AnchorOccurrence>| {
        if key.len() >= MIN_NAME_LEN && seen.insert((kind, key.clone())) {
            out.push(anchor(repo, path, line_no, kind, key));
        }
    };

    // Env idioms first (grammar owned by `host_env_name`), then the
    // literal pass skips their spans so `"RISK_ORDERS_URL"` is never
    // re-classified as a bare name.
    let mut consumed: Vec<(usize, usize)> = Vec::new();
    for (range, name) in extract_env_from_line(line, shapes) {
        consumed.push(range);
        push(AnchorKind::Env, name.to_string(), &mut out);
    }

    let low = line.to_lowercase();
    let queue_ctx = line_has_queue_context(&low, shapes);
    let topic_ctx = line_has_topic_context(&low, shapes);

    let mut quoted_spans: Vec<(usize, usize)> = Vec::new();
    for (span, content) in quoted_strings(line) {
        if consumed.iter().any(|(s, e)| span.0 >= *s && span.1 <= *e) {
            continue;
        }
        quoted_spans.push(span);
        if let Some((kind, key)) = classify_literal(&content, queue_ctx, topic_ctx) {
            push(kind, key, &mut out);
        }
    }

    // Unquoted route-shaped tokens (bare YAML/TOML keys, comments).
    // Skipped inside quoted spans so one literal counts once.
    for (span, token) in unquoted_route_tokens(line) {
        if quoted_spans
            .iter()
            .any(|(s, e)| span.0 >= *s && span.1 <= *e)
            || consumed.iter().any(|(s, e)| span.0 >= *s && span.1 <= *e)
        {
            continue;
        }
        if let Some(stem) = normalize_route_stem(&token) {
            push(AnchorKind::Route, stem, &mut out);
        }
    }

    // Generated gRPC client/stub type names — an identifier pass
    // beyond the quoted literals. The consumer side carries no proto
    // literals, only codegen names (`pb.NewOrdersClient`,
    // `OrdersStub`, `FooGrpc.FooBlockingStub`), keyed here by the
    // service name their type spells (`Orders` for `OrdersClient`,
    // `NewOrders` for `NewOrdersClient`), with the `New`-stripped
    // constructor candidate as fallback. These keys join a declared
    // `pkg.Service` group at mint time; with no declared service they
    // anchor nothing. Quoted/env spans stay owned by the passes above.
    //
    // `generated_service_key` already applies the `>= 3` service
    // floor (same as `proto_ident_key`), so these keys skip the
    // queue/topic `MIN_NAME_LEN` gate. The keys are bare segments,
    // which the literal pass can never produce (its proto keys always
    // carry a `pkg.` prefix), so a local dedupe set is sufficient.
    let mut gen_seen: HashSet<String> = HashSet::new();
    for (span, token) in identifier_tokens(line) {
        if quoted_spans
            .iter()
            .any(|(s, e)| span.0 >= *s && span.1 <= *e)
            || consumed.iter().any(|(s, e)| span.0 >= *s && span.1 <= *e)
        {
            continue;
        }
        if let Some(cand) = generated_service_key(&token, shapes) {
            if gen_seen.insert(cand.primary.clone()) {
                let mut occ = anchor(repo, path, line_no, AnchorKind::Proto, cand.primary);
                occ.constructor_fallback = cand.constructor_fallback;
                out.push(occ);
            }
        }
    }
    out
}

/// Anchors a commit SUBJECT mentions. The subject is free text, so
/// the shape gates run without line context: a bare `order-events`
/// counts as a queue-or-topic mention (whichever group exists
/// decides at mint time).
fn extract_subject_anchors(subject: &str, shapes: &AnchorShapes) -> Vec<(AnchorKind, String)> {
    let mut out: Vec<(AnchorKind, String)> = Vec::new();
    let mut seen: HashSet<(AnchorKind, String)> = HashSet::new();
    let mut push = |kind: AnchorKind, key: String| {
        if key.len() >= MIN_NAME_LEN && seen.insert((kind, key.clone())) {
            out.push((kind, key));
        }
    };
    for name in extract_env_from_line(subject, shapes)
        .into_iter()
        .map(|(_, n)| n)
    {
        push(AnchorKind::Env, name.to_string());
    }
    let mut tokens: Vec<String> = Vec::new();
    for (_span, content) in quoted_strings(subject) {
        if let Some((kind, key)) = classify_literal(&content, true, true) {
            push(kind, key);
        }
        tokens.push(content);
    }
    for (_span, token) in unquoted_route_tokens(subject) {
        if let Some(stem) = normalize_route_stem(&token) {
            push(AnchorKind::Route, stem);
        }
        tokens.push(token);
    }
    // Bare words: whitespace/punctuation-split tokens classified by
    // shape only. Queue and topic share the name space, so a plausible
    // name produces both keys — the mint step only uses keys some repo
    // actually declared.
    for token in
        subject.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')))
    {
        if !token.is_empty() {
            tokens.push(token.to_string());
        }
    }
    for token in tokens {
        let t = token.trim();
        if t.starts_with('/') {
            if let Some(stem) = normalize_route_stem(t) {
                push(AnchorKind::Route, stem);
            }
            continue;
        }
        if is_env_shape(t) {
            push(AnchorKind::Env, t.to_string());
            continue;
        }
        if let Some(key) = proto_ident_key(t) {
            push(AnchorKind::Proto, key);
            continue;
        }
        if plausible_name(t) {
            push(AnchorKind::Queue, t.to_string());
            push(AnchorKind::Topic, t.to_string());
        }
    }
    out
}

/// Classify one quoted literal's content into an anchor (the
/// allowlist gate — content that fits no structural shape returns
/// `None` and is never minted).
fn classify_literal(
    content: &str,
    queue_ctx: bool,
    topic_ctx: bool,
) -> Option<(AnchorKind, String)> {
    let mut c = content.trim();
    if c.len() < MIN_ROUTE_LEN || c.chars().any(char::is_whitespace) {
        // Routes/names never contain whitespace; a multi-word string
        // is prose, not structure. (`"GET /path"` is handled by the
        // explicit method-prefix split.)
        c = method_prefixed_route(c)?;
    }
    if is_env_shape(c) {
        // A bare UPPER_SNAKE string is not an env ACCESS — only the
        // idiom table (`os.getenv(...)` etc.) mints env anchors.
        return None;
    }
    if c.starts_with('/') {
        if let Some(key) = grpc_path_key(c) {
            return Some((AnchorKind::Proto, key));
        }
        let stem = normalize_route_stem(c)?;
        return Some((AnchorKind::Route, stem));
    }
    if let Some(key) = proto_ident_key(c) {
        return Some((AnchorKind::Proto, key));
    }
    if !plausible_name(c) {
        return None;
    }
    if queue_ctx {
        return Some((AnchorKind::Queue, c.to_string()));
    }
    if topic_ctx {
        return Some((AnchorKind::Topic, c.to_string()));
    }
    None
}

fn method_prefixed_route(c: &str) -> Option<&str> {
    const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    let (head, rest) = c.split_once(' ')?;
    if METHODS.contains(&head) && rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

/// `GET /path`-free gRPC full method path: `/pkg.Service/Method`.
fn grpc_path_key(c: &str) -> Option<String> {
    let inner = c.strip_prefix('/')?;
    if !inner.contains('.') || !inner.contains('/') {
        return None;
    }
    proto_ident_key(inner)
}

/// `pkg.Service` or `pkg.Service/Method` — the gRPC naming shape.
fn proto_ident_key(c: &str) -> Option<String> {
    let (svc_part, method) = match c.split_once('/') {
        Some((s, m)) => (s, Some(m)),
        None => (c, None),
    };
    let mut segs = svc_part.split('.');
    let pkg = segs.next()?;
    let svc = segs.next()?;
    if segs.next().is_some() {
        return None;
    }
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    if !ident(pkg) || !ident(svc) {
        return None;
    }
    let mut svc_chars = svc.chars();
    if !svc_chars.next().is_some_and(|c| c.is_uppercase()) || svc.len() < 3 {
        return None;
    }
    match method {
        Some(m) => {
            if !ident(m) || !m.chars().next().is_some_and(|c| c.is_uppercase()) {
                return None;
            }
            Some(format!("{pkg}.{svc}/{m}"))
        }
        None => Some(format!("{pkg}.{svc}")),
    }
}

/// Both service-name candidates a generated gRPC client/stub type
/// name yields. `primary` is the name the type SPELLS
/// (`NewOrdersClient` → `NewOrders`, `NewsClient` → `News`); the
/// `New`-stripped constructor alternative (`NewOrdersClient` →
/// `Orders`) rides along as a FALLBACK, consulted at fold time only
/// when the primary matches no declared service — so a `News` service
/// never falls into the `New`-strip hole, and a `NewOrders` client
/// can never pair onto `Orders` while its true service exists.
struct GeneratedCandidate {
    primary: String,
    constructor_fallback: Option<String>,
}

/// The service segment a generated gRPC client/stub type name belongs
/// to — `NewOrdersClient` / `OrdersClient` / `OrdersStub` /
/// `OrdersBlockingStub` / `FooGrpc.FooBlockingStub` yield
/// `NewOrders` / `Orders` / `Foo` as their primary candidate. The
/// segment must look like a type name (uppercase-initial, len ≥ 3 —
/// the same service gate as `proto_ident_key`); a lowercase var like
/// `ordersClient` is covered by the constructor/type name at its
/// binding site instead of by its own identifier.
fn generated_service_key(ident: &str, shapes: &AnchorShapes) -> Option<GeneratedCandidate> {
    let base = ident.rsplit('.').next().unwrap_or(ident);
    let primary = service_segment_from(base, shapes)?;
    // `NewXxxClient` may be a Go constructor for service `Xxx` — kept
    // as fallback only, for the case no `NewXxx` service is declared.
    // The strip-prefix list is shape data (`generated_names.
    // strip_prefixes`); with the bundled `New` this is the historical
    // single-prefix rule.
    let constructor_fallback = shapes
        .generated_strip_prefixes()
        .iter()
        .filter_map(|p| base.strip_prefix(p.as_str()))
        .find_map(|stripped| service_segment_from(stripped, shapes))
        .filter(|fb| fb != &primary);
    Some(GeneratedCandidate {
        primary,
        constructor_fallback,
    })
}

/// The service segment of a generated type name's base
/// (`OrdersClient`, `OrdersStub`, `OrdersBlockingStub`), gated like a
/// type name. Suffixes come from shape data
/// (`generated_names.suffixes`) and are tried in order — the first
/// that strips wins, so `BlockingStub` precedes `Stub`.
fn service_segment_from(base: &str, shapes: &AnchorShapes) -> Option<String> {
    for suffix in shapes.generated_suffixes() {
        if let Some(s) = base.strip_suffix(suffix.as_str()) {
            if s.len() >= 3 && s.chars().next().map_or(false, |c| c.is_uppercase()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Identifier tokens of one line, dot-joined qualifiers included
/// (`pb.NewOrdersClient`, `OrdersGrpc.OrdersBlockingStub`) — the
/// shapes `generated_service_key` strips its suffixes from.
fn identifier_tokens(line: &str) -> Vec<((usize, usize), String)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        loop {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            // `.Name` continues the token (a dotted qualifier), but
            // `f()` or `x.` at end of line does not.
            if i + 1 < bytes.len()
                && bytes[i] == b'.'
                && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
            {
                i += 1;
                continue;
            }
            break;
        }
        out.push(((start, i), line[start..i].to_string()));
    }
    out
}

/// Normalize a URL-path literal to its anchor stem: id-like segments
/// (digits, `{}`/`{name}` holes, uuids, long hex) collapse away, the
/// rest join back into a path. `/v1/orders/{id}` and `/v1/orders/`
/// both become `/v1/orders` — consistent with the route matcher's
/// `{}` treatment (`route_match.rs`).
fn normalize_route_stem(literal: &str) -> Option<String> {
    let path = literal.trim_end_matches([',', ';', ')', ']', '}', '"', '\'']);
    if !path.starts_with('/') || path.len() < MIN_ROUTE_LEN || path.contains("//") {
        return None;
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = 0usize;
    for seg in path.split('/') {
        if seg.is_empty() {
            continue;
        }
        let sane = seg.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '%' | '{' | '}' | '*' | '+')
        });
        if !sane {
            return None;
        }
        if is_id_like_segment(seg) {
            dropped += 1;
            continue;
        }
        kept.push(seg);
    }
    let last = kept.last()?;
    if last.len() < 2 {
        return None;
    }
    // A single remaining word is too generic to anchor unless the
    // original path carried an id/hole segment (e.g. `/orders/{id}`).
    if kept.len() < 2 && dropped == 0 {
        return None;
    }
    Some(format!("/{}", kept.join("/")))
}

fn is_id_like_segment(seg: &str) -> bool {
    seg == "{}"
        || seg == "**"
        || seg == "*"
        || seg.starts_with('{')
        || seg.chars().all(|c| c.is_ascii_digit())
        || is_uuid(seg)
        || (seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_uuid(s: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == groups.len()
        && parts
            .iter()
            .zip(groups)
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Queue/topic name gate: word-shaped, separator-bearing names only.
fn plausible_name(s: &str) -> bool {
    s.len() >= MIN_NAME_LEN
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

/// Env-var shape gate: UPPER_SNAKE with an underscore, len ≥ 4.
fn is_env_shape(s: &str) -> bool {
    s.len() >= MIN_NAME_LEN
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && s.contains('_')
}

/// Whether the lowercased line carries queue context. The keyword
/// list is shape data (`anchors.contexts.queue`) — an org override
/// adds keywords without recompiling.
fn line_has_queue_context(low: &str, shapes: &AnchorShapes) -> bool {
    shapes
        .context_keys("queue")
        .iter()
        .any(|k| low.contains(k.as_str()))
}

/// Whether the lowercased line carries topic context (`anchors.
/// contexts.topic`).
fn line_has_topic_context(low: &str, shapes: &AnchorShapes) -> bool {
    shapes
        .context_keys("topic")
        .iter()
        .any(|k| low.contains(k.as_str()))
}

/// Byte ranges + contents of every single-quoted / double-quoted
/// string literal on the line (escapes kept simple: `\"` does not
/// terminate).
fn quoted_strings(line: &str) -> Vec<((usize, usize), String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let q = bytes[i];
        if q != b'"' && q != b'\'' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let mut content = String::new();
        let mut terminated = false;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\\' && i + 1 < bytes.len() {
                content.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == q {
                terminated = true;
                i += 1;
                break;
            }
            // A quote char other than the opener ends the scan window
            // (unclosed literal — skip it).
            if b == b'"' || b == b'\'' {
                break;
            }
            let ch = line[i..].chars().next().unwrap_or('?');
            content.push(ch);
            i += ch.len_utf8();
        }
        if terminated && !content.is_empty() {
            out.push(((start, i), content));
        }
    }
    out
}

/// Unquoted `/`-leading tokens that look like URL paths (bare YAML
/// keys, TOML values without quotes, comments).
fn unquoted_route_tokens(line: &str) -> Vec<((usize, usize), String)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'/'
            && (i == 0 || !is_token_char(bytes[i.saturating_sub(1)]))
            && i + 1 < bytes.len()
            && bytes[i + 1] != b'/'
            && bytes[i + 1] != b'*'
        {
            let start = i;
            let mut j = i + 1;
            while j < bytes.len() && is_route_char(bytes[j]) {
                j += 1;
            }
            let token = line[start..j].trim_end_matches([',', ';', ')', ']', '}', '"', '\'', ':']);
            if token.contains('/') && token.len() >= MIN_ROUTE_LEN {
                out.push(((start, start + token.len()), token.to_string()));
            }
            i = j.max(start + 1);
            continue;
        }
        i += 1;
    }
    out
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || b == b'_'
        || b == b'.'
        || b == b'/'
        || b == b'-'
        || b == b'{'
        || b == b'}'
        || b == b'%'
}

fn is_route_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'_' | b'-' | b'.' | b':' | b'%' | b'{' | b'}' | b'*' | b'+' | b'/'
        )
}

/// `.proto` structure lines.
fn proto_line_package(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("package ")?;
    let name = rest.split(';').next()?.trim();
    ident_like(name).then(|| name.to_string())
}

fn proto_line_service(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("service ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn proto_line_rpc(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("rpc ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty() && rest[name.len()..].trim_start().starts_with('(')).then_some(name)
}

fn ident_like(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Env-var extraction: find each idiom from `host_env_name`'s
/// grammar on the line and hand the full call expression back to
/// `host_env_name` — single source of truth with the sensors.
/// Returns `(byte span, env var name)` pairs.
///
/// The call-shape prefixes are shape data (`anchors.env_access`);
/// a prefix an org added via `<root>/.lain/patterns/anchors.yaml`
/// that `host_env_name`'s grammar does not know still extracts via
/// [`env_name_from_call`]'s generic fallback.
fn extract_env_from_line(line: &str, shapes: &AnchorShapes) -> Vec<((usize, usize), String)> {
    let mut out = Vec::new();
    for prefix in shapes.env_paren_prefixes() {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(prefix.as_str()) {
            let start = from + rel;
            let open_end = start + prefix.len();
            let Some(close_rel) = line[open_end..].find(')') else {
                break;
            };
            let end = open_end + close_rel + 1;
            let call = &line[start..end];
            if let Some(name) = env_name_from_call(call) {
                out.push(((start, end), name));
            }
            from = end.max(start + 1);
        }
    }
    for prefix in shapes.env_bracket_prefixes() {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(prefix.as_str()) {
            let start = from + rel;
            let open_end = start + prefix.len();
            let Some(close_rel) = line[open_end..].find(']') else {
                break;
            };
            let end = open_end + close_rel + 1;
            let call = &line[start..end];
            if let Some(name) = env_name_from_call(call) {
                out.push(((start, end), name));
            }
            from = end.max(start + 1);
        }
    }
    // Dot-identifier forms (`process.env.X`) — no brackets.
    for prefix in shapes.env_ident_prefixes() {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(prefix.as_str()) {
            let start = from + rel;
            let id_start = start + prefix.len();
            let id_end = line[id_start..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map(|n| id_start + n)
                .unwrap_or(line.len());
            if id_end > id_start {
                let call = &line[start..id_end];
                if let Some(name) = env_name_from_call(call) {
                    out.push(((start, id_end), name));
                }
            }
            from = id_end.max(start + 1);
        }
    }
    out.retain(|(_, name)| is_env_shape(name));
    out
}

/// The env-var name one call-shaped span expresses. `host_env_name`
/// owns the grammar of the bundled idioms (single source of truth
/// with the sensors and returns `Some` for every bundled shape, so
/// the fallback below is unreachable for them); a call shape an org
/// added through anchor-shape data falls back to its first quoted
/// literal (`env.require("X")`), or the trailing identifier for
/// dot shapes (`app.config.X`).
fn env_name_from_call(call: &str) -> Option<String> {
    if let Some(name) = host_env_name(call) {
        return Some(name.to_string());
    }
    if let Some((_, quoted)) = quoted_strings(call).into_iter().next() {
        return Some(quoted);
    }
    let ident = call.rsplit('.').next().unwrap_or(call);
    if !ident.is_empty() && ident.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(ident.to_string());
    }
    None
}
