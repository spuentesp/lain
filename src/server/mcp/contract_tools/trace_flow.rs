//! `trace_flow` — one path across the contract boundary
//! (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §12).
//!
//! Replaces stitching `get_call_chain` + `trace_impact` +
//! `get_cross_runtime_callers` by hand. From an endpoint, a symbol or a
//! table it walks the typed flow edges and returns the chains it found
//! plus an explicit list of **gaps** — the places the chain stops and why.
//! An empty or short chain is never presented as "nothing there".
//!
//! ## Direction
//!
//! The flow edges all point *toward the data*:
//! `component -Calls-> hook -Calls-> graphql-call -Binds-> provider
//! -CallsGraphql-> resolver -Calls-> helper -ReadsTable-> table`.
//! `downstream` follows them forward (toward the data), `upstream`
//! follows them backward (toward consumers).
//!
//! Edges walked: `Calls`, `CallsHttp`, `CallsGraphql`, `Binds`,
//! `SendsHttp`, `ReadsTable`, `WritesTable`. Heuristic edge kinds
//! (`DynamicDispatch`, `BusTopic`, `RouteMatches`) are not walked: their
//! direction semantics are not uniform, and a guessed hop would look
//! like an engine fact.
//!
//! ## Shape
//!
//! Two strategies, picked with `strategy`:
//!
//! - **`breadth`** (default): a BFS that reaches each node once (by its
//!   shortest path), so fan-out through helper calls cannot explode.
//!   Chains are root-to-leaf paths of that tree — "everything, nearest
//!   first".
//! - **`depth`**: a DFS that follows one chain to its end (a leaf, a data
//!   sink or the depth limit) before backtracking — "the full chain".
//!   Cycle safety is path-local, so two chains through a shared node are
//!   both reported. Neighbours are explored boundary edges first, then
//!   `Calls`, then data edges, ties by node id. Total work is bounded by
//!   the node budget and the walk stops at `cap + 1` chains.
//!
//! In both, chains are ranked by how many contract-boundary hops they
//! cross, then by weakest-edge confidence. `depth`, `cap` and
//! `node_budget` truncation are reported in `truncation_reasons`, never
//! silent.
//!
//! `line` on a hop is the stored `line_start`: indexer symbols store
//! tree-sitter rows (0-based), sensor-minted site nodes store 1-based
//! source lines.

use super::analysis::provenance_label;
use super::envelope::{check_api_version, error_outcome, outcome, success_envelope_with_view};
use super::view::resolve_view;
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::error::LainError;
use crate::federation::contracts::index::EndpointId;
use crate::federation::contracts::model::{ContractFact, ContractKey, ServiceName};
use crate::federation::graph_backend::GraphBackend;
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::time::Instant;

inventory::submit!(ContractToolEntry {
    name: "trace_flow",
    handler: trace_flow_handle,
});

/// Default and maximum hop depth.
const DEFAULT_DEPTH: u32 = 8;
const MAX_DEPTH: u32 = 12;
/// Default and maximum number of returned chains.
const DEFAULT_CAP: usize = 50;
const MAX_CAP: usize = 500;
/// Hard bound on nodes the BFS may reach. A bound hit is reported as
/// the `node_budget` truncation reason.
const NODE_BUDGET: usize = 50_000;
/// Ambiguous-symbol errors list at most this many candidates.
const MAX_CANDIDATES: usize = 20;

// ─── core (backend-only, no MCP context) ──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDirection {
    /// Follow edges forward: toward resolvers, helpers and tables.
    Downstream,
    /// Follow edges backward: toward consumers, hooks and components.
    Upstream,
}

/// How the walk explores the flow edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlowStrategy {
    /// BFS, each node reached once by its shortest path.
    #[default]
    Breadth,
    /// DFS, full chains with path-local cycle safety.
    Depth,
}

impl FlowStrategy {
    fn label(self) -> &'static str {
        match self {
            FlowStrategy::Breadth => "breadth",
            FlowStrategy::Depth => "depth",
        }
    }
}

impl FlowDirection {
    fn label(self) -> &'static str {
        match self {
            FlowDirection::Downstream => "downstream",
            FlowDirection::Upstream => "upstream",
        }
    }
}

/// One node on a chain, with the edge that led to it (`None` for the seed).
#[derive(Debug, Clone)]
pub struct FlowHop {
    pub node: GraphNode,
    pub edge: Option<GraphEdge>,
}

#[derive(Debug, Clone)]
pub struct FlowPath {
    pub hops: Vec<FlowHop>,
    pub min_confidence: f32,
    pub boundary_hops: usize,
}

/// A place the chain stops, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowGap {
    pub node_id: String,
    pub name: String,
    pub kind: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FlowResult {
    pub paths: Vec<FlowPath>,
    pub gaps: Vec<FlowGap>,
    pub truncation_reasons: Vec<&'static str>,
    pub nodes_visited: usize,
    pub strategy: FlowStrategy,
}

fn is_flow_edge(t: &EdgeType) -> bool {
    matches!(
        t,
        EdgeType::Calls
            | EdgeType::CallsHttp
            | EdgeType::CallsGraphql
            | EdgeType::Binds
            | EdgeType::SendsHttp
            | EdgeType::ReadsTable
            | EdgeType::WritesTable
    )
}

