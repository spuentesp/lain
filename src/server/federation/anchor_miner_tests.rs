//! Tests for the cross-repo anchor miner (`anchor_miner.rs`).
//!
//! These pin the EDGE-MINTING behavior the acceptance program
//! diagnosed: careful agents reconstruct cross-repo blast radii from
//! structural name anchors, git co-change, and commit messages, while
//! the graph went silent on exactly those couplings. The miner mints
//! heuristic-grade edges from the same signals — never `verified`.

use crate::federation::anchor_miner::{
    collect_commit_records, is_minted_edge, mint_cross_repo_observations, scan_repo_occurrences,
    AnchorKind, AnchorOccurrence, CommitRecord,
};
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

fn repo(id: &str) -> RepoId {
    RepoId::new(id).unwrap()
}

/// A backend node with GlobalId identity and an explicit line span,
/// shaped like the projected federation graph.
fn spanned_node(r: &RepoId, kind: NodeType, path: &str, name: &str, span: (u32, u32)) -> GraphNode {
    let id = GlobalId::new(r, kind.clone(), path, name, Some(span.0));
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(kind, name.to_string(), path.to_string(), &ns);
    n.id = id.as_str().to_string();
    n.line_start = Some(span.0);
    n.line_end = Some(span.1);
    n
}

fn file_node(r: &RepoId, path: &str) -> GraphNode {
    let name = Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let id = GlobalId::new(r, NodeType::File, path, &name, None);
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(NodeType::File, name, path.to_string(), &ns);
    n.id = id.as_str().to_string();
    n
}

fn http_route_node(r: &RepoId, path: &str, name: &str) -> GraphNode {
    let id = GlobalId::new(r, NodeType::HttpRoute, path, name, None);
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(NodeType::HttpRoute, name.to_string(), path.to_string(), &ns);
    n.id = id.as_str().to_string();
    n
}

fn file_id(r: &RepoId, path: &str) -> String {
    GlobalId::new(
        r,
        NodeType::File,
        path,
        Path::new(path).file_name().unwrap().to_str().unwrap(),
        None,
    )
    .as_str()
    .to_string()
}

fn occurrence(
    repo_id: &str,
    path: &str,
    line: u32,
    kind: AnchorKind,
    key: &str,
) -> AnchorOccurrence {
    AnchorOccurrence {
        repo: repo_id.to_string(),
        path: path.to_string(),
        line,
        kind,
        key: key.to_string(),
        constructor_fallback: None,
    }
}

fn heuristic_edges<'a>(edges: &'a [GraphEdge], detector: &str) -> Vec<&'a GraphEdge> {
    edges
        .iter()
        .filter(|e| {
            matches!(
                &e.provenance,
                Some(EdgeProvenance::Heuristic { detector: d, .. }) if d == detector
            )
        })
        .collect()
}

// ─── Detector 1: structural name anchors ─────────────────────────────

/// TDD #1: two repos sharing the queue literal `order-events` mint a
/// heuristic BusTopic edge between the containing symbols (file node
/// fallback on the config-only side), detector `anchor:queue:order-events`,
/// grade heuristic, confidence on the 0.6 route-like rung.
#[test]
fn queue_anchor_shared_across_two_repos_mints_heuristic_edges() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t1"));
    let ledger = repo("ledger-svc");
    let orders = repo("orders-svc");
    let bind_queue = spanned_node(
        &ledger,
        NodeType::Function,
        "src/ledger.py",
        "bind_queue",
        (8, 10),
    );
    backend.upsert_node(bind_queue.clone()).unwrap();
    // orders-svc only declares the queue in amqp.toml — no symbol
    // node covers it; the miner must synthesize the file endpoint.

    let occ = [
        // ledger-svc: the `QUEUE = "order-events"` literal is
        // *inside* bind_queue's span (line 9 of 8..=10).
        occurrence(
            "ledger-svc",
            "src/ledger.py",
            9,
            AnchorKind::Queue,
            "order-events",
        ),
        // orders-svc: amqp.toml `queue = "order-events"`.
        occurrence(
            "orders-svc",
            "amqp.toml",
            2,
            AnchorKind::Queue,
            "order-events",
        ),
    ];
    let report = mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");
    assert_eq!(report.anchor_edges, 2, "both directions: {report:?}");

    let orders_file = file_id(&orders, "amqp.toml");
    let edges = backend.all_edges().unwrap();
    let minted = heuristic_edges(&edges, "anchor:queue:order-events");
    assert_eq!(minted.len(), 2, "expected the anchor pair, got {edges:?}");
    for e in &minted {
        assert_eq!(
            e.edge_type,
            EdgeType::BusTopic,
            "queue anchors ride BusTopic"
        );
        assert!(e.cross_repo, "anchor edges are cross-repo");
        let Some(EdgeProvenance::Heuristic { confidence, .. }) = &e.provenance else {
            panic!(
                "anchor edge must be Heuristic-grade, got {:?}",
                e.provenance
            );
        };
        assert!((*confidence - 0.6).abs() < 1e-6, "route-like rung 0.6");
        let endpoints = [e.source_id.as_str(), e.target_id.as_str()];
        assert!(
            endpoints.contains(&bind_queue.id.as_str())
                && endpoints.contains(&orders_file.as_str()),
            "edge must join bind_queue and the amqp.toml file node, got {e:?}"
        );
    }
    // The synthesized file endpoint must be materialized.
    assert!(backend.get_node(&orders_file).unwrap().is_some());
}

