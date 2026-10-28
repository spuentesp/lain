//! Regression: `get_service.consumers[].uses[].used_by` must be populated
//! when the federation is built by the REAL indexer + sensors.
//!
//! `federation_contracts_e2e` scenario 17 passes against a hand-built graph
//! whose node ids are already global ids. In a real federation the per-repo
//! graph keeps local ids and only the projected (federated) backend carries
//! global ids, so looking the caller up in the per-repo graph by the
//! contract index's global id found nothing and `used_by` came back empty
//! for every consumer.

use std::sync::Arc;

use lain::federation::config::SourceConfig;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::server::mcp::contract_tools::services::get_service_handle;
use lain::server::mcp::handler::McpContext;
use serde_json::{json, Value};

#[path = "common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;
use common::git_init_committed;

async fn build_real_federation() -> (tempfile::TempDir, Arc<FederatedIndex>) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();

    let orders_dir = root.join("orders");
    std::fs::create_dir_all(orders_dir.join("src")).unwrap();
    std::fs::write(
        orders_dir.join("src/orders.py"),
        "from flask import Flask\napp = Flask(__name__)\n\n@app.route(\"/api/orders\")\ndef list_orders():\n    return {}\n",
    )
    .unwrap();
    let billing_dir = root.join("billing");
    std::fs::create_dir_all(billing_dir.join("src")).unwrap();
    std::fs::write(
        billing_dir.join("src/billing.py"),
        "import requests\n\ndef fetch_orders():\n    r = requests.get('https://orders.svc/api/orders')\n    return r\n\ndef report():\n    return fetch_orders()\n",
    )
    .unwrap();
    git_init_committed(&orders_dir);
    git_init_committed(&billing_dir);

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec![],
                hosts: vec!["orders.svc".into()],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
                provides_graphql: false,
            },
            ServiceDecl {
                name: "billing".into(),
                repo: "billing".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
                provides_graphql: false,
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    };
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_contract_config(cfg);

    for (id, dir) in [("orders", &orders_dir), ("billing", &billing_dir)] {
        let source: Box<dyn RepoSource> = Box::new(
            WorkspaceDirSource::with_config(
                RepoId::new(id).unwrap(),
                dir.clone(),
                SourceConfig::WorkspaceDir { path: dir.clone() },
            )
            .unwrap(),
        );
        // NB: the call sits mid-function on purpose. Indexer ranges are
        // 0-based rows while sensor lines are 1-based, so a call on a
        // function's LAST line currently falls back to the File node
        // (tracked separately from this regression).
        source.fetch().await.unwrap();
        fed.add_repo(source, &data_dir).await.unwrap();
    }
    for id in ["orders", "billing"] {
        let id = RepoId::new(id).unwrap();
        // Real indexer + sensors, not hand-inserted nodes.
        fed.get_repo(&id)
            .expect("repo")
            .index()
            .await
            .expect("index");
        fed.project_repo(&id).await.expect("project_repo");
    }
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    (project, fed)
}

async fn get_service(fed: &Arc<FederatedIndex>, service: &str) -> Value {
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
        snapshots: None,
    };
    get_service_handle(&ctx, json!({"snapshot": "live", "service": service}))
        .await
        .unwrap()
        .structured
}

#[tokio::test]
async fn used_by_is_populated_for_consumers_found_by_the_real_sensors() {
    let (_dir, fed) = build_real_federation().await;
    let got = get_service(&fed, "orders").await;
    let consumers = got["data"]["consumers"].as_array().expect("consumers");
    let billing = consumers
        .iter()
        .find(|c| c["service"].as_str() == Some("billing"))
        .unwrap_or_else(|| panic!("billing must consume orders: {got:#}"));
    let uses = billing["uses"].as_array().expect("uses");
    assert!(!uses.is_empty(), "billing must have a use: {billing:#}");
    for u in uses {
        let used_by = u["used_by"].as_array().expect("used_by array");
        assert!(
            !used_by.is_empty(),
            "used_by must never be silently empty (at least 'unreferenced'): {u:#}"
        );
        // The caller is the real function, resolved via the projected graph.
        assert_eq!(u["caller"]["name"].as_str(), Some("fetch_orders"), "{u:#}");
    }
    // `fetch_orders` is called by `report`, which nothing calls.
    let names: Vec<&str> = uses[0]["used_by"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(names.contains(&"report"), "walk must reach `report`: {names:?}");
}
