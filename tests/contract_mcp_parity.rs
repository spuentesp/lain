//! MCP stdio/HTTP byte-parity test for contract-federation tools.
//!
//! §15.3 calls for byte-identical `structuredContent` (excluding
//! non-deterministic fields like `elapsed_ms`) across stdio and HTTP
//! transports for the same view and args.
//!
//! Spawns one stdio `ServerHandler` and one HTTP `ServerHandler`
//! (backed by real TCP loopback listener) against the same fixture,
//! asserting byte-identical results, `isError` parity, pagination parity,
//! and error envelope parity.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use lain::federation::contracts::config::ContractFederationConfig;
use lain::federation::contracts::model::ServiceName;
use lain::federation::contracts::snapshots::manager::SnapshotManager;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::graph::GraphDatabase;
use lain::server::mcp::handler::LainMcpServer;
use lain::server::tools::create_test_executor_with_graph;
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[path = "support/contracts_snapshot_harness.rs"]
mod snap_harness;

fn git_init_committed(dir: &Path) {
    let status = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .status()
        .expect("git init failed");
    assert!(status.success(), "git init failed: {status:?}");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git failed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["config", "user.email", "test@lain"]);
    run(&["config", "user.name", "lain"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "init"]);
}

async fn build_min_federation(root: &Path) -> Arc<FederatedIndex> {
    let orders_dir = root.join("orders");
    let billing_dir = root.join("billing");
    for d in [&orders_dir, &billing_dir] {
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/.keep"), b"").unwrap();
        git_init_committed(d);
    }

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));

    let cfg = ContractFederationConfig {
        services: vec![
            lain::federation::contracts::config::ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec!["src".into()],
                hosts: vec![],
                env: vec!["ORDERS_URL".into()],
                base_path: None,
                route_prefixes: vec![],
            },
            lain::federation::contracts::config::ServiceDecl {
                name: "billing".into(),
                repo: "billing".into(),
                paths: vec!["src".into()],
                hosts: vec![],
                env: vec!["BILLING_URL".into()],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    };
    fed.set_contract_config(cfg);
    fed
}

fn strip_meta(v: &Value) -> Value {
    let mut obj = v.as_object().cloned().unwrap_or_default();
    obj.remove("meta");
    obj.remove("reproducible");
    obj.remove("analyzer_version");
    obj.remove("snapshot");
    obj.remove("api_version");
    Value::Object(obj)
}

fn strip_elapsed(mut v: Value) -> Value {
    if let Some(meta) = v.get_mut("meta").and_then(|m| m.as_object_mut()) {
        meta.remove("elapsed_ms");
    }
    v
}

fn canonical_bytes(v: &Value) -> Vec<u8> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    normalize_for_parity(v, &mut seen);
    serde_json::to_vec(v).unwrap_or_default()
}

fn normalize_for_parity(v: &Value, seen: &mut BTreeSet<String>) {
    if let Value::Object(map) = v {
        let canonical = serde_json::to_string(v).unwrap_or_default();
        if seen.contains(&canonical) {
            return;
        }
        seen.insert(canonical);
        for (_, v) in map {
            normalize_for_parity(v, seen);
        }
    } else if let Value::Array(arr) = v {
        for v in arr {
            normalize_for_parity(v, seen);
        }
    }
}

/// Test harness comparing the dispatcher shared by stdio with a real
/// hyper-backed HTTP listener on an ephemeral loopback port.
struct ParityCluster {
    port: u16,
    embedded: LainMcpServer,
}

impl ParityCluster {
    async fn new(
        db_path: &Path,
        fed: Arc<FederatedIndex>,
        mgr: Option<Arc<SnapshotManager>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();

        let graph = GraphDatabase::new(&db_path.join("parity_db")).expect("graph parity");
        let exec = create_test_executor_with_graph(graph);
        let mut embedded = LainMcpServer::with_federation(exec, Arc::clone(&fed))
            .with_reindex_timeout(Some(Duration::from_millis(10)));
        if let Some(m) = mgr.as_ref() {
            embedded = embedded.with_snapshots(Arc::clone(m), None);
        }

        let http_handler = embedded.clone();

        tokio::spawn(async move {
            let _ = http_handler.run_http_listener(listener).await;
        });

        // Give listener a moment to initialize
        tokio::time::sleep(Duration::from_millis(25)).await;

        Self { port, embedded }
    }