/// TDD #2 (the CRITICAL exclusion): a shared field/property literal
/// like `customer_id` — the allowlist of anchor KINDS never admits a
/// bare identifier, so nothing is minted.
#[test]
fn field_name_literals_never_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let py = dir.path().join("svc.py");
    std::fs::write(
        &py,
        "def handle(payload):\n    return payload[\"customer_id\"]\n",
    )
    .unwrap();
    let r = repo("a-svc");
    let occ = scan_repo_occurrences(&r, dir.path());
    assert!(
        occ.is_empty(),
        "customer_id is a field name, not a structural anchor, got {occ:?}"
    );
}

/// Task 1 wires `*.avsc` / `*.schema.json` payload files into the
/// event sensor — files whose quoted `customer_id` the miner's literal
/// scan can see (`.schema.json` is a `.json`, an allowed scan
/// extension). Field names are not structural anchors: the allowlist
/// of anchor kinds admits no bare identifier, so the shared
/// `customer_id` must mint nothing — inside or outside schema files.
#[test]
fn avro_schema_field_names_do_not_become_cross_repo_anchors() {
    let orders_dir = tempfile::tempdir().unwrap();
    let billing_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(orders_dir.path().join("schemas")).unwrap();
    std::fs::create_dir_all(billing_dir.path().join("src")).unwrap();
    std::fs::write(
        orders_dir.path().join("schemas/order.avsc"),
        r#"{"name": "Order", "fields": [{"name": "customer_id"}]}"#,
    )
    .unwrap();
    // The `.schema.json` half matters most: extension `json` is in
    // `SCAN_EXTS`, so unlike `.avsc` this file is actually read by the
    // literal scan.
    std::fs::write(
        orders_dir.path().join("schemas/order.schema.json"),
        r#"{"title": "Order", "properties": {"customer_id": {"type": "string"}}}"#,
    )
    .unwrap();
    std::fs::write(
        billing_dir.path().join("src/main.py"),
        "row[\"customer_id\"]\n",
    )
    .unwrap();

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task1-avro"));
    let orders = repo("orders-svc");
    let billing = repo("billing-svc");
    backend
        .upsert_node(file_node(&orders, "schemas/order.avsc"))
        .unwrap();
    backend
        .upsert_node(file_node(&billing, "src/main.py"))
        .unwrap();

    let mut occ = scan_repo_occurrences(&orders, orders_dir.path());
    occ.extend(scan_repo_occurrences(&billing, billing_dir.path()));
    assert!(
        !occ.iter().any(|o| o.key == "customer_id"),
        "a schema field name must not classify as an anchor occurrence: {occ:?}"
    );

    let report = mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");
    let edges = backend.all_edges().unwrap();
    let anchor_edges: Vec<_> = edges
        .iter()
        .filter(|e| {
            matches!(
                &e.provenance,
                Some(EdgeProvenance::Heuristic { detector, .. })
                    if detector.starts_with("anchor:")
            )
        })
        .collect();
    assert!(
        anchor_edges.is_empty(),
        "field names are not structural anchors, inside or outside schema files: \
         occ={occ:?} minted={anchor_edges:?} report={report:?}"
    );
}

/// TDD #3: an anchor present in one repo only mints nothing — an
/// anchor is a coupling only when two repos share it.
#[test]
fn anchor_present_in_one_repo_only_mints_nothing() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t3"));
    let orders = repo("orders-svc");
    backend
        .upsert_node(file_node(&orders, "amqp.toml"))
        .unwrap();
    let occ = [occurrence(
        "orders-svc",
        "amqp.toml",
        2,
        AnchorKind::Queue,
        "order-events",
    )];
    let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
    assert_eq!(report.anchor_edges, 0, "{report:?}");
    assert!(backend.all_edges().unwrap().is_empty());
}

/// TDD #4: an anchor shared by many repos is a crowd, not a coupling
/// — no pairwise flood.
#[test]
fn crowd_anchor_in_ten_repos_mints_nothing() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t4"));
    let mut occ = Vec::new();
    for i in 0..10 {
        let r = repo(&format!("noise-{i:02}"));
        backend.upsert_node(file_node(&r, "openapi.yaml")).unwrap();
        occ.push(occurrence(
            r.as_str(),
            "openapi.yaml",
            5,
            AnchorKind::Route,
            "/v1/customers",
        ));
    }
    let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
    assert_eq!(report.anchor_edges, 0, "crowds never flood: {report:?}");
    assert_eq!(report.crowds_skipped, 1, "{report:?}");
    assert!(backend.all_edges().unwrap().is_empty());
}

// ─── Detector 2: cross-repo co-change ────────────────────────────────

/// TDD #5: commits in different repos with the same message and
/// timestamp mint a `cochange` heuristic edge at 0.3 between the
/// changed files' nodes.
#[test]
fn cross_repo_same_message_commits_mint_cochange_at_point_three() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t5"));
    let a = repo("repo-a");
    let b = repo("repo-b");
    backend.upsert_node(file_node(&a, "src/x.py")).unwrap();
    backend.upsert_node(file_node(&b, "src/y.py")).unwrap();

    let commits = vec![
        CommitRecord {
            repo: "repo-a".into(),
            message: "tune the orders pipeline".into(),
            time: 1_736_140_800,
            files: vec!["src/x.py".into()],
        },
        CommitRecord {
            repo: "repo-b".into(),
            message: "tune the orders pipeline".into(),
            time: 1_736_140_810, // same changeset, 10s apart
            files: vec!["src/y.py".into()],
        },
    ];
    let report = mint_cross_repo_observations(&backend, &[], &commits).unwrap();
    assert_eq!(report.cochange_edges, 1, "{report:?}");

    let edges = backend.all_edges().unwrap();
    let minted = heuristic_edges(&edges, "cochange");
    assert_eq!(minted.len(), 1, "got {edges:?}");
    let e = minted[0];
    assert_eq!(e.edge_type, EdgeType::CoChangedWith);
    let Some(EdgeProvenance::Heuristic { confidence, .. }) = &e.provenance else {
        panic!("cochange edge must be Heuristic-grade");
    };
    assert!((*confidence - 0.3).abs() < 1e-6, "co-change rung 0.3");
    let ends = [e.source_id.as_str(), e.target_id.as_str()];
    assert!(ends.contains(&file_id(&a, "src/x.py").as_str()));
    assert!(ends.contains(&file_id(&b, "src/y.py").as_str()));
}

