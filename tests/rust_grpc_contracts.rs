//! Rust identity is proved against providers across repos; destinations stay unknown.
use lain::federation::{
    config::{FederationConfig, RepoConfig, SourceConfig},
    contracts::{
        config::{ContractFederationConfig, ServiceDecl},
        index::ContractIndex,
        index_cache::IndexCache,
        model::{ContractFact, HostPart},
        snapshots::{
            manager::{PrepareRequest, SnapshotManager},
            SnapshotState,
        },
    },
    federated_index::FederatedIndex,
    graph_backend::PetgraphBackend,
    repo_id::RepoId,
    repo_source::{RepoSource, WorkspaceDirSource},
};
use lain::{
    git::{AnyGitSensor, GitSensorMode},
    schema::{EdgeType, RepoNamespace},
    server::{
        ingest::ingestion::{index_one_repo, IndexMode, IndexRequest},
        mcp::{
            contract_tools::{
                analysis::{get_coverage_handle, trace_impact_handle},
                context::apply_contract_context_handle,
                contracts::{check_binding_handle, get_contract_handle, list_unresolved_handle},
                evidence::read_source_handle,
            },
            handler::{HandlerStatus, McpContext},
        },
    },
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, process::Command, sync::Arc};

const RUST_PATH: &str = "environment-providers/openshell/src/lib.rs";
const KEY_CREATE: &str = "rpc:openshell.v1.OpenShell/CreateSandbox";
const KEY_GET: &str = "rpc:openshell.v1.OpenShell/GetSandbox";
fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn commit(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "fixture",
        ],
    );
}
fn commit_changes(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            message,
        ],
    );
}
fn service(name: &str, repo: &str, paths: &[&str]) -> ServiceDecl {
    ServiceDecl {
        name: name.into(),
        repo: repo.into(),
        paths: paths.iter().map(|s| s.to_string()).collect(),
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
        provides_graphql: true,
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    config: ContractFederationConfig,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        write(
            &root.join("openshell"),
            "proto/openshell.proto",
            include_str!("fixtures/rust_grpc/openshell.proto"),
        );
        write(
            &root.join("openshell"),
            "crates/openshell-sdk/src/raw.rs",
            include_str!("fixtures/rust_grpc/openshell_raw.rs"),
        );
        write(
            &root.join("nemo"),
            RUST_PATH,
            include_str!("fixtures/rust_grpc/nemo.rs"),
        );
        write(
            &root.join("nemo"),
            ".lain/patterns/rust-grpc.yaml",
            include_str!("fixtures/rust_grpc/rust-grpc.yaml"),
        );
        commit(&root.join("openshell"));
        commit(&root.join("nemo"));
        let config = ContractFederationConfig {
            services: vec![
                service(
                    "gateway",
                    "openshell",
                    &["proto", "crates/openshell-server"],
                ),
                service("sdk", "openshell", &["crates/openshell-sdk"]),
                service("fabric", "nemo", &["environment-providers/openshell"]),
            ],
            ..Default::default()
        };
        Self {
            _temp: temp,
            root,
            config,
        }
    }
    async fn live(&self) -> Arc<FederatedIndex> {
        let data = self.root.join("live-data");
        std::fs::create_dir_all(&data).unwrap();
        let fed = Arc::new(FederatedIndex::new(Arc::new(PetgraphBackend::ephemeral(
            &data,
        ))));
        fed.set_contract_config(self.config.clone());
        for name in ["openshell", "nemo"] {
            let rid = RepoId::new(name).unwrap();
            let source: Box<dyn RepoSource> =
                Box::new(WorkspaceDirSource::new(rid.clone(), self.root.join(name)).unwrap());
            source.fetch().await.unwrap();
            fed.add_repo(source, &data).await.unwrap();
            self.index(&fed, name, IndexMode::Live).await;
        }
        fed.rejoin_contracts_if_dirty().unwrap();
        fed
    }
    async fn index(&self, fed: &FederatedIndex, name: &str, mode: IndexMode) {
        let rid = RepoId::new(name).unwrap();
        let repo = fed.get_repo(&rid).unwrap();
        let git = AnyGitSensor::new(repo.local_path(), GitSensorMode::InProcess).unwrap();
        let namespace = RepoNamespace::for_test();
        let cancel = tokio_util::sync::CancellationToken::new();
        index_one_repo(IndexRequest {
            path: repo.local_path(),
            graph: repo.db(),
            lsp_pool: None,
            git: &git,
            overlay: None,
            resolver: None,
            source_repo: None,
            namespace: &namespace,
            force: true,
            cancel: &cancel,
            mode,
        })
        .await
        .unwrap();
        fed.project_repo(&rid).await.unwrap();
    }
    fn manager(&self) -> Arc<SnapshotManager> {
        let cfg = FederationConfig {
            data_dir: self.root.join("snap-data"),
            repos: ["openshell", "nemo"]
                .into_iter()
                .map(|name| RepoConfig {
                    id: name.into(),
                    packages: vec![],
                    source: SourceConfig::WorkspaceDir {
                        path: self.root.join(name),
                    },
                })
                .collect(),
            ..Default::default()
        };
        let mgr = SnapshotManager::with_cap(&cfg.data_dir, IndexCache::new(&cfg.data_dir), 2);
        mgr.set_repo_source_resolver(SnapshotManager::resolver_from_config(&cfg));
        mgr
    }
    async fn snapshot(&self, mgr: &Arc<SnapshotManager>) -> String {
        let repos = ["openshell", "nemo"]
            .into_iter()
            .map(|r| (r.into(), git(&self.root.join(r), &["rev-parse", "HEAD"])))
            .collect();
        let out = mgr
            .prepare(PrepareRequest {
                refs_: BTreeMap::new(),
                repos,
                excluded: vec![],
                from: None,
                max_base_age_s: None,
                wait_ms: 60_000,
                config: Arc::new(self.config.clone()),
            })
            .await
            .unwrap();
        let id = out.record.id;
        let record = mgr.get(&id, 60_000).await.unwrap().record;
        assert_eq!(
            record.state,
            SnapshotState::Ready,
            "{:?}",
            record.repo_states
        );
        id
    }
}
async fn call(
    fed: Option<&FederatedIndex>,
    mgr: Option<&Arc<SnapshotManager>>,
    tool: &str,
    args: Value,
) -> Value {
    let status = HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: fed,
        workspaces: None,
        status: &status,
        reload_bus: None,
        snapshots: mgr,
    };
    let out = match tool {
        "list_unresolved" => list_unresolved_handle(&ctx, args).await,
        "check_binding" => check_binding_handle(&ctx, args).await,
        "get_coverage" => get_coverage_handle(&ctx, args).await,
        "read_source" => read_source_handle(&ctx, args).await,
        "apply_contract_context" => apply_contract_context_handle(&ctx, args).await,
        "get_contract" => get_contract_handle(&ctx, args).await,
        "trace_impact" => trace_impact_handle(&ctx, args).await,
        _ => unreachable!(),
    }
    .unwrap();
    assert!(!out.is_error, "{}", out.structured);
    out.structured
}
fn rust_items(value: &Value) -> Vec<Value> {
    value["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v.get("rpc").is_some())
        .cloned()
        .collect()
}
fn assert_identities(index: &ContractIndex) {
    let keys: Vec<_> = index
        .rust_rpcs
        .values()
        .filter_map(|d| d.contract_key.as_deref())
        .collect();
    assert_eq!(keys.len(), 2, "{:#?}", index.rust_rpcs);
    assert!(keys.contains(&KEY_CREATE));
    assert!(keys.contains(&KEY_GET));
    for detail in index.rust_rpcs.values() {
        assert!(detail.enclosing_symbol.is_some(), "{detail:?}");
        assert!(!detail.provider_ids.is_empty());
    }
}
fn validate_unresolved(value: &Value) {
    let schema: Value = serde_json::from_str(include_str!(
        "../src/server/mcp/contract_tools/schemas/list_unresolved.out.json"
    ))
    .unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(&schema)
        .unwrap();
    assert!(
        validator.is_valid(&value["data"]),
        "schema mismatch: {}",
        value["data"]
    );
}

#[tokio::test]
async fn pinned_cross_repo_identities_are_actionable_without_inventing_destination() {
    let fixture = Fixture::new();
    let fed = fixture.live().await;
    let index = fed.contract_index().unwrap();
    assert_identities(&index);
    let nodes = fed.backend().list_nodes().unwrap();
    for node in nodes
        .iter()
        .filter(|n| n.name.starts_with("rpc-call:rust:"))
    {
        let [ContractFact::RpcConsumer(rpc)] = node.contract.as_slice() else {
            panic!("{node:?}");
        };
        assert_eq!(rpc.channel_host_part, HostPart::None);
        assert!(rpc.channel_target.is_none());
        assert!(node.line_end.is_none());
    }
    let result = call(
        Some(&fed),
        None,
        "list_unresolved",
        json!({"snapshot":"live"}),
    )
    .await;
    validate_unresolved(&result);
    let items = rust_items(&result);
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(item["reason"], "rpc_stub_unknown");
        assert_eq!(item["target_service"], Value::Null);
        assert_eq!(item["rpc"]["status"], "resolved");
        assert_eq!(item["consumer"]["path"], RUST_PATH);
        assert!(item["consumer"]["line"] == 46 || item["consumer"]["line"] == 69);
        assert_eq!(item["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(item["candidates"][0]["endpoint"]["service"], "gateway");
        let key = item["rpc"]["contract_key"].as_str().unwrap();
        let consumer = item["consumer"]["id"].as_str().unwrap();
        let valid = call(
            Some(&fed),
            None,
            "check_binding",
            json!({"consumer": consumer,"endpoint":{"service":"gateway","key":key}}),
        )
        .await;
        assert_eq!(valid["data"]["valid"], true, "{valid}");
        let symbol = if key == KEY_CREATE { "create" } else { "get" };
        assert!(valid["data"]["bindings_entry"]
            .as_str()
            .unwrap()
            .contains(&format!("symbol: {symbol}\n")));
        let wrong = if key == KEY_CREATE {
            KEY_GET
        } else {
            KEY_CREATE
        };
        let invalid = call(
            Some(&fed),
            None,
            "check_binding",
            json!({"consumer": consumer,"endpoint":{"service":"gateway","key":wrong}}),
        )
        .await;
        assert_eq!(invalid["data"]["valid"], false);
    }
    // A provider catalog change re-resolves identity; it must not reuse the
    // previous derived fact just because the consumer source did not change.
    write(&fixture.root.join("openshell"), "proto/duplicate.proto", "syntax = \"proto3\"; package other.v1; service OpenShell { rpc CreateSandbox(Req) returns (Res); }");
    commit_changes(&fixture.root.join("openshell"), "add duplicate provider");
    fixture.index(&fed, "openshell", IndexMode::Live).await;
    fed.rejoin_contracts_if_dirty().unwrap();
    let result = call(Some(&fed), None, "list_unresolved", json!({})).await;
    let items = rust_items(&result);
    let ambiguous = items
        .iter()
        .find(|i| i["method"] == "create_sandbox")
        .unwrap();
    assert_eq!(ambiguous["rpc"]["status"], "ambiguous_identity");
    assert!(ambiguous["rpc"]["contract_key"].is_null());
    assert_eq!(ambiguous["candidates"].as_array().unwrap().len(), 2);
    let node = fed
        .backend()
        .get_node(ambiguous["consumer"]["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(
        node.contract.is_empty(),
        "stale identity fact survived catalog ambiguity"
    );
    std::fs::remove_file(fixture.root.join("nemo").join(RUST_PATH)).unwrap();
    fixture.index(&fed, "nemo", IndexMode::Live).await;
    fed.rejoin_contracts_if_dirty().unwrap();
    assert!(fed.contract_index().unwrap().rust_rpcs.is_empty());
    assert!(fed
        .backend()
        .list_nodes()
        .unwrap()
        .iter()
        .all(|n| !n.name.starts_with("rpc-call:rust:")));
    assert!(fed
        .backend()
        .all_edges()
        .unwrap()
        .iter()
        .all(|e| !e.target_id.contains("rpc-call:rust:")));
}

#[tokio::test]
async fn snapshot_rebuilds_from_pinned_blobs_and_missing_evidence_is_incomplete() {
    let fixture = Fixture::new();
    let mgr = fixture.manager();
    let snapshot = fixture.snapshot(&mgr).await;
    // Prove assembly reads the pinned commit, not the mutable live checkout.
    std::fs::remove_file(fixture.root.join("nemo").join(RUST_PATH)).unwrap();
    std::fs::remove_file(fixture.root.join("nemo/.lain/patterns/rust-grpc.yaml")).unwrap();
    let result = call(
        None,
        Some(&mgr),
        "list_unresolved",
        json!({"snapshot":snapshot}),
    )
    .await;
    validate_unresolved(&result);
    assert_eq!(rust_items(&result).len(), 2, "{result}");
    let record = mgr.get(&snapshot, 5000).await.unwrap().record;
    let (snap, _hold) = mgr.from_snapshot(&record).unwrap();
    assert_identities(snap.contract_index.read().as_ref().unwrap());
    let item = rust_items(&result).remove(0);
    let valid = call(None, Some(&mgr), "check_binding", json!({"snapshot": snapshot,"consumer":item["consumer"]["id"],"endpoint":item["candidates"][0]["endpoint"]})).await;
    assert_eq!(valid["data"]["valid"], true);
    let line = item["consumer"]["line"].as_u64().unwrap();
    let source = call(
        None,
        Some(&mgr),
        "read_source",
        json!({"snapshot":snapshot,"repo":"nemo","path":RUST_PATH,"start":line-2,"end":line}),
    )
    .await;
    assert!(source["data"]["text"]
        .as_str()
        .unwrap()
        .contains("raw_grpc"));
    let coverage = call(
        None,
        Some(&mgr),
        "get_coverage",
        json!({"snapshot":snapshot}),
    )
    .await;
    assert_eq!(coverage["data"]["complete"], false, "{coverage}");
    // A new snapshot without its mapping must remain incomplete, rather than
    // treating absent detected facts as proof that there are no Rust RPCs.
    write(
        &fixture.root.join("nemo"),
        RUST_PATH,
        include_str!("fixtures/rust_grpc/nemo.rs"),
    );
    git(&fixture.root.join("nemo"), &["add", "-A"]);
    git(
        &fixture.root.join("nemo"),
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "remove mapping",
        ],
    );
    let missing = fixture.snapshot(&mgr).await;
    let coverage = call(
        None,
        Some(&mgr),
        "get_coverage",
        json!({"snapshot":missing}),
    )
    .await;
    assert_eq!(coverage["data"]["complete"], false);
}

#[tokio::test]
async fn same_line_clients_have_caller_paths_and_same_service_validation_is_rejected() {
    let fixture = Fixture::new();
    let sdk = fixture.root.join("openshell");
    let local_path = "crates/openshell-sdk/src/calls.rs";
    write(
        &sdk,
        local_path,
        "use openshell_core::proto::open_shell_client::OpenShellClient as C;\nfn first() { let a = C::new(ch); let b = C::new(ch); a.get_sandbox(req); b.create_sandbox(req); }\nfn entry() { first(); }\n",
    );
    write(
        &sdk,
        "crates/openshell-sdk/internal.proto",
        include_str!("fixtures/rust_grpc/openshell.proto"),
    );
    commit_changes(&sdk, "add same-line calls and same-service proto");
    let fed = fixture.live().await;
    let backend = fed.backend();
    let nodes = backend.list_nodes().unwrap();
    let calls: Vec<_> = nodes
        .iter()
        .filter(|n| n.path == local_path && n.name.starts_with("rpc-call:rust:"))
        .collect();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].id, calls[1].id);
    let first = nodes
        .iter()
        .find(|n| n.path == local_path && n.name == "first")
        .unwrap();
    let entry = nodes
        .iter()
        .find(|n| n.path == local_path && n.name == "entry")
        .unwrap();
    let edges = backend.all_edges().unwrap();
    for call_node in &calls {
        assert_eq!(call_node.container.as_deref(), Some("first"));
        assert!(edges.iter().any(|e| e.edge_type == EdgeType::Calls
            && e.source_id == first.id
            && e.target_id == call_node.id));
        let key = call_node
            .contract
            .iter()
            .flat_map(ContractFact::consumer_keys)
            .next()
            .unwrap()
            .to_string();
        let result = call(
            Some(&fed),
            None,
            "check_binding",
            json!({"consumer":call_node.id,"endpoint":{"service":"sdk","key":key}}),
        )
        .await;
        assert_eq!(result["data"]["valid"], false);
        assert!(result["data"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("same_service")));
    }
    assert!(
        edges.iter().any(|e| e.edge_type == EdgeType::Calls
            && e.source_id == entry.id
            && e.target_id == first.id),
        "multiline existing call resolution did not reach the RPC enclosing function: {edges:#?}"
    );
    let initial_edges = edges
        .iter()
        .filter(|e| e.target_id == calls[0].id || e.target_id == calls[1].id)
        .count();
    fixture.index(&fed, "openshell", IndexMode::Live).await;
    fed.rejoin_contracts_if_dirty().unwrap();
    assert_eq!(
        backend
            .all_edges()
            .unwrap()
            .iter()
            .filter(|e| e.target_id == calls[0].id || e.target_id == calls[1].id)
            .count(),
        initial_edges,
        "rescan duplicated edges"
    );
}

