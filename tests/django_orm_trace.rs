//! The start of the trace chain: a Django model's table, found by the REAL
//! indexer + sensors (no hand-built nodes), and `trace_flow` walking
//! upstream from that table to the function that reads it and on to that
//! function's caller.

use std::sync::Arc;

use lain::federation::config::SourceConfig;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::server::mcp::contract_tools::trace_flow::trace_flow_handle;
use lain::server::mcp::handler::McpContext;
use serde_json::{json, Value};

#[path = "common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;
use common::git_init_committed;

const MODELS: &str = "\
from django.db import models

class Product(models.Model):
    name = models.CharField(max_length=10)
";

// The query sits mid-function: indexer ranges are 0-based rows.
const RESOLVERS: &str = "\
from shop.product.models import Product

def resolve_product(pk):
    product = Product.objects.get(pk=pk)
    return product

def handler(pk):
    return resolve_product(pk)
";

async fn build_federation() -> (tempfile::TempDir, Arc<FederatedIndex>) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let shop = root.join("shop");
    std::fs::create_dir_all(shop.join("shop/product")).unwrap();
    std::fs::write(shop.join("shop/product/models.py"), MODELS).unwrap();
    std::fs::write(shop.join("shop/product/resolvers.py"), RESOLVERS).unwrap();
    git_init_committed(&shop);

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let cfg = ContractFederationConfig {
        provides: Vec::new(),
        services: vec![ServiceDecl {
            name: "shop".into(),
            repo: "shop".into(),
            paths: vec![],
            hosts: vec![],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
            provides_graphql: false,
        }],
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

    let source: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            RepoId::new("shop").unwrap(),
            shop.clone(),
            SourceConfig::WorkspaceDir { path: shop.clone() },
        )
        .unwrap(),
    );
    source.fetch().await.unwrap();
    fed.add_repo(source, &data_dir).await.unwrap();
    let id = RepoId::new("shop").unwrap();
    fed.get_repo(&id)
        .expect("repo")
        .index()
        .await
        .expect("index");
    fed.project_repo(&id).await.expect("project_repo");
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    (project, fed)
}

async fn call_trace_flow(fed: &Arc<FederatedIndex>, args: Value) -> Value {
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
        snapshots: None,
    };
    trace_flow_handle(&ctx, args).await.unwrap().structured
}

#[tokio::test]
async fn trace_flow_walks_upstream_from_a_django_table_to_its_readers() {
    let (_dir, fed) = build_federation().await;
    let got = call_trace_flow(
        &fed,
        json!({"from": {"table": "product_product"}, "direction": "upstream"}),
    )
    .await;
    let data = &got["data"];
    let paths = data["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("{got:#}"));
    assert!(!paths.is_empty(), "no path from the table: {got:#}");

    let hops: Vec<&Value> = paths
        .iter()
        .flat_map(|p| p["hops"].as_array().unwrap().iter())
        .collect();
    let names: Vec<&str> = hops.iter().filter_map(|h| h["name"].as_str()).collect();
    assert!(names.contains(&"product_product"), "table hop: {names:?}");
    assert!(
        names.contains(&"resolve_product"),
        "the function running the queryset: {names:?}"
    );
    assert!(
        names.contains(&"handler"),
        "and the caller of that function: {names:?}"
    );
    assert!(
        hops.iter()
            .any(|h| h["edge_type"].as_str() == Some("ReadsTable")),
        "the table is reached over a ReadsTable edge: {got:#}"
    );
}