/// A commit pair that only shares a message but sits far apart in
/// time is not a changeset — no edge.
#[test]
fn cochange_requires_timestamp_proximity() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t5b"));
    let a = repo("repo-a");
    let b = repo("repo-b");
    backend.upsert_node(file_node(&a, "src/x.py")).unwrap();
    backend.upsert_node(file_node(&b, "src/y.py")).unwrap();
    let commits = vec![
        CommitRecord {
            repo: "repo-a".into(),
            message: "wip".into(),
            time: 1_736_140_800,
            files: vec!["src/x.py".into()],
        },
        CommitRecord {
            repo: "repo-b".into(),
            message: "wip".into(),
            time: 1_746_140_800, // 100 days later
            files: vec!["src/y.py".into()],
        },
    ];
    let report = mint_cross_repo_observations(&backend, &[], &commits).unwrap();
    assert_eq!(report.cochange_edges, 0, "{report:?}");
}

/// A changeset that sweeps many repos at once is a bulk import (or a
/// fixture-style identical commit), not a coupling — skipped with the
/// same crowd convention as anchors, never a pairwise flood.
#[test]
fn cochange_bulk_changeset_is_a_crowd() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t5c"));
    let mut commits = Vec::new();
    for i in 0..10 {
        let r = repo(&format!("sweep-{i:02}"));
        backend.upsert_node(file_node(&r, "test.py")).unwrap();
        commits.push(CommitRecord {
            repo: r.as_str().to_string(),
            message: "import everything at once".into(),
            time: 1_736_140_800,
            files: vec!["test.py".into()],
        });
    }
    let report = mint_cross_repo_observations(&backend, &[], &commits).unwrap();
    assert_eq!(report.cochange_edges, 0, "{report:?}");
    assert_eq!(report.crowds_skipped, 1, "{report:?}");
    assert!(backend.all_edges().unwrap().is_empty());
}

// ─── Detector 3: commit-message references ───────────────────────────

/// A commit subject that names a structural anchor which also exists
/// in another repo mints a `commit-ref` edge at 0.3 from the changed
/// file to the anchor's endpoint over there.
#[test]
fn commit_subject_mentioning_shared_anchor_mints_commit_ref() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t6"));
    let a = repo("repo-a");
    let b = repo("repo-b");
    backend.upsert_node(file_node(&a, "consumer.py")).unwrap();
    backend.upsert_node(file_node(&b, "amqp.toml")).unwrap();
    // The anchor exists in repo-b's tree (from the scan pass) …
    let occ = [occurrence(
        "repo-b",
        "amqp.toml",
        2,
        AnchorKind::Queue,
        "order-events",
    )];
    // … and repo-a's commit subject mentions it.
    let commits = vec![CommitRecord {
        repo: "repo-a".into(),
        message: "harden the order-events consumer".into(),
        time: 1_736_140_800,
        files: vec!["consumer.py".into()],
    }];
    let report = mint_cross_repo_observations(&backend, &occ, &commits).unwrap();
    assert_eq!(report.commit_ref_edges, 1, "{report:?}");

    let edges = backend.all_edges().unwrap();
    let minted = heuristic_edges(&edges, "commit-ref");
    assert_eq!(minted.len(), 1, "got {edges:?}");
    let e = minted[0];
    assert_eq!(e.source_id, file_id(&a, "consumer.py"));
    assert_eq!(e.target_id, file_id(&b, "amqp.toml"));
    let Some(EdgeProvenance::Heuristic { confidence, .. }) = &e.provenance else {
        panic!("commit-ref edge must be Heuristic-grade");
    };
    assert!((*confidence - 0.3).abs() < 1e-6);
}

// ─── Evidence rules ──────────────────────────────────────────────────

/// TDD #7: a static/confirmed edge between the same node pair wins —
/// the miner never stacks a heuristic duplicate on top of proven glue.
#[test]
fn static_edge_between_pair_suppresses_anchor_mint() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t7"));
    let ledger = repo("ledger-svc");
    let orders = repo("orders-svc");
    let bind_queue = spanned_node(
        &ledger,
        NodeType::Function,
        "src/ledger.py",
        "bind_queue",
        (8, 10),
    );
    let orders_file = file_node(&orders, "amqp.toml");
    backend.upsert_node(bind_queue.clone()).unwrap();
    backend.upsert_node(orders_file.clone()).unwrap();
    let mut static_edge = GraphEdge::new(
        EdgeType::Binds,
        bind_queue.id.clone(),
        orders_file.id.clone(),
    );
    static_edge.provenance = Some(EdgeProvenance::Static {
        source: crate::schema::StaticSource::Lsp,
    });
    backend.upsert_edge(static_edge).unwrap();

    let occ = [
        occurrence(
            "ledger-svc",
            "src/ledger.py",
            9,
            AnchorKind::Queue,
            "order-events",
        ),
        occurrence(
            "orders-svc",
            "amqp.toml",
            2,
            AnchorKind::Queue,
            "order-events",
        ),
    ];
    let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
    assert_eq!(report.anchor_edges, 0, "static wins: {report:?}");
    let edges = backend.all_edges().unwrap();
    assert_eq!(
        edges.len(),
        1,
        "only the static edge remains, got {edges:?}"
    );
    assert!(!is_minted_edge(&edges[0]));
}