fn is_data_edge(t: &EdgeType) -> bool {
    matches!(t, EdgeType::ReadsTable | EdgeType::WritesTable)
}

/// A hop that crosses a contract boundary: the edge kinds that only exist
/// at a boundary, or a `Calls` edge into/out of a synthetic
/// `graphql-call:` consumer node (the generated-document link).
fn crosses_boundary(edge: &GraphEdge, from: &GraphNode, to: &GraphNode) -> bool {
    matches!(
        edge.edge_type,
        EdgeType::Binds
            | EdgeType::CallsGraphql
            | EdgeType::CallsHttp
            | EdgeType::SendsHttp
            | EdgeType::ReadsTable
            | EdgeType::WritesTable
    ) || is_graphql_call(from)
        || is_graphql_call(to)
}

fn is_graphql_call(n: &GraphNode) -> bool {
    n.name
        .starts_with(crate::server::sensors::util::GRAPHQL_CALL_PREFIX)
}

fn edge_confidence(e: &GraphEdge) -> f32 {
    let from_provenance = provenance_label(e.provenance.as_ref())["confidence"]
        .as_f64()
        .unwrap_or(0.0) as f32;
    match e.weight {
        Some(w) => from_provenance.min(w),
        None => from_provenance,
    }
}

/// `true` when the edge is a deterministic engine fact (static or
/// confirmed), `false` for heuristic / runtime / provenance-less edges.
fn is_engine_fact(e: &GraphEdge) -> bool {
    matches!(
        e.provenance,
        Some(EdgeProvenance::Static { .. }) | Some(EdgeProvenance::Confirmed { .. })
    )
}

struct Adjacency {
    out: HashMap<String, Vec<GraphEdge>>,
    inc: HashMap<String, Vec<GraphEdge>>,
}

fn build_adjacency(backend: &dyn GraphBackend) -> Result<Adjacency, LainError> {
    let mut out: HashMap<String, Vec<GraphEdge>> = HashMap::new();
    let mut inc: HashMap<String, Vec<GraphEdge>> = HashMap::new();
    for e in backend.all_edges()? {
        if !is_flow_edge(&e.edge_type) || e.source_id == e.target_id {
            continue;
        }
        out.entry(e.source_id.clone()).or_default().push(e.clone());
        inc.entry(e.target_id.clone()).or_default().push(e);
    }
    // Deterministic order regardless of backend iteration order.
    for v in out.values_mut() {
        v.sort_by(|a, b| {
            (&a.target_id, format!("{:?}", a.edge_type))
                .cmp(&(&b.target_id, format!("{:?}", b.edge_type)))
        });
    }
    for v in inc.values_mut() {
        v.sort_by(|a, b| {
            (&a.source_id, format!("{:?}", a.edge_type))
                .cmp(&(&b.source_id, format!("{:?}", b.edge_type)))
        });
    }
    Ok(Adjacency { out, inc })
}

fn expand_edges<'a>(adj: &'a Adjacency, forward: bool, id: &str) -> &'a [GraphEdge] {
    let map = if forward { &adj.out } else { &adj.inc };
    map.get(id).map(Vec::as_slice).unwrap_or(&[])
}

/// Walk the flow edges from `seeds` breadth-first and report chains and gaps.
pub fn compute_flow(
    backend: &dyn GraphBackend,
    seeds: &[String],
    direction: FlowDirection,
    depth: u32,
    cap: usize,
) -> Result<FlowResult, LainError> {
    compute_flow_with(
        backend,
        seeds,
        direction,
        depth,
        cap,
        FlowStrategy::Breadth,
        NODE_BUDGET,
    )
}

/// Walk the flow edges from `seeds` with an explicit `strategy` and node
/// budget (the budget bounds nodes reached for `breadth`, node entries for
/// `depth`).
pub fn compute_flow_with(
    backend: &dyn GraphBackend,
    seeds: &[String],
    direction: FlowDirection,
    depth: u32,
    cap: usize,
    strategy: FlowStrategy,
    node_budget: usize,
) -> Result<FlowResult, LainError> {
    let adj = build_adjacency(backend)?;
    match strategy {
        FlowStrategy::Breadth => {
            flow_breadth(backend, &adj, seeds, direction, depth, cap, node_budget)
        }
        FlowStrategy::Depth => flow_depth(backend, &adj, seeds, direction, depth, cap, node_budget),
    }
}