    async fn call_stdio(&self, tool: &str, args: Value) -> (Value, bool) {
        let args_map = args.as_object().cloned().unwrap_or_default();
        let (_, is_error, structured) = self.embedded.call_tool_embedded(tool, args_map).await;
        (structured.unwrap_or(Value::Null), is_error)
    }

    async fn call_http(&self, tool: &str, args: Value) -> (Value, bool) {
        let client = reqwest::Client::new();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": args
            }
        });
        let resp: Value = client
            .post(format!("http://127.0.0.1:{}/mcp", self.port))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .expect("http request send failed")
            .json()
            .await
            .expect("http response json parse failed");

        let result = &resp["result"];
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let structured = result["structuredContent"].clone();
        (structured, is_error)
    }
}

#[tokio::test]
async fn stdio_and_http_yield_byte_identical_data_for_list_services() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_min_federation(root).await;
    let cluster = ParityCluster::new(root, fed, None).await;

    let args = json!({"snapshot": "live"});
    let (stdio_struct, stdio_err) = cluster.call_stdio("list_services", args.clone()).await;
    let (http_struct, http_err) = cluster.call_http("list_services", args).await;

    assert!(!stdio_err, "stdio isError should be false");
    assert!(!http_err, "http isError should be false");
    assert_eq!(stdio_struct["data"], http_struct["data"]);

    let stdio_bytes = canonical_bytes(&strip_meta(&stdio_struct));
    let http_bytes = canonical_bytes(&strip_meta(&http_struct));
    assert_eq!(stdio_bytes, http_bytes, "list_services byte-parity");
    assert_eq!(
        strip_elapsed(stdio_struct),
        strip_elapsed(http_struct),
        "structuredContent equality"
    );
}

#[tokio::test]
async fn stdio_and_http_yield_byte_identical_data_for_get_service() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_min_federation(root).await;
    let cluster = ParityCluster::new(root, fed, None).await;

    let args = json!({"snapshot": "live", "service": "orders"});
    let (stdio_struct, stdio_err) = cluster.call_stdio("get_service", args.clone()).await;
    let (http_struct, http_err) = cluster.call_http("get_service", args).await;

    assert!(!stdio_err, "stdio isError should be false");
    assert!(!http_err, "http isError should be false");
    assert_eq!(stdio_struct["data"], http_struct["data"]);

    let stdio_bytes = canonical_bytes(&strip_meta(&stdio_struct));
    let http_bytes = canonical_bytes(&strip_meta(&http_struct));
    assert_eq!(stdio_bytes, http_bytes, "get_service byte-parity");
    assert_eq!(
        strip_elapsed(stdio_struct),
        strip_elapsed(http_struct),
        "structuredContent equality"
    );
}

#[tokio::test]
async fn service_name_sort_is_stable_across_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_min_federation(root).await;
    let cluster = ParityCluster::new(root, fed, None).await;

    let (stdio_struct, _) = cluster
        .call_stdio("list_services", json!({"snapshot": "live"}))
        .await;
    let (http_struct, _) = cluster
        .call_http("list_services", json!({"snapshot": "live"}))
        .await;

    let names_stdio: Vec<String> = stdio_struct["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["service"].as_str().unwrap_or("").to_string())
        .collect();
    let names_http: Vec<String> = http_struct["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["service"].as_str().unwrap_or("").to_string())
        .collect();

    assert_eq!(names_stdio, names_http);
    assert_eq!(
        names_stdio,
        vec!["billing".to_string(), "orders".to_string()]
    );
}

#[tokio::test]
async fn service_name_display_matches_label() {
    let s = ServiceName("orders".to_string());
    assert_eq!(format!("{s}"), "orders");
}

#[tokio::test]
async fn stdio_and_http_yield_byte_identical_data_for_diff_contracts() {
    let fix = snap_harness::build_fixture();
    let mgr = snap_harness::manager(&fix.root);
    let config = snap_harness::contract_config(&fix.root);
    let fed = Arc::new(FederatedIndex::new(Arc::new(
        PetgraphBackend::new(&fix.root.join("parity-federation")).unwrap(),
    )));

    let cluster = ParityCluster::new(&fix.root, fed, Some(Arc::clone(&mgr))).await;

    // Base = orders at `base`; head = orders at `s1-remove-customer-id`.
    let base_repos = snap_harness::all_repos_at(&fix.root, "base");
    let base_id = snap_harness::prepare_ready(&mgr, base_repos, None, config.clone()).await;
    let head_id = snap_harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s1-remove-customer-id")],
    )
    .await;

    let args = json!({"base": base_id, "head": head_id, "cap": 100});
    let (stdio_struct, stdio_err) = cluster.call_stdio("diff_contracts", args.clone()).await;
    let (http_struct, http_err) = cluster.call_http("diff_contracts", args).await;

    assert!(!stdio_err, "diff_contracts stdio error");
    assert!(!http_err, "diff_contracts http error");
    assert_eq!(stdio_struct["data"], http_struct["data"]);

    let stdio_bytes = canonical_bytes(&strip_meta(&stdio_struct));
    let http_bytes = canonical_bytes(&strip_meta(&http_struct));
    assert_eq!(
        stdio_bytes, http_bytes,
        "diff_contracts byte-parity on the same snapshot pair"
    );
    assert_eq!(
        strip_elapsed(stdio_struct),
        strip_elapsed(http_struct),
        "structuredContent equality"
    );
}

