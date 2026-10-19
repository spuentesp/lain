//! Canonical Topic hub nodes: one node per `(broker, topic)` per repo.
//!
//! `event_sensor` used to mint one `Topic` node per *site* — the id
//! embedded the file path and the site line — so a topic published in
//! three files fragmented into three nodes and the joiner saw three
//! half-topics instead of one. The hub model (owner directive: "el
//! tópico debería ser un nodo. con publishers y subs"): ONE hub node
//! per `(broker, topic)` per repo, with one `Produces`/`Consumes` edge
//! per site hanging off it. The edge's `site: SourceSite { path, line }`
//! is the per-site provenance; the hub's identity is site-independent
//! (its id carries no path or line), while its record names a real
//! site file so the ingestion orphan sweep keeps it.
//!
//! These tests pin the identity contract the later tasks consume:
//! hub id = `generate_id(Topic, "<topics>", "{broker}/{topic}", None, ns)`,
//! label = `"{broker}/{topic}"`, one edge per site (not deduped), and a
//! rescan retracts the old per-site nodes.

use lain::federation::contracts::model::{
    ContractFact, HttpMethod, ProviderFact, ProviderOrigin, SourceSite,
};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::federation::workspace::{WorkspaceSpec, WorkspacesFile};
use lain::graph::GraphDatabase;
use lain::mcp::federation_tools::{get_workspace_graph, WorkspaceGraph};
use lain::nlp::NlpEmbedder;
use lain::schema::{
    EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, StaticSource,
};
use lain::server::coordination::CoordinationProxy;
use lain::server::presence::PresenceRegistry;
use lain::server::sensors::event_sensor::scan_workspace_event;
use lain::tools::handlers::query::query_graph;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The hub identity anchor: hub ids carry no path and no line, so
/// every site converges on one node per `(broker, topic)` per repo.
/// Pinned as a literal here on purpose — this file is the contract a
/// later task consumes; drifting the anchor must fail these tests.
const TOPIC_ANCHOR: &str = "<topics>";

/// Temp workspace with one fixture file, uniquely named per test.
fn fixture_repo_with(tag: &str, rel: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_event_hub_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join(rel);
    std::fs::create_dir_all(file.parent().expect("fixture file has a parent")).unwrap();
    std::fs::write(&file, content).unwrap();
    dir
}

/// Second fixture file in an existing workspace.
fn write_file(root: &Path, rel: &str, content: &str) {
    let file = root.join(rel);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file, content).unwrap();
}

fn scan_events(root: &Path) -> GraphDatabase {
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    scan_workspace_event(&graph, root, &RepoNamespace::for_test()).expect("event scan");
    graph
}

fn topic_nodes(graph: &GraphDatabase) -> Vec<GraphNode> {
    graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .collect()
}

/// The canonical hub id the sensor must mint for a label.
fn hub_id(label: &str) -> String {
    GraphNode::generate_id(
        &NodeType::Topic,
        TOPIC_ANCHOR,
        label,
        None,
        &RepoNamespace::for_test(),
    )
}

/// Two sites publishing the same topic — the fragmentation case —
/// must converge on exactly one hub node whose identity is the label
/// alone (no path, no line).
#[test]
fn two_sites_one_topic_mint_exactly_one_hub_node() {
    let root = fixture_repo_with(
        "two_sites",
        "src/producer.ts",
        "producer.send({ topic: 'orders.created' });\n\
         producer.send({ topic: 'orders.created' });\n",
    );
    let graph = scan_events(&root);

    let hubs = topic_nodes(&graph);
    assert_eq!(
        hubs.len(),
        1,
        "two sites must converge on one hub, got {hubs:?}"
    );
    assert_eq!(hubs[0].name, "kafka/orders.created");
    assert_eq!(
        hubs[0].id,
        hub_id("kafka/orders.created"),
        "hub id must be the line-less anchor id, not a per-site id"
    );
    assert_eq!(
        hubs[0].line_start, None,
        "the hub carries no site line; sites live on the edges"
    );
    // The hub must survive the ingestion orphan sweep: `prune_orphans`
    // drops every node whose `path` is not a git-tracked file (and its
    // incident edges with it), so the hub record has to name a real
    // site file — the id anchor is not a path.
    let tracked: std::collections::HashSet<String> =
        std::collections::HashSet::from(["src/producer.ts".to_string()]);
    graph.prune_orphans(&tracked).expect("orphan sweep");
    assert_eq!(
        topic_nodes(&graph).len(),
        1,
        "the hub must survive prune_orphans or it vanishes from every \
         real index pass (and its Produces/Consumes legs with it)"
    );
}

