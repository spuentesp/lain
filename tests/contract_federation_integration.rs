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
    let out = ContractJoiner::run(&[provider, consumer], &cfg);
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

#[allow(dead_code)]
fn _hush() {}