#[tokio::test]
async fn normalization_collisions_are_unproven_and_retract_when_source_changes() {
    let fixture = Fixture::new();
    write(&fixture.root.join("openshell"), "proto/collision.proto", "syntax = \"proto3\"; package example; service URLService { rpc GetURL(Req) returns (Res); rpc GetUrl(Req) returns (Res); }");
    let path = "environment-providers/openshell/src/collision.rs";
    write(
        &fixture.root.join("nemo"),
        path,
        "fn run(c: api::url_service_client::UrlServiceClient<Ch>) { c.get_url(req); }",
    );
    commit_changes(
        &fixture.root.join("openshell"),
        "add normalization collision provider",
    );
    commit_changes(
        &fixture.root.join("nemo"),
        "add normalization collision consumer",
    );
    let fed = fixture.live().await;
    let index = fed.contract_index().unwrap();
    let (id, detail) = index
        .rust_rpcs
        .iter()
        .find(|(id, _)| id.path().as_deref() == Some(path))
        .unwrap();
    assert_eq!(detail.status, "ambiguous_identity");
    assert_eq!(detail.compatible_keys.len(), 2);
    assert!(index.unnormalized.contains(id));
    let node = fed.backend().get_node(id.as_str()).unwrap().unwrap();
    assert!(node.contract.is_empty());
    write(
        &fixture.root.join("nemo"),
        path,
        "fn run(c: HttpClient) { c.get_url(req); }",
    );
    fixture.index(&fed, "nemo", IndexMode::Live).await;
    fed.rejoin_contracts_if_dirty().unwrap();
    assert!(fed.backend().get_node(id.as_str()).unwrap().is_none());
}