/// Site provenance survives hub convergence for top-level sites (each
/// anchored on its own synthetic node): one `Produces` edge per site,
/// each carrying its own `SourceSite` — not one deduped edge. The
/// harder same-anchor case (two sites inside ONE function) is pinned
/// by `hub_keeps_one_produces_edge_per_site` below.
#[test]
fn hub_keeps_one_produces_edge_per_top_level_site() {
    let root = fixture_repo_with(
        "per_site",
        "src/producer.ts",
        "producer.send({ topic: 'orders.created' });\n\
         producer.send({ topic: 'orders.created' });\n",
    );
    let graph = scan_events(&root);

    let hub = topic_nodes(&graph).remove(0);
    let produces: Vec<_> = graph
        .all_edges()
        .into_iter()
        .filter(|e| e.edge_type == EdgeType::Produces && e.target_id == hub.id)
        .collect();
    assert_eq!(
        produces.len(),
        2,
        "site provenance must survive: one edge per site, got {produces:?}"
    );
    assert!(produces.iter().all(|e| e.site.is_some()));
    let lines: BTreeSet<u32> = produces
        .iter()
        .filter_map(|e| e.site.as_ref().map(|s| s.line))
        .collect();
    assert_eq!(
        lines,
        BTreeSet::from([1, 2]),
        "each site keeps its own provenance (lines 1 and 2)"
    );
}

/// Review Focus #2's real pin: two same-direction sites for ONE topic
/// inside ONE function share the edge anchor (the enclosing symbol),
/// so both legs have the same `(source, target, Produces)` triple and
/// only `SourceSite` tells them apart. Both must survive — the edge
/// layer's dedup key includes `site` precisely so the second site's
/// provenance is not silently dropped.
#[test]
fn hub_keeps_one_produces_edge_per_site() {
    let root = fixture_repo_with(
        "shared_symbol",
        "src/producer.ts",
        "async function publish() {\n\
         \x20 await producer.send({ topic: 'orders.created', messages: [] });\n\
         \x20 await producer.send({ topic: 'orders.created', messages: [] });\n\
         }\n",
    );
    let ns = RepoNamespace::for_test();
    let graph = GraphDatabase::new(&root.join("graph.bin")).expect("open graph");
    // The enclosing symbol both sites hang off — minted explicitly (no
    // tree-sitter indexer in `scan_workspace_event`), so both edges
    // source from the SAME node and the dedup key is actually exercised.
    let mut fn_node = GraphNode::new(
        NodeType::Function,
        "publish".into(),
        "src/producer.ts".into(),
    );
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/producer.ts",
        "publish",
        Some(1),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(4);
    graph
        .upsert_node(fn_node.clone())
        .expect("seed enclosing symbol");

    scan_workspace_event(&graph, &root, &ns).expect("event scan");

    let hub = topic_nodes(&graph).remove(0);
    let produces: Vec<_> = graph
        .all_edges()
        .into_iter()
        .filter(|e| e.edge_type == EdgeType::Produces && e.target_id == hub.id)
        .collect();
    assert!(
        produces.iter().all(|e| e.source_id == fn_node.id),
        "both sites must hang off the shared symbol — else this test \
         does not exercise the shared-anchor dedup: {produces:?}"
    );
    assert_eq!(
        produces.len(),
        2,
        "one edge per SITE, even on a shared anchor — got {produces:?}"
    );
    let sites: BTreeSet<(String, u32)> = produces
        .iter()
        .filter_map(|e| e.site.as_ref().map(|s| (s.path.clone(), s.line)))
        .collect();
    assert_eq!(
        sites,
        BTreeSet::from([
            ("src/producer.ts".to_string(), 2),
            ("src/producer.ts".to_string(), 3)
        ]),
        "each edge must carry its own SourceSite (lines 2 and 3)"
    );
}

