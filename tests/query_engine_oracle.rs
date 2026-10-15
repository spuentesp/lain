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
//! the set of returned node ids (order is a separate concern).
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
    NameSelector, QuerySpec, TypeSelector,
};
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use parking_lot::Mutex;
use petgraph::Direction as PetDirection;
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap, VecDeque};
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

/// The three collections a connect step produces; only nodes are compared.
fn connect_reference(
    graph: &GraphDatabase,
    seeds: &[String],
    connect: &ConnectOp,
) -> Vec<String> {
    let range = connect.depth.to_range();
    let lo = (*range.start()).max(1);
    let hi = *range.end();
    let directions: Vec<PetDirection> = match connect.direction {
        Direction::Both => vec![PetDirection::Outgoing, PetDirection::Incoming],
        Direction::Outgoing => vec![PetDirection::Outgoing],
        Direction::Incoming => vec![PetDirection::Incoming],
    };

    let mut found: BTreeSet<String> = BTreeSet::new();
    for seed in seeds {
        for direction in &directions {
            for (id, hops) in bfs_hops(graph, seed, *direction, &connect.edge) {
                if hops >= lo && hops <= hi {
                    found.insert(id);
                }
            }
        }
    }

    let mut nodes: Vec<String> = found.into_iter().collect();
    if let Some(target) = connect.target.as_deref() {
        nodes.retain(|id| {
            graph
                .get_node(id)
                .ok()
                .flatten()
                .is_some_and(|n| find_matches(target, &n))
        });
    }
    nodes
}

/// Evaluate a whole `QuerySpec` the slow, obvious way. Returns the ids of
/// the final working set.
fn reference_eval(graph: &GraphDatabase, spec: &QuerySpec) -> BTreeSet<String> {
    let mut current: Vec<String> = Vec::new();
    for op in &spec.ops {
        match op {
            GraphOp::Find(find) => {
                current = graph
                    .get_all_nodes()
                    .into_iter()
                    .filter(|n| find_matches(find, n))
                    .map(|n| n.id)
                    .collect();
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
                current = connect_reference(graph, &current, connect);
            }
            // Excluded from the generator; a spec carrying one is not a
            // valid oracle input.
            GraphOp::SemanticFilter(_) | GraphOp::Group(_) | GraphOp::Sort(_) | GraphOp::Limit(_) => {
                panic!("oracle input must not contain {:?}", op)
            }
        }
    }
    current.into_iter().collect()
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
        (0..NODE_TYPES.len(), 0..NODE_TYPES.len())
            .prop_map(|(a, b)| Some(TypePlan::Or(a, b))),
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
    (
        arb_type_plan(),
        arb_name_plan(),
        arb_id_plan(),
        arb_label_plan(),
        prop_oneof![Just(None), Just(Some("src".to_string())), Just(Some("f1".to_string()))],
    )
        .prop_map(|(type_sel, name, id, label, path)| FindPlan {
            type_sel,
            name,
            id,
            label,
            path,
        })
}

fn arb_op_plan() -> impl Strategy<Value = OpPlan> {
    prop_oneof![
        (arb_type_plan(), arb_label_plan(), arb_name_plan()).prop_map(
            |(type_sel, label, name)| OpPlan::Filter {
                type_sel,
                label,
                name
            }
        ),
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
                    .map(|(from, to, edge_type)| EdgeSpec { from, to, edge_type })
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
    let _ = std::fs::remove_dir_all(&dir);
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
    let mut exec = Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let result = exec.execute(&spec).expect("executor accepts a valid spec");
    let meta = result.meta.clone().expect("executor always reports meta");
    assert!(
        !meta.truncated,
        "cap is disabled in the harness; nothing may truncate: {:?}",
        meta
    );

    let fast: BTreeSet<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
    let slow = reference_eval(&graph, &spec);
    assert_eq!(
        fast, slow,
        "executor and reference disagree\ngraph: {graph_spec:?}\nquery: {query_plan:?}\nwire: {wire}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ─── Deterministic regressions for the audited bugs ──────────────────

fn scratch_graph(name: &str) -> (GraphDatabase, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "lain_query_oracle_scratch_{}_{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
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
    let hit = reference_eval(&graph, &spec);
    assert_eq!(hit, BTreeSet::from([id_a.clone()]));

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec = Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
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
    assert!(got.is_empty(), "a wrong find.id must select nothing, got {got:?}");
    let _ = std::fs::remove_dir_all(&dir);
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
    let slow = reference_eval(&graph, &spec);
    assert_eq!(slow.len(), 1, "reference selects the labelled node");

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec = Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let got: BTreeSet<String> = exec
        .execute(&spec)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(got, slow, "executor must match labels other than deprecated");
    let _ = std::fs::remove_dir_all(&dir);
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
            assert_eq!(reference_eval(&graph, &spec), expect, "reference hop sets");
            let got: BTreeSet<String> = exec
                .execute(&spec)
                .unwrap()
                .nodes
                .iter()
                .map(|n| n.id.clone())
                .collect();
            assert_eq!(got, expect, "executor hop sets (swap={swap})");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    let slow = reference_eval(&graph, &spec);
    assert_eq!(slow, BTreeSet::from([b.clone()]), "a must not report itself");

    let embedder = NlpEmbedder::new_stub();
    let cache = std::sync::Arc::new(Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(8).unwrap(),
    )));
    let mut exec = Executor::with_default_limit(&graph, &embedder, &cache, std::path::Path::new(""), 0);
    let got: BTreeSet<String> = exec
        .execute(&spec)
        .unwrap()
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(got, slow);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── The oracle itself ───────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Fast executor ≡ reference evaluator, on random graphs and random
    /// valid queries, after a JSON wire round-trip. Sets only: ordering
    /// (sort/group/limit) is out of scope for now.
    #[test]
    fn executor_agrees_with_the_reference(graph in arb_graph_spec(), query in arb_query_plan()) {
        run_case(&graph, &query);
    }
}
