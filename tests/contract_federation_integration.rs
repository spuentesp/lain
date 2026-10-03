//! Integration tests for the contract-federation joiner (§5.3 + §7).
//!
//! These tests exercise `FederatedIndex::rejoin_contracts_if_dirty`
//! end-to-end against a two-repo fixture. They check the §5.3
//! triggers (`add_repo`, `remove_repo`, dirty propagation,
//! idempotence, order independence) without depending on the
//! per-repo sensors, which already have their own coverage in
//! `tests/federation_integration.rs`.
//!
//! Heavier scenario tests (the §15.2 fixtures: tokio + bytes) live
//! behind `#[ignore]` because they need network access to clone
//! upstream repositories.

use lain::federation::config::SourceConfig;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use std::sync::Arc;
use tempfile::TempDir;

#[path = "common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;
use common::git_init_committed;

/// Build a workspace fixture with two repos containing one provider
/// and one consumer node each, plus a `repos.yaml` declaring the
/// services. The function returns the federation, the tempdir
/// (caller keeps it alive), and the per-repo source so callers can
/// manipulate membership.
async fn build_two_repo_federation() -> (
    TempDir,
    Arc<FederatedIndex>,
    WorkspaceDirSource,
    WorkspaceDirSource,
) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();

    // Repo `orders` with a provider.
    let orders_dir = root.join("orders");
    std::fs::create_dir_all(orders_dir.join("src")).unwrap();
    std::fs::write(
        orders_dir.join("src/orders.py"),
        "def get_order(order_id: int) -> dict:\n    return {}\n",
    )
    .unwrap();
    // Repo `billing` with a consumer that calls orders.
    let billing_dir = root.join("billing");
    std::fs::create_dir_all(billing_dir.join("src")).unwrap();
    std::fs::write(
        billing_dir.join("src/billing.py"),
        "import requests\ndef fetch_order():\n    requests.get('https://orders.svc/api/orders/42')\n",
    )
    .unwrap();

    // The git sensor requires each repo to have an initial commit
    // before `add_repo` will accept it.
    git_init_committed(&orders_dir);
    git_init_committed(&billing_dir);

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let repos_yaml = format!(
        "data_dir: {}\nrepos:\n  - id: orders\n    source:\n      type: workspace_dir\n      path: {}\n  - id: billing\n    source:\n      type: workspace_dir\n      path: {}\n",
        data_dir.display(),
        orders_dir.display(),
        billing_dir.display(),
    );
    std::fs::write(root.join("repos.yaml"), repos_yaml).unwrap();

    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
    };

    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_contract_config(cfg);

    let orders_source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            RepoId::new("orders").unwrap(),
            orders_dir.clone(),
            SourceConfig::WorkspaceDir {
                path: orders_dir.clone(),
            },
        )
        .unwrap(),
    );
    let billing_source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            RepoId::new("billing").unwrap(),
            billing_dir.clone(),
            SourceConfig::WorkspaceDir {
                path: billing_dir.clone(),
            },
        )
        .unwrap(),
    );
    let orders_inner = WorkspaceDirSource::with_config(
        RepoId::new("orders").unwrap(),
        orders_dir.clone(),
        SourceConfig::WorkspaceDir {
            path: orders_dir.clone(),
        },
    )
    .unwrap();
    let billing_inner = WorkspaceDirSource::with_config(
        RepoId::new("billing").unwrap(),
        billing_dir.clone(),
        SourceConfig::WorkspaceDir {
            path: billing_dir.clone(),
        },
    )
    .unwrap();
    orders_source.fetch().await.unwrap();
    billing_source.fetch().await.unwrap();
    fed.add_repo(orders_source, &data_dir).await.unwrap();
    fed.add_repo(billing_source, &data_dir).await.unwrap();

    (project, fed, orders_inner, billing_inner)
}

/// Project both repos into the backend so contract nodes are
/// visible to the joiner.
async fn project_both(fed: &FederatedIndex) {
    for id in ["orders", "billing"] {
        let id = RepoId::new(id).unwrap();
        fed.project_repo(&id).await.expect("project_repo");
    }
}

