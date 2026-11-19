//! Service references survive indexing and reach the public MCP inventory.
use std::sync::Arc;

use lain::federation::contracts::config::ContractFederationConfig;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::server::mcp::contract_tools::ContractToolEntry;
use lain::server::mcp::handler::{HandlerStatus, McpContext};
use serde_json::json;

#[path = "../common/mod.rs"]
#[allow(clippy::duplicate_mod)]
mod common;

#[tokio::test]
async fn agent_lists_resolved_service_references_with_source_and_usage() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("client");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/client.py"),
        "import requests\n\ndef fetch():\n    return requests.get(\"http://orders.svc/orders\")\n",
    )
    .unwrap();
    std::fs::write(repo.join("README.md"), "See http://orders.svc/orders\n").unwrap();
    common::git_init_committed(&repo);
    let data = root.path().join("data");
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(&data).unwrap()));
    let id = RepoId::new("client").unwrap();
    fed.add_repo(
        Box::new(WorkspaceDirSource::new(id.clone(), repo).unwrap()),
        &data,
    )
    .await
    .unwrap();
    fed.set_contract_config(ContractFederationConfig::load_from_str(
        "services:\n  - name: client\n    repo: client\n  - name: orders\n    repo: orders\n    hosts: [orders.svc]\n",
    ).unwrap());
    fed.get_repo(&id).unwrap().index().await.unwrap();
    fed.project_repo(&id).await.unwrap();
    let status = HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(&fed),
        workspaces: None,
        status: &status,
        reload_bus: None,
        snapshots: None,
    };
    let tool = inventory::iter::<ContractToolEntry>
        .into_iter()
        .find(|entry| entry.name == "list_service_refs")
        .expect("public tool registered");
    let result = (tool.handler)(&ctx, json!({"snapshot": "live", "usage": "production"}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.structured);
    let items = result.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "documentation must not become production");
    assert_eq!(items[0]["path"], "src/client.py");
    assert_eq!(items[0]["line"], 4);
    assert_eq!(
        items[0]["outcome"],
        json!({"kind": "resolved", "service": "orders"})
    );
    assert_eq!(items[0]["linked_to_call"], true);
    assert_eq!(items[0]["edge"]["provenance"], "ref");
    assert_eq!(items[0]["edge"]["usage"], "production");
    let docs = (tool.handler)(&ctx, json!({"snapshot":"live", "usage":"documentation"}))
        .await
        .unwrap();
    assert_eq!(
        docs.structured["data"]["items"][0]["edge"]["usage"],
        "documentation"
    );

    // Source defaults resolve through const aliases, survive graph projection,
    // contribute to external coverage, and remain visibly marked as defaults.
    let checkout = fed.get_repo(&id).unwrap();
    std::fs::write(checkout.local_path().join("src/default.js"), "const B = process.env.PORTAL_URL || 'https://portal.nousresearch.com'; const A = B + '/api'; fetch(`${A}/oauth/token`);\n").unwrap();
    common::git_init_committed(checkout.local_path());
    checkout.index().await.unwrap();
    fed.project_repo(&id).await.unwrap();
    let coverage = inventory::iter::<ContractToolEntry>
        .into_iter()
        .find(|entry| entry.name == "get_coverage")
        .unwrap();
    let result = (coverage.handler)(&ctx, json!({"snapshot":"live"}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.structured);
    assert!(
        result.structured["data"]["external"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["host"] == "portal.nousresearch.com"),
        "{}",
        result.structured
    );
    assert!(result.structured["data"]["url_defaults"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["default"] == true && item["env"] == json!(["PORTAL_URL"])));

    let named = (tool.handler)(&ctx, json!({"snapshot": "saved"}))
        .await
        .unwrap();
    assert!(named.is_error, "named snapshots are explicitly unsupported");
    assert_eq!(named.structured["error"]["code"], "invalid_argument");
}

