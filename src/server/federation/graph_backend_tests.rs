//! Contract tests for GraphBackend. The same tests will run against PetgraphBackend
//! in Task 7. Here we use a simple in-memory HashMap impl to define the contract.
use crate::error::LainError;
use crate::federation::graph_backend::{
    impact_propagation, GraphBackend, ImpactHop, ImpactPath, ImpactResult, PetgraphBackend,
    Propagation,
};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::RwLock;

pub struct HashMapBackend {
    nodes: RwLock<HashMap<String, GraphNode>>,
    edges: RwLock<Vec<GraphEdge>>,
}

impl HashMapBackend {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
        }
    }
}

impl GraphBackend for HashMapBackend {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        self.nodes.write().unwrap().insert(node.id.clone(), node);
        Ok(())
    }
    fn remove_nodes(&self, global_ids: &[String]) -> Result<usize, LainError> {
        let mut nodes = self.nodes.write().unwrap();
        let mut removed = 0usize;
        for id in global_ids {
            if nodes.remove(id).is_some() {
                removed += 1;
            }
        }
        drop(nodes);
        self.edges
            .write()
            .unwrap()
            .retain(|e| !global_ids.contains(&e.source_id) && !global_ids.contains(&e.target_id));
        Ok(removed)
    }
    fn remove_edges(&self, edges: &[GraphEdge]) -> Result<usize, LainError> {
        let targets: std::collections::HashSet<(String, String, EdgeType)> = edges
            .iter()
            .map(|e| {
                (
                    e.source_id.clone(),
                    e.target_id.clone(),
                    e.edge_type.clone(),
                )
            })
            .collect();
        let before = self.edges.read().unwrap().len();
        self.edges.write().unwrap().retain(|e| {
            !targets.contains(&(
                e.source_id.clone(),
                e.target_id.clone(),
                e.edge_type.clone(),
            ))
        });
        Ok(before - self.edges.read().unwrap().len())
    }
    fn upsert_node_global(
        &self,
        global_id: &str,
        kind: NodeType,
        path: &str,
        name: &str,
    ) -> Result<(), LainError> {
        let mut node = GraphNode::new(kind, name.to_string(), path.to_string());
        node.id = global_id.to_string();
        self.upsert_node(node)
    }
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        self.edges.write().unwrap().push(edge);
        Ok(())
    }
    fn upsert_edges_batch(&self, edges: &[GraphEdge]) -> Result<(), LainError> {
        let mut g = self.edges.write().unwrap();
        g.extend_from_slice(edges);
        Ok(())
    }
    fn upsert_nodes_batch(&self, nodes: &[GraphNode]) -> Result<(), LainError> {
        let mut g = self.nodes.write().unwrap();
        for node in nodes {
            g.insert(node.id.clone(), node.clone());
        }
        Ok(())
    }
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        Ok(self.nodes.read().unwrap().get(global_id).cloned())
    }
    fn find_nodes_by_name(&self, name: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(self
            .nodes
            .read()
            .unwrap()
            .values()
            .filter(|n| n.name == name)
            .cloned()
            .collect())
    }
    fn list_nodes(&self) -> Result<Vec<GraphNode>, LainError> {
        Ok(self.nodes.read().unwrap().values().cloned().collect())
    }
    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self.edges.read().unwrap().clone())
    }
    fn traverse(
        &self,
        _start: &str,
        _edge: EdgeType,
        _depth: Range<u32>,
        _direction: petgraph::Direction,
    ) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn traverse_impact(
        &self,
        starts: &[&str],
        depth: u32,
        cap: usize,
        min_confidence: f32,
    ) -> Result<ImpactResult, LainError> {
        // Mirror of `PetgraphBackend::traverse_impact` against the
        // in-memory node + edge vectors. Used by the contract tests
        // to pin the §5.2 algorithm without paying the per-write
        // disk-sync cost of `PetgraphBackend`.
        let nodes: HashMap<String, GraphNode> = self
            .nodes
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let edges = self.edges.read().unwrap().clone();
        let mut by_target: HashMap<String, Vec<(f32, String, GraphEdge)>> = HashMap::new();
        for e in edges {
            let conf = e.weight.unwrap_or(1.0);
            if conf < min_confidence {
                continue;
            }
            match impact_propagation(&e.edge_type) {
                Propagation::Incoming => by_target.entry(e.target_id.clone()).or_default().push((
                    conf,
                    e.source_id.clone(),
                    e,
                )),
                Propagation::Outgoing => by_target.entry(e.source_id.clone()).or_default().push((
                    conf,
                    e.target_id.clone(),
                    e,
                )),
                Propagation::Stop => {}
            }
        }
        for v in by_target.values_mut() {
            v.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            });
        }

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        for s in starts {
            if nodes.contains_key(*s) && visited.insert((*s).to_string()) {
                queue.push_back(((*s).to_string(), 0));
            }
        }
        let mut paths: Vec<ImpactPath> = Vec::new();
        let mut truncated = false;
        let mut pred: HashMap<String, (String, GraphEdge, f32)> = HashMap::new();

        while let Some((current, current_depth)) = queue.pop_front() {
            let successors = by_target.get(&current);
            let has_unvisited_successor = successors
                .map(|v| v.iter().any(|(_, id, _)| !visited.contains(id)))
                .unwrap_or(false);
            let at_depth = current_depth >= depth;
            if at_depth || !has_unvisited_successor {
                if paths.len() >= cap {
                    truncated = true;
                } else {
                    paths.push(reconstruct_test_path(&current, &pred, &nodes));
                }
            }
            if at_depth {
                truncated = true;
                continue;
            }
            if let Some(successors) = successors {
                for (conf, next_id, edge) in successors {
                    if !visited.insert(next_id.clone()) {
                        continue;
                    }
                    let new_min_conf = if let Some((_, _, prev_conf)) = pred.get(&current) {
                        conf.min(*prev_conf)
                    } else {
                        *conf
                    };
                    pred.insert(
                        next_id.clone(),
                        (current.clone(), edge.clone(), new_min_conf),
                    );
                    queue.push_back((next_id.clone(), current_depth + 1));
                }
            }
        }

        paths.sort_by(|a, b| {
            b.min_confidence
                .partial_cmp(&a.min_confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.hops.len().cmp(&b.hops.len()))
                .then_with(|| {
                    let a_leaf = a.hops.last().map(|h| h.node.id.as_str()).unwrap_or("");
                    let b_leaf = b.hops.last().map(|h| h.node.id.as_str()).unwrap_or("");
                    a_leaf.cmp(b_leaf)
                })
        });

        Ok(ImpactResult { paths, truncated })
    }
    fn find_path(&self, _from: &str, _to: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn subgraph_around(
        &self,
        _center: &str,
        _radius: u32,
    ) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        Ok(Vec::new())
    }
    fn node_count(&self) -> usize {
        self.nodes.read().unwrap().len()
    }
    fn edge_count(&self) -> usize {
        self.edges.read().unwrap().len()
    }
}

