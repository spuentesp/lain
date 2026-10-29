//! Contract tests for GraphBackend. The same tests will run against PetgraphBackend
//! in Task 7. Here we use a simple in-memory HashMap impl to define the contract.
use crate::error::LainError;
use crate::federation::graph_backend::{
    impact_propagation, GraphBackend, ImpactHop, ImpactPath, ImpactResult, PetgraphBackend,
    Propagation, FEDERATION_GRAPH_MAGIC,
};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::RwLock;

pub struct HashMapBackend {
    nodes: RwLock<HashMap<String, GraphNode>>,
    edges: RwLock<Vec<GraphEdge>>,
    /// How many times `all_edges` (a full clone) was called, so a test can
    /// prove a per-item path stays on `edges_to`.
    all_edges_calls: std::sync::atomic::AtomicUsize,
    /// Same for `list_nodes`.
    list_nodes_calls: std::sync::atomic::AtomicUsize,
}

impl HashMapBackend {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
            all_edges_calls: std::sync::atomic::AtomicUsize::new(0),
            list_nodes_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn all_edges_calls(&self) -> usize {
        self.all_edges_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Number of stored edges, without going through the counted accessor.
    pub(crate) fn edges_to_total_for_test(&self) -> usize {
        self.edges.read().unwrap().len()
    }

    pub(crate) fn list_nodes_calls(&self) -> usize {
        self.list_nodes_calls
            .load(std::sync::atomic::Ordering::SeqCst)
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
        self.list_nodes_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.nodes.read().unwrap().values().cloned().collect())
    }
    // By-reference visits, like the petgraph backend's: no clone, no counter.
    fn visit_edges(&self, f: &mut dyn FnMut(&GraphEdge)) -> Result<(), LainError> {
        for e in self.edges.read().unwrap().iter() {
            f(e);
        }
        Ok(())
    }
    fn visit_nodes(&self, f: &mut dyn FnMut(&GraphNode)) -> Result<(), LainError> {
        for n in self.nodes.read().unwrap().values() {
            f(n);
        }
        Ok(())
    }
    fn edges_from(
        &self,
        source_id: &str,
        edge_type: EdgeType,
    ) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .edges
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.edge_type == edge_type && e.source_id == source_id)
            .cloned()
            .collect())
    }
    fn all_edges(&self) -> Result<Vec<GraphEdge>, LainError> {
        self.all_edges_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.edges.read().unwrap().clone())
    }
    // Answers without `all_edges`, like the petgraph backend's index lookup.
    fn edges_to(&self, target_id: &str, edge_type: EdgeType) -> Result<Vec<GraphEdge>, LainError> {
        Ok(self
            .edges
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.edge_type == edge_type && e.target_id == target_id)
            .cloned()
            .collect())
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
    // Write a federated_graph.bin with header = 1 (pre-bump).
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("federated_graph.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(FEDERATION_GRAPH_MAGIC);
    bytes.extend_from_slice(&1u32.to_le_bytes()); // legacy version
    bytes.extend_from_slice(&[0u8; 16]); // payload placeholder
    std::fs::write(&bin_path, &bytes).unwrap();

    // Loading must return FederationSchemaMismatch, not a parse error
    // and not a silent success.
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
        EdgeType::CallsGraphql,
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
            | EdgeType::CallsGraphql
            | EdgeType::SendsHttp
            | EdgeType::Binds
            | EdgeType::RequestSchema
            | EdgeType::ResponseSchema
            | EdgeType::HasField
            | EdgeType::ReadsField
            // Anchor-miner stage: heuristic BusTopic / RouteMatches
            // hops now flow through impact paths (their confidence
            // keeps them filterable; claims grade them
            // needs-investigation).
            | EdgeType::BusTopic
            | EdgeType::RouteMatches => Propagation::Incoming,
            _ => Propagation::Stop,
        };
        assert_eq!(got, expected, "propagation mismatch for {e:?}");
    }
    // PR 9 actual state, plus the BusTopic/RouteMatches anchor-miner
    // flip: 10 Incoming, 0 Outgoing, 14 Stop.
    assert_eq!(cases.len(), 25, "every EdgeType variant must be listed");
    let incoming_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Incoming)
        .count();
    assert_eq!(incoming_count, 11);
    let outgoing_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Outgoing)
        .count();
    assert_eq!(outgoing_count, 0);
    let stop_count = cases
        .iter()
        .filter(|e| impact_propagation(e) == Propagation::Stop)
        .count();
    assert_eq!(stop_count, 14);
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