/// Task 2: a typed AMQP `Consumes` edge (the `Static{Regex}` shape
/// the event sensor mints for `ch.basic_consume`) joining the same
/// node pair suppresses the anchor miner's heuristic `anchor:queue:*`
/// twin — same static-wins rule as TDD #7, exercised on the AMQP
/// pair from the enterprise fixture's blind spot.
#[test]
fn typed_amqp_consumes_edge_suppresses_queue_anchor_mint() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task2-amqp"));
    let ledger = repo("ledger-svc");
    let orders = repo("orders-svc");
    let consume = spanned_node(
        &ledger,
        NodeType::Function,
        "src/ledger.py",
        "consume_orders",
        (1, 1),
    );
    let declare = spanned_node(
        &orders,
        NodeType::Function,
        "src/legacy_amqp.rs",
        "declare_queue",
        (1, 1),
    );
    backend.upsert_node(consume.clone()).unwrap();
    backend.upsert_node(declare.clone()).unwrap();
    // The typed edge the event sensor mints for the consumer side —
    // Static provenance, same node pair the anchor would join.
    let mut typed = GraphEdge::new(EdgeType::Consumes, consume.id.clone(), declare.id.clone());
    typed.provenance = Some(EdgeProvenance::Static {
        source: crate::schema::StaticSource::Regex,
    });
    backend.upsert_edge(typed).unwrap();

    let occ = [
        occurrence(
            "orders-svc",
            "src/legacy_amqp.rs",
            1,
            AnchorKind::Queue,
            "order-events",
        ),
        occurrence(
            "ledger-svc",
            "src/ledger.py",
            1,
            AnchorKind::Queue,
            "order-events",
        ),
    ];
    let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
    assert_eq!(
        report.anchor_edges, 0,
        "static evidence wins; the heuristic twin must not duplicate it: {report:?}"
    );
    let edges = backend.all_edges().unwrap();
    assert!(
        !edges.iter().any(|e| matches!(
            &e.provenance,
            Some(EdgeProvenance::Heuristic { detector, .. }) if detector.starts_with("anchor:queue")
        )),
        "static evidence wins; the heuristic twin must not duplicate it: {edges:?}"
    );
}

/// Task 2 fix: the REAL typed shape — each repo's fn carries a
/// Static `Consumes`/`Produces` edge to an `amqp/order-events` Topic
/// node (fn → Topic, within one repo) — suppresses the cross-repo
/// `anchor:queue:order-events` twin. The exact-pair rule cannot see
/// this (the fn→Topic pair key never equals the fn↔fn pair the mint
/// would create); the topic-coverage rule does. Both topic-node
/// layouts are pinned: one Topic node per repo (the federation
/// projection keeps per-repo Topic nodes distinct) and one shared
/// Topic node.
#[test]
fn typed_amqp_topic_edges_suppress_cross_repo_queue_anchor_mint() {
    for shared_topic in [false, true] {
        let label = if shared_topic {
            "shared topic node"
        } else {
            "per-repo topic nodes"
        };
        let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task2-amqp-typed"));
        let ledger = repo("ledger-svc");
        let orders = repo("orders-svc");
        let consume = spanned_node(
            &ledger,
            NodeType::Function,
            "src/ledger.py",
            "consume_orders",
            (1, 1),
        );
        let declare = spanned_node(
            &orders,
            NodeType::Function,
            "src/legacy_amqp.rs",
            "declare_queue",
            (1, 1),
        );
        backend.upsert_node(consume.clone()).unwrap();
        backend.upsert_node(declare.clone()).unwrap();

        // The event sensor's Topic nodes: named `{broker}/{queue}`.
        let ledger_topic = spanned_node(
            &ledger,
            NodeType::Topic,
            "src/ledger.py",
            "amqp/order-events",
            (3, 3),
        );
        let orders_topic = if shared_topic {
            ledger_topic.clone()
        } else {
            spanned_node(
                &orders,
                NodeType::Topic,
                "src/legacy_amqp.rs",
                "amqp/order-events",
                (3, 3),
            )
        };
        backend.upsert_node(ledger_topic.clone()).unwrap();
        if !shared_topic {
            backend.upsert_node(orders_topic.clone()).unwrap();
        }

        // The typed edges the event sensor mints: fn → Topic, Static
        // provenance, within one repo — never fn ↔ fn.
        let mut consume_edge = GraphEdge::new(
            EdgeType::Consumes,
            consume.id.clone(),
            ledger_topic.id.clone(),
        );
        consume_edge.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::Regex,
        });
        backend.upsert_edge(consume_edge).unwrap();
        let mut produce_edge = GraphEdge::new(
            EdgeType::Produces,
            declare.id.clone(),
            orders_topic.id.clone(),
        );
        produce_edge.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::Regex,
        });
        backend.upsert_edge(produce_edge).unwrap();

        let occ = [
            occurrence(
                "orders-svc",
                "src/legacy_amqp.rs",
                1,
                AnchorKind::Queue,
                "order-events",
            ),
            occurrence(
                "ledger-svc",
                "src/ledger.py",
                1,
                AnchorKind::Queue,
                "order-events",
            ),
        ];
        let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
        assert_eq!(
            report.anchor_edges, 0,
            "{label}: the typed fn -> Topic edges already cover the coupling; \
             the heuristic twin must not duplicate them: {report:?}"
        );
        let edges = backend.all_edges().unwrap();
        assert!(
            !edges.iter().any(|e| matches!(
                &e.provenance,
                Some(EdgeProvenance::Heuristic { detector, .. }) if detector.starts_with("anchor:queue")
            )),
            "{label}: heuristic twin must not duplicate the typed edges: {edges:?}"
        );
    }
}

