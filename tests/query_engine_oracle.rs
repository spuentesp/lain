//! Differential oracle for the query engine: the fast executor must agree
//! with a naive reference evaluator on every random (graph, query) pair.
//!
//! A consumer audit found `docs/query-language.md` promising behavior the
//! engine did not deliver (`find.id` inert, `{"not": [...]}` unreachable on
//! the wire, labels other than `deprecated` unmatchable). This harness makes
//! that class of bug loud: generate a small random graph and a random valid
//! `QuerySpec`, evaluate it twice — once through the real `Executor`, once
//! through a deliberately naive reference (full scan, no indexes, plain BFS)
//! that implements the documented semantics — and assert the two agree on
//! the set of returned node ids (order is a separate concern), on the walked
//! edge set, and on the exact selected path of every reported node.
//!
//! Every generated spec is pushed through its JSON wire form before either
//! side sees it, so wire-shape bugs are in scope: a selector form that
//! cannot survive `serde_json` round-trip fails here even if the in-memory
//! enum variant works.
//!
//! **Excluded ops (for now):** `sort`, `group`, `limit`, and
//! `semantic_filter`. Ordering, grouping, pagination and embedding
//! semantics are not pinned by this oracle yet — the generator only emits
//! `find` / `filter` / `connect`. The executor's default result cap is
//! disabled in the harness (`with_default_limit(.., 0)`) so the comparison
//! is always on full, pre-truncation sets.

use lain::graph::GraphDatabase;
use lain::nlp::NlpEmbedder;
use lain::query::executor::Executor;
use lain::query::spec::{
    ConnectOp, DepthSpec, Direction, EdgeSelector, FilterOp, FindOp, GraphOp, LabelSelector,
    LimitOp, NameSelector, QueryResult, QuerySpec, TypeSelector,
};
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use parking_lot::Mutex;
use petgraph::Direction as PetDirection;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

// ─── Reference evaluator ─────────────────────────────────────────────
//
// Implements the semantics `docs/query-language.md` describes, with no
// shortcuts: every predicate scans the full node set, every traversal is a
// plain breadth-first search over the full neighbour lists.
//
// * `find` / `filter` / `target` match on `type`, `name`, `id`, `label`,
//   `path` exactly as documented. A node's query label is `"deprecated"`
//   when `is_deprecated` is set, else its `label` field — matching the
//   producers, which surface `is_deprecated` as the `deprecated` label.
// * `connect` reports a node when its **shortest** matching-edge path from
//   some seed is inside the reported hop range `max(min, 1)..=max`
//   (`depth: N` means hops 1..=N; start nodes sit at hop 0 and are never
//   reported). `direction: both` is the union of a pure-outgoing and a
//   pure-incoming walk, each with its own shortest-path depths — never a
//   path that mixes directions.
// * `paths` holds, per reported node, the selected shortest matching-edge
//   path from the seed of the walk that reached it: fewest hops, then the
//   outgoing walk over the incoming one, then the lowest seed id, then at
//   each hop the lowest-id equal-distance predecessor (and, among parallel
//   matching edges on that pair, the smallest edge type name). `edges` is
//   the deduplicated union of the edges on those paths.

/// A node's label as the query language exposes it.
fn query_label(node: &GraphNode) -> Option<&str> {
    if node.is_deprecated {
        Some("deprecated")
    } else {
        node.label.as_deref()
    }
}

fn name_matches(sel: &NameSelector, name: &str) -> bool {
    sel.matches(name)
}

fn find_matches(find: &FindOp, node: &GraphNode) -> bool {
    if let Some(sel) = &find.type_selector {
        if !sel.matches(&node.node_type.to_string()) {
            return false;
        }
    }
    if let Some(sel) = &find.name {
        if !name_matches(sel, &node.name) {
            return false;
        }
    }
    if let Some(want) = &find.id {
        if want != &node.id {
            return false;
        }
    }
    if let Some(sel) = &find.label_selector {
        if !sel.matches(query_label(node)) {
            return false;
        }
    }
    if let Some(want) = &find.path {
        if !node.path.contains(want.as_str()) {
            return false;
        }
    }
    true
}

/// Plain BFS from `start` over edges whose type `edge_sel` matches.
/// Returns the shortest hop count to every reached node. The start seeds
/// the visited set at hop 0, so it is never re-reported through a cycle
/// or a self-loop — "start nodes are never reported".
fn bfs_hops(
    graph: &GraphDatabase,
    start: &str,
    direction: PetDirection,
    edge_sel: &EdgeSelector,
) -> HashMap<String, u32> {
    let mut hops: HashMap<String, u32> = HashMap::new();
    hops.insert(start.to_string(), 0);
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(start.to_string());
    while let Some(current) = queue.pop_front() {
        let depth = hops[&current];
        for (neighbour, edge) in graph.get_neighbors(&current, direction) {
            if !edge_sel.matches(&edge.edge_type.to_string()) {
                continue;
            }
            if hops.contains_key(&neighbour.id) {
                continue;
            }
            hops.insert(neighbour.id.clone(), depth + 1);
            queue.push_back(neighbour.id.clone());
        }
    }
    hops
}

/// The selected shortest path for one reported node: the walk's node ids in
/// walk order (seed first, reported node last) and one `(from, to, type)`
/// edge triple per hop, in the edge's stored direction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefPath {
    seed: String,
    /// `true` when the path was taken from the pure-incoming walk. Used only
    /// for selection (equal hops prefer the outgoing walk); never mixed.
    incoming: bool,
    nodes: Vec<String>,
    edges: Vec<(String, String, String)>,
}

impl RefPath {
    fn hops(&self) -> u32 {
        self.nodes.len() as u32 - 1
    }
}

/// An edge's wire identity: from-node id, to-node id, type name.
fn edge_triple(edge: &GraphEdge) -> (String, String, String) {
    (
        edge.source_id.clone(),
        edge.target_id.clone(),
        edge.edge_type.to_string(),
    )
}

/// Reconstruct the selected shortest path from `seed` to `target` inside one
/// pure walk, walking hops backwards. Each hop goes to the **lowest-id**
/// predecessor sitting at the next-lower hop count — the documented
/// tie-break — and among parallel matching edges on that pair the
/// lexicographically smallest edge type name.
fn ref_path(
    graph: &GraphDatabase,
    seed: &str,
    target: &str,
    hops: &HashMap<String, u32>,
    direction: PetDirection,
    edge_sel: &EdgeSelector,
) -> RefPath {
    // The walk moves `current -> neighbour` along `direction`; walking it
    // backwards flips the direction.
    let backwards = match direction {
        PetDirection::Outgoing => PetDirection::Incoming,
        PetDirection::Incoming => PetDirection::Outgoing,
    };
    let mut nodes = vec![target.to_string()];
    let mut edges = Vec::new();
    let mut current = target.to_string();
    while current != seed {
        let depth = hops[&current];
        let mut best: Option<(String, (String, String, String))> = None;
        for (pred, edge) in graph.get_neighbors(&current, backwards) {
            if !edge_sel.matches(&edge.edge_type.to_string()) {
                continue;
            }
            if hops.get(&pred.id).copied() != Some(depth - 1) {
                continue;
            }
            let key = (pred.id.clone(), edge_triple(&edge));
            let better = match &best {
                None => true,
                Some((best_pred, best_edge)) => {
                    (key.0.as_str(), key.1 .2.as_str()) < (best_pred.as_str(), best_edge.2.as_str())
                }
            };
            if better {
                best = Some(key);
            }
        }
        let (pred_id, triple) = best.expect("BFS guarantees a shortest-path predecessor");
        edges.push(triple);
        nodes.push(pred_id.clone());
        current = pred_id;
    }
    nodes.reverse();
    edges.reverse();
    RefPath {
        seed: seed.to_string(),
        incoming: matches!(direction, PetDirection::Incoming),
        nodes,
        edges,
    }
}