// ─── edges_to: the per-node lookup that replaces clone-and-scan ──────

/// `edges_to` filters by target AND edge type, on the indexed petgraph
/// backend and on the default scan alike.
#[test]
fn edges_to_filters_by_target_and_type_on_both_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let petgraph = PetgraphBackend::new(tmp.path()).unwrap();
    let simple = HashMapBackend::new();
    for b in [&petgraph as &dyn GraphBackend, &simple as &dyn GraphBackend] {
        b.upsert_nodes_batch(&[
            node("r:Function:a.py:a:0", "a", "a.py"),
            node("r:Function:b.py:b:0", "b", "b.py"),
            node("r:Function:t.py:t:0", "t", "t.py"),
        ])
        .unwrap();
        b.upsert_edges_batch(&[
            edge(
                EdgeType::Calls,
                "r:Function:a.py:a:0".into(),
                "r:Function:t.py:t:0".into(),
                1.0,
            ),
            edge(
                EdgeType::Calls,
                "r:Function:b.py:b:0".into(),
                "r:Function:t.py:t:0".into(),
                1.0,
            ),
            edge(
                EdgeType::SendsHttp,
                "r:Function:a.py:a:0".into(),
                "r:Function:t.py:t:0".into(),
                1.0,
            ),
            edge(
                EdgeType::Calls,
                "r:Function:t.py:t:0".into(),
                "r:Function:a.py:a:0".into(),
                1.0,
            ),
        ])
        .unwrap();
        let mut calls: Vec<String> = b
            .edges_to("r:Function:t.py:t:0", EdgeType::Calls)
            .unwrap()
            .into_iter()
            .map(|e| e.source_id)
            .collect();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                "r:Function:a.py:a:0".to_string(),
                "r:Function:b.py:b:0".to_string()
            ]
        );
        assert_eq!(
            b.edges_to("r:Function:t.py:t:0", EdgeType::SendsHttp)
                .unwrap()
                .len(),
            1
        );
        assert!(b.edges_to("nope", EdgeType::Calls).unwrap().is_empty());
    }
}

/// `get_service` runs `walk_backend` once per consumer row: it must stay
/// on `edges_to` and never clone the whole edge set.
#[test]
fn used_by_walk_never_clones_the_edge_set() {
    let b = HashMapBackend::new();
    b.upsert_nodes_batch(&[
        node("leaf", "leaf", "x.py"),
        node("mid", "mid", "x.py"),
        node("top", "top", "x.py"),
    ])
    .unwrap();
    b.upsert_edges_batch(&[
        edge(EdgeType::Calls, "mid".into(), "leaf".into(), 1.0),
        edge(EdgeType::Calls, "top".into(), "mid".into(), 1.0),
    ])
    .unwrap();
    let r = crate::server::mcp::contract_tools::used_by::walk_backend(&b, "leaf", 4);
    assert_eq!(
        r.entries.len(),
        1,
        "the unreferenced top of the chain: {:?}",
        r.entries
    );
    assert_eq!(r.entries[0]["name"], serde_json::json!("top"));
    assert_eq!(
        b.all_edges_calls(),
        0,
        "walk_backend cloned the whole edge set"
    );
}