/// Migration: a graph carrying the OLD per-site Topic nodes (id
/// embedded path + line) must, on rescan, retract them and leave
/// exactly one hub — with no dangling edges. Also the loader-compat
/// check for the id-scheme change: the scheme is content, not format,
/// so a persisted old-scheme graph must still open and be readable.
#[test]
fn rescan_retracts_old_per_site_topic_nodes() {
    let root = fixture_repo_with(
        "rescan",
        "src/producer.ts",
        "producer.send({ topic: 'orders.created' });\n",
    );
    write_file(
        &root,
        "src/extra.ts",
        "producer.send({ topic: 'orders.created' });\n",
    );
    let ns = RepoNamespace::for_test();
    let db_path = root.join("graph.bin");
    let graph = GraphDatabase::new(&db_path).expect("open graph");

    // Seed what the old per-site id scheme left behind: one Topic per
    // (path, line), each with the Provider fact the old sensor wrote
    // and an incident edge from a plain symbol node (unowned — it must
    // survive the rescan, its stale edges must not).
    let mut fn_node = GraphNode::new(
        NodeType::Function,
        "legacy_publisher".into(),
        "src/legacy.ts".into(),
    );
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/legacy.ts",
        "legacy_publisher",
        Some(1),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(3);
    graph.upsert_node(fn_node.clone()).expect("seed symbol");

    let mut old_ids = Vec::new();
    for (path, line) in [("src/producer.ts", 1u32), ("src/extra.ts", 1u32)] {
        let old_id = GraphNode::generate_id(
            &NodeType::Topic,
            path,
            "kafka/orders.created",
            Some(line),
            &ns,
        );
        let mut node = GraphNode::new(NodeType::Topic, "kafka/orders.created".into(), path.into());
        node.id = old_id.clone();
        node.line_start = Some(line);
        node.contract = vec![ContractFact::Provider(ProviderFact {
            method: HttpMethod::Any,
            template: "orders.created".into(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        })];
        graph.upsert_node(node).expect("seed old per-site node");
        graph
            .insert_edges_batch(&[GraphEdge {
                edge_type: EdgeType::Produces,
                source_id: fn_node.id.clone(),
                target_id: old_id.clone(),
                weight: Some(1.0),
                cross_repo: false,
                provenance: Some(EdgeProvenance::Static {
                    source: StaticSource::Regex,
                }),
                site: Some(SourceSite {
                    path: path.into(),
                    line,
                }),
                detail: None,
            }])
            .expect("seed old edge");
        old_ids.push(old_id);
    }

    let hub = hub_id("kafka/orders.created");
    assert!(
        old_ids.iter().all(|id| *id != hub),
        "seed ids must differ from the hub id or this test is vacuous"
    );

    // Loader-compat: persist the old-scheme graph and reopen it. The
    // id scheme is content, not format — the loader must read it back.
    graph.save_to_disk_sync().expect("persist seeded graph");
    drop(graph);
    let graph = GraphDatabase::new(&db_path).expect("loader reads the old-scheme graph");
    for id in &old_ids {
        assert!(
            graph.get_all_nodes().iter().any(|n| n.id == *id),
            "loader must read the old per-site id {id} back"
        );
    }

    scan_workspace_event(&graph, &root, &ns).expect("rescan");

    let topics = topic_nodes(&graph);
    assert_eq!(
        topics.len(),
        1,
        "exactly one hub must remain, got {topics:?}"
    );
    assert_eq!(topics[0].id, hub, "the survivor must be the canonical hub");
    assert_eq!(
        topics[0].path, "src/extra.ts",
        "the hub path must be the MIN site path among its legs \
         (deterministic), not whichever site the walk reached first"
    );
    for id in &old_ids {
        assert!(
            !graph.get_all_nodes().iter().any(|n| n.id == *id),
            "old per-site node {id} must be retracted on rescan"
        );
    }
    let produces = graph
        .all_edges()
        .into_iter()
        .filter(|e| e.edge_type == EdgeType::Produces && e.target_id == hub)
        .count();
    assert_eq!(
        produces, 2,
        "the hub must keep one leg per site after the migration"
    );

    // No edges dangle off the retraction.
    let ids: BTreeSet<String> = graph.get_all_nodes().into_iter().map(|n| n.id).collect();
    for e in graph.all_edges() {
        assert!(
            ids.contains(&e.source_id),
            "dangling edge source {}",
            e.source_id
        );
        assert!(
            ids.contains(&e.target_id),
            "dangling edge target {}",
            e.target_id
        );
    }
}

// ─── Hub visibility in the dashboard workspace graph (Task 2) ────────
//
// `get_workspace_graph` feeds the dashboard's force-directed map and
// filters to an explicit node/edge allowlist. Before this change the
// canonical `Topic` hubs and their `Produces`/`Consumes` legs were
// invisible there. These tests drive the production visibility path —
// `event_sensor` → per-repo graph → `project_repo` → federated
// backend → `get_workspace_graph` — and pin what the view must (and
// must not) show. The LSP-driven index pipeline is skipped on
// purpose: its sensor invocation is Task 1's contract (pinned above),
// and the view filter is what this section pins.

/// Boot a federation whose member repos are `repos` (`id`, scan root),
/// run the event sensor over each root into that repo's graph, project
/// into the federated backend, and return the dashboard view. Each
/// root is opened as a git repo because `RepoIndex::new` instantiates
/// a `GitSensor` against it.
async fn call_get_workspace_graph(repos: &[(&str, &Path)], filter: Option<&str>) -> WorkspaceGraph {
    let data = tempfile::tempdir().expect("federation data dir");
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(data.path()).expect("petgraph backend"));
    let fed = FederatedIndex::new(backend);
    let mut members = Vec::new();
    for (id, root) in repos {
        git2::Repository::init(root).expect("fixture root opens as a git repo");
        let rid = RepoId::new(id).expect("valid repo id");
        let source = Box::new(
            WorkspaceDirSource::new(rid.clone(), root.to_path_buf()).expect("workspace source"),
        );
        fed.add_repo(source, data.path()).await.expect("add_repo");
        let idx = fed.get_repo(&rid).expect("registered repo");
        scan_workspace_event(idx.db(), root, &RepoNamespace::for_test()).expect("event scan");
        fed.project_repo(&rid).await.expect("project_repo");
        members.push((*id).to_string());
    }
    let workspaces = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "hub-view".into(),
            description: None,
            source: None,
            members,
        }],
    };
    let out = get_workspace_graph(&fed, &workspaces, filter).expect("workspace graph");
    // Harness precondition: the hubs must have reached the federated
    // backend before the view filters them — a red run must mean "the
    // view filter dropped them", not "projection never wrote them".
    let projected = fed
        .backend()
        .list_nodes()
        .expect("list_nodes")
        .iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .count();
    assert!(projected > 0, "harness precondition: hubs not projected");
    out
}