/// A connect step as the reference sees it: the reported node ids and the
/// selected shortest path per node. A node is reported when some pure walk
/// places it inside the reported hop range; its path is the best such walk
/// by (fewest hops, outgoing walk before incoming, lowest seed id).
fn connect_reference(
    graph: &GraphDatabase,
    seeds: &[String],
    connect: &ConnectOp,
) -> (Vec<String>, HashMap<String, RefPath>) {
    let range = connect.depth.to_range();
    let lo = (*range.start()).max(1);
    let hi = *range.end();
    let directions: Vec<PetDirection> = match connect.direction {
        Direction::Both => vec![PetDirection::Outgoing, PetDirection::Incoming],
        Direction::Outgoing => vec![PetDirection::Outgoing],
        Direction::Incoming => vec![PetDirection::Incoming],
    };

    let mut walks: Vec<(String, PetDirection, HashMap<String, u32>)> = Vec::new();
    for seed in seeds {
        for &direction in &directions {
            walks.push((
                seed.clone(),
                direction,
                bfs_hops(graph, seed, direction, &connect.edge),
            ));
        }
    }

    let mut paths: HashMap<String, RefPath> = HashMap::new();
    for (seed, direction, hops) in &walks {
        let incoming = matches!(direction, PetDirection::Incoming);
        for (id, &h) in hops {
            if h < lo || h > hi {
                continue;
            }
            // Selection key: fewest hops, then the outgoing walk, then the
            // lowest seed id (`false` sorts before `true`, so `incoming`
            // loses every equal-hop tie).
            let key = (h, incoming, seed.as_str());
            let better = match paths.get(id) {
                None => true,
                Some(prev) => key < (prev.hops(), prev.incoming, prev.seed.as_str()),
            };
            if better {
                paths.insert(
                    id.clone(),
                    ref_path(graph, seed, id, hops, *direction, &connect.edge),
                );
            }
        }
    }

    let mut nodes: Vec<String> = paths.keys().cloned().collect();
    nodes.sort();
    if let Some(target) = connect.target.as_deref() {
        nodes.retain(|id| {
            graph
                .get_node(id)
                .ok()
                .flatten()
                .is_some_and(|n| find_matches(target, &n))
        });
        let keep: BTreeSet<&str> = nodes.iter().map(String::as_str).collect();
        paths.retain(|id, _| keep.contains(id.as_str()));
    }
    (nodes, paths)
}

/// Evaluate a whole `QuerySpec` the slow, obvious way. Returns the ids of
/// the final working set and the selected path of every reported node.
fn reference_eval(
    graph: &GraphDatabase,
    spec: &QuerySpec,
) -> (BTreeSet<String>, HashMap<String, RefPath>) {
    let mut current: Vec<String> = Vec::new();
    let mut paths: HashMap<String, RefPath> = HashMap::new();
    for op in &spec.ops {
        match op {
            GraphOp::Find(find) => {
                current = graph
                    .get_all_nodes()
                    .into_iter()
                    .filter(|n| find_matches(find, n))
                    .map(|n| n.id)
                    .collect();
                paths.clear();
            }
            GraphOp::Filter(filter) => {
                current.retain(|id| {
                    let Ok(Some(node)) = graph.get_node(id) else {
                        return false;
                    };
                    if let Some(sel) = &filter.type_filter {
                        if !sel.matches(&node.node_type.to_string()) {
                            return false;
                        }
                    }
                    if let Some(sel) = &filter.label_filter {
                        if !sel.matches(query_label(&node)) {
                            return false;
                        }
                    }
                    if let Some(sel) = &filter.name {
                        if !name_matches(sel, &node.name) {
                            return false;
                        }
                    }
                    true
                });
            }
            GraphOp::Connect(connect) => {
                let (nodes, found) = connect_reference(graph, &current, connect);
                current = nodes;
                paths = found;
            }
            // Excluded from the generator; a spec carrying one is not a
            // valid oracle input.
            GraphOp::SemanticFilter(_)
            | GraphOp::Group(_)
            | GraphOp::Sort(_)
            | GraphOp::Limit(_) => {
                panic!("oracle input must not contain {:?}", op)
            }
        }
    }
    // Paths describe the reported nodes only; a filter step drops nodes
    // without dropping their neighbours' paths from the map.
    let reported: BTreeSet<&str> = current.iter().map(String::as_str).collect();
    paths.retain(|id, _| reported.contains(id.as_str()));
    (current.into_iter().collect(), paths)
}

// ─── Graph + query generators ────────────────────────────────────────

const NODE_TYPES: [NodeType; 6] = [
    NodeType::Function,
    NodeType::Method,
    NodeType::Class,
    NodeType::Struct,
    NodeType::File,
    NodeType::Module,
];

const EDGE_TYPES: [EdgeType; 5] = [
    EdgeType::Calls,
    EdgeType::Contains,
    EdgeType::Uses,
    EdgeType::CoChangedWith,
    EdgeType::Imports,
];

const LABEL_VALUES: [&str; 5] = ["deprecated", "test", "async_trait", "no_mangle", "missing"];