#[test]
fn contract_upsert_node_roundtrips() {
    let b = HashMapBackend::new();
    let n = GraphNode::new(NodeType::Function, "f".into(), "src/lib.rs".into());
    b.upsert_node(n.clone()).unwrap();
    assert_eq!(b.node_count(), 1);
    assert_eq!(b.get_node(&n.id).unwrap().unwrap().id, n.id);
}

#[test]
fn contract_upsert_edge_increments_count() {
    let b = HashMapBackend::new();
    let n1 = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let n2 = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    b.upsert_node(n1.clone()).unwrap();
    b.upsert_node(n2.clone()).unwrap();
    b.upsert_edge(GraphEdge::new(
        EdgeType::Calls,
        n1.id.clone(),
        n2.id.clone(),
    ))
    .unwrap();
    assert_eq!(b.node_count(), 2);
    assert_eq!(b.edge_count(), 1);
}

/// F10 — `remove_edges` retracts edges without touching their
/// endpoints. The federation's reconciliation pass uses it to drop
/// edges the source repo no longer reports while keeping the
/// caller/callee nodes live.
#[test]
fn contract_remove_edges_drops_only_matching_endpoints_stay() {
    let b = HashMapBackend::new();
    let n1 = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let n2 = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    b.upsert_node(n1.clone()).unwrap();
    b.upsert_node(n2.clone()).unwrap();
    let edge = GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone());
    b.upsert_edge(edge.clone()).unwrap();

    let removed = b.remove_edges(std::slice::from_ref(&edge)).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(b.edge_count(), 0);
    assert_eq!(b.node_count(), 2, "endpoints must survive edge removal");

    // Removing an already-gone edge is a no-op, not an error.
    let removed_again = b.remove_edges(&[edge]).unwrap();
    assert_eq!(removed_again, 0);
}