#[allow(clippy::too_many_arguments)]
fn flow_breadth(
    backend: &dyn GraphBackend,
    adj: &Adjacency,
    seeds: &[String],
    direction: FlowDirection,
    depth: u32,
    cap: usize,
    node_budget: usize,
) -> Result<FlowResult, LainError> {
    let forward = direction == FlowDirection::Downstream;
    // The edges we expand from a node, and the neighbour each one reaches.
    let expand = |id: &str| expand_edges(adj, forward, id);
    let neighbour = |e: &GraphEdge| -> String {
        if forward {
            e.target_id.clone()
        } else {
            e.source_id.clone()
        }
    };

    let mut nodes: HashMap<String, GraphNode> = HashMap::new();
    let mut gaps: Vec<FlowGap> = Vec::new();
    let get = |id: &str, nodes: &mut HashMap<String, GraphNode>| -> Option<GraphNode> {
        if let Some(n) = nodes.get(id) {
            return Some(n.clone());
        }
        let n = backend.get_node(id).ok().flatten()?;
        nodes.insert(id.to_string(), n.clone());
        Some(n)
    };

    // BFS tree: node id -> (parent id, edge that reached it, depth).
    let mut parent: HashMap<String, (String, GraphEdge)> = HashMap::new();
    let mut depth_of: HashMap<String, u32> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut truncation: BTreeSet<&'static str> = BTreeSet::new();
    let mut seed_ids: Vec<String> = Vec::new();
    for s in seeds {
        if depth_of.contains_key(s) {
            continue;
        }
        if get(s, &mut nodes).is_none() {
            continue;
        }
        depth_of.insert(s.clone(), 0);
        order.push(s.clone());
        queue.push_back(s.clone());
        seed_ids.push(s.clone());
    }

    while let Some(id) = queue.pop_front() {
        let d = depth_of[&id];
        if d >= depth {
            continue;
        }
        for e in expand(&id) {
            let next = neighbour(e);
            if depth_of.contains_key(&next) {
                continue;
            }
            if order.len() >= node_budget {
                truncation.insert("node_budget");
                break;
            }
            if get(&next, &mut nodes).is_none() {
                gaps.push(FlowGap {
                    node_id: next.clone(),
                    name: next.clone(),
                    kind: "dangling_edge",
                    reason: format!(
                        "{:?} edge from {id} points at a node the view does not hold",
                        e.edge_type
                    ),
                });
                continue;
            }
            depth_of.insert(next.clone(), d + 1);
            parent.insert(next.clone(), (id.clone(), e.clone()));
            order.push(next.clone());
            queue.push_back(next);
        }
    }

    // Depth truncation: a node on the depth frontier that still reaches
    // a node the walk never visited.
    for id in &order {
        if depth_of[id] == depth
            && expand(id)
                .iter()
                .any(|e| !depth_of.contains_key(&neighbour(e)))
        {
            truncation.insert("depth");
            break;
        }
    }

    // Leaves of the BFS tree, then root-to-leaf paths.
    let parents: HashSet<&String> = parent.values().map(|(p, _)| p).collect();
    let mut paths: Vec<FlowPath> = Vec::new();
    for id in &order {
        if parents.contains(id) {
            continue;
        }
        let mut chain: Vec<(String, Option<GraphEdge>)> = Vec::new();
        let mut cur = id.clone();
        loop {
            match parent.get(&cur) {
                Some((p, e)) => {
                    chain.push((cur.clone(), Some(e.clone())));
                    cur = p.clone();
                }
                None => {
                    chain.push((cur.clone(), None));
                    break;
                }
            }
        }
        chain.reverse();
        paths.push(build_path(&chain, &nodes));
    }
    rank_paths(&mut paths);
    if paths.len() > cap {
        paths.truncate(cap);
        truncation.insert("cap");
    }

    let reached_data = parent.values().any(|(_, e)| is_data_edge(&e.edge_type));
    gaps.extend(find_gaps(
        direction,
        &order,
        &nodes,
        adj,
        &seed_ids,
        reached_data,
    ));
    gaps.sort_by(|a, b| (a.kind, &a.node_id).cmp(&(b.kind, &b.node_id)));
    gaps.dedup();

    Ok(FlowResult {
        paths,
        gaps,
        truncation_reasons: truncation.into_iter().collect(),
        nodes_visited: order.len(),
        strategy: FlowStrategy::Breadth,
    })
}

/// Turn a root-to-leaf `chain` of `(node id, edge that reached it)` into a
/// [`FlowPath`], skipping nodes the cache does not hold.
fn build_path(
    chain: &[(String, Option<GraphEdge>)],
    nodes: &HashMap<String, GraphNode>,
) -> FlowPath {
    let mut hops: Vec<FlowHop> = Vec::with_capacity(chain.len());
    let mut min_confidence = 1.0_f32;
    let mut boundary_hops = 0usize;
    for (i, (nid, edge)) in chain.iter().enumerate() {
        let Some(node) = nodes.get(nid).cloned() else {
            continue;
        };
        if let Some(e) = edge {
            min_confidence = min_confidence.min(edge_confidence(e));
            if let Some(prev) = i.checked_sub(1).and_then(|j| nodes.get(&chain[j].0)) {
                if crosses_boundary(e, prev, &node) {
                    boundary_hops += 1;
                }
            }
        }
        hops.push(FlowHop {
            node,
            edge: edge.clone(),
        });
    }
    FlowPath {
        hops,
        min_confidence,
        boundary_hops,
    }
}