/// How a generated node gets its label, in the shapes the producers use.
#[derive(Debug, Clone, Copy)]
enum LabelShape {
    /// No label at all.
    None,
    /// LSP-style deprecated: the flag alone, `label` unset.
    DeprecatedFlagOnly,
    /// Tree-sitter-style deprecated: flag plus the `deprecated` label.
    DeprecatedLabelled,
    /// `#[test]` / test-container style: flag clear, label set.
    Attr(&'static str),
}

#[derive(Debug, Clone)]
struct NodeSpec {
    node_type: usize,
    name: String,
    path: String,
    label: LabelShape,
}

#[derive(Debug, Clone)]
struct EdgeSpec {
    from: usize,
    to: usize,
    edge_type: usize,
}

#[derive(Debug, Clone)]
struct GraphSpec {
    nodes: Vec<NodeSpec>,
    edges: Vec<EdgeSpec>,
}

#[derive(Debug, Clone)]
enum NamePlan {
    Exact(String),
    Glob(String),
    StartsWith(String),
    EndsWith(String),
}

impl NamePlan {
    fn materialize(&self) -> NameSelector {
        match self {
            NamePlan::Exact(s) => NameSelector::Exact(s.clone()),
            NamePlan::Glob(s) => NameSelector::Glob(s.clone()),
            NamePlan::StartsWith(s) => NameSelector::StartsWith(s.clone()),
            NamePlan::EndsWith(s) => NameSelector::EndsWith(s.clone()),
        }
    }
}

#[derive(Debug, Clone)]
enum TypePlan {
    One(usize),
    Or(usize, usize),
}

impl TypePlan {
    fn materialize(&self) -> TypeSelector {
        match *self {
            TypePlan::One(i) => TypeSelector::Single(NODE_TYPES[i].to_string()),
            TypePlan::Or(a, b) => {
                TypeSelector::Or(vec![NODE_TYPES[a].to_string(), NODE_TYPES[b].to_string()])
            }
        }
    }
}

#[derive(Debug, Clone)]
enum LabelPlan {
    One(&'static str),
    Or(&'static str, &'static str),
    Not(&'static str, &'static str),
}

impl LabelPlan {
    fn materialize(&self) -> LabelSelector {
        match *self {
            LabelPlan::One(s) => LabelSelector::Single(s.to_string()),
            LabelPlan::Or(a, b) => LabelSelector::Or(vec![a.to_string(), b.to_string()]),
            LabelPlan::Not(a, b) => LabelSelector::Not(vec![a.to_string(), b.to_string()]),
        }
    }
}

#[derive(Debug, Clone)]
enum IdPlan {
    /// Match one of the generated nodes by index.
    Existing(usize),
    /// An id no node has.
    Bogus,
}

#[derive(Debug, Clone)]
enum EdgePlan {
    One(usize),
    Or(usize, usize),
    Not(usize, usize),
}

impl EdgePlan {
    fn materialize(&self) -> EdgeSelector {
        match *self {
            EdgePlan::One(i) => EdgeSelector::Single(EDGE_TYPES[i].to_string()),
            EdgePlan::Or(a, b) => {
                EdgeSelector::Or(vec![EDGE_TYPES[a].to_string(), EDGE_TYPES[b].to_string()])
            }
            EdgePlan::Not(a, b) => {
                EdgeSelector::Not(vec![EDGE_TYPES[a].to_string(), EDGE_TYPES[b].to_string()])
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FindPlan {
    type_sel: Option<TypePlan>,
    name: Option<NamePlan>,
    id: Option<IdPlan>,
    label: Option<LabelPlan>,
    path: Option<String>,
}

impl FindPlan {
    fn materialize(&self, ids: &[String]) -> FindOp {
        FindOp {
            type_selector: self.type_sel.as_ref().map(TypePlan::materialize),
            name: self.name.as_ref().map(NamePlan::materialize),
            id: self.id.as_ref().map(|p| match p {
                IdPlan::Existing(i) => ids[*i % ids.len()].clone(),
                IdPlan::Bogus => "no-such-node-id".to_string(),
            }),
            label_selector: self.label.as_ref().map(LabelPlan::materialize),
            path: self.path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
enum OpPlan {
    Filter {
        type_sel: Option<TypePlan>,
        label: Option<LabelPlan>,
        name: Option<NamePlan>,
    },
    Connect {
        edge: EdgePlan,
        direction: u8,
        /// (min, max) hop bounds; `max < min` is allowed and selects nothing.
        depth: (u32, u32),
        single: bool,
        target: Option<FindPlan>,
    },
}

#[derive(Debug, Clone)]
struct QueryPlan {
    find: FindPlan,
    tail: Vec<OpPlan>,
}

impl QueryPlan {
    fn materialize(&self, ids: &[String]) -> QuerySpec {
        let mut ops = vec![GraphOp::Find(self.find.materialize(ids))];
        for op in &self.tail {
            ops.push(match op {
                OpPlan::Filter {
                    type_sel,
                    label,
                    name,
                } => GraphOp::Filter(FilterOp {
                    type_filter: type_sel.as_ref().map(TypePlan::materialize),
                    label_filter: label.as_ref().map(LabelPlan::materialize),
                    name: name.as_ref().map(NamePlan::materialize),
                }),
                OpPlan::Connect {
                    edge,
                    direction,
                    depth,
                    single,
                    target,
                } => GraphOp::Connect(ConnectOp {
                    edge: edge.materialize(),
                    direction: match direction {
                        0 => Direction::Outgoing,
                        1 => Direction::Incoming,
                        _ => Direction::Both,
                    },
                    depth: if *single {
                        DepthSpec::Single(depth.1)
                    } else {
                        DepthSpec::Range {
                            min: depth.0,
                            max: depth.1,
                        }
                    },
                    target: target.as_ref().map(|t| Box::new(t.materialize(ids))),
                }),
            });
        }
        QuerySpec::new(ops)
    }
}

fn arb_name() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,3}",
        "foo",
        "bar_1",
        "test_x",
        "y_test",
        "a.b",
        "handler",
        "\\*x",
        "f\\?o",
    ]
}

fn arb_name_plan() -> BoxedStrategy<Option<NamePlan>> {
    prop_oneof![
        Just(None),
        arb_name().prop_map(|s| Some(NamePlan::Exact(s))),
        "[a-z*?]{1,4}".prop_map(|s| Some(NamePlan::Glob(s))),
        ("[a-z]{0,3}", "x").prop_map(|(p, _)| Some(NamePlan::StartsWith(p))),
        ("y", "[a-z]{0,3}").prop_map(|(_, s)| Some(NamePlan::EndsWith(s))),
    ]
    .boxed()
}

fn arb_type_plan() -> BoxedStrategy<Option<TypePlan>> {
    prop_oneof![
        Just(None),
        (0..NODE_TYPES.len()).prop_map(|i| Some(TypePlan::One(i))),
        (0..NODE_TYPES.len(), 0..NODE_TYPES.len()).prop_map(|(a, b)| Some(TypePlan::Or(a, b))),
    ]
    .boxed()
}

fn arb_label_plan() -> BoxedStrategy<Option<LabelPlan>> {
    prop_oneof![
        Just(None),
        (0..LABEL_VALUES.len()).prop_map(|i| Some(LabelPlan::One(LABEL_VALUES[i]))),
        (0..LABEL_VALUES.len(), 0..LABEL_VALUES.len())
            .prop_map(|(a, b)| Some(LabelPlan::Or(LABEL_VALUES[a], LABEL_VALUES[b]))),
        (0..LABEL_VALUES.len(), 0..LABEL_VALUES.len())
            .prop_map(|(a, b)| Some(LabelPlan::Not(LABEL_VALUES[a], LABEL_VALUES[b]))),
    ]
    .boxed()
}

fn arb_id_plan() -> BoxedStrategy<Option<IdPlan>> {
    prop_oneof![
        Just(None),
        (0..8usize).prop_map(|i| Some(IdPlan::Existing(i))),
        Just(Some(IdPlan::Bogus)),
    ]
    .boxed()
}

fn arb_find_plan() -> impl Strategy<Value = FindPlan> {
    let free = (
        arb_type_plan(),
        arb_name_plan(),
        arb_id_plan(),
        arb_label_plan(),
        prop_oneof![
            Just(None),
            Just(Some("src".to_string())),
            Just(Some("f1".to_string()))
        ],
    )
        .prop_map(|(type_sel, name, id, label, path)| FindPlan {
            type_sel,
            name,
            id,
            label,
            path,
        });
    // A plan anchored on one real node id, with every other selector clear.
    // The free flavour above usually selects nothing (its independent
    // name/type/label clauses rarely agree on one node), and an oracle that
    // never seeds a traversal never compares edges or paths at all.
    let anchored = (0..8usize).prop_map(|i| FindPlan {
        type_sel: None,
        name: None,
        id: Some(IdPlan::Existing(i)),
        label: None,
        path: None,
    });
    prop_oneof![free, anchored]
}

fn arb_op_plan() -> impl Strategy<Value = OpPlan> {
    prop_oneof![
        (arb_type_plan(), arb_label_plan(), arb_name_plan()).prop_map(|(type_sel, label, name)| {
            OpPlan::Filter {
                type_sel,
                label,
                name,
            }
        }),
        (
            prop_oneof![
                (0..EDGE_TYPES.len()).prop_map(EdgePlan::One),
                (0..EDGE_TYPES.len(), 0..EDGE_TYPES.len()).prop_map(|(a, b)| EdgePlan::Or(a, b)),
                (0..EDGE_TYPES.len(), 0..EDGE_TYPES.len()).prop_map(|(a, b)| EdgePlan::Not(a, b)),
            ],
            0..3u8,
            (0u32..4, 0u32..5),
            any::<bool>(),
            prop_oneof![Just(None), arb_find_plan().prop_map(Some)],
        )
            .prop_map(|(edge, direction, depth, single, target)| OpPlan::Connect {
                edge,
                direction,
                depth,
                single,
                target,
            }),
    ]
}

fn arb_query_plan() -> impl Strategy<Value = QueryPlan> {
    (arb_find_plan(), prop::collection::vec(arb_op_plan(), 0..3))
        .prop_map(|(find, tail)| QueryPlan { find, tail })
}

fn arb_label_shape() -> impl Strategy<Value = LabelShape> {
    prop_oneof![
        Just(LabelShape::None),
        Just(LabelShape::DeprecatedFlagOnly),
        Just(LabelShape::DeprecatedLabelled),
        Just(LabelShape::Attr("test")),
        Just(LabelShape::Attr("async_trait")),
        Just(LabelShape::Attr("no_mangle")),
    ]
}

fn arb_node_spec(i: usize) -> impl Strategy<Value = NodeSpec> {
    (
        0..NODE_TYPES.len(),
        arb_name(),
        Just(format!("src/f{i}.rs")),
        arb_label_shape(),
    )
        .prop_map(|(node_type, name, path, label)| NodeSpec {
            node_type,
            name,
            path,
            label,
        })
}

fn arb_graph_spec() -> impl Strategy<Value = GraphSpec> {
    (1usize..12).prop_flat_map(|n| {
        (
            proptest::collection::vec(arb_node_spec(0), n..=n),
            proptest::collection::vec((0..n, 0..n, 0..EDGE_TYPES.len()), 0..16),
        )
            .prop_map(|(nodes, edges)| {
                // Distinct paths, so `GraphNode::new` ids stay distinct.
                let nodes: Vec<NodeSpec> = nodes
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut s)| {
                        s.path = format!("src/f{i}.rs");
                        s
                    })
                    .collect();
                let edges = edges
                    .into_iter()
                    .map(|(from, to, edge_type)| EdgeSpec {
                        from,
                        to,
                        edge_type,
                    })
                    .collect();
                GraphSpec { nodes, edges }
            })
    })
}

// ─── Building and running a case ─────────────────────────────────────

fn build_graph(spec: &GraphSpec) -> (GraphDatabase, Vec<String>, PathBuf) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "lain_query_oracle_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    cleanup_scratch(&dir);
    let graph = GraphDatabase::new(&dir).unwrap();

    let mut ids = Vec::new();
    for node in &spec.nodes {
        let mut n = GraphNode::new(
            NODE_TYPES[node.node_type].clone(),
            node.name.clone(),
            node.path.clone(),
        );
        match node.label {
            LabelShape::None => {}
            LabelShape::DeprecatedFlagOnly => n.is_deprecated = true,
            LabelShape::DeprecatedLabelled => {
                n.is_deprecated = true;
                n.label = Some("deprecated".to_string());
            }
            LabelShape::Attr(l) => n.label = Some(l.to_string()),
        }
        ids.push(n.id.clone());
        graph.upsert_node(n).unwrap();
    }
    for edge in &spec.edges {
        graph
            .insert_edge(&GraphEdge::new(
                EDGE_TYPES[edge.edge_type].clone(),
                ids[edge.from].clone(),
                ids[edge.to].clone(),
            ))
            .unwrap();
    }
    (graph, ids, dir)
}

fn run_case(graph_spec: &GraphSpec, query_plan: &QueryPlan) {
    let (graph, ids, dir) = build_graph(graph_spec);
    let spec = query_plan.materialize(&ids);

    // Wire round-trip first: both sides must see the same spec that a JSON
    // client actually sent. A form that cannot survive this is dead on the
    // wire no matter what the in-memory enum can express.
    let wire = serde_json::to_value(&spec).expect("QuerySpec serializes");
    let spec: QuerySpec =
        serde_json::from_value(wire.clone()).expect("QuerySpec survives its JSON wire form");
    assert_eq!(
        serde_json::to_value(&spec).unwrap(),
        wire,
        "QuerySpec must round-trip through JSON unchanged"
    );

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    // Cap disabled: the oracle compares full, pre-truncation sets.
    let mut exec =
        Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let result = exec.execute(&spec).expect("executor accepts a valid spec");
    let meta = result.meta.clone().expect("executor always reports meta");
    assert!(
        !meta.truncated,
        "cap is disabled in the harness; nothing may truncate: {:?}",
        meta
    );

    let fast: BTreeSet<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
    let (slow, slow_paths) = reference_eval(&graph, &spec);
    assert_eq!(
        fast, slow,
        "executor and reference disagree on node ids\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}"
    );

    // Paths: exactly one per reported node when the reference has any,
    // parallel to `nodes`, and equal to the reference's selected path —
    // node sequence and edge sequence, seed first.
    assert_eq!(
        result.paths.is_empty(),
        slow_paths.is_empty(),
        "paths must appear exactly when the reference has them\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}\nfast paths: {:?}\nslow paths: {:?}",
        result.paths, slow_paths
    );
    if !slow_paths.is_empty() {
        assert_eq!(
            result.paths.len(),
            result.nodes.len(),
            "paths parallel nodes\ngraph: {graph_spec:?}\nquery: {query_plan:?}"
        );
        for (node, path) in result.nodes.iter().zip(result.paths.iter()) {
            let want = slow_paths
                .get(&node.id)
                .expect("reference has a path for every reported node");
            let got_nodes: Vec<&str> = path.nodes.iter().map(|n| n.id.as_str()).collect();
            assert_eq!(
                got_nodes, want.nodes,
                "path node sequence for {}\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}",
                node.id
            );
            let got_edges: Vec<(String, String, String)> = path
                .edges
                .iter()
                .map(|e| (e.from.clone(), e.to.clone(), e.edge_type.clone()))
                .collect();
            assert_eq!(
                got_edges, want.edges,
                "path edge sequence for {}\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}",
                node.id
            );
            assert_eq!(
                path.nodes.last().map(|n| n.id.as_str()),
                Some(node.id.as_str()),
                "paths[i] must end at nodes[i]"
            );
        }
    }

    // Edges: the deduplicated union of the reported path edges, in the
    // documented (from, to, type) sort order, with the stable composite id.
    let mut slow_edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    for p in slow_paths.values() {
        slow_edges.extend(p.edges.iter().cloned());
    }
    let fast_edges: BTreeSet<(String, String, String)> = result
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.edge_type.clone()))
        .collect();
    assert_eq!(
        fast_edges, slow_edges,
        "executor and reference disagree on walked edges\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}"
    );
    for (a, b) in result.edges.iter().zip(result.edges.iter().skip(1)) {
        assert!(
            (a.from.as_str(), a.to.as_str(), a.edge_type.as_str())
                < (b.from.as_str(), b.to.as_str(), b.edge_type.as_str()),
            "edges must be sorted by (from, to, type) and deduplicated: {a:?} then {b:?}"
        );
    }
    for e in &result.edges {
        assert_eq!(
            e.id,
            format!("{}->{}:{}", e.from, e.to, e.edge_type),
            "edge id must be the (from, to, type) key"
        );
    }