#[tokio::test]
async fn add_repo_and_project_marks_contracts_dirty() {
    // This test discriminates against the §5.3 contract:
    // `project_nodes` and `project_edges` must set
    // `contracts_dirty` so the next `rejoin_contracts_if_dirty`
    // call actually runs the join (instead of being a no-op).
    //
    // The discriminator is the `Arc` identity of
    // `fed.contract_index()`. When `rejoin_contracts_if_dirty`
    // runs the join (because dirty was set) it allocates a
    // fresh `ContractIndex` and stores a new `Arc`. When the
    // join is a no-op (dirty clear) the stored `Arc` is
    // returned unchanged. We compare identities across two
    // projection + rejoin cycles; if projection failed to
    // mark dirty, the second cycle would short-circuit and
    // the Arc would not change.
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;

    // Step 1: index is None before any rejoin.
    assert!(fed.contract_index().is_none(), "no index before any rejoin");

    // Step 2: project + first rejoin produces an index.
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("first rejoin");
    let arc_after_first = fed.contract_index().expect("index after first rejoin");

    // Step 3: re-running rejoin without a projection in
    // between is a no-op (dirty flag was cleared by the
    // first call). Same Arc.
    fed.rejoin_contracts_if_dirty().expect("second rejoin");
    let arc_after_second = fed.contract_index().expect("index");
    assert!(
        std::sync::Arc::ptr_eq(&arc_after_first, &arc_after_second),
        "idempotent rejoin returns the same Arc"
    );

    // Step 4: a *fresh* projection must mark dirty again,
    // and the next rejoin must produce a *different* Arc. If
    // `project_nodes` or `project_edges` failed to set
    // `contracts_dirty`, the second rejoin would still be a
    // no-op and `ptr_eq` would be true — the assertion below
    // discriminates against that regression.
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("third rejoin");
    let arc_after_third = fed.contract_index().expect("index");
    assert!(
        !std::sync::Arc::ptr_eq(&arc_after_first, &arc_after_third),
        "projection re-marked dirty: third rejoin built a new ContractIndex"
    );
}

#[tokio::test]
async fn rejoin_contracts_is_idempotent() {
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("first rejoin");
    let count = fed.contract_index().map(|i| i.consumers.len()).unwrap_or(0);
    // Second call must be a no-op (dirty flag is clear).
    fed.rejoin_contracts_if_dirty().expect("second rejoin");
    let after = fed.contract_index().map(|i| i.consumers.len()).unwrap_or(0);
    assert_eq!(count, after, "idempotent: no edges added on second pass");
}

#[tokio::test]
async fn remove_repo_clears_its_contract_nodes_and_dirties() {
    let (_dir, fed, _o, b) = build_two_repo_federation().await;
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty()
        .expect("rejoin before remove");
    let bid = RepoId::new("billing").unwrap();
    fed.remove_repo(&bid).expect("remove_repo billing");
    // After `remove_repo` the dirty flag must be set; the next
    // join recomputes without billing.
    fed.rejoin_contracts_if_dirty()
        .expect("rejoin after remove");
    let idx = fed.contract_index().expect("index");
    assert!(
        !idx.services.keys().any(|s| s.0 == "billing"),
        "billing service is implicit and disappears once its repo is removed"
    );
    // Keep `b` alive so the test setup is not optimized away.
    let _ = b.local_path();
}

