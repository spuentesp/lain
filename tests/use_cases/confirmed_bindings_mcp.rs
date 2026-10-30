//! Existing RPC facts exercise the MCP mapping workflow independently of sensors.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lain::federation::contracts::config::{ConfirmedBinding, ContractFederationConfig};
use lain::federation::contracts::index::{ConsumerTarget, UnresolvedReason};
use lain::federation::contracts::model::{
    ContractFact, HostPart, ProviderFact, RpcConsumerFact, RpcProviderFact, RpcSystem,
    TopicConsumerFact, TopicConsumerKind,
};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::health::RepoHealth;
use lain::federation::repo_id::{GlobalId, RepoId};
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::schema::{EdgeProvenance, GraphNode, NodeType, RepoNamespace};
use lain::server::mcp::contract_tools::analysis::trace_impact_handle;
use lain::server::mcp::contract_tools::context::apply_contract_context_handle;
use lain::server::mcp::contract_tools::contracts::{
    check_binding_handle, get_contract_handle, list_unresolved_handle,
};
use lain::server::mcp::handler::{HandlerStatus, McpContext};
use serde_json::{json, Value};

const KEY: &str = "rpc:orders.v1/Get";
const INITIAL: &str = "\
repos: []
operator_note: keep-this
services:
  - {name: orders, repo: orders}
  - {name: backup, repo: backup}
  - {name: billing, repo: billing}
provides:
  - {service: orders, key: 'http:GET /manual', source: operator}
host_aliases:
  - {host: MANUAL_ORIGIN, service: orders, source: operator}
";

fn fact_node(repo: &str, name: &str, line: u32, fact: ContractFact) -> GraphNode {
    let mut node = GraphNode::new_in(
        NodeType::Function,
        name.into(),
        "src/client.py".into(),
        &RepoNamespace::for_test(),
    );
    node.id = GlobalId::new(
        &RepoId::new(repo).unwrap(),
        NodeType::Function,
        "src/client.py",
        name,
        Some(line),
    )
    .as_str()
    .into();
    node.repo_id = Some(repo.into());
    node.line_start = Some(line);
    node.line_end = Some(line);
    node.contract = vec![fact];
    node
}

fn rpc_consumer(line: u32) -> GraphNode {
    fact_node(
        "billing",
        "get_order",
        line,
        ContractFact::RpcConsumer(RpcConsumerFact {
            system: RpcSystem::Grpc,
            service: "orders.v1".into(),
            method: "Get".into(),
            channel_target: Some("unknown:50051".into()),
            channel_host_part: HostPart::Literal("unknown".into()),
        }),
    )
}

fn rpc_provider(repo: &str) -> GraphNode {
    fact_node(
        repo,
        "orders.v1.Get",
        5,
        ContractFact::RpcProvider(RpcProviderFact {
            system: RpcSystem::Grpc,
            service: "orders.v1".into(),
            method: "Get".into(),
            request_type: "GetRequest".into(),
            response_type: "GetResponse".into(),
            handler: None,
        }),
    )
}

struct Fixture {
    root: tempfile::TempDir,
    fed: Arc<FederatedIndex>,
    path: PathBuf,
    consumer: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("repos.yaml");
        std::fs::write(&path, INITIAL).unwrap();
        let fed = load(root.path(), "data").await;
        Self {
            root,
            fed,
            path,
            consumer: rpc_consumer(10).id,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }
}