    cleanup_scratch(&dir);
}

// ─── Deterministic regressions for the audited bugs ──────────────────

/// Remove a scratch graph directory *and* the WAL `GraphDatabase` leaves
/// beside it (`<path>.wal` — a sibling file, see
/// `graph::wal::wal_path_for`). `remove_dir_all` alone leaks the WAL into
/// the temp dir; one leaked file per case is enough to exhaust a quota.
fn cleanup_scratch(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    let mut wal = dir.as_os_str().to_owned();
    wal.push(".wal");
    let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
}

fn scratch_graph(name: &str) -> (GraphDatabase, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "lain_query_oracle_scratch_{}_{}",
        std::process::id(),
        name
    ));
    cleanup_scratch(&dir);
    (GraphDatabase::new(&dir).unwrap(), dir)
}

fn insert(graph: &GraphDatabase, node: GraphNode) -> String {
    let id = node.id.clone();
    graph.upsert_node(node).unwrap();
    id
}

/// `find.id` selects exactly that node — at top level, not just inside a
/// `connect` `target` (which is the only place the engine used to honour it).
#[test]
fn find_id_selects_exactly_that_node() {
    let (graph, dir) = scratch_graph("find_id");
    let id_a = insert(
        &graph,
        GraphNode::new(NodeType::Function, "a".into(), "src/a.rs".into()),
    );
    insert(
        &graph,
        GraphNode::new(NodeType::Function, "b".into(), "src/b.rs".into()),
    );

    let spec = QuerySpec::new(vec![GraphOp::Find(FindOp {
        id: Some(id_a.clone()),
        ..Default::default()
    })]);
    let (hit, _) = reference_eval(&graph, &spec);
    assert_eq!(hit, BTreeSet::from([id_a.clone()]));

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec =
        Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let got: BTreeSet<String> = exec
        .execute(&spec)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(got, BTreeSet::from([id_a]), "find.id must select its node");

    let miss = QuerySpec::new(vec![GraphOp::Find(FindOp {
        id: Some("no-such-node".into()),
        ..Default::default()
    })]);
    let got: BTreeSet<String> = exec
        .execute(&miss)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert!(
        got.is_empty(),
        "a wrong find.id must select nothing, got {got:?}"
    );
    cleanup_scratch(&dir);
}