/// The live path: `walk_backend` over the petgraph backend finds the entry
/// point two hops up, through the indexed `edges_to`.
#[test]
fn used_by_walk_on_the_petgraph_backend_reaches_the_entry_point() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    let leaf = node("r:Function:x.py:leaf:0", "leaf", "x.py");
    let mid = node("r:Function:x.py:mid:5", "mid", "x.py");
    let mut top = node("r:Function:x.py:top:10", "top", "x.py");
    top.entry = Some(crate::federation::contracts::model::EntryKind::HttpHandler);
    b.upsert_nodes_batch(&[leaf, mid, top]).unwrap();
    b.upsert_edges_batch(&[
        edge(
            EdgeType::Calls,
            "r:Function:x.py:mid:5".into(),
            "r:Function:x.py:leaf:0".into(),
            1.0,
        ),
        edge(
            EdgeType::Calls,
            "r:Function:x.py:top:10".into(),
            "r:Function:x.py:mid:5".into(),
            1.0,
        ),
    ])
    .unwrap();
    let r =
        crate::server::mcp::contract_tools::used_by::walk_backend(&b, "r:Function:x.py:leaf:0", 4);
    assert_eq!(r.entries.len(), 1, "{:?}", r.entries);
    assert_eq!(r.entries[0]["kind"], serde_json::json!("http_handler"));
    assert_eq!(r.entries[0]["name"], serde_json::json!("top"));
    assert!(!r.truncated);
}

// ─── streamed save: the files on disk ────────────────────────────────

fn seeded_backend(dir: &std::path::Path, n: usize) -> PetgraphBackend {
    let b = PetgraphBackend::new(dir).unwrap();
    let ids: Vec<String> = (0..n)
        .map(|i| format!("r:Function:src/f{}.py:fn_{i:03}:{i}", i % 7))
        .collect();
    let nodes: Vec<GraphNode> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| node(id, &format!("fn_{i:03}"), &format!("src/f{}.py", i % 7)))
        .collect();
    b.upsert_nodes_batch(&nodes).unwrap();
    let edges: Vec<GraphEdge> = ids
        .windows(2)
        .map(|w| edge(EdgeType::Calls, w[0].clone(), w[1].clone(), 1.0))
        .collect();
    b.upsert_edges_batch(&edges).unwrap();
    b
}