#[tokio::test]
async fn generic_receiver_shadowing_never_emits_an_rpc_fact() {
    let fixture = Fixture::new();
    let path = "environment-providers/openshell/src/generic.rs";
    write(&fixture.root.join("nemo"), path,
        "use openshell_core::proto::open_shell_client::OpenShellClient as C; fn f<C: OtherClient>(c: C) { c.get_sandbox(req); }");
    commit_changes(&fixture.root.join("nemo"), "add generic receiver shadow");
    let fed = fixture.live().await;
    assert!(fed
        .contract_index()
        .unwrap()
        .rust_rpcs
        .keys()
        .all(|id| id.path().as_deref() != Some(path)));
    assert!(fed
        .backend()
        .list_nodes()
        .unwrap()
        .iter()
        .filter(|node| node.path == path)
        .flat_map(|node| &node.contract)
        .all(|fact| !matches!(fact, ContractFact::RpcConsumer(_))));
}

#[test]
fn proto_word_boundaries_preserve_acronyms_and_digit_runs() {
    use lain::server::sensors::util::rust_proto_snake_case as snake;
    for (input, expected) in [
        ("CreateSandbox", "create_sandbox"),
        ("GetURL", "get_url"),
        ("URLService", "url_service"),
        ("GetHTTP2API", "get_http2api"),
        ("HTTP2Api", "http2_api"),
        ("get__url", "get_url"),
    ] {
        assert_eq!(snake(input), expected, "{input}");
    }
}