/// TDD #6 (claims integration): a chain that crosses only a minted
/// anchor edge classifies as `needs-investigation` with the detector
/// named — never `verified`.
#[test]
fn anchor_chain_classifies_as_needs_investigation() {
    use crate::federation::graph_backend::GraphBackend as _;
    use crate::server::claims::{claims_from_impact_paths, render_claim_lines};

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t8"));
    let ledger = repo("ledger-svc");
    let orders = repo("orders-svc");
    let bind_queue = spanned_node(
        &ledger,
        NodeType::Function,
        "src/ledger.py",
        "bind_queue",
        (8, 10),
    );
    backend.upsert_node(bind_queue.clone()).unwrap();
    let occ = [
        occurrence(
            "ledger-svc",
            "src/ledger.py",
            9,
            AnchorKind::Queue,
            "order-events",
        ),
        occurrence(
            "orders-svc",
            "amqp.toml",
            2,
            AnchorKind::Queue,
            "order-events",
        ),
    ];
    mint_cross_repo_observations(&backend, &occ, &[]).unwrap();

    let orders_file = file_id(&orders, "amqp.toml");
    let result = backend
        .traverse_impact(&[orders_file.as_str()], 6, 50, 0.0)
        .unwrap();
    assert!(
        !result.paths.is_empty(),
        "BusTopic anchor edges must propagate through traverse_impact"
    );
    let starts = vec![orders_file.clone()];
    let rendered = render_claim_lines(claims_from_impact_paths(&result.paths, &starts), None);
    assert!(
        rendered.contains(
            "AFFECTED: ledger-svc:src/ledger.py:bind_queue  EVIDENCE: needs-investigation"
        ),
        "anchor chain must be needs-investigation, got:\n{rendered}"
    );
    assert!(
        rendered.contains("detector=anchor:queue:order-events"),
        "the detector must be named, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("EVIDENCE: verified"),
        "heuristic anchors must never verify, got:\n{rendered}"
    );
}

/// Minted edges are recognized by provenance so the projection
/// reconciliation never retracts them as stale.
#[test]
fn minted_edges_are_recognized_by_detector() {
    let a = "repo-a:Function:x.py:f:1";
    let b = "repo-b:File:amqp.toml:amqp.toml:0";
    let mut anchor = GraphEdge::new(EdgeType::BusTopic, a.into(), b.into());
    anchor.provenance = Some(EdgeProvenance::Heuristic {
        detector: "anchor:queue:order-events".into(),
        confidence: 0.6,
    });
    assert!(is_minted_edge(&anchor));
    let mut cochange = GraphEdge::new(EdgeType::CoChangedWith, a.into(), b.into());
    cochange.provenance = Some(EdgeProvenance::Heuristic {
        detector: "cochange".into(),
        confidence: 0.3,
    });
    assert!(is_minted_edge(&cochange));
    // Sensor-emitted heuristic edges (e.g. dynamic dispatch) are NOT
    // miner-minted: reconciliation keeps treating them normally.
    let mut dispatch = GraphEdge::new(EdgeType::DynamicDispatch, a.into(), b.into());
    dispatch.provenance = Some(EdgeProvenance::Heuristic {
        detector: "message_bus_publisher".into(),
        confidence: 0.7,
    });
    assert!(!is_minted_edge(&dispatch));
    let statik = GraphEdge::new(EdgeType::Calls, a.into(), b.into());
    assert!(!is_minted_edge(&statik));
}

// ─── Extractor behavior (scan_repo_occurrences) ──────────────────────

/// Provider-side route literals (openapi paths) anchor to the
/// `HttpRoute` node the openapi sensor minted — the very node
/// `trace_impact` seeds an endpoint trace from — not to the yaml
/// file. The consumer side anchors to the containing function.
#[test]
fn route_anchor_binds_to_provider_http_route_node() {
    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-t1b"));
    let orders = repo("orders-svc");
    let fraud = repo("fraud-check");
    let route = http_route_node(&orders, "openapi.yaml", "GET /v1/orders/{}");
    backend.upsert_node(route.clone()).unwrap();
    let check = spanned_node(
        &fraud,
        NodeType::Function,
        "src/main/java/FraudCheck.java",
        "check",
        (12, 20),
    );
    backend.upsert_node(check.clone()).unwrap();

    let occ = [
        occurrence(
            "orders-svc",
            "openapi.yaml",
            12,
            AnchorKind::Route,
            "/v1/orders",
        ),
        occurrence(
            "fraud-check",
            "src/main/java/FraudCheck.java",
            14,
            AnchorKind::Route,
            "/v1/orders",
        ),
    ];
    let report = mint_cross_repo_observations(&backend, &occ, &[]).unwrap();
    assert_eq!(report.anchor_edges, 2, "{report:?}");
    let edges = backend.all_edges().unwrap();
    let minted = heuristic_edges(&edges, "anchor:route:/v1/orders");
    assert_eq!(minted.len(), 2, "got {edges:?}");
    for e in &minted {
        assert_eq!(e.edge_type, EdgeType::RouteMatches);
        let ends = [e.source_id.as_str(), e.target_id.as_str()];
        assert!(
            ends.contains(&route.id.as_str()) && ends.contains(&check.id.as_str()),
            "edge must join the HttpRoute node and check(), got {e:?}"
        );
    }
}