/// The dashboard map must show the hub with BOTH legs — the
/// publisher's `Produces` and the subscriber's `Consumes`. Sites sit
/// at file top level, so both legs hang off the sensor's `topic-read:`
/// synthetic anchors (the "no enclosing symbol" source path).
#[tokio::test]
async fn workspace_graph_shows_topic_hub_with_its_legs() {
    let root = fixture_repo_with(
        "ws_graph_legs",
        "src/events.ts",
        "producer.send({ topic: 'orders.created' });\n\
         consumer.run({ topics: ['orders.created'], eachMessage: handler });\n",
    );
    // A payload schema linked to the same topic: `PayloadSchema` must
    // ride along whenever a schema is linked (interface contract).
    write_file(
        &root,
        "schemas/order.avsc",
        "{\"type\": \"record\", \"name\": \"Order\", \"topic\": \"orders.created\", \
         \"fields\": [{\"name\": \"customer_id\", \"type\": \"string\"}]}",
    );
    let out = call_get_workspace_graph(&[("a", root.as_path())], None).await;

    let topics: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .collect();
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0].name, "kafka/orders.created");
    let legs: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Produces || e.edge_type == EdgeType::Consumes)
        .collect();
    assert_eq!(legs.len(), 2, "hub must show publisher and subscriber legs");
    assert!(
        legs.iter().any(|e| e.edge_type == EdgeType::Produces)
            && legs.iter().any(|e| e.edge_type == EdgeType::Consumes),
        "one leg per direction, got {legs:?}"
    );
    // `PayloadSchema` shows when a schema is linked — with its `Schema`
    // endpoint, since the view keeps an edge only when both of its
    // endpoints are admitted.
    let schema_id = out
        .nodes
        .iter()
        .find(|n| n.node_type == NodeType::Schema)
        .map(|n| n.id.clone())
        .expect("linked Schema endpoint must show");
    assert!(
        out.edges
            .iter()
            .any(|e| e.edge_type == EdgeType::PayloadSchema && e.target_id == schema_id),
        "hub -> Schema PayloadSchema link missing: {:?}",
        out.edges
    );
}