/// The save is streamed to two files in one pass. `federated_graph.bin` is the
/// schema envelope followed by exactly the `.payload` bytes, and a fresh
/// backend reloads the same graph from it.
#[test]
fn streamed_save_writes_header_plus_payload_and_reloads() {
    use crate::federation::graph_backend::{FEDERATION_GRAPH_HEADER_LEN, FEDERATION_GRAPH_VERSION};
    let tmp = tempfile::tempdir().unwrap();
    let b = seeded_backend(tmp.path(), 120);
    let (nodes, edges) = (b.node_count(), b.edge_count());
    assert_eq!((nodes, edges), (120, 119));
    drop(b);

    let bin = std::fs::read(tmp.path().join("federated_graph.bin")).unwrap();
    let payload = std::fs::read(tmp.path().join("federated_graph.bin.payload")).unwrap();
    assert_eq!(&bin[..FEDERATION_GRAPH_MAGIC.len()], FEDERATION_GRAPH_MAGIC);
    assert_eq!(
        &bin[FEDERATION_GRAPH_MAGIC.len()..FEDERATION_GRAPH_HEADER_LEN],
        &FEDERATION_GRAPH_VERSION.to_le_bytes()
    );
    assert_eq!(&bin[FEDERATION_GRAPH_HEADER_LEN..], payload.as_slice());

    let again = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!((again.node_count(), again.edge_count()), (nodes, edges));
    let sample = again
        .get_node("r:Function:src/f3.py:fn_003:3")
        .unwrap()
        .unwrap();
    assert_eq!(sample.name, "fn_003");
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

/// A federated file written the old way (clone the graph, build the payload
/// in memory, prepend the header) must still load: the format did not move.
#[test]
fn a_file_written_by_the_old_buffered_path_still_loads() {
    use crate::federation::graph_backend::FEDERATION_GRAPH_VERSION;
    let src = tempfile::tempdir().unwrap();
    let source = seeded_backend(src.path(), 80);
    let old_payload = source.db().encode_snapshot_buffered();
    let (nodes, edges) = (source.node_count(), source.edge_count());
    drop(source);

    let dst = tempfile::tempdir().unwrap();
    let mut old_file = Vec::new();
    old_file.extend_from_slice(FEDERATION_GRAPH_MAGIC);
    old_file.extend_from_slice(&FEDERATION_GRAPH_VERSION.to_le_bytes());
    old_file.extend_from_slice(&old_payload);
    std::fs::write(dst.path().join("federated_graph.bin"), &old_file).unwrap();

    let loaded = PetgraphBackend::new(dst.path()).unwrap();
    assert_eq!((loaded.node_count(), loaded.edge_count()), (nodes, edges));
    assert!(loaded
        .get_node("r:Function:src/f0.py:fn_000:0")
        .unwrap()
        .is_some());
}

/// A torn `.bin` (the old file cut short) is still rejected by the loader,
/// which is the guard the atomic rename exists to make unreachable.
#[test]
fn a_truncated_federated_file_is_still_rejected() {
    let src = tempfile::tempdir().unwrap();
    drop(seeded_backend(src.path(), 40));
    let mut bytes = std::fs::read(src.path().join("federated_graph.bin")).unwrap();
    bytes.truncate(bytes.len() / 2);
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(dst.path().join("federated_graph.bin"), &bytes).unwrap();
    assert!(PetgraphBackend::new(dst.path()).is_err());
}

// ─── edge/node visitors and indexed lookups ──────────────────────────

fn seeded_backends(dir: &std::path::Path) -> (PetgraphBackend, HashMapBackend) {
    let petgraph = PetgraphBackend::new(dir).unwrap();
    let simple = HashMapBackend::new();
    for b in [&petgraph as &dyn GraphBackend, &simple as &dyn GraphBackend] {
        b.upsert_nodes_batch(&[
            node("r:Function:a.py:a:0", "a", "a.py"),
            node("r:Function:b.py:b:0", "b", "b.py"),
            node("r:Function:t.py:t:0", "t", "t.py"),
        ])
        .unwrap();
        b.upsert_edges_batch(&[
            edge(
                EdgeType::Calls,
                "r:Function:a.py:a:0".into(),
                "r:Function:t.py:t:0".into(),
                1.0,
            ),
            edge(
                EdgeType::Calls,
                "r:Function:b.py:b:0".into(),
                "r:Function:t.py:t:0".into(),
                1.0,
            ),
            edge(
                EdgeType::SendsHttp,
                "r:Function:a.py:a:0".into(),
                "r:Function:b.py:b:0".into(),
                1.0,
            ),
            edge(
                EdgeType::Calls,
                "r:Function:t.py:t:0".into(),
                "r:Function:a.py:a:0".into(),
                1.0,
            ),
        ])
        .unwrap();
    }
    (petgraph, simple)
}

/// `visit_edges` / `visit_nodes` see exactly what `all_edges` / `list_nodes`
/// return, in the same order, on the indexed and the default implementation.
#[test]
fn visitors_see_what_the_cloning_accessors_return() {
    let tmp = tempfile::tempdir().unwrap();
    let (petgraph, simple) = seeded_backends(tmp.path());
    for b in [&petgraph as &dyn GraphBackend, &simple as &dyn GraphBackend] {
        let mut visited_edges: Vec<(String, String, String)> = Vec::new();
        b.visit_edges(&mut |e| {
            visited_edges.push((
                e.source_id.clone(),
                e.target_id.clone(),
                format!("{:?}", e.edge_type),
            ))
        })
        .unwrap();
        let listed_edges: Vec<(String, String, String)> = b
            .all_edges()
            .unwrap()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, format!("{:?}", e.edge_type)))
            .collect();
        assert_eq!(visited_edges, listed_edges);
        assert_eq!(visited_edges.len(), 4);

        let mut visited_nodes: Vec<String> = Vec::new();
        b.visit_nodes(&mut |n| visited_nodes.push(n.id.clone()))
            .unwrap();
        let mut listed_nodes: Vec<String> =
            b.list_nodes().unwrap().into_iter().map(|n| n.id).collect();
        // The simple backend lists a HashMap (no order); the contract is the
        // same set, and the same order as `list_nodes` on petgraph.
        visited_nodes.sort();
        listed_nodes.sort();
        assert_eq!(visited_nodes, listed_nodes);
        assert_eq!(visited_nodes.len(), 3);
    }
}