fn scan_text(name: &str, text: &str) -> Vec<(AnchorKind, String)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(name), text).unwrap();
    let mut out: Vec<(AnchorKind, String)> = scan_repo_occurrences(&repo("scan-svc"), dir.path())
        .into_iter()
        .map(|o| (o.kind, o.key))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Route literals normalize like the route matcher: id segments and
/// `{}` holes collapse away, so `/v1/orders/{id}` and `/v1/orders/`
/// are the same anchor.
#[test]
fn route_literals_normalize_to_one_stem() {
    let a = scan_text("x.py", "u = base + \"/v1/orders/\" + oid\n");
    let b = scan_text("y.py", "u = base + f\"/v1/orders/{oid}\"\n");
    let c = scan_text("openapi.yaml", "paths:\n  /v1/orders/{id}:\n    get: {}\n");
    assert_eq!(a, vec![(AnchorKind::Route, "/v1/orders".into())], "{a:?}");
    assert_eq!(b, a, "f-string ids collapse the same way");
    assert_eq!(c, a, "bare yaml path keys are route anchors too");
}

/// The env idiom table (`host_env_name`) feeds env anchors; the shape
/// gate admits only UPPER_SNAKE names.
#[test]
fn env_idiom_shapes_extract_env_anchors() {
    let cases = [
        ("a.py", "v = os.environ[\"FIN_ORDERS_BASE\"]\n"),
        ("b.py", "v = os.getenv(\"KAFKA_BOOT_URL\", \"x\")\n"),
        (
            "c.java",
            "String u = System.getenv(\"RISK_ORDERS_URL\") + \"/v1/orders/\";\n",
        ),
        ("d.go", "v := os.Getenv(\"ORDERS_GRPC_ADDR\")\n"),
        ("e.ts", "const v = process.env.NOTIFY_QUEUE_URL;\n"),
    ];
    for (name, text) in cases {
        let got = scan_text(name, text);
        assert!(
            got.iter().any(|(k, _)| *k == AnchorKind::Env),
            "{name}: env idiom not detected, got {got:?} for {text:?}"
        );
    }
    let envs: Vec<_> = scan_text(
        "c.java",
        "String u = System.getenv(\"RISK_ORDERS_URL\") + \"/v1/orders/\";\n",
    )
    .into_iter()
    .filter(|(k, _)| *k == AnchorKind::Env)
    .collect();
    assert_eq!(envs, vec![(AnchorKind::Env, "RISK_ORDERS_URL".into())]);
    // Lowercase / short names never anchor.
    let rejected = scan_text("f.py", "v = os.environ.get(\"path\")\n");
    assert!(
        !rejected.iter().any(|(k, _)| *k == AnchorKind::Env),
        "{rejected:?}"
    );
}

/// Queue and topic literals only anchor inside their context — never
/// as bare words.
#[test]
fn queue_and_topic_context_shapes() {
    let queue = scan_text(
        "ledger.py",
        "channel.queue_bind(queue=\"order-events\", exchange=\"orders\")\n",
    );
    assert!(
        queue.contains(&(AnchorKind::Queue, "order-events".into())),
        "{queue:?}"
    );
    let topic = scan_text(
        "worker.py",
        "consumer.subscribe([\"order.events\"])  # kafka topic\n",
    );
    assert!(
        topic.contains(&(AnchorKind::Topic, "order.events".into())),
        "{topic:?}"
    );
    // The same words with no structural context are not anchors.
    let bare = scan_text("notes.py", "msg = \"order-events\"\n");
    assert!(
        !bare.iter().any(|(k, _)| *k == AnchorKind::Queue),
        "{bare:?}"
    );
}

/// Proto service/method identifiers (`package.Service/Method`) are
/// anchors; field names are not.
#[test]
fn proto_identifiers_extract() {
    let got = scan_text(
        "orders.proto",
        "package orders;\n\nservice Orders {\n  rpc GetOrder(Req) returns (Res);\n}\n",
    );
    assert!(
        got.contains(&(AnchorKind::Proto, "orders.Orders".into())),
        "{got:?}"
    );
    assert!(
        got.contains(&(AnchorKind::Proto, "orders.Orders/GetOrder".into())),
        "{got:?}"
    );
    let code = scan_text("client.py", "stub = \"orders.GetOrder\"\n");
    assert!(
        code.contains(&(AnchorKind::Proto, "orders.GetOrder".into())),
        "{code:?}"
    );
}