#[tokio::test]
async fn service_refs_persist_traversable_config_and_test_bindings() {
    use lain::schema::{EdgeType, NodeType};
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("nat-examples");
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(repo.join("config.yaml"), "url: http://localhost:8080/nat\n").unwrap();
    std::fs::write(
        repo.join("tests/config.yaml"),
        "url: http://localhost:8080/nat\n",
    )
    .unwrap();
    common::git_init_committed(&repo);
    let data = root.path().join("data");
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(&data).unwrap()));
    let id = RepoId::new("nat-examples").unwrap();
    fed.add_repo(
        Box::new(WorkspaceDirSource::new(id.clone(), repo.clone()).unwrap()),
        &data,
    )
    .await
    .unwrap();
    fed.set_contract_config(ContractFederationConfig::load_from_str(
        "services:\n  - name: nat-examples\n    repo: nat-examples\n  - name: nat\n    repo: nat\n    hosts: ['localhost:8080']\n").unwrap());
    fed.get_repo(&id).unwrap().index().await.unwrap();
    fed.project_repo(&id).await.unwrap();
    fed.rejoin_contracts_if_dirty().unwrap();
    let edges: Vec<_> = fed
        .backend()
        .all_edges()
        .unwrap()
        .into_iter()
        .filter(|e| e.edge_type == EdgeType::Binds && e.site.is_some())
        .collect();
    assert_eq!(
        edges.len(),
        2,
        "resolved reference bindings must be persisted"
    );
    let restored = PetgraphBackend::new(&data).unwrap();
    use lain::federation::graph_backend::GraphBackend;
    assert_eq!(
        restored
            .all_edges()
            .unwrap()
            .iter()
            .filter(|e| matches!(e.provenance, Some(lain::schema::EdgeProvenance::Ref { .. })))
            .count(),
        2
    );
    let graph = fed.backend().query_graph_view().unwrap();
    let query: lain::query::QuerySpec = serde_json::from_value(json!({"ops":[
        {"op":"find", "type":"ServiceRef"}, {"op":"filter", "path":"config.yaml"},
        {"op":"connect", "edge":"Binds"},
        {"op":"filter", "provenance":"ref", "usage":{"ne":"test"}}
    ]}))
    .unwrap();
    let embedder = lain::nlp::NlpEmbedder::new_stub();
    let cache = Arc::new(parking_lot::Mutex::new(lru::LruCache::new(
        std::num::NonZeroUsize::new(10).unwrap(),
    )));
    let result = lain::query::Executor::new(&graph, &embedder, &cache, root.path())
        .execute(&query)
        .unwrap();
    let all_sources: lain::query::QuerySpec = serde_json::from_value(json!({"ops":[
        {"op":"find", "type":"ServiceRef"},
        {"op":"filter", "usage":{"ne":"test"}, "provenance":"ref", "resolution":"resolved"},
        {"op":"connect", "edge":"Binds"}
    ]}))
    .unwrap();
    let filtered = lain::query::Executor::new(&graph, &embedder, &cache, root.path())
        .execute(&all_sources)
        .unwrap();
    assert_eq!(filtered.count, 1);
    assert!(filtered
        .edges
        .iter()
        .all(|e| e.usage == Some(lain::schema::ReferenceUsage::Configuration)));
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["nodes"].as_array().unwrap().len(), 1, "{result}");
    assert!(edges
        .iter()
        .all(|e| graph.get_node(&e.source_id).unwrap().unwrap().node_type == NodeType::ServiceRef));
    assert_eq!(result["paths"][0]["edges"][0]["kind"], "ref");
    assert_eq!(result["paths"][0]["edges"][0]["usage"], "config");
    // Rejoin retracts stale bindings when a source reference disappears.
    std::fs::write(repo.join("config.yaml"), "url: external\n").unwrap();
    fed.refresh_service_refs(&id);
    fed.mark_contracts_dirty();
    fed.rejoin_contracts_if_dirty().unwrap();
    assert_eq!(
        fed.backend()
            .all_edges()
            .unwrap()
            .iter()
            .filter(|e| e.edge_type == EdgeType::Binds && e.site.is_some())
            .count(),
        1
    );
}