/// Review Focus #3 (cross-repo): the same `broker/topic` in two repos
/// is TWO hubs (per-repo identity), linked at the contract layer —
/// the view must NOT merge them.
#[tokio::test]
async fn cross_repo_same_topic_stays_two_hubs() {
    let pub_root = fixture_repo_with(
        "cross_pub",
        "src/publisher.ts",
        "producer.send({ topic: 'orders' });\n",
    );
    let sub_root = fixture_repo_with(
        "cross_sub",
        "src/subscriber.ts",
        "consumer.run({ topics: ['orders'], eachMessage: handler });\n",
    );
    let out = call_get_workspace_graph(
        &[("a", pub_root.as_path()), ("b", sub_root.as_path())],
        None,
    )
    .await;

    let topics: Vec<_> = out
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Topic)
        .collect();
    assert_eq!(
        topics.len(),
        2,
        "per-repo identity: one hub per repo, never merged in the view: {topics:?}"
    );
    assert!(
        topics.iter().all(|n| n.name == "kafka/orders"),
        "both hubs name the same broker/topic: {topics:?}"
    );
    let hub_repos: BTreeSet<String> = topics.iter().filter_map(|n| n.repo_id.clone()).collect();
    assert_eq!(
        hub_repos,
        BTreeSet::from(["a".to_string(), "b".to_string()]),
        "one hub per member repo"
    );
    let legs: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Produces || e.edge_type == EdgeType::Consumes)
        .collect();
    assert_eq!(
        legs.len(),
        2,
        "one leg per hub — a merge would drop or double one: {legs:?}"
    );
    assert!(
        out.edges
            .iter()
            .any(|e| e.edge_type == EdgeType::Consumes && !e.cross_repo),
        "each hub keeps its own legs inside its repo"
    );
    assert!(
        legs.iter().all(|e| !e.cross_repo),
        "hub legs are intra-repo by construction; cross-repo linking \
         belongs to the contract layer"
    );
}

/// `query_graph`'s `find` type selector must flow `Topic` hubs through
/// (`type: "Topic"`). The selector path is type-generic and needed no
/// change for the hub model — this assert keeps that claim honest.
#[test]
fn query_graph_type_selector_finds_topic_hubs() {
    let root = fixture_repo_with(
        "query_topic",
        "src/producer.ts",
        "producer.send({ topic: 'orders.created' });\n",
    );
    let graph = scan_events(&root);
    let embedder = NlpEmbedder::new_stub();
    let cache = Arc::new(parking_lot::Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(16).unwrap(),
    )));
    let presence = PresenceRegistry::new();
    let occupancy = CoordinationProxy::new(std::env::temp_dir());
    let mut args = Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!({"ops": [{"op": "find", "type": "Topic"}]}),
    );
    let text = query_graph(
        Path::new(""),
        &graph,
        &embedder,
        &cache,
        &presence,
        &occupancy,
        Some(&args),
        100,
    )
    .expect("query_graph");
    let v: Value = serde_json::from_str(&text).expect("query_graph returns JSON");
    let hits: Vec<_> = v["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter(|n| n["type"] == "Topic")
        .collect();
    assert_eq!(hits.len(), 1, "find type:Topic must return the hub: {v}");
}

// ─── Federated reconcile honors site identity ────────────────────────
//
// `project_edges` used to diff the federated edge set against the
// incoming projection by `(edge_type, source_id, target_id)` only,
// while `Produces`/`Consumes` identity includes `site`. A leg that
// MOVED lines (any edit above a publish/subscribe site) kept its old
// site-variant alive — the triple matched — and the new one stacked
// beside it: a phantom duplicate leg in the dashboard this branch
// ships, healed only by a full reindex.