/// Boundary hops desc, weakest-edge confidence desc, length desc, leaf id.
fn rank_paths(paths: &mut [FlowPath]) {
    paths.sort_by(|a, b| {
        b.boundary_hops
            .cmp(&a.boundary_hops)
            .then_with(|| {
                b.min_confidence
                    .partial_cmp(&a.min_confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.hops.len().cmp(&a.hops.len()))
            .then_with(|| leaf_id(a).cmp(leaf_id(b)))
    });
}

/// Exploration priority of an edge in `depth` mode: contract-boundary
/// edges first, then plain calls, then the data edges that end a chain.
fn edge_priority(t: &EdgeType) -> u8 {
    match t {
        EdgeType::CallsGraphql => 0,
        EdgeType::Binds => 1,
        EdgeType::CallsHttp => 2,
        EdgeType::SendsHttp => 3,
        EdgeType::Calls => 4,
        EdgeType::ReadsTable => 5,
        EdgeType::WritesTable => 6,
        _ => 7,
    }
}

struct DfsState<'a> {
    backend: &'a dyn GraphBackend,
    adj: &'a Adjacency,
    forward: bool,
    depth: u32,
    cap: usize,
    node_budget: usize,
    nodes: HashMap<String, GraphNode>,
    gaps: Vec<FlowGap>,
    truncation: BTreeSet<&'static str>,
    /// Distinct node ids touched, in first-touch order.
    order: Vec<String>,
    seen: HashSet<String>,
    /// Node entries so far (the budget counter).
    steps: usize,
    reached_data: bool,
    paths: Vec<FlowPath>,
    stop: bool,
}

impl DfsState<'_> {
    fn node(&mut self, id: &str) -> Option<GraphNode> {
        if let Some(n) = self.nodes.get(id) {
            return Some(n.clone());
        }
        let n = self.backend.get_node(id).ok().flatten()?;
        self.nodes.insert(id.to_string(), n.clone());
        Some(n)
    }

    fn neighbour(&self, e: &GraphEdge) -> String {
        if self.forward {
            e.target_id.clone()
        } else {
            e.source_id.clone()
        }
    }

    /// Visit `id`, reached over `via`; `chain` holds the path so far and
    /// `on_path` its ids (the cycle guard).
    fn visit(
        &mut self,
        id: &str,
        via: Option<GraphEdge>,
        chain: &mut Vec<(String, Option<GraphEdge>)>,
        on_path: &mut HashSet<String>,
    ) {
        if self.stop {
            return;
        }
        if self.steps >= self.node_budget {
            self.truncation.insert("node_budget");
            self.stop = true;
            return;
        }
        self.steps += 1;
        if self.seen.insert(id.to_string()) {
            self.order.push(id.to_string());
        }
        chain.push((id.to_string(), via));
        on_path.insert(id.to_string());

        let d = (chain.len() - 1) as u32;
        let mut edges: Vec<GraphEdge> = expand_edges(self.adj, self.forward, id).to_vec();
        edges.sort_by(|a, b| {
            (edge_priority(&a.edge_type), self.neighbour(a))
                .cmp(&(edge_priority(&b.edge_type), self.neighbour(b)))
        });
        // Neighbours that are real nodes and not already on this path.
        let mut next: Vec<(String, GraphEdge)> = Vec::new();
        for e in edges {
            let n = self.neighbour(&e);
            if on_path.contains(&n) {
                continue;
            }
            if self.node(&n).is_none() {
                self.gaps.push(FlowGap {
                    node_id: n.clone(),
                    name: n.clone(),
                    kind: "dangling_edge",
                    reason: format!(
                        "{:?} edge from {id} points at a node the view does not hold",
                        e.edge_type
                    ),
                });
                continue;
            }
            next.push((n, e));
        }

        if d >= self.depth {
            if !next.is_empty() {
                self.truncation.insert("depth");
            }
            self.emit(chain);
        } else if next.is_empty() {
            self.emit(chain);
        } else {
            for (n, e) in next {
                if self.stop {
                    break;
                }
                if is_data_edge(&e.edge_type) {
                    self.reached_data = true;
                }
                self.visit(&n, Some(e), chain, on_path);
            }
        }

        on_path.remove(id);
        chain.pop();
    }

    fn emit(&mut self, chain: &[(String, Option<GraphEdge>)]) {
        // `cap + 1` chains prove the cap was hit; stop collecting there.
        if self.paths.len() > self.cap {
            self.stop = true;
            return;
        }
        let path = build_path(chain, &self.nodes);
        self.paths.push(path);
        if self.paths.len() > self.cap {
            self.truncation.insert("cap");
            self.stop = true;
        }
    }
}