#[tokio::test]
async fn stdio_and_http_share_the_same_snapshot_query_view() {
    let fixture = snap_harness::build_fixture();
    let manager = snap_harness::manager(&fixture.root);
    let config = snap_harness::contract_config(&fixture.root);
    let federation = Arc::new(FederatedIndex::new(Arc::new(
        PetgraphBackend::new(&fixture.root.join("snapshot-query-parity")).unwrap(),
    )));
    let cluster = ParityCluster::new(&fixture.root, federation, Some(Arc::clone(&manager))).await;
    let commits = snap_harness::all_repos_at(&fixture.root, "base");
    let snapshot = snap_harness::prepare_ready(&manager, commits.clone(), None, config).await;

    let list_args = json!({"snapshot": snapshot});
    let (stdio_list, stdio_error) = cluster
        .call_stdio("list_contracts", list_args.clone())
        .await;
    let (http_list, http_error) = cluster.call_http("list_contracts", list_args).await;
    assert!(!stdio_error && !http_error);
    assert_eq!(strip_elapsed(stdio_list.clone()), strip_elapsed(http_list));
    assert_eq!(
        stdio_list["view"]["git_commits"],
        serde_json::to_value(&commits).unwrap()
    );

    let contract = stdio_list["data"]["items"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|item| {
                item["providers"]
                    .as_array()
                    .is_some_and(|providers| !providers.is_empty())
            })
        })
        .expect("contract with provider");
    let endpoint = contract["endpoint"].clone();
    let provider = contract["providers"][0]["id"]
        .as_str()
        .expect("provider id")
        .to_string();

    for (tool, args) in [
        (
            "trace_impact",
            json!({
                "snapshot": snapshot,
                "from": {"endpoint": endpoint},
                "depth": 4,
                "cap": 20,
            }),
        ),
        (
            "resolve_evidence",
            json!({"snapshot": snapshot, "refs": [provider]}),
        ),
    ] {
        let (stdio, stdio_error) = cluster.call_stdio(tool, args.clone()).await;
        let (http, http_error) = cluster.call_http(tool, args).await;
        assert!(!stdio_error, "{tool} stdio error: {stdio:#?}");
        assert!(!http_error, "{tool} HTTP error: {http:#?}");
        assert_eq!(
            strip_elapsed(stdio),
            strip_elapsed(http),
            "{tool} snapshot parity"
        );
    }
}

#[tokio::test]
async fn stdio_and_http_parity_on_error_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_min_federation(root).await;
    let cluster = ParityCluster::new(root, fed, None).await;

    // Missing snapshot argument
    let args = json!({"snapshot": "snap_nonexistent", "service": "orders"});
    let (stdio_struct, stdio_err) = cluster.call_stdio("get_service", args.clone()).await;
    let (http_struct, http_err) = cluster.call_http("get_service", args).await;

    assert!(stdio_err, "stdio should report isError: true");
    assert!(http_err, "http should report isError: true");

    assert_eq!(stdio_struct["error"]["code"], http_struct["error"]["code"]);
    assert_eq!(
        stdio_struct["error"]["message"],
        http_struct["error"]["message"]
    );
}