/// `{"not": [...]}` must be reachable from JSON for both selector kinds.
/// Untagged enums made the `Not` variant structurally unreachable on the
/// wire: a JSON object matches no variant of `String | Vec<String>`.
#[test]
fn not_selectors_survive_the_json_wire() {
    let label: QuerySpec = serde_json::from_value(serde_json::json!({
        "ops": [{ "op": "find", "label": { "not": ["deprecated"] } }]
    }))
    .expect("{\"label\":{\"not\":[...]}} must parse");
    match &label.ops[0] {
        GraphOp::Find(f) => match f.label_selector.as_ref().unwrap() {
            LabelSelector::Not(v) => assert_eq!(v, &vec!["deprecated".to_string()]),
            other => panic!("expected Not, got {other:?}"),
        },
        other => panic!("expected find, got {other:?}"),
    }

    let edge: QuerySpec = serde_json::from_value(serde_json::json!({
        "ops": [{ "op": "connect", "edge": { "not": ["Calls"] }, "depth": 1 }]
    }))
    .expect("{\"edge\":{\"not\":[...]}} must parse");
    match &edge.ops[0] {
        GraphOp::Connect(c) => match &c.edge {
            EdgeSelector::Not(v) => assert_eq!(v, &vec!["Calls".to_string()]),
            other => panic!("expected Not, got {other:?}"),
        },
        other => panic!("expected connect, got {other:?}"),
    }

    // And the wire form is `{"not": [...]}`, round-trip stable.
    let wire = serde_json::to_value(&label.ops[0]).unwrap();
    assert_eq!(wire["label"], serde_json::json!({"not": ["deprecated"]}));
}

/// A `find` by label must select nodes by the label they actually carry,
/// not only the `deprecated` flag.
#[test]
fn find_label_matches_the_nodes_actual_label() {
    let (graph, dir) = scratch_graph("find_label");
    insert(
        &graph,
        GraphNode::new(NodeType::Function, "a".into(), "src/a.rs".into()),
    );
    let mut test_fn = GraphNode::new(NodeType::Function, "b".into(), "src/b.rs".into());
    test_fn.label = Some("test".to_string());
    insert(&graph, test_fn);

    let spec = QuerySpec::new(vec![GraphOp::Find(FindOp {
        label_selector: Some(LabelSelector::Single("test".into())),
        ..Default::default()
    })]);
    let (slow, _) = reference_eval(&graph, &spec);
    assert_eq!(slow.len(), 1, "reference selects the labelled node");

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec =
        Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let got: BTreeSet<String> = exec
        .execute(&spec)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(
        got, slow,
        "executor must match labels other than deprecated"
    );
    cleanup_scratch(&dir);
}