/// F10 — `remove_edges` matches on the full `(edge_type, source_id,
/// target_id)` triple. An edge that differs only in `edge_type` (e.g.
/// `Contains` vs `Calls`) is not collateral damage of removing a
/// `Calls` edge between the same endpoints.
#[test]
fn contract_remove_edges_only_matches_full_triple() {
    let b = HashMapBackend::new();
    let n1 = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let n2 = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    b.upsert_node(n1.clone()).unwrap();
    b.upsert_node(n2.clone()).unwrap();
    b.upsert_edge(GraphEdge::new(
        EdgeType::Calls,
        n1.id.clone(),
        n2.id.clone(),
    ))
    .unwrap();
    b.upsert_edge(GraphEdge::new(EdgeType::Uses, n1.id.clone(), n2.id.clone()))
        .unwrap();

    let removed = b
        .remove_edges(&[GraphEdge::new(
            EdgeType::Calls,
            n1.id.clone(),
            n2.id.clone(),
        )])
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(b.edge_count(), 1, "Uses edge survives Calls removal");
    let remaining = b.all_edges().unwrap();
    assert_eq!(remaining[0].edge_type, EdgeType::Uses);
}

#[test]
fn contract_get_missing_returns_none() {
    let b = HashMapBackend::new();
    assert!(b.get_node("nope").unwrap().is_none());
}

#[test]
fn petgraph_backend_persists_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.upsert_node_global(
        "repo1:Function:src/lib.rs:f:0",
        NodeType::Function,
        "src/lib.rs",
        "f",
    )
    .unwrap();
    assert_eq!(b.node_count(), 1);
    drop(b);

    let b2 = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!(b2.node_count(), 1);
    assert!(b2
        .get_node("repo1:Function:src/lib.rs:f:0")
        .unwrap()
        .is_some());
}

#[test]
fn petgraph_backend_rejects_pre_bump_version_header() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("federated_graph.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"LNF2");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 16]);
    std::fs::write(&bin_path, &bytes).unwrap();

    let err = match PetgraphBackend::new(dir.path()) {
        Ok(_) => panic!("expected FederationSchemaMismatch"),
        Err(e) => e,
    };
    match err {
        LainError::FederationSchemaMismatch { found, required } => {
            assert_eq!(found, 1);
            // `required` follows the live `FEDERATION_GRAPH_VERSION`
            // constant so this test does not need to be rewritten on
            // every schema bump — only when the bump changes the
            // *meaning* of the test (rejecting v1, v2, …).
            assert_eq!(
                required,
                crate::federation::graph_backend::FEDERATION_GRAPH_VERSION
            );
        }
        other => panic!("expected FederationSchemaMismatch, got {other:?}"),
    }
}

#[test]
fn petgraph_backend_rejects_headerless_legacy_payload() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("federated_graph.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(&bin_path, &bytes).unwrap();

    let err = match PetgraphBackend::new(dir.path()) {
        Ok(_) => panic!("expected FederationSchemaMismatch"),
        Err(e) => e,
    };
    assert!(
        matches!(err, LainError::FederationSchemaMismatch { .. }),
        "expected FederationSchemaMismatch, got {err:?}"
    );
}

#[test]
fn petgraph_backend_rejects_short_file() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("federated_graph.bin");
    std::fs::write(&bin_path, [0u8; 4]).unwrap();

    let err = match PetgraphBackend::new(dir.path()) {
        Ok(_) => panic!("expected FederationSchemaMismatch"),
        Err(e) => e,
    };
    assert!(
        matches!(err, LainError::FederationSchemaMismatch { .. }),
        "expected FederationSchemaMismatch, got {err:?}"
    );
}

// ─── PR 4 — F2 typed impact traversal (`traverse_impact`) ─────────────
//
// The algorithm (§5.2) lives on `GraphBackend::traverse_impact`; the
// table test pins the propagation enum. BFS / determinism tests run
// against a `HashMapBackend` that hosts a deliberately-crafted fixture
// with multiple predecessors and edges below the confidence floor.