#[tokio::test]
async fn stdio_and_http_parity_on_pagination() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_min_federation(root).await;
    let cluster = ParityCluster::new(root, fed, None).await;

    // Cap to 1 item to trigger pagination
    let args = json!({"snapshot": "live", "limit": 1});
    let (stdio_struct, stdio_err) = cluster.call_stdio("list_services", args.clone()).await;
    let (http_struct, http_err) = cluster.call_http("list_services", args).await;

    assert!(!stdio_err);
    assert!(!http_err);
    assert_eq!(
        stdio_struct["data"]["items"].as_array().map(|a| a.len()),
        Some(1)
    );
    assert_eq!(
        http_struct["data"]["items"].as_array().map(|a| a.len()),
        Some(1)
    );
    assert_eq!(
        stdio_struct["data"]["cursor"],
        http_struct["data"]["cursor"]
    );
    assert_eq!(
        strip_elapsed(stdio_struct),
        strip_elapsed(http_struct),
        "paginated structuredContent parity"
    );
}

/// Every contract tool must be byte-identical across stdio and HTTP on
/// a fixture that **has CODEOWNERS**.
///
/// The tests above run a mini-federation with no CODEOWNERS, which is
/// exactly why they missed the `get_service.used_by[].owners`
/// divergence: one HTTP instance returned owners on four entries,
/// while stdio and a second HTTP instance returned the key absent
/// entirely. Parity must be proven on the fixture that exercises the
/// feature.
#[tokio::test]
async fn every_contract_tool_is_byte_identical_across_transports() {
    let fix = snap_harness::build_fixture();
    let mgr = snap_harness::manager(&fix.root);
    let config = snap_harness::contract_config(&fix.root);
    let fed = Arc::new(FederatedIndex::new(Arc::new(
        PetgraphBackend::new(&fix.root.join("parity-all-tools")).unwrap(),
    )));
    let cluster = ParityCluster::new(&fix.root, fed, Some(Arc::clone(&mgr))).await;

    let base_repos = snap_harness::all_repos_at(&fix.root, "base");
    let base_id = snap_harness::prepare_ready(&mgr, base_repos, None, config.clone()).await;
    let head_id = snap_harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s21-code-only-handler")],
    )
    .await;

    let calls: Vec<(&str, Value)> = vec![
        ("list_services", json!({"snapshot": base_id, "limit": 50})),
        ("list_contracts", json!({"snapshot": base_id, "limit": 100})),
        (
            "get_contract",
            json!({"snapshot": base_id, "key": "http:GET /api/orders/{}"}),
        ),
        (
            "get_service",
            json!({"snapshot": base_id, "service": "orders", "limit": 50}),
        ),
        ("get_coverage", json!({"snapshot": base_id})),
        ("list_unresolved", json!({"snapshot": base_id, "limit": 50})),
        (
            "trace_impact",
            json!({"snapshot": base_id, "from": {"endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"}}, "depth": 3}),
        ),
        ("diff_contracts", json!({"base": base_id, "head": head_id})),
        (
            "resolve_evidence",
            json!({"snapshot": base_id, "refs": ["orders:HttpRoute:src/main.rs:GET /api/orders/%3Aid:17"], "context_lines": 2}),
        ),
    ];

    for (tool, args) in calls {
        let (stdio_struct, stdio_err) = cluster.call_stdio(tool, args.clone()).await;
        let (http_struct, http_err) = cluster.call_http(tool, args).await;
        assert_eq!(
            stdio_err, http_err,
            "{tool}: isError differs across transports"
        );
        assert!(!stdio_err, "{tool} errored: {stdio_struct:#?}");

        let stdio_bytes = canonical_bytes(&strip_meta(&stdio_struct));
        let http_bytes = canonical_bytes(&strip_meta(&http_struct));
        assert_eq!(
            stdio_bytes, http_bytes,
            "{tool} differs between stdio and HTTP"
        );
    }

    // And the parity must include the `owners` key the mini-fixture
    // cannot exercise.
    let (svc, _) = cluster
        .call_stdio(
            "get_service",
            json!({"snapshot": base_id, "service": "orders", "limit": 50}),
        )
        .await;
    let mut owners_seen = 0usize;
    fn count_owners(v: &Value, n: &mut usize) {
        match v {
            Value::Object(m) => {
                for (k, val) in m {
                    if k == "owners" {
                        *n += 1;
                    } else {
                        count_owners(val, n);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| count_owners(i, n)),
            _ => {}
        }
    }
    count_owners(&svc, &mut owners_seen);
    assert!(
        owners_seen > 0,
        "the T1 fixture has CODEOWNERS; get_service must expose `owners` \
         so this parity test can actually catch a divergence"
    );
}
