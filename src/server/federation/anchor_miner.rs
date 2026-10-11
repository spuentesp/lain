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

/// Directories the scanner never descends into.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "third_party",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".lain",
    ".lain-data",
    "coverage",
    ".next",
];

/// File extensions the scanner reads (source + the config formats
/// structural names live in: `amqp.toml`, `openapi.yaml`, `.env`).
const SCAN_EXTS: &[&str] = &[
    "rs",
    "py",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "go",
    "java",
    "kt",
    "kts",
    "rb",
    "cs",
    "php",
    "proto",
    "toml",
    "yaml",
    "yml",
    "json",
    "env",
    "properties",
    "cfg",
    "ini",
    "tf",
    "scala",
];

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
    /// (or `pkg.Service/Method`), or env var name.
    pub key: String,
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
}

/// True for edges minted by this pass (recognized by detector name).
/// The projection reconciliation in `project_edges` skips these so a
/// watcher-triggered reproject never retracts them; the next full
/// index build re-mints fresh.
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
pub fn mint_cross_repo_observations(
    backend: &dyn GraphBackend,
    occurrences: &[AnchorOccurrence],
    commits: &[CommitRecord],
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
    let mut minted: HashSet<(String, String, EdgeType)> = HashSet::new();
    for e in &existing {
        minted.insert((
            e.source_id.clone(),
            e.target_id.clone(),
            e.edge_type.clone(),
        ));
    }

    let mut index = build_node_index(backend.list_nodes()?);

    // ── group occurrences by (kind, key) ────────────────────────────
    let mut groups: BTreeMap<(AnchorKind, String), Vec<&AnchorOccurrence>> = BTreeMap::new();
    for occ in occurrences {
        groups
            .entry((occ.kind, occ.key.clone()))
            .or_default()
            .push(occ);
    }
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
        for i in 0..repos.len() {
            for j in (i + 1)..repos.len() {
                for a in &per_repo[repos[i]] {
                    for b in &per_repo[repos[j]] {
                        if blocked.contains(&pair_key(a, b)) {
                            continue;
                        }
                        for (s, t) in [(a.clone(), b.clone()), (b.clone(), a.clone())] {
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
                &mut new_edges,
                &cluster,
            );
        }
    }

    // ── Detector 3: commit-message references ───────────────────────
    for c in commits {
        let subject_anchors = extract_subject_anchors(&c.message);
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

    // Materialize synthesized file endpoints before the edges that
    // point at them — the backend drops edges with missing endpoints.
    for (gid, path, name) in new_file_nodes {
        backend.upsert_node_global(&gid, NodeType::File, &path, &name)?;
    }
    if !new_edges.is_empty() {
        backend.upsert_edges_batch(&new_edges)?;
    }
    Ok(report)
}

fn pair_key(a: &str, b: &str) -> [String; 2] {
    if a <= b {
        [a.to_string(), b.to_string()]
    } else {
        [b.to_string(), a.to_string()]
    }
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
/// not a coupling — and is skipped.
fn process_cochange_cluster(
    report: &mut MintReport,
    index: &NodeIndex,
    blocked: &BTreeSet<[String; 2]>,
    minted: &mut HashSet<(String, String, EdgeType)>,
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
pub fn scan_repo_occurrences(repo: &RepoId, local_path: &Path) -> Vec<AnchorOccurrence> {
    let mut out = Vec::new();
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
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
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
            if !SCAN_EXTS.contains(&ext) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_SCAN_FILE_BYTES {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(local_path) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.extend(extract_from_text(repo.as_str(), &rel, &content));
        }
    }
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
fn extract_from_text(repo: &str, path: &str, content: &str) -> Vec<AnchorOccurrence> {
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
        out.extend(extract_from_line(repo, path, line_no, line));
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
    }
}

/// Extract every anchor from one line of any supported language.
fn extract_from_line(repo: &str, path: &str, line_no: u32, line: &str) -> Vec<AnchorOccurrence> {
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
    for (range, name) in extract_env_from_line(line) {
        consumed.push(range);
        push(AnchorKind::Env, name.to_string(), &mut out);
    }

    let low = line.to_lowercase();
    let queue_ctx = line_has_queue_context(&low);
    let topic_ctx = line_has_topic_context(&low);

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
    out
}

/// Anchors a commit SUBJECT mentions. The subject is free text, so
/// the shape gates run without line context: a bare `order-events`
/// counts as a queue-or-topic mention (whichever group exists
/// decides at mint time).
fn extract_subject_anchors(subject: &str) -> Vec<(AnchorKind, String)> {
    let mut out: Vec<(AnchorKind, String)> = Vec::new();
    let mut seen: HashSet<(AnchorKind, String)> = HashSet::new();
    let mut push = |kind: AnchorKind, key: String| {
        if key.len() >= MIN_NAME_LEN && seen.insert((kind, key.clone())) {
            out.push((kind, key));
        }
    };
    for name in extract_env_from_line(subject).into_iter().map(|(_, n)| n) {
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

fn line_has_queue_context(low: &str) -> bool {
    const KEYS: &[&str] = &[
        "queue",
        "amqp",
        "pika",
        "routing_key",
        "basic_consume",
        "basic_publish",
        "rabbit",
    ];
    KEYS.iter().any(|k| low.contains(k))
}

fn line_has_topic_context(low: &str) -> bool {
    const KEYS: &[&str] = &[
        "topic",
        "kafka",
        "kafkajs",
        "rdkafka",
        "confluent",
        "pubsub",
        "publish",
        "subscribe",
        "producer",
        "consumer",
        "firehose",
        "futurerecord",
        "nats",
        "sns",
        "sqs",
    ];
    KEYS.iter().any(|k| low.contains(k))
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
fn extract_env_from_line(line: &str) -> Vec<((usize, usize), String)> {
    const PAREN_PREFIXES: &[&str] = &[
        "os.environ.get(",
        "os.getenv(",
        "os.Getenv(",
        "System.getenv(",
    ];
    let mut out = Vec::new();
    for prefix in PAREN_PREFIXES {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(prefix) {
            let start = from + rel;
            let open_end = start + prefix.len();
            let Some(close_rel) = line[open_end..].find(')') else {
                break;
            };
            let end = open_end + close_rel + 1;
            let call = &line[start..end];
            if let Some(name) = host_env_name(call) {
                out.push(((start, end), name.to_string()));
            }
            from = end.max(start + 1);
        }
    }
    for prefix in ["os.environ[", "process.env["] {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(prefix) {
            let start = from + rel;
            let open_end = start + prefix.len();
            let Some(close_rel) = line[open_end..].find(']') else {
                break;
            };
            let end = open_end + close_rel + 1;
            let call = &line[start..end];
            if let Some(name) = host_env_name(call) {
                out.push(((start, end), name.to_string()));
            }
            from = end.max(start + 1);
        }
    }
    // `process.env.X` — identifier form, no brackets.
    let prefix = "process.env.";
    let mut from = 0usize;
    while let Some(rel) = line[from..].find(prefix) {
        let start = from + rel;
        let id_start = start + prefix.len();
        let id_end = line[id_start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|n| id_start + n)
            .unwrap_or(line.len());
        if id_end > id_start {
            let call = &line[start..id_end];
            if let Some(name) = host_env_name(call) {
                out.push(((start, id_end), name.to_string()));
            }
        }
        from = id_end.max(start + 1);
    }
    out.retain(|(_, name)| is_env_shape(name));
    out
}
