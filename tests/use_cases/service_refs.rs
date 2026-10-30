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

    let named = (tool.handler)(&ctx, json!({"snapshot": "saved"}))
        .await
        .unwrap();
    assert!(named.is_error, "named snapshots are explicitly unsupported");
    assert_eq!(named.structured["error"]["code"], "invalid_argument");
}