/// Depth-first walk: full root-to-leaf chains, path-local cycle safety,
/// bounded by the node budget and `cap + 1` chains.
#[allow(clippy::too_many_arguments)]
fn flow_depth(
    backend: &dyn GraphBackend,
    adj: &Adjacency,
    seeds: &[String],
    direction: FlowDirection,
    depth: u32,
    cap: usize,
    node_budget: usize,
) -> Result<FlowResult, LainError> {
    let mut st = DfsState {
        backend,
        adj,
        forward: direction == FlowDirection::Downstream,
        depth,
        cap,
        node_budget,
        nodes: HashMap::new(),
        gaps: Vec::new(),
        truncation: BTreeSet::new(),
        order: Vec::new(),
        seen: HashSet::new(),
        steps: 0,
        reached_data: false,
        paths: Vec::new(),
        stop: false,
    };
    let mut seed_ids: Vec<String> = Vec::new();
    for s in seeds {
        if seed_ids.contains(s) {
            continue;
        }
        if st.node(s).is_none() {
            continue;
        }
        seed_ids.push(s.clone());
    }
    for s in &seed_ids {
        let mut chain = Vec::new();
        let mut on_path = HashSet::new();
        st.visit(s, None, &mut chain, &mut on_path);
    }

    let mut paths = std::mem::take(&mut st.paths);
    rank_paths(&mut paths);
    if paths.len() > cap {
        paths.truncate(cap);
        st.truncation.insert("cap");
    }
    let mut gaps = std::mem::take(&mut st.gaps);
    gaps.extend(find_gaps(
        direction,
        &st.order,
        &st.nodes,
        adj,
        &seed_ids,
        st.reached_data,
    ));
    gaps.sort_by(|a, b| (a.kind, &a.node_id).cmp(&(b.kind, &b.node_id)));
    gaps.dedup();
    Ok(FlowResult {
        paths,
        gaps,
        truncation_reasons: st.truncation.into_iter().collect(),
        nodes_visited: st.order.len(),
        strategy: FlowStrategy::Depth,
    })
}

fn leaf_id(p: &FlowPath) -> &str {
    p.hops.last().map(|h| h.node.id.as_str()).unwrap_or("")
}

/// The contract-endpoint label of a provider node, if it is one.
fn provider_label(n: &GraphNode) -> Option<String> {
    n.contract.iter().find_map(|f| match f {
        ContractFact::GraphqlProvider(p) => Some(format!("graphql:{}:{}", p.op, p.field)),
        ContractFact::Provider(_) => Some(n.name.clone()),
        _ => None,
    })
}

fn is_consumer(n: &GraphNode) -> bool {
    n.node_type == NodeType::HttpClientCall
        || is_graphql_call(n)
        || n.contract.iter().any(|f| {
            matches!(
                f,
                ContractFact::Consumer(_) | ContractFact::GraphqlConsumer(_)
            )
        })
}

fn has_edge(map: &HashMap<String, Vec<GraphEdge>>, id: &str, types: &[EdgeType]) -> bool {
    map.get(id)
        .is_some_and(|es| es.iter().any(|e| types.contains(&e.edge_type)))
}

/// Gaps are judged on the nodes the walk reached, against the *full*
/// adjacency in both directions: "this provider has no resolver edge" is
/// a statement about the graph, whichever way we walked.
fn find_gaps(
    direction: FlowDirection,
    visited: &[String],
    nodes: &HashMap<String, GraphNode>,
    adj: &Adjacency,
    seeds: &[String],
    reached_data: bool,
) -> Vec<FlowGap> {
    let mut gaps = Vec::new();
    let mut saw_function = false;
    for id in visited {
        let Some(n) = nodes.get(id) else { continue };
        if matches!(n.node_type, NodeType::Function | NodeType::Method) && !is_graphql_call(n) {
            saw_function = true;
        }
        let mut push = |kind: &'static str, reason: String| {
            gaps.push(FlowGap {
                node_id: id.clone(),
                name: n.name.clone(),
                kind,
                reason,
            });
        };
        if let Some(label) = provider_label(n) {
            match direction {
                FlowDirection::Downstream => {
                    if !has_edge(&adj.out, id, &[EdgeType::CallsGraphql, EdgeType::CallsHttp]) {
                        push(
                            "no_handler",
                            format!(
                                "no resolver/handler edge for {label}: the framework is not \
                                 recognised or the implementing function is not indexed"
                            ),
                        );
                    }
                }
                FlowDirection::Upstream => {
                    if !has_edge(&adj.inc, id, &[EdgeType::Binds]) {
                        push(
                            "no_consumers",
                            format!(
                                "no bound consumers for {label}: none exist, or they live in \
                                 repos this federation does not index"
                            ),
                        );
                    }
                }
            }
        }
        if is_consumer(n) {
            match direction {
                FlowDirection::Downstream => {
                    if !has_edge(&adj.out, id, &[EdgeType::Binds]) {
                        push(
                            "unbound_consumer",
                            "consumer call site is not bound to any provider".to_string(),
                        );
                    }
                }
                FlowDirection::Upstream => {
                    if !has_edge(&adj.inc, id, &[EdgeType::Calls, EdgeType::SendsHttp]) {
                        push(
                            "unlinked_call_site",
                            "synthetic call node has no linked function: nothing in the \
                             indexed code was matched to it (generated client code may not \
                             be committed)"
                                .to_string(),
                        );
                    }
                }
            }
        }
        if n.node_type == NodeType::Table
            && direction == FlowDirection::Upstream
            && !has_edge(&adj.inc, id, &[EdgeType::ReadsTable, EdgeType::WritesTable])
        {
            push(
                "no_table_access",
                "no function reads or writes this table: only literal SQL is indexed \
                 (ORM queries are not)"
                    .to_string(),
            );
        }
    }
    if direction == FlowDirection::Downstream && saw_function {
        let seeded_table = seeds
            .iter()
            .any(|s| nodes.get(s).is_some_and(|n| n.node_type == NodeType::Table));
        if !seeded_table && !reached_data {
            gaps.push(FlowGap {
                node_id: seeds.first().cloned().unwrap_or_default(),
                name: nodes
                    .get(seeds.first().map(String::as_str).unwrap_or(""))
                    .map(|n| n.name.clone())
                    .unwrap_or_default(),
                kind: "no_data_sink",
                reason: "no table reached: ReadsTable/WritesTable edges exist only for \
                         literal SQL, so ORM queries (Django, SQLAlchemy, Prisma) are not \
                         indexed as a data sink"
                    .to_string(),
            });
        }
    }
    gaps
}