/// `depth` counts hops along a **shortest** matching-edge path. A node two
/// hops away must be reported by `depth: 2` even when a longer path to it
/// is walked first.
#[test]
fn depth_counts_shortest_path_hops() {
    // The same logical graph under both edge-insertion orders of `a`'s
    // two edges: a walk that reports first-discovery depths instead of
    // shortest-path hops breaks on exactly one of them (whichever order
    // the storage hands the neighbours back in), so the oracle must hold
    // for both.
    for swap in [false, true] {
        let (graph, dir) = scratch_graph(&format!("depth_shortest_{swap}"));
        let a = insert(
            &graph,
            GraphNode::new(NodeType::Function, "a".into(), "src/a.rs".into()),
        );
        let b = insert(
            &graph,
            GraphNode::new(NodeType::Function, "b".into(), "src/b.rs".into()),
        );
        let c = insert(
            &graph,
            GraphNode::new(NodeType::Function, "c".into(), "src/c.rs".into()),
        );
        let d = insert(
            &graph,
            GraphNode::new(NodeType::Function, "d".into(), "src/d.rs".into()),
        );
        let e = insert(
            &graph,
            GraphNode::new(NodeType::Function, "e".into(), "src/e.rs".into()),
        );
        // a→b→e and a→c→d→e. Hop counts from a: b=1, c=1, d=2, e=2 — e's
        // shortest path is a→b→e (2 hops); the a→c→d→e route is 3 hops.
        let edges: [(&String, &String); 5] = if swap {
            [(&a, &c), (&c, &d), (&d, &e), (&a, &b), (&b, &e)]
        } else {
            [(&a, &b), (&b, &e), (&a, &c), (&c, &d), (&d, &e)]
        };
        for (from, to) in edges {
            graph
                .insert_edge(&GraphEdge::new(EdgeType::Calls, from.clone(), to.clone()))
                .unwrap();
        }

        let connect = |depth: DepthSpec| {
            QuerySpec::new(vec![
                // Seed by name (unique here), not `id`, so this test isolates
                // hop-counting from the `find.id` behaviour.
                GraphOp::Find(FindOp {
                    name: Some(NameSelector::Exact("a".into())),
                    ..Default::default()
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Outgoing,
                    depth,
                    target: None,
                }),
            ])
        };

        let embedder = NlpEmbedder::new_stub();
        let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(8).unwrap(),
        )));
        let mut exec =
            Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);

        for (depth, expect) in [
            (DepthSpec::Single(1), BTreeSet::from([b.clone(), c.clone()])),
            // e sits at hop 2 — a walk that discovers it at hop 3 first is
            // wrong, and `depth: 2` must still report it.
            (
                DepthSpec::Single(2),
                BTreeSet::from([b.clone(), c.clone(), d.clone(), e.clone()]),
            ),
            (
                DepthSpec::Range { min: 2, max: 2 },
                BTreeSet::from([d.clone(), e.clone()]),
            ),
            // Nothing is 3 hops away (d and e are both at hop 2), so "hops
            // exactly 3" must select nothing — a walk that reports
            // first-discovery depths reports `e` here after wandering
            // through the 3-hop route to it.
            (DepthSpec::Range { min: 3, max: 3 }, BTreeSet::new()),
        ] {
            let spec = connect(depth);
            let (expected_hops, _) = reference_eval(&graph, &spec);
            assert_eq!(expected_hops, expect, "reference hop sets");
            let got: BTreeSet<String> = exec
                .execute(&spec)
                .unwrap()
                .nodes
                .iter()
                .map(|n| n.id.clone())
                .collect();
            assert_eq!(got, expect, "executor hop sets (swap={swap})");
        }
        cleanup_scratch(&dir);
    }
}

/// Start nodes are never reported — not at hop 0, and not again through a
/// cycle back to themselves.
#[test]
fn start_nodes_are_not_reported_through_cycles() {
    let (graph, dir) = scratch_graph("start_cycle");
    let a = insert(
        &graph,
        GraphNode::new(NodeType::Function, "a".into(), "src/a.rs".into()),
    );
    let b = insert(
        &graph,
        GraphNode::new(NodeType::Function, "b".into(), "src/b.rs".into()),
    );
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, a.clone(), b.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, b.clone(), a.clone()))
        .unwrap();

    let spec = QuerySpec::new(vec![
        // Seed by name (unique here), not `id`, so this test isolates
        // cycle reporting from the `find.id` behaviour.
        GraphOp::Find(FindOp {
            name: Some(NameSelector::Exact("a".into())),
            ..Default::default()
        }),
        GraphOp::Connect(ConnectOp {
            edge: EdgeSelector::Single("Calls".into()),
            direction: Direction::Outgoing,
            depth: DepthSpec::Range { min: 1, max: 3 },
            target: None,
        }),
    ]);
    let (slow, _) = reference_eval(&graph, &spec);
    assert_eq!(
        slow,
        BTreeSet::from([b.clone()]),
        "a must not report itself"
    );

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec =
        Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let got: BTreeSet<String> = exec
        .execute(&spec)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(got, slow);
    cleanup_scratch(&dir);
}

// ─── edges / paths: walked edges and selected shortest paths ────────────
//
// The contract `docs/query-language.md` states: when the reported nodes
// came from a `connect`, `paths` holds one entry per reported node
// (parallel to `nodes`) with the selected shortest matching-edge path from
// the seed, and `edges` is the deduplicated union of the edges on those
// paths, sorted by (from, to, type). These tests pin the selection rule
// itself; the proptest below checks the executor against an independent
// implementation of the same rule.

type PathShape = (Vec<String>, Vec<(String, String, String)>);

/// A function node with a caller-chosen id, so tie-break expectations read
/// as ids (`"a" < "b"`) instead of opaque UUIDs.
fn node_with_id(id: &str) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, id.to_string(), format!("src/{id}.rs"));
    n.id = id.to_string();
    n
}

fn find_by_name(name: &str) -> GraphOp {
    GraphOp::Find(FindOp {
        name: Some(NameSelector::Exact(name.to_string())),
        ..Default::default()
    })
}

fn connect_calls(direction: Direction, depth: DepthSpec) -> GraphOp {
    GraphOp::Connect(ConnectOp {
        edge: EdgeSelector::Single("Calls".to_string()),
        direction,
        depth,
        target: None,
    })
}

fn exec_query(graph: &GraphDatabase, spec: &QuerySpec, default_limit: usize) -> QueryResult {
    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec = Executor::with_default_limit(
        graph,
        &embedder,
        &cache,
        std::path::Path::new(""),
        default_limit,
    );
    exec.execute(spec).expect("executor accepts a valid spec")
}

/// One `(path node ids, path edge triples)` shape per reported node, keyed
/// by node id. Panics when `paths` is not parallel to `nodes`.
fn path_shapes(result: &QueryResult) -> BTreeMap<String, PathShape> {
    assert_eq!(
        result.paths.len(),
        result.nodes.len(),
        "paths must parallel nodes"
    );
    result
        .nodes
        .iter()
        .zip(result.paths.iter())
        .map(|(n, p)| {
            (
                n.id.clone(),
                (
                    p.nodes.iter().map(|x| x.id.clone()).collect(),
                    p.edges
                        .iter()
                        .map(|e| (e.from.clone(), e.to.clone(), e.edge_type.clone()))
                        .collect(),
                ),
            )
        })
        .collect()
}

/// The wire `edges` as (from, to, type) triples, in reported order.
fn edge_shapes(result: &QueryResult) -> Vec<(String, String, String)> {
    result
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.edge_type.clone()))
        .collect()
}