async fn load(root: &Path, data_name: &str) -> Arc<FederatedIndex> {
    let data_dir = root.join(data_name);
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&data_dir).unwrap());
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_config_path(root.join("repos.yaml"));
    fed.set_contract_config(ContractFederationConfig::load(&root.join("repos.yaml")).unwrap());
    for repo in ["orders", "backup", "billing"] {
        let dir = root.join(repo);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/client.py"), "# fact fixture\n").unwrap();
        if !dir.join(".git").exists() {
            let git = git2::Repository::init(&dir).unwrap();
            let tree = git.index().unwrap().write_tree().unwrap();
            let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
            git.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "init",
                &git.find_tree(tree).unwrap(),
                &[],
            )
            .unwrap();
        }
        let id = RepoId::new(repo).unwrap();
        let source: Box<dyn RepoSource> = Box::new(
            WorkspaceDirSource::with_config(
                id.clone(),
                dir.clone(),
                lain::federation::config::SourceConfig::WorkspaceDir { path: dir },
            )
            .unwrap(),
        );
        source.fetch().await.unwrap();
        fed.add_repo(source, &data_dir).await.unwrap();
        let indexed = fed.get_repo(&id).unwrap();
        indexed.set_health(RepoHealth::Ready);
        let nodes = if repo == "billing" {
            vec![rpc_consumer(10), rpc_provider(repo)]
        } else {
            vec![rpc_provider(repo)]
        };
        indexed.db().insert_nodes_batch(&nodes).unwrap();
        for node in nodes {
            fed.register_contract_node_for_test(id.clone(), node.id);
        }
        fed.project_repo(&id).await.unwrap();
    }
    fed.mark_contracts_dirty();
    fed.rejoin_contracts_if_dirty().unwrap();
    fed
}

fn context<'a>(fed: &'a FederatedIndex, status: &'a HandlerStatus) -> McpContext<'a> {
    McpContext {
        server: None,
        federation: Some(fed),
        workspaces: None,
        status,
        reload_bus: None,
        snapshots: None,
    }
}

fn binding() -> Value {
    json!({
        "consumer": {"repo": "billing", "path": "src/client.py", "symbol": "get_order", "key": KEY},
        "provider": {"service": "orders", "key": KEY}
    })
}

async fn apply(fed: &FederatedIndex, bindings: Value, dry_run: bool) -> Value {
    let status = HandlerStatus::for_test();
    apply_contract_context_handle(
        &context(fed, &status),
        json!({"snapshot": "live", "bindings": bindings, "dry_run": dry_run}),
    )
    .await
    .unwrap()
    .structured
}