/// `edges_from` filters by source AND type on both implementations.
#[test]
fn edges_from_filters_by_source_and_type_on_both_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let (petgraph, simple) = seeded_backends(tmp.path());
    for b in [&petgraph as &dyn GraphBackend, &simple as &dyn GraphBackend] {
        let calls: Vec<String> = b
            .edges_from("r:Function:a.py:a:0", EdgeType::Calls)
            .unwrap()
            .into_iter()
            .map(|e| e.target_id)
            .collect();
        assert_eq!(calls, vec!["r:Function:t.py:t:0".to_string()]);
        assert_eq!(
            b.edges_from("r:Function:a.py:a:0", EdgeType::SendsHttp)
                .unwrap()
                .len(),
            1
        );
        assert!(b
            .edges_from("r:Function:b.py:b:0", EdgeType::SendsHttp)
            .unwrap()
            .is_empty());
        assert!(b.edges_from("nope", EdgeType::Calls).unwrap().is_empty());
    }
}

// ─── traverse_impact: the rewrite must equal the code it replaced ────

// The pre-rewrite implementation, verbatim apart from taking the backend as
// an argument. It clones every node and every qualifying edge per call —
// the cost the rewrite removed — and stays here only as the oracle.
#[allow(clippy::type_complexity)]
fn reference_traverse_impact(
    backend: &dyn GraphBackend,
    starts: &[&str],
    depth: u32,
    cap: usize,
    min_confidence: f32,
) -> Result<ImpactResult, LainError> {
    // Snapshot the relevant data up front so the BFS walks a
    // borrowed view rather than holding the graph lock across the
    // whole traversal. `all_edges` is `O(E)` per call, but the
    // federation's BFS hot path runs against a snapshot the
    // server already keeps in memory; the per-call overhead is
    // acceptable here, and `MemgraphBackend` (the deferred escape
    // hatch called out in `federation/AGENTS.md`) can override this
    // with index-only walks without changing the signature.
    let mut nodes: std::collections::HashMap<String, GraphNode> = backend
        .list_nodes()?
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();
    let edges = backend.all_edges()?;
    let mut by_target: std::collections::HashMap<String, Vec<(f32, String, GraphEdge)>> =
        std::collections::HashMap::new();
    for e in edges {
        // Confidence must be evidence-backed. Explicit weights win;
        // otherwise derive it from provenance. A legacy edge with
        // neither is unknown (0.0), not implicitly verified static data.
        let conf = e.weight.unwrap_or(match e.provenance.as_ref() {
            Some(crate::schema::EdgeProvenance::Static { .. })
            | Some(crate::schema::EdgeProvenance::Confirmed { .. }) => 1.0,
            Some(crate::schema::EdgeProvenance::Heuristic { confidence, .. }) => *confidence,
            Some(crate::schema::EdgeProvenance::Runtime { .. }) => 0.9,
            None => 0.0,
        });
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
    // Stable predecessor selection: each entry holds the
    // candidate's confidence and source id; the BFS picks the
    // highest-confidence, then lexicographically smallest id.
    for entries in by_target.values_mut() {
        entries.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
    }

    // BFS from every start at distance 0. The brief: "an endpoint
    // has one start per provider node" — every entry of `starts`
    // is seeded independently.
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    for s in starts {
        if nodes.contains_key(*s) && visited.insert((*s).to_string()) {
            queue.push_back(((*s).to_string(), 0));
        }
    }

    let mut paths: Vec<ImpactPath> = Vec::new();
    let mut truncated = false;

    // For each visited node, remember (predecessor id, incoming
    // edge, min_confidence so far). Used to reconstruct paths
    // when a leaf is reached. The first predecessor to claim a
    // node wins (already ordered by the sort above), so output
    // is deterministic.
    let mut pred: std::collections::HashMap<String, (String, GraphEdge, f32)> =
        std::collections::HashMap::new();

    while let Some((current, current_depth)) = queue.pop_front() {
        let successors = by_target.get(&current);
        let has_unvisited_successor = successors
            .map(|v| v.iter().any(|(_, id, _)| !visited.contains(id)))
            .unwrap_or(false);

        // §5.2 emission rule: emit a path when (a) at `depth` or
        // (b) no unvisited successor. The seed itself, when it
        // has no edges, also satisfies (b).
        let at_depth = current_depth >= depth;
        if at_depth || !has_unvisited_successor {
            if paths.len() >= cap {
                truncated = true;
            } else {
                paths.push(reference_reconstruct_path(&current, &pred, &mut nodes));
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
                // Edge confidence already passed the floor at
                // indexing time. The `min_confidence` carried on
                // the path is the minimum of every edge on it,
                // updated here as we descend.
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

    // Sort by min_confidence desc, then length asc, then leaf id.
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

fn reference_reconstruct_path(
    leaf: &str,
    pred: &std::collections::HashMap<String, (String, GraphEdge, f32)>,
    nodes: &mut std::collections::HashMap<String, GraphNode>,
) -> ImpactPath {
    // Walk back through `pred` from `leaf` to the seed. Hops are
    // emitted in seed → leaf order so callers can read them as a
    // story: "the seed, reached via edge E1, then node N1, via edge
    // E2, then node N2, …".
    let mut rev: Vec<ImpactHop> = Vec::new();
    let mut current = leaf.to_string();
    let mut min_conf = 1.0_f32;
    while let Some((prev_id, edge, conf)) = pred.get(&current) {
        // The hop carries the node we arrived AT (i.e. `current`),
        // not the predecessor.
        let node = nodes.get(&current).cloned().unwrap_or_else(|| {
            // Defensive: an entry in `pred` whose node vanished
            // between edge indexing and path emission would be a
            // torn-write we cannot recover from. Surface an empty
            // placeholder so the test still parses; the emit path is
            // deterministic and the trace will point at this hop.
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
        // Degenerate path: the leaf IS a seed (or has no predecessors).
        // Emit a single self-loop hop so callers can read the path's
        // node without unwrapping an empty `hops` vec. The edge
        // carries the same id as source and target so it doesn't
        // masquerade as a real edge in any consumer.
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

type PathSig = (Vec<(String, String, String, Option<u32>, String)>, u32);

/// Everything observable about a result: every hop's edge identity and
/// weight, the node reached, each path's min confidence, and `truncated`.
fn impact_signature(r: &ImpactResult) -> (bool, Vec<PathSig>) {
    let paths = r
        .paths
        .iter()
        .map(|p| {
            let hops = p
                .hops
                .iter()
                .map(|h| {
                    (
                        h.edge.source_id.clone(),
                        h.edge.target_id.clone(),
                        format!("{:?}", h.edge.edge_type),
                        h.edge.weight.map(f32::to_bits),
                        h.node.id.clone(),
                    )
                })
                .collect();
            (hops, p.min_confidence.to_bits())
        })
        .collect();
    (r.truncated, paths)
}

struct Xorshift(u64);
impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Randomized graphs with parallel edges of different types, self loops,
/// many equal-confidence ties, every provenance kind, and edge types of all
/// three propagation classes: the new index-based `traverse_impact` returns
/// exactly what the old clone-everything one did, query for query.
#[test]
fn traverse_impact_equals_the_reference_on_random_graphs() {
    use crate::schema::{EdgeProvenance, StaticSource};
    let types = [
        EdgeType::Calls,
        EdgeType::CallsHttp,
        EdgeType::CallsGraphql,
        EdgeType::SendsHttp,
        EdgeType::Binds,
        EdgeType::HasField,
        EdgeType::ReadsTable,
        EdgeType::WritesTable,
        EdgeType::BusTopic,
        EdgeType::RouteMatches,
        EdgeType::Consumes,
        EdgeType::Produces,
        EdgeType::Contains,
    ];
    let mut compared = 0usize;
    let mut nonempty = 0usize;
    for seed in 1..=40u64 {
        let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xA24B_AED4_963E_E407));
        let n = 12 + rng.below(40);
        let ids: Vec<String> = (0..n)
            .map(|i| format!("r:Function:f{i}.py:n{i}:0"))
            .collect();
        let tmp = tempfile::tempdir().unwrap();
        let backend = PetgraphBackend::new(tmp.path()).unwrap();
        backend
            .upsert_nodes_batch(
                &ids.iter()
                    .enumerate()
                    .map(|(i, id)| node(id, &format!("n{i}"), &format!("f{i}.py")))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut edges = Vec::new();
        for _ in 0..(n * 4) {
            let (s, t) = (rng.below(n), rng.below(n));
            let ty = types[rng.below(types.len())].clone();
            if !seen.insert((s, t, format!("{ty:?}"))) {
                continue;
            }
            let mut e = GraphEdge::new(ty, ids[s].clone(), ids[t].clone());
            e.weight = [None, Some(0.3), Some(0.6), Some(0.9), Some(1.0)][rng.below(5)];
            e.provenance = match rng.below(4) {
                0 => None,
                1 => Some(EdgeProvenance::Static {
                    source: StaticSource::Regex,
                }),
                2 => Some(EdgeProvenance::Heuristic {
                    detector: "d".into(),
                    confidence: [0.2, 0.5, 0.8][rng.below(3)],
                }),
                _ => Some(EdgeProvenance::Runtime {
                    trace_id: "t".into(),
                    last_seen_unix: 0,
                }),
            };
            edges.push(e);
        }
        backend.upsert_edges_batch(&edges).unwrap();

        for _ in 0..6 {
            let mut starts: Vec<&str> = (0..1 + rng.below(3))
                .map(|_| ids[rng.below(n)].as_str())
                .collect();
            if rng.below(5) == 0 {
                starts.push("r:Function:missing.py:ghost:0");
            }
            let depth = [1u32, 2, 3, 6][rng.below(4)];
            let cap = [1usize, 3, 50][rng.below(3)];
            let min_conf = [0.0f32, 0.5, 0.95][rng.below(3)];
            let new = backend
                .traverse_impact(&starts, depth, cap, min_conf)
                .unwrap();
            let reference =
                reference_traverse_impact(&backend, &starts, depth, cap, min_conf).unwrap();
            assert_eq!(
                impact_signature(&new),
                impact_signature(&reference),
                "seed {seed}: starts={starts:?} depth={depth} cap={cap} min_conf={min_conf}"
            );
            compared += 1;
            if new.paths.iter().any(|p| p.hops.len() > 1) {
                nonempty += 1;
            }
        }
    }
    assert_eq!(compared, 240);
    assert!(
        nonempty > 40,
        "the generator must exercise real multi-hop paths, saw {nonempty}"
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

mod envelope_fuzz {
    //! `federated_graph.bin` is read at startup from disk the operator may have
    //! hand-edited, truncated by a crash, or filled by another version. Loading
    //! must produce a graph or a *typed* refusal; never a panic, and never a
    //! silently partial graph from a damaged body.
    use super::*;
    use proptest::prelude::*;

    fn valid_file() -> (tempfile::TempDir, Vec<u8>) {
        let tmp = tempfile::tempdir().unwrap();
        let b = PetgraphBackend::new(tmp.path()).unwrap();
        for i in 0..5 {
            b.upsert_node_global(
                &format!("r:Function:src/lib.rs:f{i}:0"),
                NodeType::Function,
                "src/lib.rs",
                &format!("f{i}"),
            )
            .unwrap();
        }
        drop(b);
        let bytes = std::fs::read(tmp.path().join("federated_graph.bin")).unwrap();
        (tmp, bytes)
    }

    fn load(bytes: &[u8]) -> Result<usize, LainError> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("federated_graph.bin"), bytes).unwrap();
        PetgraphBackend::new(dir.path()).map(|b| b.node_count())
    }

    fn typed(e: &LainError) -> bool {
        matches!(
            e,
            LainError::FederationSchemaMismatch { .. } | LainError::FederationPayloadCorrupt { .. }
        )
    }

    #[test]
    fn a_valid_file_reloads_every_node() {
        let (_t, bytes) = valid_file();
        assert_eq!(load(&bytes).unwrap(), 5);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
            if let Err(e) = load(&bytes) { prop_assert!(typed(&e), "untyped error: {e:?}"); }
        }

        /// Arbitrary bytes behind a *valid* header exercise the payload validator.
        #[test]
        fn valid_header_with_garbage_body_is_refused_or_loads_cleanly(
            body in prop::collection::vec(any::<u8>(), 0..300),
        ) {
            let mut bytes = FEDERATION_GRAPH_MAGIC.to_vec();
            bytes.extend_from_slice(&crate::federation::graph_backend::FEDERATION_GRAPH_VERSION.to_le_bytes());
            bytes.extend_from_slice(&body);
            if let Err(e) = load(&bytes) { prop_assert!(typed(&e), "untyped error: {e:?}"); }
        }

        /// Every truncation of a real file is refused (not loaded as a smaller graph).
        #[test]
        fn truncated_real_files_are_never_loaded_partially(cut_frac in 0.0f64..0.999) {
            let (_t, bytes) = valid_file();
            let cut = ((bytes.len() as f64) * cut_frac) as usize;
            match load(&bytes[..cut]) {
                Ok(n) => prop_assert_eq!(n, 5, "a truncated file loaded as a partial graph ({} bytes of {})", cut, bytes.len()),
                Err(e) => prop_assert!(typed(&e), "untyped error: {e:?}"),
            }
        }

        /// A flipped byte either keeps the graph intact or is refused; it never panics.
        #[test]
        fn single_byte_corruption_never_panics(pos_frac in 0.0f64..1.0, xor in 1u8..=255) {
            let (_t, mut bytes) = valid_file();
            let pos = ((bytes.len() - 1) as f64 * pos_frac) as usize;
            bytes[pos] ^= xor;
            if let Err(e) = load(&bytes) { prop_assert!(typed(&e), "untyped error: {e:?}"); }
        }
    }
}

#[test]
fn bulk_section_writes_the_file_once_when_it_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("federated_graph.bin");
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.begin_bulk();
    b.upsert_node_global(
        "repo1:Function:src/lib.rs:f:0",
        NodeType::Function,
        "src/lib.rs",
        "f",
    )
    .unwrap();
    assert!(!bin.exists(), "no checkpoint while a bulk section is open");
    assert!(b.has_node("repo1:Function:src/lib.rs:f:0").unwrap());
    b.end_bulk().unwrap();
    assert!(bin.exists(), "closing a dirty section writes the file");
    drop(b);
    let reloaded = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!(reloaded.node_count(), 1);
}

#[test]
fn nested_bulk_sections_save_only_when_the_outermost_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("federated_graph.bin");
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.begin_bulk();
    b.begin_bulk();
    b.upsert_node_global(
        "repo1:Function:src/lib.rs:f:0",
        NodeType::Function,
        "src/lib.rs",
        "f",
    )
    .unwrap();
    b.end_bulk().unwrap();
    assert!(!bin.exists(), "the inner section must not save");
    b.end_bulk().unwrap();
    assert!(bin.exists(), "the outer section saves");
}

#[test]
fn closing_a_clean_bulk_section_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("federated_graph.bin");
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.begin_bulk();
    b.end_bulk().unwrap();
    assert!(
        !bin.exists(),
        "no writes inside the section, so nothing to save"
    );
}

#[test]
fn has_node_reports_only_nodes_that_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    assert!(!b.has_node("repo1:Function:src/lib.rs:f:0").unwrap());
    b.upsert_node_global(
        "repo1:Function:src/lib.rs:f:0",
        NodeType::Function,
        "src/lib.rs",
        "f",
    )
    .unwrap();
    assert!(b.has_node("repo1:Function:src/lib.rs:f:0").unwrap());
}