// ─── MCP handler ──────────────────────────────────────────────────────

pub fn trace_flow_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let started = Instant::now();
    Box::pin(async move {
        if let Err(details) = check_api_version(&args_map) {
            return Ok(error_outcome(
                "unsupported_api_version",
                "api_version not served",
                Some(details),
                "live",
                started,
            ));
        }
        match run_trace_flow(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

fn snapshot_label(args: &Map<String, Value>) -> String {
    args.get("snapshot")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "live".to_string())
}

/// What `from` resolved to: the seed node ids plus the direction to use
/// when the caller did not pick one.
#[derive(Debug)]
struct Seeds {
    ids: Vec<String>,
    default_direction: FlowDirection,
}

async fn run_trace_flow(
    ctx: &McpContext<'_>,
    args: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snap = snapshot_label(&args);
    let invalid = |msg: String, details: Value| {
        error_outcome("invalid_argument", msg, Some(details), &snap, started)
    };
    let from = args.get("from").and_then(Value::as_object).ok_or_else(|| {
        invalid(
            "missing required argument: from".into(),
            json!({"arg": "from"}),
        )
    })?;

    let depth = args
        .get("depth")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(DEFAULT_DEPTH);
    if depth == 0 || depth > MAX_DEPTH {
        return Err(error_outcome(
            "range_too_large",
            format!("depth {depth} outside 1..={MAX_DEPTH}"),
            Some(json!({"limit": depth, "max": MAX_DEPTH, "requested": depth})),
            &snap,
            started,
        ));
    }
    let cap = args
        .get("cap")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_CAP);
    if cap == 0 || cap > MAX_CAP {
        return Err(error_outcome(
            "range_too_large",
            format!("cap {cap} outside 1..={MAX_CAP}"),
            Some(json!({"limit": cap, "max": MAX_CAP, "requested": cap})),
            &snap,
            started,
        ));
    }
    let explicit_direction = match args.get("direction").and_then(Value::as_str) {
        None => None,
        Some("downstream") => Some(FlowDirection::Downstream),
        Some("upstream") => Some(FlowDirection::Upstream),
        Some(other) => {
            return Err(invalid(
                format!("invalid direction: {other:?}"),
                json!({"arg": "direction", "expected": ["downstream", "upstream"]}),
            ));
        }
    };

    let strategy = match args.get("strategy").and_then(Value::as_str) {
        None | Some("breadth") => FlowStrategy::Breadth,
        Some("depth") => FlowStrategy::Depth,
        Some(other) => {
            return Err(invalid(
                format!("invalid strategy: {other:?}"),
                json!({"arg": "strategy", "expected": ["breadth", "depth"]}),
            ));
        }
    };

    let present = ["endpoint", "symbol", "table"]
        .iter()
        .filter(|k| from.get(**k).is_some_and(|v| !v.is_null()))
        .count();
    if present != 1 {
        return Err(invalid(
            "exactly one of endpoint, symbol, table is required".into(),
            json!({"arg": "from"}),
        ));
    }

    let resolved = resolve_view(ctx, &args, started).await?;
    let backend = resolved.backend();

    let seeds = if let Some(ep) = from.get("endpoint").filter(|v| !v.is_null()) {
        let service = ep.get("service").and_then(Value::as_str).unwrap_or("");
        let key_raw = ep.get("key").and_then(Value::as_str).unwrap_or("");
        let key = ContractKey::from_str(key_raw).map_err(|e| {
            invalid(
                format!("malformed endpoint.key: {e}"),
                json!({"arg": "from.endpoint.key"}),
            )
        })?;
        let index = resolved.index().cloned().ok_or_else(|| {
            error_outcome(
                "contract_not_found",
                "no contract index available for this view",
                Some(json!({"endpoint": null})),
                &snap,
                started,
            )
        })?;
        let id: EndpointId = (ServiceName(service.to_string()), key);
        let endpoint = index.endpoints.get(&id).ok_or_else(|| {
            error_outcome(
                "contract_not_found",
                "endpoint is not present in the selected view",
                Some(json!({"endpoint": {"service": service, "key": key_raw}})),
                &snap,
                started,
            )
        })?;
        Seeds {
            ids: endpoint
                .providers
                .iter()
                .map(|p| p.node_id.as_str().to_string())
                .collect(),
            default_direction: FlowDirection::Downstream,
        }
    } else if let Some(sym) = from.get("symbol").and_then(Value::as_str) {
        let repo = from.get("repo_id").and_then(Value::as_str);
        resolve_symbol_seed(backend.as_ref(), sym, repo)
            .map_err(|e| e.into_outcome(&snap, started))?
    } else {
        let table = from.get("table").and_then(Value::as_str).unwrap_or("");
        resolve_table_seed(backend.as_ref(), table).map_err(|e| e.into_outcome(&snap, started))?
    };

    let direction = explicit_direction.unwrap_or(seeds.default_direction);
    let mut result = compute_flow_with(
        backend.as_ref(),
        &seeds.ids,
        direction,
        depth,
        cap,
        strategy,
        NODE_BUDGET,
    )
    .map_err(|e| {
        error_outcome(
            "invalid_argument",
            format!("trace_flow failed: {e}"),
            None,
            &snap,
            started,
        )
    })?;
    mark_pending_gaps(&mut result, resolved.scope());

    let data = flow_to_value(
        &Value::Object(from.clone()),
        direction,
        &seeds.ids,
        &result,
        resolved.scope(),
    );
    let text = render_trace_flow(&data);
    let envelope = success_envelope_with_view(
        data.clone(),
        resolved.label(),
        resolved.reproducible(),
        resolved.view_info(),
        started,
    );
    Ok(outcome(envelope, &data, text))
}

/// A seed-resolution failure, mapped onto the existing error codes (the
/// golden suite pins one test per code, so no new code is introduced).
#[derive(Debug)]
enum SeedError {
    NotFound {
        what: &'static str,
        value: String,
        hint: &'static str,
    },
    Ambiguous {
        symbol: String,
        candidates: Vec<Value>,
    },
}

impl SeedError {
    fn into_outcome(self, snap: &str, started: Instant) -> ToolOutcome {
        match self {
            SeedError::NotFound { what, value, hint } => error_outcome(
                "contract_not_found",
                format!("{what} {value:?} not found in the selected view"),
                Some(json!({ what: value, "hint": hint })),
                snap,
                started,
            ),
            SeedError::Ambiguous { symbol, candidates } => error_outcome(
                "invalid_argument",
                format!("symbol {symbol:?} matches several nodes; pass a node id or `repo_id`"),
                Some(json!({
                    "arg": "from.symbol",
                    "reason": "ambiguous_symbol",
                    "candidates": candidates,
                })),
                snap,
                started,
            ),
        }
    }
}

fn repo_of(id: &str) -> String {
    GlobalId::parse(id)
        .map(|g| g.repo_id().to_string())
        .unwrap_or_default()
}

fn candidate_value(n: &GraphNode) -> Value {
    json!({
        "id": n.id,
        "node_type": format!("{:?}", n.node_type),
        "path": n.path,
        "line": n.line_start,
        "repo": repo_of(&n.id),
    })
}

fn resolve_symbol_seed(
    backend: &dyn GraphBackend,
    symbol: &str,
    repo: Option<&str>,
) -> Result<Seeds, SeedError> {
    let not_found = || SeedError::NotFound {
        what: "symbol",
        value: symbol.to_string(),
        hint: "pass a node id from query_graph/find_symbol, or a name with `repo_id`",
    };
    let by_id = backend.get_node(symbol).ok().flatten();
    let node = if let Some(n) = by_id {
        n
    } else {
        let mut matches: Vec<GraphNode> = backend
            .find_nodes_by_name(symbol)
            .map_err(|_| not_found())?
            .into_iter()
            .filter(|n| repo.is_none_or(|r| repo_of(&n.id) == r))
            .collect();
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        match matches.len() {
            0 => return Err(not_found()),
            1 => matches.remove(0),
            _ => {
                return Err(SeedError::Ambiguous {
                    symbol: symbol.to_string(),
                    candidates: matches
                        .iter()
                        .take(MAX_CANDIDATES)
                        .map(candidate_value)
                        .collect(),
                });
            }
        }
    };
    Ok(Seeds {
        ids: vec![node.id],
        default_direction: FlowDirection::Downstream,
    })
}

fn resolve_table_seed(backend: &dyn GraphBackend, table: &str) -> Result<Seeds, SeedError> {
    let name = table.strip_prefix("table:").unwrap_or(table);
    let mut ids: Vec<String> = backend
        .find_nodes_by_name(name)
        .map_err(|_| SeedError::NotFound {
            what: "table",
            value: table.to_string(),
            hint: "",
        })?
        .into_iter()
        .filter(|n| n.node_type == NodeType::Table)
        .map(|n| n.id)
        .collect();
    ids.sort();
    if ids.is_empty() {
        return Err(SeedError::NotFound {
            what: "table",
            value: table.to_string(),
            hint: "no table node: only tables named in literal SQL are indexed; \
                   ORM models (Django, SQLAlchemy, Prisma) are not",
        });
    }
    Ok(Seeds {
        ids,
        default_direction: FlowDirection::Upstream,
    })
}

/// Repos the view lists as `unreviewed` (still indexing / not ready),
/// as `"repo (reason)"` labels, sorted.
fn pending_repos(scope: &Value) -> Vec<String> {
    let mut out: Vec<String> = scope["unreviewed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let repo = u["repo"].as_str()?;
            Some(match u["reason"].as_str() {
                Some(reason) => format!("{repo} ({reason})"),
                None => repo.to_string(),
            })
        })
        .collect();
    out.sort();
    out
}