#[test]
fn impact_propagation_table_pr9_state() {
    use crate::federation::graph_backend::{impact_propagation, Propagation};
    // Pin the PR 9 propagation carve-out. The §5.2 code block
    // describes the *final* propagation table; the tracker checklist
    // governs what actually ships per PR. PR 9 turns on
    // `ReadsField` (joining the `Calls` / `CallsHttp` / `SendsHttp` /
    // `Binds` / `RequestSchema` / `ResponseSchema` / `HasField` group
    // PRs 4 / 7 / 8 already switched on).
    //
    // Per-PR trajectory — incoming / outgoing / stop:
    //   PR 4  →  1 / 0 / 23   (Calls)
    //   PR 7  →  4 / 0 / 20   (CallsHttp, SendsHttp, Binds added)
    //   PR 8  →  7 / 0 / 17   (RequestSchema, ResponseSchema, HasField added)
    //   PR 9  →  8 / 0 / 16   (ReadsField)
    //   PR 15 → 10 / 1 / 13   (PayloadSchema, Produces, Consumes; first Outgoing)
    //
    // The match is exhaustive: a new `EdgeType` variant that hasn't
    // been decided would fail to compile, which is the §5.2 contract
    // on `impact_propagation`.
    let cases: &[EdgeType] = &[
        EdgeType::Calls,
        EdgeType::CallsHttp,
        EdgeType::SendsHttp,
        EdgeType::Binds,
        EdgeType::Consumes,
        EdgeType::ReadsField,
        EdgeType::HasField,
        EdgeType::RequestSchema,
        EdgeType::ResponseSchema,
        EdgeType::PayloadSchema,
        EdgeType::Produces,
        EdgeType::Contains,
        EdgeType::Imports,
        EdgeType::CoChangedWith,
        EdgeType::Pattern,
        EdgeType::Uses,
        EdgeType::Implements,
        EdgeType::DeployedTo,
        EdgeType::CrossRepoSameSymbol,
        EdgeType::DynamicDispatch,
        EdgeType::BusTopic,
        EdgeType::RouteMatches,
        EdgeType::RuntimeCall,
        EdgeType::ReadsFrom,
    ];
    for e in cases {
        let got = impact_propagation(e);
        let expected = match e {
            EdgeType::Calls
            | EdgeType::CallsHttp
            | EdgeType::SendsHttp
            | EdgeType::Binds
            | EdgeType::RequestSchema
            | EdgeType::ResponseSchema
            | EdgeType::HasField
            | EdgeType::ReadsField => Propagation::Incoming,
            _ => Propagation::Stop,
        };
        assert_eq!(got, expected, "propagation mismatch for {e:?}");
    }
    // PR 9 actual state: 8 Incoming, 0 Outgoing, 16 Stop.
    assert_eq!(cases.len(), 24, "every EdgeType variant must be listed");
    let incoming_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Incoming)
        .count();
    assert_eq!(incoming_count, 8);
    let outgoing_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Outgoing)
        .count();
    assert_eq!(outgoing_count, 0);
    let stop_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Stop)
        .count();
    assert_eq!(stop_count, 16);
}

/// `traverse_impact` should ignore start ids that don't exist and
/// return an empty result. Empty `starts` is also empty output.
#[test]
fn traverse_impact_unknown_starts_return_empty() {
    let b = HashMapBackend::new();
    let r = b
        .traverse_impact(&["nope:Function:src/a.rs:f:0"], 5, 100, 0.0)
        .unwrap();
    assert!(r.paths.is_empty());
    assert!(!r.truncated);

    let r2 = b.traverse_impact(&[], 5, 100, 0.0).unwrap();
    assert!(r2.paths.is_empty());
    assert!(!r2.truncated);
}