/// `edges`/`paths` of a `connect` result: one path per reported node
/// (seed first), and `edges` the deduplicated union of the path edges.
#[test]
fn connect_results_carry_walked_edges_and_shortest_paths() {
    let (graph, dir) = scratch_graph("edges_paths_chain");
    // s → a → b. Depth 2 reports a and b with their walk paths.
    let s = insert(&graph, node_with_id("s"));
    let a = insert(&graph, node_with_id("a"));
    let b = insert(&graph, node_with_id("b"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), a.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, a.clone(), b.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Outgoing, DepthSpec::Single(2)),
        ]),
        0,
    );

    assert_eq!(res.count, 2);
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([
            (
                "a".to_string(),
                (
                    vec!["s".to_string(), "a".to_string()],
                    vec![("s".to_string(), "a".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "b".to_string(),
                (
                    vec!["s".to_string(), "a".to_string(), "b".to_string()],
                    vec![
                        ("s".to_string(), "a".to_string(), "Calls".to_string()),
                        ("a".to_string(), "b".to_string(), "Calls".to_string()),
                    ],
                ),
            ),
        ])
    );
    // Sorted by (from, to, type): "a" < "s".
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("a".to_string(), "b".to_string(), "Calls".to_string()),
            ("s".to_string(), "a".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// When several shortest paths reach a node, the selected one runs through
/// the lowest-id equal-distance predecessor at every hop.
#[test]
fn path_tie_break_prefers_the_lowest_predecessor_id() {
    let (graph, dir) = scratch_graph("edges_paths_tie_pred");
    // s→a, s→b, a→c, b→c: c sits at hop 2 over two equal-length paths.
    let s = insert(&graph, node_with_id("s"));
    let a = insert(&graph, node_with_id("a"));
    let b = insert(&graph, node_with_id("b"));
    let c = insert(&graph, node_with_id("c"));
    for (from, to) in [(&s, &a), (&s, &b), (&a, &c), (&b, &c)] {
        graph
            .insert_edge(&GraphEdge::new(EdgeType::Calls, from.clone(), to.clone()))
            .unwrap();
    }

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Outgoing, DepthSpec::Single(2)),
        ]),
        0,
    );

    // "a" < "b", so c's path is s→a→c; the s→b→c route's edges stay out.
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([
            (
                "a".to_string(),
                (
                    vec!["s".to_string(), "a".to_string()],
                    vec![("s".to_string(), "a".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "b".to_string(),
                (
                    vec!["s".to_string(), "b".to_string()],
                    vec![("s".to_string(), "b".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "c".to_string(),
                (
                    vec!["s".to_string(), "a".to_string(), "c".to_string()],
                    vec![
                        ("s".to_string(), "a".to_string(), "Calls".to_string()),
                        ("a".to_string(), "c".to_string(), "Calls".to_string()),
                    ],
                ),
            ),
        ])
    );
    // Only the selected path's edges: (b→c) was walked but is not on it.
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("a".to_string(), "c".to_string(), "Calls".to_string()),
            ("s".to_string(), "a".to_string(), "Calls".to_string()),
            ("s".to_string(), "b".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// Parallel matching edges between one pair of nodes: the hop takes the
/// smallest edge type name's edge (tie-break level 5).
#[test]
fn path_tie_break_prefers_the_smallest_edge_type_name() {
    let (graph, dir) = scratch_graph("edges_paths_tie_edge_type");
    // s→a→b with TWO matching edges on the a→b pair. `Uses` is inserted
    // first, so winning with `Calls` cannot be neighbour order luck.
    let s = insert(&graph, node_with_id("s"));
    let a = insert(&graph, node_with_id("a"));
    let b = insert(&graph, node_with_id("b"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), a.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Uses, a.clone(), b.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, a.clone(), b.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            GraphOp::Connect(ConnectOp {
                edge: EdgeSelector::Or(vec!["Calls".to_string(), "Uses".to_string()]),
                direction: Direction::Outgoing,
                depth: DepthSpec::Single(2),
                target: None,
            }),
        ]),
        0,
    );

    assert_eq!(res.count, 2);
    // The a→b hop is the `Calls` edge ("Calls" < "Uses"), never `Uses`.
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([
            (
                "a".to_string(),
                (
                    vec!["s".to_string(), "a".to_string()],
                    vec![("s".to_string(), "a".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "b".to_string(),
                (
                    vec!["s".to_string(), "a".to_string(), "b".to_string()],
                    vec![
                        ("s".to_string(), "a".to_string(), "Calls".to_string()),
                        ("a".to_string(), "b".to_string(), "Calls".to_string()),
                    ],
                ),
            ),
        ])
    );
    // The larger type's parallel edge is not on the selected path, so it
    // stays out of `edges` entirely.
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("a".to_string(), "b".to_string(), "Calls".to_string()),
            ("s".to_string(), "a".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// `direction: both` with a node one hop away each way: the outgoing
/// walk's path wins the equal-length tie.
#[test]
fn equal_hops_prefer_the_outgoing_walks_path() {
    let (graph, dir) = scratch_graph("edges_paths_both_tie");
    let s = insert(&graph, node_with_id("s"));
    let x = insert(&graph, node_with_id("x"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), x.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, x.clone(), s.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Both, DepthSpec::Single(1)),
        ]),
        0,
    );

    // One path for x, over the outgoing s→x edge — never (x→s).
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([(
            "x".to_string(),
            (
                vec!["s".to_string(), "x".to_string()],
                vec![("s".to_string(), "x".to_string(), "Calls".to_string())],
            ),
        )])
    );
    assert_eq!(
        edge_shapes(&res),
        vec![("s".to_string(), "x".to_string(), "Calls".to_string())]
    );
    cleanup_scratch(&dir);
}

/// `direction: both` reports one path per node: the shorter of the two
/// pure walks, even when it runs against the edge direction.
#[test]
fn both_walks_report_the_shorter_path() {
    let (graph, dir) = scratch_graph("edges_paths_both_shorter");
    // y→s is one hop on the incoming walk; s→m→y is two hops outgoing.
    let s = insert(&graph, node_with_id("s"));
    let m = insert(&graph, node_with_id("m"));
    let y = insert(&graph, node_with_id("y"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, y.clone(), s.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), m.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, m.clone(), y.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Both, DepthSpec::Single(2)),
        ]),
        0,
    );

    assert_eq!(res.count, 2);
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([
            (
                "m".to_string(),
                (
                    vec!["s".to_string(), "m".to_string()],
                    vec![("s".to_string(), "m".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "y".to_string(),
                (
                    vec!["s".to_string(), "y".to_string()],
                    // The incoming walk's edge, stored y→s: shorter beats
                    // the outgoing s→m→y route.
                    vec![("y".to_string(), "s".to_string(), "Calls".to_string())],
                ),
            ),
        ])
    );
    // Only m's own path keeps (s→m); (m→y) is on no selected path.
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("s".to_string(), "m".to_string(), "Calls".to_string()),
            ("y".to_string(), "s".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// `edges` covers the transit edges walked to reach a reported node, even
/// when the transit node itself is outside the reported hop range.
#[test]
fn edges_cover_transit_nodes_the_depth_range_does_not_report() {
    let (graph, dir) = scratch_graph("edges_paths_transit");
    // s→a→b with depth {min:2,max:2}: only b is reported, but its path
    // walks through a, so (s→a) is part of the walked edges.
    let s = insert(&graph, node_with_id("s"));
    let a = insert(&graph, node_with_id("a"));
    let b = insert(&graph, node_with_id("b"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), a.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, a.clone(), b.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Outgoing, DepthSpec::Range { min: 2, max: 2 }),
        ]),
        0,
    );

    assert_eq!(res.count, 1);
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([(
            "b".to_string(),
            (
                vec!["s".to_string(), "a".to_string(), "b".to_string()],
                vec![
                    ("s".to_string(), "a".to_string(), "Calls".to_string()),
                    ("a".to_string(), "b".to_string(), "Calls".to_string()),
                ],
            ),
        )])
    );
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("a".to_string(), "b".to_string(), "Calls".to_string()),
            ("s".to_string(), "a".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// Find/filter-only pipelines have no walked edges; skip-if-empty keeps
/// the fields off the wire entirely.
#[test]
fn find_only_results_carry_no_edges_or_paths() {
    let (graph, dir) = scratch_graph("edges_paths_find_only");
    insert(&graph, node_with_id("s"));

    let res = exec_query(&graph, &QuerySpec::new(vec![find_by_name("s")]), 0);
    assert_eq!(res.count, 1);
    assert!(res.edges.is_empty(), "find-only must not invent edges");
    assert!(res.paths.is_empty(), "find-only must not invent paths");

    let wire = serde_json::to_value(&res).expect("QueryResult serializes");
    assert!(
        wire.get("edges").is_none(),
        "empty edges stay off the wire: {wire}"
    );
    assert!(
        wire.get("paths").is_none(),
        "empty paths stay off the wire: {wire}"
    );
    cleanup_scratch(&dir);
}

/// The default result cap trims nodes; `edges`/`paths` follow exactly the
/// nodes that were reported.
#[test]
fn truncation_carries_edges_and_paths_of_the_reported_nodes_only() {
    let (graph, dir) = scratch_graph("edges_paths_truncated");
    let s = insert(&graph, node_with_id("s"));
    for id in ["a", "b"] {
        let n = insert(&graph, node_with_id(id));
        graph
            .insert_edge(&GraphEdge::new(EdgeType::Calls, s.clone(), n))
            .unwrap();
    }

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Outgoing, DepthSpec::Single(1)),
        ]),
        1, // default cap: one node
    );

    assert_eq!(res.count, 1);
    let meta = res.meta.clone().expect("meta always present");
    assert!(meta.truncated, "the cap must be reported: {meta:?}");
    assert_eq!(meta.matched_before_limit, Some(2));
    let shapes = path_shapes(&res);
    assert_eq!(shapes.len(), 1, "one path for the one reported node");
    let (id, (nodes, edges)) = shapes.into_iter().next().unwrap();
    assert_eq!(nodes, vec!["s".to_string(), id.clone()]);
    assert_eq!(edges, vec![("s".to_string(), id, "Calls".to_string())]);
    assert_eq!(
        edge_shapes(&res).len(),
        1,
        "only the reported node's edge is walked"
    );
    cleanup_scratch(&dir);
}