/// Task 3: a gRPC consumer that references only GENERATED code —
/// `pb.NewOrdersClient` / `OrdersClient`, no proto literals — must
/// still anchor to the proto service it belongs to. The miner keys
/// the generated type names by their bare service segment (`Orders`)
/// and joins it ASCII-case-insensitively to the declared
/// `orders.Orders` key; the edge keeps the declared key as its
/// detector label, heuristic 0.6 on `Calls` like every other anchor.
#[test]
fn generated_stub_names_anchor_to_proto_service() {
    // orders-svc declares service Orders in protos/orders.proto;
    // logistics-svc references only generated code — no proto literals.
    let orders_dir = tempfile::tempdir().unwrap();
    let logistics_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(orders_dir.path().join("protos")).unwrap();
    std::fs::create_dir_all(logistics_dir.path().join("src")).unwrap();
    std::fs::write(
        orders_dir.path().join("protos/orders.proto"),
        "package orders;\nservice Orders {\n  rpc GetOrder(GetOrderRequest) returns (Order);\n}\n",
    )
    .unwrap();
    std::fs::write(
        logistics_dir.path().join("src/handler.go"),
        "c := pb.NewOrdersClient(conn)\nresp, err := c.GetOrder(ctx, req)\nvar s pb.OrdersClient\n",
    )
    .unwrap();

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task3-generated"));
    let orders = repo("orders-svc");
    let logistics = repo("logistics-svc");
    backend
        .upsert_node(file_node(&orders, "protos/orders.proto"))
        .unwrap();
    backend
        .upsert_node(file_node(&logistics, "src/handler.go"))
        .unwrap();

    let mut occ = scan_repo_occurrences(&orders, orders_dir.path());
    occ.extend(scan_repo_occurrences(&logistics, logistics_dir.path()));
    mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");

    let edges = backend.all_edges().unwrap();
    assert!(
        edges.iter().any(|e| {
            matches!(
                &e.provenance,
                Some(EdgeProvenance::Heuristic { detector, .. })
                    if detector.starts_with("anchor:proto")
            ) && e.source_id.contains("orders-svc")
                && e.target_id.contains("logistics-svc")
        }),
        "OrdersClient/NewOrdersClient must anchor to the Orders service: {edges:?}"
    );
    // Edge shape unchanged: the detector keeps the DECLARED proto key,
    // both directions, heuristic 0.6 on Calls — never verified.
    let minted = heuristic_edges(&edges, "anchor:proto:orders.Orders");
    assert_eq!(minted.len(), 2, "both directions, got {edges:?}");
    for e in &minted {
        assert_eq!(e.edge_type, EdgeType::Calls, "proto anchors ride Calls");
        assert!(e.cross_repo, "anchor edges are cross-repo");
        let Some(EdgeProvenance::Heuristic { confidence, .. }) = &e.provenance else {
            panic!(
                "proto anchor must be Heuristic-grade, got {:?}",
                e.provenance
            );
        };
        assert!((*confidence - 0.6).abs() < 1e-6, "route-like rung 0.6");
    }
}

/// Task 3 (Review Focus 3): a generated stub name anchors only to a
/// REAL declared proto service — support-portal's own `TicketsClient`
/// must NOT link to `orders.Orders`. With no `service Tickets`
/// anywhere, the generated key forms no minting group and nothing is
/// produced.
#[test]
fn generated_stub_never_crosses_unrelated_proto_services() {
    // support-portal's own TicketsClient must NOT link to orders.Orders.
    let orders_dir = tempfile::tempdir().unwrap();
    let portal_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(orders_dir.path().join("protos")).unwrap();
    std::fs::create_dir_all(portal_dir.path().join("src")).unwrap();
    std::fs::write(
        orders_dir.path().join("protos/orders.proto"),
        "package orders;\nservice Orders {\n  rpc GetOrder(..) returns (Order);\n}\n",
    )
    .unwrap();
    std::fs::write(
        portal_dir.path().join("src/show.ts"),
        "const c = new TicketsClient(channel);\nc.showTicket(id);\n",
    )
    .unwrap();

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task3-collision"));
    let orders = repo("orders-svc");
    let portal = repo("support-portal");
    backend
        .upsert_node(file_node(&orders, "protos/orders.proto"))
        .unwrap();
    backend
        .upsert_node(file_node(&portal, "src/show.ts"))
        .unwrap();

    let mut occ = scan_repo_occurrences(&orders, orders_dir.path());
    occ.extend(scan_repo_occurrences(&portal, portal_dir.path()));
    mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");

    let edges = backend.all_edges().unwrap();
    assert!(
        !edges.iter().any(|e| matches!(
            &e.provenance,
            Some(EdgeProvenance::Heuristic { detector, .. }) if detector.starts_with("anchor:proto")
        )),
        "TicketsClient has no Orders service segment: {edges:?}"
    );
}

/// Fix round 1: a service whose name STARTS with `New` — the `News`
/// service's generated names (`NewsClient`, `news_pb2_grpc.NewsStub`)
/// must anchor to `news.News`. The unconditional `New`-prefix strip
/// used to reduce them to `sClient`/`sStub` and anchor nothing.
#[test]
fn generated_name_for_new_prefixed_service_anchors_true_service() {
    // news-svc declares service News in protos/news.proto;
    // news-portal references only generated code.
    let news_dir = tempfile::tempdir().unwrap();
    let portal_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(news_dir.path().join("protos")).unwrap();
    std::fs::create_dir_all(portal_dir.path().join("src")).unwrap();
    std::fs::write(
        news_dir.path().join("protos/news.proto"),
        "package news;\nservice News {\n  rpc GetFeed(FeedRequest) returns (Feed);\n}\n",
    )
    .unwrap();
    std::fs::write(
        portal_dir.path().join("src/client.py"),
        "stub = news_pb2_grpc.NewsStub(channel)\n",
    )
    .unwrap();
    std::fs::write(
        portal_dir.path().join("src/app.ts"),
        "const c = new NewsClient(channel);\n",
    )
    .unwrap();

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task3-fix1-news"));
    let news = repo("news-svc");
    let portal = repo("news-portal");
    backend
        .upsert_node(file_node(&news, "protos/news.proto"))
        .unwrap();
    backend
        .upsert_node(file_node(&portal, "src/client.py"))
        .unwrap();
    backend
        .upsert_node(file_node(&portal, "src/app.ts"))
        .unwrap();

    let mut occ = scan_repo_occurrences(&news, news_dir.path());
    occ.extend(scan_repo_occurrences(&portal, portal_dir.path()));
    mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");

    let edges = backend.all_edges().unwrap();
    let minted = heuristic_edges(&edges, "anchor:proto:news.News");
    assert!(
        !minted.is_empty(),
        "NewsClient/NewsStub must anchor to the News service: {edges:?}"
    );
    for e in &minted {
        let ends = [e.source_id.as_str(), e.target_id.as_str()];
        assert!(
            ends.iter().any(|id| id.contains("news-svc"))
                && ends.iter().any(|id| id.contains("news-portal")),
            "anchor must join news-svc and news-portal, got {e:?}"
        );
    }
}