/// Three-node chain with `Calls` edges `caller → shared ← other_caller`
/// (the seed is `shared`, depth 1). Traversal is BFS over `Incoming`
/// (callers); each caller reaches `shared` via one `Calls` edge. With
/// confidence 1.0 on every edge and `depth = 5`, both callers appear
/// as paths. Sorted deterministically by leaf GlobalId.
#[test]
fn traverse_impact_emits_paths_for_two_incoming_callers() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_a = node(
        "repo-a:Function:src/w.rs:caller_one:0",
        "caller_one",
        "src/w.rs",
    );
    let n_b = node(
        "repo-b:Function:src/w.rs:caller_two:0",
        "caller_two",
        "src/w.rs",
    );
    b.upsert_node(n_shared.clone()).unwrap();
    b.upsert_node(n_a.clone()).unwrap();
    b.upsert_node(n_b.clone()).unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_a.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_b.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    assert!(!r.truncated, "plenty of room; should not truncate");
    assert_eq!(r.paths.len(), 2);
    let leaves: Vec<&str> = r
        .paths
        .iter()
        .map(|p| p.hops.last().expect("non-empty").node.id.as_str())
        .collect();
    // Sort order: min_confidence desc, then length asc, then leaf id.
    // Both paths have min_confidence=1.0, length=1, leaves differ by
    // lexicographic GlobalId.
    assert_eq!(
        leaves,
        vec![
            "repo-a:Function:src/w.rs:caller_one:0",
            "repo-b:Function:src/w.rs:caller_two:0",
        ]
    );
}

/// A transitive caller — `caller → middle → shared` — produces a path
/// of length 2 with `min_confidence = min(conf(caller→middle),
/// conf(middle→shared))`. The leaf is `caller`; hops are emitted in
/// seed → leaf order, so hops[0] is "arrived at middle via
/// middle→shared" and hops[1] is "arrived at caller via caller→middle".
#[test]
fn traverse_impact_transitive_path_carries_min_confidence() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_middle = node("repo-a:Function:src/y.rs:middle:0", "middle", "src/y.rs");
    let n_caller = node("repo-a:Function:src/z.rs:caller:0", "caller", "src/z.rs");
    for n in [&n_shared, &n_middle, &n_caller] {
        b.upsert_node(n.clone()).unwrap();
    }
    // Edges point INTO the callee: caller→middle, middle→shared.
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_caller.id.clone(),
        n_middle.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        0.6,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    assert_eq!(r.paths.len(), 1);
    let p = &r.paths[0];
    assert_eq!(p.hops.len(), 2);
    // Hops in seed → leaf order.
    assert_eq!(p.hops[0].edge.target_id, n_shared.id);
    assert_eq!(p.hops[0].node.id, n_middle.id);
    assert_eq!(p.hops[1].edge.target_id, n_middle.id);
    assert_eq!(p.hops[1].node.id, n_caller.id);
    // Leaf is `caller`, reached via `hops.last()`.
    assert_eq!(p.hops.last().unwrap().node.id, n_caller.id);
    // min_confidence is the min across hops.
    assert!((p.min_confidence - 0.6).abs() < 1e-6);
}

/// Edges whose confidence is below `min_confidence` are not followed.
/// With min_confidence = 0.5, the 0.6 edge survives and the 0.4 edge
/// is dropped — only the high-confidence caller reaches `shared`.
#[test]
fn traverse_impact_drops_edges_below_min_confidence() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_hi = node("repo-a:Function:src/a.rs:hi:0", "hi", "src/a.rs");
    let n_lo = node("repo-a:Function:src/b.rs:lo:0", "lo", "src/b.rs");
    for n in [&n_shared, &n_hi, &n_lo] {
        b.upsert_node(n.clone()).unwrap();
    }
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_hi.id.clone(),
        n_shared.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_lo.id.clone(),
        n_shared.id.clone(),
        0.4,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.5).unwrap();
    assert_eq!(r.paths.len(), 1);
    let leaf = &r.paths[0].hops.last().unwrap().node.id;
    assert_eq!(leaf, "repo-a:Function:src/a.rs:hi:0");
}