#[tokio::test]
async fn inferred_rpc_confirmation_persists_and_rebuilds_without_lsp() {
    let mut fixture = Fixture::new();
    let fed = fixture.live().await;
    let path = fixture.root.join("repos.yaml");
    let document = json!({
        "data_dir": fixture.root.join("reloaded-data"),
        "repos": [
            {"id": "openshell", "source": {"type": "workspace_dir", "path": fixture.root.join("openshell")}},
            {"id": "nemo", "source": {"type": "workspace_dir", "path": fixture.root.join("nemo")}}
        ],
        "contract": fixture.config,
        "operator_note": "preserve-this"
    });
    std::fs::write(&path, serde_yaml::to_string(&document).unwrap()).unwrap();
    fed.set_config_path(path.clone());
    let initial = std::fs::read(&path).unwrap();
    let initial_hash = fixture.config.config_hash();
    let manager = fixture.manager();
    let old_snapshot = fixture.snapshot(&manager).await;
    let unresolved = call(
        Some(&fed),
        None,
        "list_unresolved",
        json!({"snapshot": "live"}),
    )
    .await;
    let item = rust_items(&unresolved)
        .into_iter()
        .find(|i| i["rpc"]["contract_key"] == KEY_CREATE)
        .unwrap();
    assert_eq!(item["reason"], "rpc_stub_unknown");
    let consumer = item["consumer"]["id"].as_str().unwrap().to_string();
    let enclosing = fed
        .contract_index()
        .unwrap()
        .rust_rpcs
        .iter()
        .find(|(id, _)| id.as_str() == consumer)
        .unwrap()
        .1
        .enclosing_symbol
        .clone()
        .unwrap();
    let checked = call(
        Some(&fed),
        None,
        "check_binding",
        json!({
            "snapshot": "live", "consumer": consumer,
            "endpoint": {"service": "gateway", "key": KEY_CREATE}
        }),
    )
    .await;
    assert_eq!(checked["data"]["valid"], true, "{checked}");
    let bindings: Vec<lain::federation::contracts::config::ConfirmedBinding> =
        serde_yaml::from_str(checked["data"]["bindings_entry"].as_str().unwrap()).unwrap();
    let preview = call(
        Some(&fed),
        None,
        "apply_contract_context",
        json!({
            "snapshot": "live", "bindings": bindings, "dry_run": true
        }),
    )
    .await;
    assert_eq!(preview["data"]["applied"], false);
    assert_eq!(std::fs::read(&path).unwrap(), initial);
    let applied = call(
        Some(&fed),
        None,
        "apply_contract_context",
        json!({
            "snapshot": "live", "bindings": bindings, "dry_run": false
        }),
    )
    .await;
    assert_eq!(applied["data"]["applied"], true, "{applied}");
    let written = std::fs::read(&path).unwrap();
    let duplicate = call(
        Some(&fed),
        None,
        "apply_contract_context",
        json!({
            "snapshot": "live", "bindings": bindings, "dry_run": false
        }),
    )
    .await;
    assert_eq!(duplicate["data"]["applied"], true);
    assert!(duplicate["data"]["added_bindings"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), written);
    assert!(String::from_utf8(written)
        .unwrap()
        .contains("preserve-this"));
    assert_rpc_confirmation(&fed, None, "live", &consumer, &enclosing).await;

    // A fresh loader reads the persisted declaration. Rebuild the source graph
    // without LSP rather than relying on the first federation's derived facts.
    let reloaded = lain::federation::loader::load_federation(&path)
        .await
        .unwrap();
    for name in ["openshell", "nemo"] {
        fixture.index(&reloaded, name, IndexMode::Snapshot).await;
    }
    reloaded.rejoin_contracts_if_dirty().unwrap();
    assert_rpc_confirmation(&reloaded, None, "live", &consumer, &enclosing).await;
    let persisted = ContractFederationConfig::load(&path).unwrap();
    assert_eq!(persisted.bindings.len(), 1);
    assert_eq!(
        reloaded.contract_config().unwrap().config_hash(),
        persisted.config_hash()
    );

    // New snapshots capture the confirmed config; old snapshots stay immutable.
    assert_ne!(initial_hash, persisted.config_hash());
    let old_record = manager.get(&old_snapshot, 5000).await.unwrap().record;
    assert_eq!(old_record.config_hash, initial_hash);
    let old_contract = call(
        Some(&reloaded),
        Some(&manager),
        "get_contract",
        json!({
            "snapshot": old_snapshot, "service": "gateway", "key": KEY_CREATE
        }),
    )
    .await;
    assert!(
        old_contract["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|i| i["consumers"].as_array().unwrap())
            .all(|c| c["site"]["id"] != consumer),
        "old snapshot acquired a live binding: {old_contract}"
    );
    fixture.config = persisted;
    let snapshot = fixture.snapshot(&manager).await;
    assert_rpc_confirmation(&reloaded, Some(&manager), &snapshot, &consumer, &enclosing).await;
}

async fn assert_rpc_confirmation(
    fed: &FederatedIndex,
    manager: Option<&Arc<SnapshotManager>>,
    snapshot: &str,
    consumer: &str,
    enclosing: &str,
) {
    let contract = call(
        Some(fed),
        manager,
        "get_contract",
        json!({
            "snapshot": snapshot, "service": "gateway", "key": KEY_CREATE
        }),
    )
    .await;
    let bound = contract["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["consumers"].as_array().unwrap())
        .find(|c| c["site"]["id"] == consumer)
        .unwrap();
    assert_eq!(bound["binding"]["kind"], "confirmed", "{contract}");
    assert!(bound["binding"]["source"]
        .as_str()
        .unwrap()
        .contains("repos.yaml#bindings"));
    let impact = call(
        Some(fed),
        manager,
        "trace_impact",
        json!({
            "snapshot": snapshot,
            "from": {"endpoint": {"service": "gateway", "key": KEY_CREATE}}, "depth": 4
        }),
    )
    .await;
    let hops: Vec<_> = impact["data"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["hops"].as_array().unwrap())
        .collect();
    assert!(
        hops.iter()
            .any(|h| h["node"] == consumer && h["edge"] == "Binds"),
        "{impact}"
    );
    assert!(
        hops.iter()
            .any(|h| h["node"] == enclosing && h["edge"] == "Calls"),
        "{impact}"
    );
}
