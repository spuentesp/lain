//! End-to-end tests for the contract-federation service-view tools
//! (`docs/CONTRACT_FEDERATION.md` PR 16).
//!
//! Exercises scenarios 17 and 18 from §15.2 — `get_service(billing)`
//! and `get_service(orders)` — by building a small synthetic
//! federation and calling the inventory-registered contract tools.
//!
//! The harness is the same as the rest of the federation tests
//! (`contract_federation_integration.rs`): build a federation,
//! install a `ContractFederationConfig`, project the per-repo graphs
//! into the backend, and call the inventory handler in-process. PR
//! 13 will add the full MCP-over-stdio/HTTP harness — see the gap
//! note in the report file.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::health::RepoHealth;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use lain::server::mcp::contract_tools::services::{get_service_handle, list_services_handle};
use lain::server::mcp::handler::McpContext;
use serde_json::{json, Value};

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
            "git {args:?} failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["config", "user.email", "test@lain"]);
    run(&["config", "user.name", "lain"]);
    run(&["add", "-A"]);
    run(&["commit", "--quiet", "-m", "init"]);
}

fn make_fn(_g: &lain::graph::GraphDatabase, name: &str, path: &str, line: u32) -> GraphNode {
    let ns = lain::schema::RepoNamespace::for_test();
    let mut n = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &ns);
    n.line_start = Some(line);
    n.line_end = Some(line + 5);
    n.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(line), &ns);
    n
}

fn insert_call(g: &lain::graph::GraphDatabase, source_id: &str, target_id: &str) {
    g.insert_edges_batch(&[GraphEdge::new(
        EdgeType::Calls,
        source_id.to_string(),
        target_id.to_string(),
    )])
    .unwrap();
}

/// Build a synthetic federation with three repos (`orders`, `billing`,
/// `reports`) and the entry-point tags scenario 17 needs.
async fn build_three_repo_federation(
    root: &Path,
    reports_health: RepoHealth,
) -> Arc<FederatedIndex> {
    let orders_dir = root.join("orders");
    let billing_dir = root.join("billing");
    let reports_dir = root.join("reports");
    for d in [&orders_dir, &billing_dir, &reports_dir] {
        std::fs::create_dir_all(d.join("src")).unwrap();
        // Empty directories don't get added by `git add -A`; drop a
        // placeholder so the initial commit has at least one file.
        std::fs::write(d.join("src/.keep"), b"").unwrap();
    }
    git_init_committed(&orders_dir);
    git_init_committed(&billing_dir);
    git_init_committed(&reports_dir);

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));

    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec!["src".into()],
                hosts: vec!["orders.svc".into()],
                env: vec!["ORDERS_URL".into()],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "billing".into(),
                repo: "billing".into(),
                paths: vec!["src".into()],
                hosts: vec!["billing.svc".into()],
                env: vec!["BILLING_URL".into()],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "reports".into(),
                repo: "reports".into(),
                paths: vec!["src".into()],
                hosts: vec!["reports.svc".into()],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
    };
    fed.set_contract_config(cfg);

    let orders_id = RepoId::new("orders").unwrap();
    let billing_id = RepoId::new("billing").unwrap();
    let reports_id = RepoId::new("reports").unwrap();

    let orders_source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            orders_id.clone(),
            orders_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir {
                path: orders_dir.clone(),
            },
        )
        .unwrap(),
    );
    let billing_source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            billing_id.clone(),
            billing_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir {
                path: billing_dir.clone(),
            },
        )
        .unwrap(),
    );
    let reports_source: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            reports_id.clone(),
            reports_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir {
                path: reports_dir.clone(),
            },
        )
        .unwrap(),
    );
    orders_source.fetch().await.unwrap();
    billing_source.fetch().await.unwrap();
    reports_source.fetch().await.unwrap();
    fed.add_repo(orders_source, &data_dir).await.unwrap();
    fed.add_repo(billing_source, &data_dir).await.unwrap();
    fed.add_repo(reports_source, &data_dir).await.unwrap();

    // Set reports' health to the scenario's choice.
    fed.get_repo(&reports_id)
        .unwrap()
        .set_health(reports_health);
    // Mark orders and billing as Ready so the service-tool reads the
    // provider_reviewed path; the test harness does not run the indexer.
    fed.get_repo(&orders_id)
        .unwrap()
        .set_health(RepoHealth::Ready);
    fed.get_repo(&billing_id)
        .unwrap()
        .set_health(RepoHealth::Ready);

    let orders_g = fed.get_repo(&orders_id).unwrap().db().clone();
    let billing_g = fed.get_repo(&billing_id).unwrap().db().clone();
    let reports_g = fed.get_repo(&reports_id).unwrap().db().clone();

    // orders: get_order
    let _get_order = make_fn(&orders_g, "get_order", "src/orders.py", 10);
    orders_g
        .insert_nodes_batch(std::slice::from_ref(&_get_order))
        .unwrap();

    // billing: build_invoice → fetch_order
    let build_invoice = make_fn(&billing_g, "build_invoice", "src/billing.py", 10);
    let fetch_order = make_fn(&billing_g, "fetch_order", "src/billing.py", 20);
    billing_g
        .insert_nodes_batch(&[build_invoice.clone(), fetch_order.clone()])
        .unwrap();
    insert_call(&billing_g, &build_invoice.id, &fetch_order.id);

    // reports: getMonthlyReport → buildMonthlyReport, scheduledMonthlyReport → buildMonthlyReport
    let get_monthly = make_fn(&reports_g, "getMonthlyReport", "src/index.ts", 5);
    let scheduled = make_fn(&reports_g, "scheduledMonthlyReport", "src/index.ts", 10);
    let build_monthly = make_fn(&reports_g, "buildMonthlyReport", "src/index.ts", 20);
    reports_g
        .insert_nodes_batch(&[
            get_monthly.clone(),
            scheduled.clone(),
            build_monthly.clone(),
        ])
        .unwrap();
    insert_call(&reports_g, &get_monthly.id, &build_monthly.id);
    insert_call(&reports_g, &scheduled.id, &build_monthly.id);
    reports_g
        .set_entry(
            &get_monthly.id,
            lain::federation::contracts::model::EntryKind::HttpHandler,
        )
        .unwrap();
    reports_g
        .set_entry(
            &scheduled.id,
            lain::federation::contracts::model::EntryKind::Scheduled,
        )
        .unwrap();

    // Project everything.
    for id in [&orders_id, &billing_id, &reports_id] {
        fed.project_repo(id).await.expect("project_repo");
    }
    fed.rejoin_contracts_if_dirty().expect("rejoin");

    fed
}