/// Tie-break on the predecessor's confidence. Both `winner` and
/// `loser` call `middle`, which calls `shared`. The BFS visits both
/// callers (each is a separate leaf); the `min_confidence` of each
/// path differs, so the output is sorted by `min_confidence` desc:
/// `winner` (path confidence = min(0.9, 1.0) = 0.9) before `loser`
/// (path confidence = min(0.5, 1.0) = 0.5).
#[test]
fn traverse_impact_predecessor_tiebreak_picks_higher_confidence() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_middle = node("repo-a:Function:src/y.rs:middle:0", "middle", "src/y.rs");
    let n_winner = node("repo-a:Function:src/a.rs:winner:0", "winner", "src/a.rs");
    let n_loser = node("repo-a:Function:src/b.rs:loser:0", "loser", "src/b.rs");
    for n in [&n_shared, &n_middle, &n_winner, &n_loser] {
        b.upsert_node(n.clone()).unwrap();
    }
    // middle → shared (confidence 1.0).
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();
    // winner → middle at confidence 0.9; loser → middle at 0.5.
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_winner.id.clone(),
        n_middle.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_loser.id.clone(),
        n_middle.id.clone(),
        0.5,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    // Both leaves are visited; two paths are emitted.
    assert_eq!(r.paths.len(), 2);
    let leaves: Vec<&str> = r
        .paths
        .iter()
        .map(|p| p.hops.last().unwrap().node.id.as_str())
        .collect();
    // `winner`'s path has higher min_confidence, so it sorts first.
    assert_eq!(
        leaves,
        vec![
            "repo-a:Function:src/a.rs:winner:0",
            "repo-a:Function:src/b.rs:loser:0",
        ]
    );
    // Path confidences: min(0.9, 1.0) = 0.9 for winner, min(0.5, 1.0) = 0.5 for loser.
    assert!((r.paths[0].min_confidence - 0.9).abs() < 1e-6);
    assert!((r.paths[1].min_confidence - 0.5).abs() < 1e-6);
}

/// Tie-break on the predecessor's id when confidences tie. Both
/// `zeta` and `alpha` call `middle` at confidence 0.7. The BFS visits
/// both leaves; with path confidences equal, the order falls through
/// to length (both length 2) and then to the leaf's GlobalId
/// (`alpha:0` < `zeta:0`).
#[test]
fn traverse_impact_predecessor_tiebreak_picks_smaller_global_id() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_middle = node("repo-a:Function:src/y.rs:middle:0", "middle", "src/y.rs");
    let n_zeta = node("repo-a:Function:src/a.rs:zeta:0", "zeta", "src/a.rs");
    let n_alpha = node("repo-a:Function:src/b.rs:alpha:0", "alpha", "src/b.rs");
    for n in [&n_shared, &n_middle, &n_zeta, &n_alpha] {
        b.upsert_node(n.clone()).unwrap();
    }
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();
    // Both predecessors at the SAME confidence.
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_zeta.id.clone(),
        n_middle.id.clone(),
        0.7,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_alpha.id.clone(),
        n_middle.id.clone(),
        0.7,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    assert_eq!(r.paths.len(), 2);
    let leaves: Vec<&str> = r
        .paths
        .iter()
        .map(|p| p.hops.last().unwrap().node.id.as_str())
        .collect();
    // Both paths have min_confidence = 0.7 and length 2; ties fall
    // through to the leaf's GlobalId (`a.rs:zeta:0` < `b.rs:alpha:0`,
    // since `a` < `b` at the first differing position).
    assert_eq!(
        leaves,
        vec![
            "repo-a:Function:src/a.rs:zeta:0",
            "repo-a:Function:src/b.rs:alpha:0",
        ]
    );
}

/// `depth` cuts the search — `caller → middle → deep → shared`. With
/// depth = 1, only `deep` is reached; `middle` and `caller` are out of
/// range. `truncated = true` because depth cut the search.
#[test]
fn traverse_impact_depth_cap_marks_truncated() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_deep = node("repo-a:Function:src/d.rs:deep:0", "deep", "src/d.rs");
    let n_middle = node("repo-a:Function:src/m.rs:middle:0", "middle", "src/m.rs");
    let n_caller = node("repo-a:Function:src/c.rs:caller:0", "caller", "src/c.rs");
    for n in [&n_shared, &n_deep, &n_middle, &n_caller] {
        b.upsert_node(n.clone()).unwrap();
    }
    // caller → middle → deep → shared
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_caller.id.clone(),
        n_middle.id.clone(),
        1.0,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_deep.id.clone(),
        1.0,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_deep.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 1, 100, 0.0).unwrap();
    assert_eq!(r.paths.len(), 1);
    assert_eq!(
        r.paths[0].hops.last().unwrap().node.id,
        "repo-a:Function:src/d.rs:deep:0"
    );
    assert!(r.truncated, "depth cap should mark truncated");

    // depth = 0 returns only the seed itself? No: §5.2 emits a path
    // for every visited node with no unvisited successor or sitting
    // at depth. With depth = 0 no edge is followed, so the seed has no
    // successor and a path is emitted for it.
    let r0 = b.traverse_impact(&[&n_shared.id], 0, 100, 0.0).unwrap();
    assert_eq!(r0.paths.len(), 1);
    assert_eq!(r0.paths[0].hops.last().unwrap().node.id, n_shared.id);
    assert!(r0.truncated);
}