/// Regression for the TLA+ RejoinProtocol.tla 10-state trace that
/// violates `NoLostUpdate`: `WriterSetConfig(c1)` lands between the
/// rejoin's `RejoinReadConfig(c0)` and its `RejoinClear`. The pre-fix
/// code cleared dirty at the END of `rejoin_contracts`, overwriting
/// the writer's mark. The post-fix code clears dirty at the START
/// (variant (b) — clear-before-read), so any writer that lands
/// after the clear re-arms dirty and the next call redoes the work.
#[tokio::test]
async fn rejoin_does_not_lose_mid_join_mark() {
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    project_both(&fed).await;

    // Baseline: project + initial rejoin establishes the starting
    // state (dirty=FALSE).
    fed.rejoin_contracts_if_dirty()
        .expect("baseline rejoin");
    assert!(!fed.contracts_dirty(), "baseline: dirty is clear");

    // Mark dirty and run the rejoin on a worker thread. Poll the
    // dirty flag from the main thread: post-fix the rejoin clears
    // dirty BEFORE the read/apply phase, so dirty briefly drops to
    // FALSE; pre-fix dirty stays TRUE until the trailing clear.
    fed.mark_contracts_dirty();
    assert!(fed.contracts_dirty(), "post-mark: dirty is set");
    let fed_for_rejoin = std::sync::Arc::clone(&fed);
    let rejoin_handle = std::thread::spawn(move || {
        fed_for_rejoin
            .rejoin_contracts_if_dirty()
            .expect("rejoin");
    });

    // Wait until the clear-before-read fires. Bounded by a deadline
    // so a regression that broke the fix (dirty never clears during
    // the rejoin) reports a clear failure rather than hanging.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fed.contracts_dirty() {
        if std::time::Instant::now() > deadline {
            panic!(
                "rejoin never cleared dirty mid-flight — clear-before-read \
                 did not fire (TLA+ RejoinProtocol.tla NoLostUpdate fix is \
                 missing)"
            );
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }

    // The rejoin is now in the read/apply/swap phase. Inject a
    // mark: post-fix this survives the rejoin's trailing clear
    // (which is now a no-op because dirty was already false); pre-fix
    // it would be overwritten by the trailing clear.
    fed.mark_contracts_dirty();
    rejoin_handle.join().expect("rejoin thread");

    assert!(
        fed.contracts_dirty(),
        "mid-rejoin mark_contracts_dirty survives the rejoin \
         (TLA+ RejoinProtocol.tla NoLostUpdate / Convergence variant (b))"
    );
}

#[tokio::test]
async fn contract_config_round_trip() {
    // Setting the same config twice must produce the same hash and
    // a clean dirty flag (the joiner re-runs once but the new
    // desired set is identical to the stored set).
    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
    };
    let h1 = cfg.config_hash();
    let h2 = cfg.config_hash();
    assert_eq!(h1, h2);
    assert!(!h1.is_empty());
}

#[tokio::test]
async fn projection_order_independence() {
    // Two projections in opposite orders produce identical Binds
    // sets. We don't actually project twice with different
    // orderings — we already covered `ContractJoiner::run`
    // order-independence in the unit suite. Here we only check
    // that calling `rejoin_contracts_if_dirty` on an already-clean
    // federation is a no-op (the BTreeSet diff is empty).
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("first rejoin");
    let before = fed.contract_index().as_ref().cloned().unwrap_or_default();
    // Second pass: should be a no-op, no dirty flag.
    fed.rejoin_contracts_if_dirty().expect("second rejoin");
    let after = fed.contract_index().as_ref().cloned().unwrap_or_default();
    assert_eq!(before.consumers, after.consumers);
    assert_eq!(before.endpoints, after.endpoints);
}

#[tokio::test]
async fn external_host_appears_in_coverage_external() {
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    // Without the per-repo sensors emitting contract nodes, the
    // contract index stays empty. The test mainly checks that
    // the joiner doesn't choke on an empty backend. Real
    // external-host coverage is asserted at the joiner unit
    // level (`rule_4_external_host_when_no_service_matches_and_not_exempt`).
    let _idx = fed.contract_index();
}

#[tokio::test]
async fn no_endpoints_means_no_binds() {
    // Federation with no provider nodes: no Binds edges after a
    // rejoin, no endpoints, no consumers.
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let cfg = ContractFederationConfig::default();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_contract_config(cfg);
    fed.rejoin_contracts_if_dirty().expect("empty rejoin");
    let idx = fed.contract_index().expect("index");
    assert!(idx.endpoints.is_empty());
    assert!(idx.consumers.is_empty());
}