async fn assert_confirmed_via_mcp(fed: &FederatedIndex, consumer: &str) {
    let status = HandlerStatus::for_test();
    let ctx = context(fed, &status);
    let output = get_contract_handle(&ctx, json!({"snapshot": "live", "key": KEY}))
        .await
        .unwrap();
    assert!(!output.is_error, "{:?}", output.structured);
    let item = output.structured["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["endpoint"]["service"] == "orders")
        .unwrap();
    let bound = item["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|site| site["site"]["id"] == consumer)
        .unwrap();
    assert_eq!(bound["binding"]["kind"], "confirmed");
    assert!(bound["binding"]["source"]
        .as_str()
        .unwrap()
        .contains("repos.yaml#bindings"));
    let impact = trace_impact_handle(
        &ctx,
        json!({
            "snapshot": "live", "from": {"endpoint": {"service": "orders", "key": KEY}}, "depth": 3
        }),
    )
    .await
    .unwrap();
    assert!(!impact.is_error, "{:?}", impact.structured);
    assert!(
        impact.structured["data"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|path| path["hops"].as_array().unwrap())
            .any(|hop| hop["node"] == consumer && hop["edge"] == "Binds"),
        "{:?}",
        impact.structured
    );
}

#[tokio::test]
async fn unresolved_rpc_check_preview_apply_and_reload() {
    let fixture = Fixture::new().await;
    let status = HandlerStatus::for_test();
    let ctx = context(&fixture.fed, &status);
    let initial_hash = fixture.fed.contract_config().unwrap().config_hash();
    assert!(matches!(
        fixture
            .fed
            .contract_index()
            .unwrap()
            .consumer(&GlobalId::from_canonical(&fixture.consumer))
            .unwrap()
            .target,
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::RpcStubUnknown,
            ..
        })
    ));
    let unresolved = list_unresolved_handle(&ctx, json!({"snapshot": "live"}))
        .await
        .unwrap();
    assert!(!unresolved.is_error, "{:?}", unresolved.structured);
    assert!(unresolved
        .structured
        .to_string()
        .contains(&fixture.consumer));
    let before = fixture.bytes();
    let check = check_binding_handle(
        &ctx,
        json!({
            "snapshot": "live", "consumer": fixture.consumer,
            "endpoint": {"service": "orders", "key": KEY}
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        check.structured["data"]["valid"], true,
        "{:?}",
        check.structured
    );
    let entries: Vec<ConfirmedBinding> =
        serde_yaml::from_str(check.structured["data"]["bindings_entry"].as_str().unwrap()).unwrap();
    assert_eq!(fixture.bytes(), before, "check_binding stays pure");
    let entries = serde_json::to_value(entries).unwrap();
    let preview = apply(&fixture.fed, entries.clone(), true).await;
    assert_eq!(preview["data"]["applied"], false, "{preview}");
    assert_eq!(
        preview["data"]["added_bindings"].as_array().unwrap().len(),
        1
    );
    assert_eq!(fixture.bytes(), before);
    assert_eq!(
        fixture.fed.contract_config().unwrap().config_hash(),
        initial_hash
    );
    assert!(!lain::federation::contracts::context::backup_path(&fixture.path).exists());
    assert!(!lain::server::state_lock::lock_path_for(&fixture.path).exists());
    let preview_cfg =
        ContractFederationConfig::load_from_str(preview["data"]["yaml"].as_str().unwrap()).unwrap();
    assert_eq!(preview_cfg.bindings.len(), 1);
    let applied = apply(&fixture.fed, entries.clone(), false).await;
    assert_eq!(applied["data"]["applied"], true, "{applied}");
    let persisted = ContractFederationConfig::load(&fixture.path).unwrap();
    assert_eq!(persisted.bindings.len(), 1);
    assert_eq!(persisted.provides, preview_cfg.provides);
    assert_eq!(persisted.host_aliases, preview_cfg.host_aliases);
    assert_ne!(
        initial_hash,
        persisted.config_hash(),
        "bindings change snapshot config identity"
    );
    assert!(String::from_utf8(fixture.bytes())
        .unwrap()
        .contains("keep-this"));
    assert_confirmed_via_mcp(&fixture.fed, &fixture.consumer).await;
    let written = fixture.bytes();
    let duplicate = apply(&fixture.fed, entries, false).await;
    assert_eq!(duplicate["data"]["applied"], true, "{duplicate}");
    assert!(duplicate["data"]["added_bindings"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(fixture.bytes(), written);
    let reloaded = load(fixture.root.path(), "reloaded-data").await;
    assert_eq!(
        reloaded.contract_config().unwrap().config_hash(),
        persisted.config_hash()
    );
    assert_confirmed_via_mcp(&reloaded, &fixture.consumer).await;
}

#[tokio::test]
async fn binding_validation_selects_the_matching_topic_fact_resolution() {
    let fixture = Fixture::new().await;
    let repo_ns = RepoNamespace::for_test();
    let mut consumer = GraphNode::new_in(
        NodeType::Function,
        "subscribe_both".into(),
        "src/client.py".into(),
        &repo_ns,
    );
    consumer.id = GlobalId::new(
        &RepoId::new("billing").unwrap(),
        NodeType::Function,
        "src/client.py",
        "subscribe_both",
        Some(22),
    )
    .as_str()
    .into();
    consumer.repo_id = Some("billing".into());
    consumer.line_start = Some(22);
    consumer.line_end = Some(22);
    consumer.contract = vec![
        ContractFact::TopicConsumer(TopicConsumerFact {
            broker: "kafka".into(),
            name: "orders.created".into(),
            kind: TopicConsumerKind::Subscription,
        }),
        ContractFact::TopicConsumer(TopicConsumerFact {
            broker: "kafka".into(),
            name: "audit.logged".into(),
            kind: TopicConsumerKind::Subscription,
        }),
    ];
    let make_provider = |repo: &str, topic: &str, line: u32| {
        let mut node = GraphNode::new_in(
            NodeType::Topic,
            format!("kafka/{topic}"),
            "src/providers.py".into(),
            &RepoNamespace::for_test(),
        );
        node.id = GlobalId::new(
            &RepoId::new(repo).unwrap(),
            NodeType::Topic,
            "src/providers.py",
            &format!("kafka/{topic}"),
            Some(line),
        )
        .as_str()
        .into();
        node.repo_id = Some(repo.into());
        node.line_start = Some(line);
        node.line_end = Some(line);
        node.contract = vec![ContractFact::Provider(ProviderFact {
            method: lain::federation::contracts::model::HttpMethod::Any,
            template: topic.into(),
            handler: None,
            operation_id: None,
            origin: lain::federation::contracts::model::ProviderOrigin::Code,
        })];
        node
    };
    let billing = RepoId::new("billing").unwrap();
    let backup = RepoId::new("backup").unwrap();
    let orders = RepoId::new("orders").unwrap();
    for (repo, nodes) in [
        (billing.clone(), vec![consumer]),
        (
            backup.clone(),
            vec![make_provider("backup", "orders.created", 30)],
        ),
        (
            orders.clone(),
            vec![make_provider("orders", "audit.logged", 31)],
        ),
    ] {
        fixture
            .fed
            .get_repo(&repo)
            .unwrap()
            .db()
            .insert_nodes_batch(&nodes)
            .unwrap();
        for node in nodes {
            fixture
                .fed
                .register_contract_node_for_test(repo.clone(), node.id);
        }
        fixture.fed.project_repo(&repo).await.unwrap();
    }
    fixture.fed.mark_contracts_dirty();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let site = GlobalId::new(
        &billing,
        NodeType::Function,
        "src/client.py",
        "subscribe_both",
        Some(22),
    );
    let index = fixture.fed.contract_index().unwrap();
    let resolutions = index.consumers.get(&site).cloned().unwrap_or_else(|| {
        panic!(
            "consumer {site:?} missing; indexed consumers: {:?}",
            index.consumers.keys()
        )
    });
    assert_eq!(resolutions.len(), 2);
    assert!(resolutions[0]
        .bound_endpoints
        .iter()
        .any(|(_, key)| key.to_string() == "topic:kafka/orders.created"));
    assert!(matches!(
        resolutions[1].target,
        Some(ConsumerTarget::Binds { .. })
    ));

    let mut mapping = binding();
    mapping["consumer"] = json!({
        "repo": "billing", "path": "src/client.py", "symbol": "subscribe_both",
        "key": "topic:kafka/audit.logged"
    });
    mapping["provider"] = json!({
        "service": "orders", "key": "topic:kafka/audit.logged"
    });
    let output = apply(&fixture.fed, json!([mapping]), false).await;
    assert_eq!(output["data"]["applied"], true, "{output}");
    let resolutions = fixture.fed.contract_index().unwrap().consumers[&site].clone();
    assert!(resolutions.iter().any(|resolution| {
        matches!(
            resolution.target,
            Some(ConsumerTarget::Binds {
                provenance: EdgeProvenance::Confirmed { .. },
                ..
            })
        ) && resolution.bound_endpoints.iter().any(|(service, key)| {
            service.0 == "orders" && key.to_string() == "topic:kafka/audit.logged"
        })
    }));
}

#[tokio::test]
async fn invalid_rpc_mappings_never_write() {
    let fixture = Fixture::new().await;
    let original = fixture.bytes();
    let mut wrong_rpc = binding();
    wrong_rpc["consumer"]["key"] = json!("rpc:orders.v1/Delete");
    let mut missing_consumer = binding();
    missing_consumer["consumer"]["symbol"] = json!("deleted_function");
    let mut missing_provider = binding();
    missing_provider["consumer"]["key"] = json!("rpc:orders.v1/Missing");
    missing_provider["provider"]["key"] = json!("rpc:orders.v1/Missing");
    let mut same_service = binding();
    same_service["provider"]["service"] = json!("billing");
    for (invalid, reason) in [
        (wrong_rpc, "incompatible_keys"),
        (missing_consumer, "missing_consumer"),
        (missing_provider, "missing_provider"),
        (same_service, "same_service"),
    ] {
        let output = apply(&fixture.fed, json!([invalid]), false).await;
        assert_eq!(output["error"]["details"]["reason"], reason, "{output}");
        assert_eq!(
            fixture.bytes(),
            original,
            "rejected input must not alter repos.yaml"
        );
    }
    let status = HandlerStatus::for_test();
    let snapshot = apply_contract_context_handle(
        &context(&fixture.fed, &status),
        json!({"snapshot": "snap_old", "dry_run": false, "bindings": [binding()]}),
    )
    .await
    .unwrap();
    assert!(snapshot.is_error);
    assert_eq!(fixture.bytes(), original);
}

#[tokio::test]
async fn conflicting_destination_is_an_error_without_replacement() {
    let fixture = Fixture::new().await;
    assert_eq!(
        apply(&fixture.fed, json!([binding()]), false).await["data"]["applied"],
        true
    );
    let original = fixture.bytes();
    let mut conflict = binding();
    conflict["provider"]["service"] = json!("backup");
    let refused = apply(&fixture.fed, json!([conflict]), false).await;
    assert!(refused["error"].is_object(), "{refused}");
    assert_eq!(fixture.bytes(), original);
    assert_confirmed_via_mcp(&fixture.fed, &fixture.consumer).await;
}

#[tokio::test]
async fn successful_check_does_not_authorize_a_deleted_consumer() {
    let fixture = Fixture::new().await;
    let status = HandlerStatus::for_test();
    let check = check_binding_handle(
        &context(&fixture.fed, &status),
        json!({
            "snapshot": "live", "consumer": fixture.consumer,
            "endpoint": {"service": "orders", "key": KEY}
        }),
    )
    .await
    .unwrap();
    assert_eq!(check.structured["data"]["valid"], true);
    fixture
        .fed
        .get_repo(&RepoId::new("billing").unwrap())
        .unwrap()
        .db()
        .remove_nodes_by_ids(std::slice::from_ref(&fixture.consumer))
        .unwrap();
    fixture
        .fed
        .project_repo(&RepoId::new("billing").unwrap())
        .await
        .unwrap();
    fixture.fed.mark_contracts_dirty();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let original = fixture.bytes();
    let stale = apply(&fixture.fed, json!([binding()]), false).await;
    assert!(stale["error"].is_object(), "{stale}");
    assert_eq!(fixture.bytes(), original);
}

#[tokio::test]
async fn legacy_selector_fanout_is_refused() {
    let fixture = Fixture::new().await;
    let other = rpc_consumer(20);
    let id = RepoId::new("billing").unwrap();
    fixture
        .fed
        .get_repo(&id)
        .unwrap()
        .db()
        .insert_nodes_batch(std::slice::from_ref(&other))
        .unwrap();
    fixture
        .fed
        .register_contract_node_for_test(id.clone(), other.id);
    fixture.fed.project_repo(&id).await.unwrap();
    fixture.fed.mark_contracts_dirty();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let original = fixture.bytes();
    let output = apply(&fixture.fed, json!([binding()]), false).await;
    assert!(output["error"].is_object(), "{output}");
    assert_eq!(output["error"]["code"], "ambiguous_binding", "{output}");
    assert_eq!(
        output["error"]["details"]["matched_consumers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(fixture.bytes(), original);
}

#[tokio::test]
async fn existing_binding_not_yet_loaded_is_rejoined_before_success() {
    let fixture = Fixture::new().await;
    let mut doc: serde_yaml::Value = serde_yaml::from_slice(&fixture.bytes()).unwrap();
    doc["bindings"] = serde_yaml::to_value(vec![binding()]).unwrap();
    std::fs::write(&fixture.path, serde_yaml::to_string(&doc).unwrap()).unwrap();
    let output = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(output["data"]["applied"], true, "{output}");
    assert_confirmed_via_mcp(&fixture.fed, &fixture.consumer).await;
}

#[tokio::test]
async fn failed_rejoin_restores_config_and_allows_a_consistent_retry() {
    let fixture = Fixture::new().await;
    let original = fixture.bytes();
    let hash = fixture.fed.contract_config().unwrap().config_hash();
    // A directory at the graph checkpoint destination makes atomic persistence
    // fail deterministically, including when this test runs as root.
    let graph_path = fixture.root.path().join("data/federated_graph.bin");
    let saved_graph = graph_path.with_extension("saved");
    std::fs::rename(&graph_path, &saved_graph).unwrap();
    std::fs::create_dir(&graph_path).unwrap();
    let output = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(output["error"]["code"], "rejoin_failed", "{output}");
    assert_eq!(output["error"]["details"]["restored"], true, "{output}");
    assert_eq!(fixture.bytes(), original);
    assert_eq!(fixture.fed.contract_config().unwrap().config_hash(), hash);
    assert!(matches!(
        fixture
            .fed
            .contract_index()
            .unwrap()
            .consumer(&GlobalId::from_canonical(&fixture.consumer))
            .unwrap()
            .target,
        Some(ConsumerTarget::Unresolved { .. })
    ));
    std::fs::remove_dir(&graph_path).unwrap();
    std::fs::rename(&saved_graph, &graph_path).unwrap();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let retried = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(retried["data"]["applied"], true, "{retried}");
    assert_confirmed_via_mcp(&fixture.fed, &fixture.consumer).await;
}

#[path = "../support/contracts_snapshot_harness.rs"]
mod snapshot_harness;

#[tokio::test]
async fn binding_config_requires_repreparing_both_sides_of_a_snapshot_diff() {
    use lain::federation::config::{FederationConfig, RepoConfig, SourceConfig};
    use lain::federation::contracts::index_cache::IndexCache;
    use lain::federation::contracts::snapshots::manager::SnapshotManager;
    use lain::federation::contracts::snapshots::snapshot_record_path;
    use lain::server::mcp::contract_tools::analysis::diff_contracts_handle;
    use std::collections::BTreeMap;

    let fixture = Fixture::new().await;
    let data_dir = fixture.root.path().join("snapshots-data");
    let sources = FederationConfig {
        data_dir: data_dir.clone(),
        repos: ["orders", "backup", "billing"]
            .iter()
            .map(|id| RepoConfig {
                id: (*id).into(),
                packages: Vec::new(),
                source: SourceConfig::WorkspaceDir {
                    path: fixture.root.path().join(id),
                },
            })
            .collect(),
        ..Default::default()
    };
    let manager = SnapshotManager::with_cap(&data_dir, IndexCache::new(&data_dir), 2);
    manager.set_repo_source_resolver(SnapshotManager::resolver_from_config(&sources));
    let mut base_refs = BTreeMap::new();
    for repo in ["orders", "backup", "billing"] {
        let git = git2::Repository::open(fixture.root.path().join(repo)).unwrap();
        base_refs.insert(
            repo.to_string(),
            git.head().unwrap().target().unwrap().to_string(),
        );
    }
    let old_config = fixture.fed.contract_config().unwrap();
    let old_base =
        snapshot_harness::prepare_ready(&manager, base_refs.clone(), None, old_config).await;
    let old_record_path = snapshot_record_path(manager.data_dir(), &old_base);
    let old_record_bytes = std::fs::read(&old_record_path).unwrap();

    let applied = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(applied["data"]["applied"], true, "{applied}");
    let new_config = fixture.fed.contract_config().unwrap();
    let git = git2::Repository::open(fixture.root.path().join("billing")).unwrap();
    let mut index = git.index().unwrap();
    index.add_path(Path::new("src/client.py")).unwrap();
    let tree_id = index.write_tree().unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    let parent = git.head().unwrap().peel_to_commit().unwrap();
    let commit = git
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "head",
            &git.find_tree(tree_id).unwrap(),
            &[&parent],
        )
        .unwrap();
    let mut head_refs = base_refs.clone();
    head_refs.insert("billing".into(), commit.to_string());
    let new_head =
        snapshot_harness::prepare_ready(&manager, head_refs, None, new_config.clone()).await;
    let status = HandlerStatus::for_test();
    let ctx = snapshot_harness::snapshot_ctx(&manager, &status);
    let mismatch = diff_contracts_handle(&ctx, json!({"base": old_base, "head": new_head}))
        .await
        .unwrap();
    assert!(mismatch.is_error, "{:?}", mismatch.structured);
    assert_eq!(
        mismatch.structured["error"]["details"]["reason"],
        "config_mismatch"
    );
    let new_base = snapshot_harness::prepare_ready(&manager, base_refs, None, new_config).await;
    assert_ne!(old_base, new_base);
    let matching = diff_contracts_handle(&ctx, json!({"base": new_base, "head": new_head}))
        .await
        .unwrap();
    assert!(!matching.is_error, "{:?}", matching.structured);
    let before: serde_json::Value = serde_json::from_slice(&old_record_bytes).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(old_record_path).unwrap()).unwrap();
    assert_eq!(before["config_hash"], after["config_hash"]);
    assert_eq!(before["join_config"], after["join_config"]);
    assert_eq!(before["repos"], after["repos"]);
    assert_eq!(before["state"], after["state"]);
}

#[tokio::test]
async fn successful_check_does_not_authorize_a_deleted_provider() {
    let fixture = Fixture::new().await;
    let status = HandlerStatus::for_test();
    let check = check_binding_handle(
        &context(&fixture.fed, &status),
        json!({
            "snapshot": "live", "consumer": fixture.consumer,
            "endpoint": {"service": "orders", "key": KEY}
        }),
    )
    .await
    .unwrap();
    assert_eq!(check.structured["data"]["valid"], true);
    let provider = rpc_provider("orders");
    let repo = RepoId::new("orders").unwrap();
    fixture
        .fed
        .get_repo(&repo)
        .unwrap()
        .db()
        .remove_nodes_by_ids(&[provider.id])
        .unwrap();
    fixture.fed.project_repo(&repo).await.unwrap();
    fixture.fed.mark_contracts_dirty();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let original = fixture.bytes();
    let output = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(
        output["error"]["details"]["reason"], "missing_provider",
        "{output}"
    );
    assert_eq!(fixture.bytes(), original);
}

#[tokio::test]
async fn existing_symbol_cannot_be_mapped_with_a_different_rpc_fact() {
    let fixture = Fixture::new().await;
    let provider = fact_node(
        "orders",
        "orders.v1.Delete",
        30,
        ContractFact::RpcProvider(RpcProviderFact {
            system: RpcSystem::Grpc,
            service: "orders.v1".into(),
            method: "Delete".into(),
            request_type: "DeleteRequest".into(),
            response_type: "DeleteResponse".into(),
            handler: None,
        }),
    );
    let repo = RepoId::new("orders").unwrap();
    fixture
        .fed
        .get_repo(&repo)
        .unwrap()
        .db()
        .insert_nodes_batch(std::slice::from_ref(&provider))
        .unwrap();
    fixture
        .fed
        .register_contract_node_for_test(repo.clone(), provider.id);
    fixture.fed.project_repo(&repo).await.unwrap();
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let mut incorrect = binding();
    incorrect["consumer"]["key"] = json!("rpc:orders.v1/Delete");
    incorrect["provider"]["key"] = json!("rpc:orders.v1/Delete");
    let original = fixture.bytes();
    let refused = apply(&fixture.fed, json!([incorrect]), false).await;
    assert_eq!(
        refused["error"]["details"]["reason"], "missing_consumer",
        "{refused}"
    );
    assert_eq!(fixture.bytes(), original);
}

#[tokio::test]
async fn rejoin_revalidates_consumer_service_after_loading_candidate_config() {
    let fixture = Fixture::new().await;
    // Use a service alias so moving its ownership doesn't collide with a
    // configured repo ID and get rejected before the intended rejoin check.
    let mut live: serde_yaml::Value = serde_yaml::from_slice(&fixture.bytes()).unwrap();
    live["services"][0]["name"] = serde_yaml::Value::String("api".into());
    live["provides"][0]["service"] = serde_yaml::Value::String("api".into());
    live["host_aliases"][0]["service"] = serde_yaml::Value::String("api".into());
    let live_yaml = serde_yaml::to_string(&live).unwrap();
    let live_config = ContractFederationConfig::load_from_str(&live_yaml).unwrap();
    let repos: Vec<String> = ["orders", "backup", "billing"]
        .iter()
        .map(|id| (*id).into())
        .collect();
    live_config.validate(&repos).unwrap();
    std::fs::write(&fixture.path, &live_yaml).unwrap();
    fixture.fed.set_contract_config(live_config);
    fixture.fed.rejoin_contracts_if_dirty().unwrap();
    let initial_hash = fixture.fed.contract_config().unwrap().config_hash();
    // Disk moves api to the consumer's repo while the current index still
    // calls this consumer billing. The candidate keeps a real api RPC provider.
    let mut edited = live;
    edited["services"] =
        serde_yaml::from_str("- {name: api, repo: billing}\n- {name: backup, repo: backup}\n")
            .unwrap();
    let edited = serde_yaml::to_string(&edited).unwrap();
    ContractFederationConfig::load_from_str(&edited)
        .unwrap()
        .validate(&repos)
        .unwrap();
    std::fs::write(&fixture.path, &edited).unwrap();
    let mut mapping = binding();
    mapping["provider"]["service"] = json!("api");
    let output = apply(&fixture.fed, json!([mapping]), false).await;
    assert_eq!(output["error"]["code"], "rejoin_failed", "{output}");
    assert_eq!(output["error"]["details"]["restored"], true, "{output}");
    assert_eq!(fixture.bytes(), edited.as_bytes());
    assert_eq!(
        fixture.fed.contract_config().unwrap().config_hash(),
        initial_hash
    );
    assert!(matches!(
        fixture
            .fed
            .contract_index()
            .unwrap()
            .consumer(&GlobalId::from_canonical(&fixture.consumer))
            .unwrap()
            .target,
        Some(ConsumerTarget::Unresolved { .. })
    ));
}

#[tokio::test]
async fn a_held_config_transaction_refuses_another_apply() {
    let fixture = Fixture::new().await;
    let original = fixture.bytes();
    let transaction =
        lain::federation::contracts::context::acquire_transaction(&fixture.path).unwrap();
    let output = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(output["error"]["code"], "conflict", "{output}");
    assert_eq!(fixture.bytes(), original);
    assert!(!lain::federation::contracts::context::backup_path(&fixture.path).exists());
    drop(transaction);
    let retried = apply(&fixture.fed, json!([binding()]), false).await;
    assert_eq!(retried["data"]["applied"], true, "{retried}");
}