/// `cap` limits the number of emitted paths and sets `truncated`.
#[test]
fn traverse_impact_cap_marks_truncated() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    b.upsert_node(n_shared.clone()).unwrap();
    for i in 0..3 {
        let id = format!("repo-a:Function:src/c{i}.rs:c{i}:0");
        let n = node(&id, &format!("c{i}"), &format!("src/c{i}.rs"));
        b.upsert_node(n.clone()).unwrap();
        b.upsert_edge(edge(
            EdgeType::Calls,
            n.id.clone(),
            n_shared.id.clone(),
            1.0,
        ))
        .unwrap();
    }
    let r = b.traverse_impact(&[&n_shared.id], 5, 2, 0.0).unwrap();
    assert_eq!(r.paths.len(), 2);
    assert!(r.truncated);
}

/// Path emission: a node with no unvisited successor emits a path
/// even when it's not at `depth`. Here `middle` has no caller, so the
/// path stops there; `caller` reaches `middle`, and `middle` is a
/// leaf relative to further propagation (no outgoing `Calls` from
/// it). The result is one path ending at `caller`.
#[test]
fn traverse_impact_emits_path_at_leaf_with_unvisited_successor() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_middle = node("repo-a:Function:src/m.rs:middle:0", "middle", "src/m.rs");
    let n_caller = node("repo-a:Function:src/c.rs:caller:0", "caller", "src/c.rs");
    for n in [&n_shared, &n_middle, &n_caller] {
        b.upsert_node(n.clone()).unwrap();
    }
    // caller → middle → shared. `middle` has only one caller (caller).
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_caller.id.clone(),
        n_middle.id.clone(),
        1.0,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    assert_eq!(r.paths.len(), 1);
    let leaf = &r.paths[0].hops.last().unwrap().node.id;
    assert_eq!(leaf, "repo-a:Function:src/c.rs:caller:0");
}

/// Edges whose `edge_type` is not `Incoming` (in PR 4: only `Calls`)
/// are not followed. `Uses` and `Imports` should be ignored even when
/// their confidence is high.
#[test]
fn traverse_impact_skips_non_incoming_edge_types() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_user = node("repo-a:Function:src/u.rs:user:0", "user", "src/u.rs");
    let n_importer = node("repo-a:File:src/i.rs:0", "i.rs", "src/i.rs");
    for n in [&n_shared, &n_user, &n_importer] {
        b.upsert_node(n.clone()).unwrap();
    }
    // Non-Incoming edges (Uses, Imports) — must be ignored.
    b.upsert_edge(edge(
        EdgeType::Uses,
        n_user.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Imports,
        n_importer.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();
    // Plus a real Calls edge that should be followed.
    let n_caller = node("repo-a:Function:src/c.rs:caller:0", "caller", "src/c.rs");
    b.upsert_node(n_caller.clone()).unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_caller.id.clone(),
        n_shared.id.clone(),
        1.0,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    assert_eq!(r.paths.len(), 1);
    let leaf = &r.paths[0].hops.last().unwrap().node.id;
    assert_eq!(leaf, "repo-a:Function:src/c.rs:caller:0");
}

/// Determinism: same input → identical output ordering. Run the
/// traversal twice and assert byte-equal results.
#[test]
fn traverse_impact_output_is_deterministic_across_runs() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_middle = node("repo-a:Function:src/m.rs:middle:0", "middle", "src/m.rs");
    let n_a = node("repo-a:Function:src/a.rs:a:0", "a", "src/a.rs");
    let n_b = node("repo-a:Function:src/b.rs:b:0", "b", "src/b.rs");
    let n_c = node("repo-a:Function:src/c.rs:c:0", "c", "src/c.rs");
    for n in [&n_shared, &n_middle, &n_a, &n_b, &n_c] {
        b.upsert_node(n.clone()).unwrap();
    }
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_a.id.clone(),
        n_shared.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_b.id.clone(),
        n_middle.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        0.9,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_c.id.clone(),
        n_shared.id.clone(),
        0.9,
    ))
    .unwrap();

    let r1 = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    let r2 = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();

    fn shape(r: &ImpactResult) -> Vec<(String, f32, usize)> {
        r.paths
            .iter()
            .map(|p| {
                let leaf = p.hops.last().unwrap().node.id.clone();
                (leaf, p.min_confidence, p.hops.len())
            })
            .collect()
    }
    assert_eq!(shape(&r1), shape(&r2));
}