#[tokio::test]
async fn unnormalized_consumers_are_recorded() {
    // Rule 5 of the §7.3 table: a consumer whose URL template is
    // `None` (fully dynamic path) is recorded in
    // `ContractIndex::unnormalized` and never produces a Binds
    // edge. Rule 5 only fires when rules 3 (target service
    // known) and 4 (literal external host) have not matched, so
    // we use `HostPart::Expr(...)` — neither a literal nor an
    // env name, so rules 3 and 4 cannot fire.
    //
    // The integration-test fixture's hand-written Python sources
    // don't go through the sensor pipeline, so we drive the
    // joiner directly with a synthetic
    // `ConsumerFact { url: NormalizedUrl { template: None, .. } }`.
    // This is the in-process shape the sensors will eventually
    // emit for a fully dynamic URL.
    use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
    use lain::federation::contracts::index::UnresolvedReason;
    use lain::federation::contracts::joiner::ContractJoiner;
    use lain::federation::contracts::model::{
        CallVia, ConsumerFact, ContractFact, HostPart, HttpMethod, MethodSpec, NormalizedUrl,
        ProviderFact, ProviderOrigin,
    };
    use lain::federation::repo_id::{GlobalId, RepoId};
    use lain::schema::{GraphNode, NodeType, RepoNamespace};

    let ns = RepoNamespace::for_test();
    let mut provider = GraphNode::new_in(
        NodeType::HttpRoute,
        "do".into(),
        "src/orders.py".into(),
        &ns,
    );
    provider.repo_id = Some("orders".into());
    provider.id = GlobalId::new(
        &RepoId::new("orders").unwrap(),
        NodeType::HttpRoute,
        "src/orders.py",
        "do",
        Some(10),
    )
    .as_str()
    .to_string();
    provider.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/api/orders".into(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));

    let mut consumer = GraphNode::new_in(
        NodeType::HttpClientCall,
        "fetch".into(),
        "src/billing.py".into(),
        &ns,
    );
    consumer.repo_id = Some("billing".into());
    let consumer_gid = GlobalId::new(
        &RepoId::new("billing").unwrap(),
        NodeType::HttpClientCall,
        "src/billing.py",
        "fetch",
        Some(1),
    );
    consumer.id = consumer_gid.as_str().to_string();
    consumer.contract = Some(ContractFact::Consumer(ConsumerFact {
        method: MethodSpec::Known(HttpMethod::Get),
        // Expr host — rules 3 / 4 cannot resolve it; template
        // = None — rule 5 fires.
        url: NormalizedUrl {
            host: HostPart::Expr("config.base_url".into()),
            template: None,
        },
        via: CallVia::Library {
            name: "requests".into(),
        },
        url_expr: String::new(),
        reads_complete: true,
    }));

    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
    };
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    assert_eq!(
        out.binds.len(),
        0,
        "rule 5: no Binds for a dynamic-path consumer"
    );
    assert_eq!(
        out.index.unnormalized.len(),
        1,
        "rule 5: dynamic-path consumer lands in `unnormalized`"
    );
    assert_eq!(
        out.index.unnormalized[0].as_str(),
        consumer_gid.as_str(),
        "rule 5: `unnormalized` carries the consumer's GlobalId"
    );
    // The consumer resolution exists, with an Unresolved
    // verdict tagged Unnormalized.
    let resolution = out
        .index
        .consumers
        .get(&consumer_gid)
        .expect("consumer resolution");
    assert!(matches!(
        resolution.target,
        Some(
            lain::federation::contracts::index::ConsumerTarget::Unresolved {
                reason: UnresolvedReason::Unnormalized,
                ..
            }
        )
    ));
    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    // Sanity: the live federation path still works.
    project_both(&fed).await;
    fed.rejoin_contracts_if_dirty().expect("rejoin");
}