/// A contract with no bound consumers while repos are still indexing is
/// not evidence that none exist. Re-label those gaps `consumers_pending`
/// (retry once the repos are ready) and name the repos; with every repo
/// reviewed the `no_consumers` gap is left exactly as it was.
fn mark_pending_gaps(r: &mut FlowResult, scope: &Value) {
    let pending = pending_repos(scope);
    if pending.is_empty() {
        return;
    }
    let mut changed = false;
    for g in r.gaps.iter_mut().filter(|g| g.kind == "no_consumers") {
        g.kind = "consumers_pending";
        g.reason = format!(
            "no bound consumers seen yet, but these repos are still indexing: {} — \
             retry once they are ready before concluding none exist",
            pending.join(", ")
        );
        changed = true;
    }
    if changed {
        r.gaps
            .sort_by(|a, b| (a.kind, &a.node_id).cmp(&(b.kind, &b.node_id)));
    }
}

fn hop_to_value(h: &FlowHop) -> Value {
    let (edge_type, provenance, engine_fact) = match &h.edge {
        Some(e) => (
            json!(format!("{:?}", e.edge_type)),
            provenance_label(e.provenance.as_ref()),
            is_engine_fact(e),
        ),
        // The seed was named by the caller, not inferred.
        None => (Value::Null, Value::Null, true),
    };
    json!({
        "id": h.node.id,
        "name": h.node.name,
        "node_type": format!("{:?}", h.node.node_type),
        "repo": repo_of(&h.node.id),
        "path": h.node.path,
        "line": h.node.line_start,
        "edge_type": edge_type,
        "provenance": provenance,
        "engine_fact": engine_fact,
    })
}