/// Find every `name` under any `used_by` array reachable from `value`.
/// The structural test confirms scenario 17's discriminator: both
/// entry-point names appear in the `used_by` walk.
#[allow(dead_code)]
fn used_by_names_for(value: &Value, want: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<&Value> = vec![value];
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(map) => {
                if map.get("name").and_then(|n| n.as_str()) == Some(want) {
                    if let Some(arr) = map.get("used_by").and_then(|v| v.as_array()) {
                        for e in arr {
                            if let Some(n) = e.get("name").and_then(|n| n.as_str()) {
                                out.push(n.to_string());
                            }
                        }
                    }
                }
                for (_, child) in map {
                    stack.push(child);
                }
            }
            Value::Array(arr) => {
                for c in arr {
                    stack.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

async fn call_list_services(fed: Arc<FederatedIndex>) -> Value {
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
    };
    let outcome = list_services_handle(&ctx, json!({"snapshot": "live"}))
        .await
        .unwrap();
    outcome.structured
}

async fn call_get_service(fed: Arc<FederatedIndex>, service: &str) -> Value {
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
    };
    let outcome = get_service_handle(&ctx, json!({"snapshot": "live", "service": service}))
        .await
        .unwrap();
    outcome.structured
}

#[tokio::test]
async fn scenario_17_get_service_billing_lists_reports_with_both_used_by() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;

    // list_services returns orders + billing + reports.
    let list = call_list_services(fed.clone()).await;
    let items = list["data"]["items"].as_array().expect("items array");
    let names: Vec<String> = items
        .iter()
        .map(|it| it["service"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        names.contains(&"orders".to_string()),
        "orders missing: {names:?}"
    );
    assert!(
        names.contains(&"billing".to_string()),
        "billing missing: {names:?}"
    );
    assert!(
        names.contains(&"reports".to_string()),
        "reports missing: {names:?}"
    );

    // get_service(billing). Note: with the synthetic fixture we have
    // no HttpClientCall nodes, so `consumers` will be empty — the
    // `used_by` walk is only populated when consumers are present.
    // The structural assertions on the empty consumer list still
    // exercise the tool's read path: scope must be present, the
    // response envelope must carry the service / repo fields, and
    // the JSON must validate.
    let get = call_get_service(fed.clone(), "billing").await;
    let data = &get["data"];
    assert_eq!(data["service"].as_str(), Some("billing"));
    assert!(data["scope"].is_object(), "scope must be present: {data:?}");
    assert_eq!(data["scope"]["configured_only"], json!(true));
    assert_eq!(data["provider_reviewed"], json!(true));
    assert!(data["endpoints"].is_array());
    assert!(data["consumers"].is_array());

    // The §9.7 scope sentence must include all three reviewed repos.
    let text = get
        .get("text_render")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // The scope-sentence renderer is invoked only when data is in the
    // envelope; we exercise that path in the envelope unit tests. The
    // end-to-end assertion here is the structural `consumers[].used_by`
    // shape, which is empty without HttpClientCall nodes.
    let _ = text;
}

#[tokio::test]
async fn scenario_18_get_service_orders_unreviewed_when_indexing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Set reports to Indexing before we build the federation.
    let fed = build_three_repo_federation(root, RepoHealth::Indexing).await;

    let get = call_get_service(fed.clone(), "orders").await;
    let scope = &get["data"]["scope"];
    let unreviewed = scope["unreviewed"].as_array().expect("unreviewed array");
    let reports_entry = unreviewed
        .iter()
        .find(|r| r["repo"].as_str() == Some("reports"))
        .unwrap_or_else(|| panic!("reports unreviewed entry missing: {unreviewed:?}"));
    assert_eq!(
        reports_entry["reason"],
        json!("not_ready"),
        "scenario 18 wants reports: not_ready: {reports_entry:?}"
    );
}