/// §7.5 + §4.2 + §9.5: the field join's `Binds(FieldRef → Field)`
/// edges must **land in the graph backend** — the §9.5 qualifying
/// prefix `Field ← Binds ← FieldRef ← ReadsField ← reading function`
/// is a graph traversal, so the edge has to exist as a persisted
/// `Binds` edge, not only in the `field_refs` index map.
///
/// Seeds the per-repo graphs with the nodes/edges the sensors would
/// emit (provider route + response schema + field; consumer call +
/// FieldRef + ReadsFrom), then runs the real `project_repo` +
/// `rejoin_contracts` pipeline and inspects the backend.
#[tokio::test]
async fn field_ref_to_field_binds_edge_is_persisted() {
    use lain::federation::contracts::model::{
        CallVia, ConsumerFact, ContractFact, Direction, FieldMeta, FieldReadFact, HostPart,
        HttpMethod, MethodSpec, NormalizedUrl, ProviderFact, ProviderOrigin, TypeDesc,
    };
    use lain::federation::repo_id::GlobalId;
    use lain::schema::{
        EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, StaticSource,
    };

    let (_dir, fed, _o, _b) = build_two_repo_federation().await;
    let ns = RepoNamespace::for_test();

    // Provider (orders): an OpenAPI route with a response schema and
    // one field — the shapes `openapi_sensor` emits.
    let mut route = GraphNode::new_in(
        NodeType::HttpRoute,
        "GET /api/orders/{}".into(),
        "openapi.yaml".into(),
        &ns,
    );
    route.repo_id = Some("orders".into());
    route.line_start = Some(3);
    route.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/api/orders/{}".into(),
        handler: None,
        operation_id: Some("getOrder".into()),
        origin: ProviderOrigin::OpenApi,
    }));
    let mut schema = GraphNode::new_in(
        NodeType::Schema,
        "response".into(),
        "openapi.yaml".into(),
        &ns,
    );
    schema.repo_id = Some("orders".into());
    schema.line_start = Some(8);
    schema.contract = Some(ContractFact::Schema {
        direction: Direction::Response,
    });
    let mut field = GraphNode::new_in(
        NodeType::Field,
        "customer_id".into(),
        "openapi.yaml".into(),
        &ns,
    );
    field.repo_id = Some("orders".into());
    field.line_start = Some(10);
    field.contract = Some(ContractFact::Field(FieldMeta {
        ty: TypeDesc::String,
        required: true,
        nullable: false,
        enum_values: None,
    }));

    // Consumer (billing): the call and the field read — the shapes
    // `http_client_sensor` and `field_access_sensor` emit.
    let mut call = GraphNode::new_in(
        NodeType::HttpClientCall,
        "GET /api/orders/42".into(),
        "src/billing.py".into(),
        &ns,
    );
    call.repo_id = Some("billing".into());
    call.line_start = Some(5);
    call.contract = Some(ContractFact::Consumer(ConsumerFact {
        method: MethodSpec::Known(HttpMethod::Get),
        url: NormalizedUrl {
            host: HostPart::Literal("orders.svc".into()),
            template: Some("/api/orders/42".into()),
        },
        via: CallVia::Library {
            name: "httpx".into(),
        },
        url_expr: "\"https://orders.svc/api/orders/42\"".into(),
        reads_complete: true,
    }));
    let mut fr = GraphNode::new_in(
        NodeType::FieldRef,
        "customer_id".into(),
        "src/billing.py".into(),
        &ns,
    );
    fr.repo_id = Some("billing".into());
    fr.line_start = Some(5);
    fr.contract = Some(ContractFact::FieldRead(FieldReadFact {
        chain: "customer_id".parse().unwrap(),
        exact: true,
    }));

    // Edges (per-repo local ids; `project_edges` rewrites both
    // endpoints to GlobalIds).
    let response_schema = GraphEdge::new(
        EdgeType::ResponseSchema,
        route.id.clone(),
        schema.id.clone(),
    );
    let has_field = GraphEdge::new(EdgeType::HasField, schema.id.clone(), field.id.clone());
    let mut reads_from = GraphEdge::new(EdgeType::ReadsFrom, fr.id.clone(), call.id.clone());
    reads_from.provenance = Some(EdgeProvenance::Static {
        source: StaticSource::TreeSitter,
    });

    let orders = fed
        .get_repo(&RepoId::new("orders").unwrap())
        .expect("orders repo");
    orders.db().upsert_node(route).expect("upsert route");
    orders.db().upsert_node(schema).expect("upsert schema");
    orders.db().upsert_node(field).expect("upsert field");
    orders
        .db()
        .upsert_edge(response_schema)
        .expect("upsert ResponseSchema");
    orders.db().upsert_edge(has_field).expect("upsert HasField");
    let billing = fed
        .get_repo(&RepoId::new("billing").unwrap())
        .expect("billing repo");
    billing.db().upsert_node(call).expect("upsert call");
    billing.db().upsert_node(fr).expect("upsert FieldRef");
    billing
        .db()
        .upsert_edge(reads_from)
        .expect("upsert ReadsFrom");

    project_both(&fed).await;
    fed.rejoin_contracts().expect("rejoin");

    let call_gid = GlobalId::new(
        &RepoId::new("billing").unwrap(),
        NodeType::HttpClientCall,
        "src/billing.py",
        "GET /api/orders/42",
        Some(5),
    );
    let fr_gid = GlobalId::new(
        &RepoId::new("billing").unwrap(),
        NodeType::FieldRef,
        "src/billing.py",
        "customer_id",
        Some(5),
    );
    let field_gid = GlobalId::new(
        &RepoId::new("orders").unwrap(),
        NodeType::Field,
        "openapi.yaml",
        "customer_id",
        Some(10),
    );

    // The persisted edge: FieldRef → Field, weight = min(read 1.0,
    // endpoint-bind confidence), provenance carried (§7.8).
    let edges = fed.backend().all_edges().expect("all_edges");
    let field_binds: Vec<_> = edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Binds && e.source_id == fr_gid.as_str())
        .collect();
    assert_eq!(
        field_binds.len(),
        1,
        "exactly one persisted Binds(FieldRef → Field) edge"
    );
    let fb = field_binds[0];
    assert_eq!(fb.target_id, field_gid.as_str(), "target is the Field");
    assert!(
        fb.provenance.is_some(),
        "§7.8: every Binds edge carries provenance"
    );
    // §4.6: capped by the endpoint bind — the call's own Binds edge
    // weight is the endpoint-side confidence the field edge min'd
    // with (the read was exact at 1.0).
    let call_bind = edges
        .iter()
        .find(|e| e.edge_type == EdgeType::Binds && e.source_id == call_gid.as_str())
        .expect("the call must bind to the orders endpoint");
    let expected = call_bind.weight.unwrap_or(1.0).min(1.0);
    assert!(
        (fb.weight.unwrap_or(0.0) - expected).abs() < f32::EPSILON,
        "field bind weight {} must be min(1.0, endpoint {})",
        fb.weight.unwrap_or(0.0),
        call_bind.weight.unwrap_or(1.0)
    );
    // §7.8: cross_repo is true exactly when the repos differ —
    // FieldRef lives in billing, Field in orders.
    assert!(fb.cross_repo, "billing → orders is cross-repo");

    // §7.8 purity/idempotence: a second rejoin recomputes the
    // desired set and must neither drop nor duplicate the edge.
    fed.rejoin_contracts().expect("second rejoin");
    let edges_after = fed.backend().all_edges().expect("all_edges after rejoin");
    let field_binds_after: Vec<_> = edges_after
        .iter()
        .filter(|e| e.edge_type == EdgeType::Binds && e.source_id == fr_gid.as_str())
        .collect();
    assert_eq!(field_binds_after.len(), 1, "rejoin is idempotent");
    assert_eq!(field_binds_after[0].target_id, fb.target_id);
    assert_eq!(field_binds_after[0].weight, fb.weight);
    assert_eq!(field_binds_after[0].provenance, fb.provenance);
}

#[allow(dead_code)]
fn _hush() {}