/// Sort order: paths ordered by min_confidence desc, then length
/// asc, then leaf GlobalId. Construct three paths covering all three
/// ordering axes.
#[test]
fn traverse_impact_paths_are_sorted_by_confidence_then_length_then_leaf() {
    let b = HashMapBackend::new();
    let n_shared = node("repo-a:Function:src/x.rs:shared:0", "shared", "src/x.rs");
    let n_a = node("repo-a:Function:src/a.rs:a:0", "a", "src/a.rs");
    let n_b = node("repo-a:Function:src/b.rs:b:0", "b", "src/b.rs");
    let n_c = node("repo-a:Function:src/c.rs:c:0", "c", "src/c.rs");
    let n_middle = node("repo-a:Function:src/m.rs:middle:0", "middle", "src/m.rs");
    for n in [&n_shared, &n_a, &n_b, &n_c, &n_middle] {
        b.upsert_node(n.clone()).unwrap();
    }
    // Path A: shared ← a (length 1, conf 0.9)
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_a.id.clone(),
        n_shared.id.clone(),
        0.9,
    ))
    .unwrap();
    // Path B: shared ← b (length 1, conf 0.5)
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_b.id.clone(),
        n_shared.id.clone(),
        0.5,
    ))
    .unwrap();
    // Path C: shared ← middle ← c (length 2, conf 0.7)
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_middle.id.clone(),
        n_shared.id.clone(),
        0.7,
    ))
    .unwrap();
    b.upsert_edge(edge(
        EdgeType::Calls,
        n_c.id.clone(),
        n_middle.id.clone(),
        0.7,
    ))
    .unwrap();

    let r = b.traverse_impact(&[&n_shared.id], 5, 100, 0.0).unwrap();
    let leaves: Vec<&str> = r
        .paths
        .iter()
        .map(|p| p.hops.last().unwrap().node.id.as_str())
        .collect();
    // Expected order: A (0.9, len 1), C (0.7, len 2), B (0.5, len 1).
    assert_eq!(
        leaves,
        vec![
            "repo-a:Function:src/a.rs:a:0",
            "repo-a:Function:src/c.rs:c:0",
            "repo-a:Function:src/b.rs:b:0",
        ]
    );
}

// ─── helpers ─────────────────────────────────────────────────────────

fn node(id: &str, name: &str, path: &str) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.to_string(), path.to_string());
    n.id = id.to_string();
    n
}

fn edge(t: EdgeType, source: String, target: String, weight: f32) -> GraphEdge {
    let mut e = GraphEdge::new(t, source, target);
    e.weight = Some(weight);
    e
}

fn reconstruct_test_path(
    leaf: &str,
    pred: &HashMap<String, (String, GraphEdge, f32)>,
    nodes: &HashMap<String, GraphNode>,
) -> ImpactPath {
    let mut rev: Vec<ImpactHop> = Vec::new();
    let mut current = leaf.to_string();
    let mut min_conf = 1.0_f32;
    while let Some((prev_id, edge, conf)) = pred.get(&current) {
        let node = nodes.get(&current).cloned().unwrap_or_else(|| {
            let mut n = GraphNode::new(NodeType::Function, String::new(), String::new());
            n.id = current.clone();
            n
        });
        rev.push(ImpactHop {
            edge: edge.clone(),
            node,
        });
        min_conf = min_conf.min(*conf);
        current = prev_id.clone();
    }
    if rev.is_empty() {
        let node = nodes.get(leaf).cloned().unwrap_or_else(|| {
            let mut n = GraphNode::new(NodeType::Function, String::new(), String::new());
            n.id = leaf.to_string();
            n
        });
        let mut placeholder_edge =
            GraphEdge::new(EdgeType::Calls, leaf.to_string(), leaf.to_string());
        placeholder_edge.weight = Some(1.0);
        rev.push(ImpactHop {
            edge: placeholder_edge,
            node,
        });
    } else {
        rev.reverse();
    }
    ImpactPath {
        hops: rev,
        min_confidence: if pred.is_empty() { 1.0 } else { min_conf },
    }
}