/// Fix round 1: `NewOrdersClient` SPELLS service `NewOrders`. When
/// both `v2.NewOrders` and `orders.Orders` are declared it must anchor
/// to its true service and never pair onto `orders.Orders` — the
/// `New`-strip is a constructor fallback only, used when the primary
/// name matches no declared service.
#[test]
fn new_orders_client_prefers_new_orders_service_over_orders() {
    let v2_dir = tempfile::tempdir().unwrap();
    let orders_dir = tempfile::tempdir().unwrap();
    let logistics_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(v2_dir.path().join("protos")).unwrap();
    std::fs::create_dir_all(orders_dir.path().join("protos")).unwrap();
    std::fs::create_dir_all(logistics_dir.path().join("src")).unwrap();
    std::fs::write(
        v2_dir.path().join("protos/new_orders.proto"),
        "package v2;\nservice NewOrders {\n  rpc Get(GetRequest) returns (Resp);\n}\n",
    )
    .unwrap();
    std::fs::write(
        orders_dir.path().join("protos/orders.proto"),
        "package orders;\nservice Orders {\n  rpc GetOrder(GetOrderRequest) returns (Order);\n}\n",
    )
    .unwrap();
    std::fs::write(
        logistics_dir.path().join("src/handler.go"),
        "c := pb.NewOrdersClient(conn)\nresp, err := c.GetOrder(ctx, req)\n",
    )
    .unwrap();

    let backend = PetgraphBackend::ephemeral(Path::new("/tmp/anchor-miner-task3-fix1-neworders"));
    let v2 = repo("v2-svc");
    let orders = repo("orders-svc");
    let logistics = repo("logistics-svc");
    backend
        .upsert_node(file_node(&v2, "protos/new_orders.proto"))
        .unwrap();
    backend
        .upsert_node(file_node(&orders, "protos/orders.proto"))
        .unwrap();
    backend
        .upsert_node(file_node(&logistics, "src/handler.go"))
        .unwrap();

    let mut occ = scan_repo_occurrences(&v2, v2_dir.path());
    occ.extend(scan_repo_occurrences(&orders, orders_dir.path()));
    occ.extend(scan_repo_occurrences(&logistics, logistics_dir.path()));
    mint_cross_repo_observations(&backend, &occ, &[]).expect("mint succeeds");

    let edges = backend.all_edges().unwrap();
    // The true service: v2.NewOrders (its method-key twin dedupes).
    let preferred = heuristic_edges(&edges, "anchor:proto:v2.NewOrders");
    assert!(
        !preferred.is_empty(),
        "NewOrdersClient must anchor to v2.NewOrders: {edges:?}"
    );
    for e in &preferred {
        let ends = [e.source_id.as_str(), e.target_id.as_str()];
        assert!(
            ends.iter().any(|id| id.contains("v2-svc"))
                && ends.iter().any(|id| id.contains("logistics-svc")),
            "v2.NewOrders anchor must join v2-svc and logistics-svc, got {e:?}"
        );
    }
    // Never the OTHER declared service: the New-strip is fallback only.
    assert!(
        !edges.iter().any(|e| matches!(
            &e.provenance,
            Some(EdgeProvenance::Heuristic { detector, .. })
                if detector.starts_with("anchor:proto:orders.Orders")
        )),
        "NewOrdersClient must never pair onto orders.Orders: {edges:?}"
    );
}

/// The full exclusion: `customer_id` and friends never become anchors
/// regardless of file type.
#[test]
fn generic_identifiers_are_excluded() {
    for (name, text) in [
        ("a.py", "row[\"customer_id\"]\n"),
        ("b.ts", "const x = payload[\"customer_id\"];\n"),
        ("c.go", "m[\"customer_id\"] = v\n"),
        ("d.java", "data.getString(\"customer_id\");\n"),
    ] {
        let got = scan_text(name, text);
        assert!(
            !got.iter().any(|(_, k)| k.contains("customer_id")),
            "{name} leaked a field name as anchor: {got:?}"
        );
    }
}

/// `collect_commit_records` mirrors the git sensor's history view.
#[test]
fn commit_records_carry_subject_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.py"), "x = 1\n").unwrap();
    let run = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "user.name", "t"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "seed the thing"]);

    let git = crate::server::git::AnyGitSensor::from_env(root).unwrap();
    let records = collect_commit_records(&repo("inline-svc"), &git, 10);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].message, "seed the thing");
    assert_eq!(records[0].files, vec!["a.py".to_string()]);
    assert_eq!(records[0].repo, "inline-svc");
}