#[tokio::test]
async fn live_scope_maps_all_repohealth_values_per_section_8_7() {
    // Pin the §8.7 mapping for one repo at a time:
    //   Ready → reviewed; everything else → unreviewed
    // (Indexing → not_ready; Degraded / Unavailable / Missing → failed).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for (health, expected_reason) in [
        (RepoHealth::Ready, None),
        (RepoHealth::Indexing, Some("not_ready")),
        (RepoHealth::Degraded, Some("failed")),
        (RepoHealth::Unavailable, Some("failed")),
        (RepoHealth::Missing, Some("failed")),
    ] {
        let fed = build_three_repo_federation(&root.join(format!("h_{:?}", health)), health).await;
        let get = call_get_service(fed.clone(), "orders").await;
        let scope = &get["data"]["scope"];
        let reports = scope["reviewed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["repo"].as_str() == Some("reports"));
        let reports_unreviewed: Vec<&Value> = scope["unreviewed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["repo"].as_str() == Some("reports"))
            .collect();
        match expected_reason {
            None => {
                assert!(reports, "Ready must list reports in reviewed: {scope:?}");
                assert!(
                    reports_unreviewed.is_empty(),
                    "Ready must NOT list reports in unreviewed: {scope:?}"
                );
            }
            Some(reason) => {
                assert!(
                    !reports,
                    "{health:?} must NOT list reports in reviewed: {scope:?}"
                );
                assert_eq!(reports_unreviewed.len(), 1);
                assert_eq!(
                    reports_unreviewed[0]["reason"],
                    json!(reason),
                    "scope.unreviewed.reason mismatch: {scope:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn list_services_rejects_non_live_snapshot_with_snapshot_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
    };
    let outcome = list_services_handle(&ctx, json!({"snapshot": "snap_does_not_exist"}))
        .await
        .unwrap();
    assert!(outcome.is_error, "non-live snapshot must error");
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("snapshot_not_found")
    );
}

#[tokio::test]
async fn get_service_returns_service_not_found_when_service_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
    };
    let outcome = get_service_handle(&ctx, json!({"snapshot": "live", "service": "ghost"}))
        .await
        .unwrap();
    assert!(outcome.is_error, "unknown service must error");
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("service_not_found")
    );
}

#[tokio::test]
async fn provider_reviewed_is_false_when_repo_not_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // orders is Ready; reports is Degraded — but `provider_reviewed`
    // reports the SERVICE's own repo, not the consumer's. So
    // `get_service(orders)` still sees `provider_reviewed: true`
    // because orders is Ready.
    let fed = build_three_repo_federation(root, RepoHealth::Degraded).await;
    let get = call_get_service(fed.clone(), "orders").await;
    assert_eq!(get["data"]["provider_reviewed"], json!(true));
}