fn flow_to_value(
    from: &Value,
    direction: FlowDirection,
    seeds: &[String],
    r: &FlowResult,
    scope: &Value,
) -> Value {
    let paths: Vec<Value> = r
        .paths
        .iter()
        .map(|p| {
            json!({
                "min_confidence": p.min_confidence,
                "boundary_hops": p.boundary_hops,
                "hops": p.hops.iter().map(hop_to_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let gaps: Vec<Value> = r
        .gaps
        .iter()
        .map(|g| {
            json!({
                "node": g.node_id,
                "name": g.name,
                "kind": g.kind,
                "reason": g.reason,
            })
        })
        .collect();
    let mut data = json!({
        "from": from,
        "direction": direction.label(),
        "seeds": seeds,
        "paths": paths,
        "gaps": gaps,
        "truncated": !r.truncation_reasons.is_empty(),
        "truncation_reasons": r.truncation_reasons,
        "nodes_visited": r.nodes_visited,
        "scope": scope,
    });
    // Both keys are omitted in the default case, so a default `breadth`
    // call on a fully indexed view keeps its original output shape.
    if r.strategy != FlowStrategy::Breadth {
        data["strategy"] = json!(r.strategy.label());
    }
    let pending = pending_repos(scope);
    if !pending.is_empty() {
        data["repos_not_ready"] = json!(pending);
    }
    data
}

fn render_trace_flow(data: &Value) -> String {
    let paths = data["paths"].as_array().cloned().unwrap_or_default();
    let gaps = data["gaps"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "# trace_flow {} ({} path(s), {} gap(s){})\n",
        data["direction"].as_str().unwrap_or(""),
        paths.len(),
        gaps.len(),
        if data["truncated"].as_bool().unwrap_or(false) {
            ", truncated"
        } else {
            ""
        }
    );
    for p in paths.iter().take(5) {
        let mut line = String::from("- ");
        for (i, h) in p["hops"].as_array().into_iter().flatten().enumerate() {
            if i > 0 {
                let solid = if h["engine_fact"].as_bool().unwrap_or(false) {
                    "-"
                } else {
                    "~"
                };
                line.push_str(&format!(
                    " {solid}[{}]{solid}> ",
                    h["edge_type"].as_str().unwrap_or("?")
                ));
            }
            line.push_str(h["name"].as_str().unwrap_or("?"));
        }
        line.push('\n');
        out.push_str(&line);
    }
    let by_kind: BTreeMap<String, usize> = gaps.iter().fold(BTreeMap::new(), |mut m, g| {
        *m.entry(g["kind"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
        m
    });
    for (kind, n) in by_kind {
        let example = gaps
            .iter()
            .find(|g| g["kind"].as_str() == Some(kind.as_str()))
            .and_then(|g| g["reason"].as_str())
            .unwrap_or("");
        out.push_str(&format!("gap {kind} x{n}: {example}\n"));
    }
    out
}

#[cfg(test)]
#[path = "trace_flow_tests.rs"]
mod tests;