/// Two projections with a leg that MOVED lines: the federated edge
/// count for that pair must stay 1, asserted on `fed.backend()` (the
/// dashboard's data source) and on `get_workspace_graph` itself. The
/// enclosing symbol is line-stable (its id carries line 1, and the
/// edit lands inside its body), so the pair identity is unchanged and
/// only the leg's `SourceSite` moves — the exact shape the loose
/// reconcile key used to miss.
#[tokio::test]
async fn moved_leg_keeps_exactly_one_federated_edge_for_its_pair() {
    let root = fixture_repo_with(
        "moved_leg",
        "src/producer.ts",
        "async function publish() {\n\
         \x20 await producer.send({ topic: 'orders.created', messages: [] });\n\
         }\n",
    );
    let ns = RepoNamespace::for_test();

    let data = tempfile::tempdir().expect("federation data dir");
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(data.path()).expect("petgraph backend"));
    let fed = FederatedIndex::new(backend.clone());
    git2::Repository::init(&root).expect("fixture root opens as a git repo");
    let rid = RepoId::new("a").expect("valid repo id");
    let source =
        Box::new(WorkspaceDirSource::new(rid.clone(), root.clone()).expect("workspace source"));
    fed.add_repo(source, data.path()).await.expect("add_repo");
    let idx = fed.get_repo(&rid).expect("registered repo");

    // The enclosing symbol both projections hang the leg on — minted
    // explicitly (no tree-sitter indexer in `scan_workspace_event`),
    // with a line-stable id so moving the site inside its body does
    // not move the pair.
    let mut fn_node = GraphNode::new(
        NodeType::Function,
        "publish".into(),
        "src/producer.ts".into(),
    );
    fn_node.id = GraphNode::generate_id(
        &NodeType::Function,
        "src/producer.ts",
        "publish",
        Some(1),
        &ns,
    );
    fn_node.line_start = Some(1);
    fn_node.line_end = Some(10);
    idx.db()
        .upsert_node(fn_node.clone())
        .expect("seed enclosing symbol");

    // Projection 1: the publish site sits at line 2.
    scan_workspace_event(idx.db(), &root, &ns).expect("event scan");
    fed.project_repo(&rid).await.expect("first projection");

    let produces = |edges: &[GraphEdge]| -> Vec<GraphEdge> {
        edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Produces)
            .cloned()
            .collect()
    };
    let before = produces(&backend.all_edges().expect("edges"));
    assert_eq!(before.len(), 1, "one site, one leg: {before:?}");
    let pair = (before[0].source_id.clone(), before[0].target_id.clone());
    assert_eq!(
        before[0].site.as_ref().map(|s| (s.path.as_str(), s.line)),
        Some(("src/producer.ts", 2)),
        "harness precondition: the leg carries its SourceSite"
    );

    // Move the leg: an edit above the site (inside the function) shifts
    // it to line 3. The pair is unchanged; only `site` moves.
    write_file(
        &root,
        "src/producer.ts",
        "async function publish() {\n\
         \x20 const before = 1; // edit above the site\n\
         \x20 await producer.send({ topic: 'orders.created', messages: [] });\n\
         }\n",
    );
    scan_workspace_event(idx.db(), &root, &ns).expect("rescan");
    // Harness precondition: the per-repo side holds exactly one leg,
    // at the new site — a red run must mean the FEDERATION duplicated,
    // not that the sensor did.
    let local_legs: Vec<GraphEdge> = idx
        .db()
        .all_edges()
        .into_iter()
        .filter(|e| e.edge_type == EdgeType::Produces)
        .collect();
    assert_eq!(
        local_legs
            .iter()
            .map(|e| e.site.as_ref().map(|s| s.line))
            .collect::<Vec<_>>(),
        vec![Some(3)],
        "rescan replaces the sensor's own output: one leg at line 3, got {local_legs:?}"
    );
    fed.project_repo(&rid).await.expect("second projection");

    let after = produces(&backend.all_edges().expect("edges"));
    let for_pair: Vec<GraphEdge> = after
        .iter()
        .filter(|e| (e.source_id.clone(), e.target_id.clone()) == pair)
        .cloned()
        .collect();
    assert_eq!(
        for_pair.len(),
        1,
        "the moved leg must retract its old site-variant, not stack beside \
         it — the reconcile key includes `site` for Produces/Consumes: {for_pair:?}"
    );
    assert_eq!(
        for_pair[0].site.as_ref().map(|s| (s.path.as_str(), s.line)),
        Some(("src/producer.ts", 3)),
        "the surviving leg is the moved one"
    );

    // The dashboard path renders from the same backend — the phantom
    // must not surface there either.
    let workspaces = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "hub-view".into(),
            description: None,
            source: None,
            members: vec!["a".to_string()],
        }],
    };
    let out = get_workspace_graph(&fed, &workspaces, None).expect("workspace graph");
    let legs: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Produces)
        .collect();
    assert_eq!(legs.len(), 1, "dashboard must render one leg, got {legs:?}");
}