/// An explicit `limit` op trims nodes; `edges`/`paths` follow exactly
/// the N survivors — no path or edge is kept for a trimmed node.
#[test]
fn limit_op_carries_edges_and_paths_of_the_reported_nodes_only() {
    let (graph, dir) = scratch_graph("edges_paths_limit");
    // s→a→b→c at depth 3 reports a, b, c (BFS order); limit 2 keeps a, b.
    let s = insert(&graph, node_with_id("s"));
    let a = insert(&graph, node_with_id("a"));
    let b = insert(&graph, node_with_id("b"));
    let c = insert(&graph, node_with_id("c"));
    for (from, to) in [(&s, &a), (&a, &b), (&b, &c)] {
        graph
            .insert_edge(&GraphEdge::new(EdgeType::Calls, from.clone(), to.clone()))
            .unwrap();
    }

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            find_by_name("s"),
            connect_calls(Direction::Outgoing, DepthSpec::Single(3)),
            GraphOp::Limit(LimitOp {
                count: 2,
                offset: 0,
            }),
        ]),
        0,
    );

    assert_eq!(res.count, 2);
    // Paths for the two survivors only; trimmed `c` gets no path.
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([
            (
                "a".to_string(),
                (
                    vec!["s".to_string(), "a".to_string()],
                    vec![("s".to_string(), "a".to_string(), "Calls".to_string())],
                ),
            ),
            (
                "b".to_string(),
                (
                    vec!["s".to_string(), "a".to_string(), "b".to_string()],
                    vec![
                        ("s".to_string(), "a".to_string(), "Calls".to_string()),
                        ("a".to_string(), "b".to_string(), "Calls".to_string()),
                    ],
                ),
            ),
        ])
    );
    // The trimmed node's edge (b→c) is not among the walked edges.
    assert_eq!(
        edge_shapes(&res),
        vec![
            ("a".to_string(), "b".to_string(), "Calls".to_string()),
            ("s".to_string(), "a".to_string(), "Calls".to_string()),
        ]
    );
    cleanup_scratch(&dir);
}

/// Equal-length paths from different seeds: the lower seed id wins.
#[test]
fn equal_hops_across_seeds_prefer_the_lowest_seed_id() {
    let (graph, dir) = scratch_graph("edges_paths_seed_tie");
    // s1→c and s2→c; both seeds one hop from c ("s1" < "s2").
    let s1 = insert(&graph, node_with_id("s1"));
    let s2 = insert(&graph, node_with_id("s2"));
    let c = insert(&graph, node_with_id("c"));
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s1.clone(), c.clone()))
        .unwrap();
    graph
        .insert_edge(&GraphEdge::new(EdgeType::Calls, s2.clone(), c.clone()))
        .unwrap();

    let res = exec_query(
        &graph,
        &QuerySpec::new(vec![
            GraphOp::Find(FindOp {
                name: Some(NameSelector::StartsWith("s".to_string())),
                ..Default::default()
            }),
            connect_calls(Direction::Outgoing, DepthSpec::Single(1)),
        ]),
        0,
    );

    assert_eq!(res.count, 1);
    assert_eq!(
        path_shapes(&res),
        BTreeMap::from([(
            "c".to_string(),
            (
                vec!["s1".to_string(), "c".to_string()],
                vec![("s1".to_string(), "c".to_string(), "Calls".to_string())],
            ),
        )])
    );
    cleanup_scratch(&dir);
}

// ─── The oracle itself ───────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Fast executor ≡ reference evaluator, on random graphs and random
    /// valid queries, after a JSON wire round-trip: node-id set, walked edge
    /// set, and the exact selected path per node. Ordering (sort/group/
    /// limit) is out of scope for now.
    #[test]
    fn executor_agrees_with_the_reference(graph in arb_graph_spec(), query in arb_query_plan()) {
        run_case(&graph, &query);
    }
}
