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
use lain::federation::repo_id::GlobalId;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
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

    // billing: provider node (HttpRoute) + handler.
    let ns = RepoNamespace::for_test();
    let billing_route_id = GlobalId::new(
        &billing_id,
        NodeType::HttpRoute,
        "src/billing.py",
        "GET /invoices/{}",
        Some(1),
    )
    .as_str()
    .to_string();
    let mut billing_route = GraphNode::new_in(
        NodeType::HttpRoute,
        "GET /invoices/{}".to_string(),
        "src/billing.py".to_string(),
        &ns,
    );
    billing_route.id = billing_route_id.clone();
    billing_route.line_start = Some(1);
    billing_route.contract = Some(ContractFact::Provider(
        lain::federation::contracts::model::ProviderFact {
            method: lain::federation::contracts::model::HttpMethod::Get,
            template: "/invoices/{}".to_string(),
            handler: Some(lain::federation::contracts::model::SymbolKey {
                repo: billing_id.clone(),
                path: "src/billing.py".to_string(),
                container: None,
                name: "build_invoice".to_string(),
            }),
            origin: lain::federation::contracts::model::ProviderOrigin::Code,
            operation_id: None,
        },
    ));
    billing_g
        .insert_nodes_batch(std::slice::from_ref(&billing_route))
        .unwrap();
    let build_invoice = make_fn(&billing_g, "build_invoice", "src/billing.py", 10);
    let fetch_order = make_fn(&billing_g, "fetch_order", "src/billing.py", 20);
    billing_g
        .insert_nodes_batch(&[build_invoice.clone(), fetch_order.clone()])
        .unwrap();
    insert_call(&billing_g, &build_invoice.id, &fetch_order.id);
    // Bind billing's route to its handler.
    billing_g
        .insert_edges_batch(&[GraphEdge::new(
            EdgeType::CallsHttp,
            billing_route_id.clone(),
            build_invoice.id.clone(),
        )])
        .unwrap();

    // reports: getMonthlyReport → buildMonthlyReport, scheduledMonthlyReport → buildMonthlyReport.
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

    // Synthesize the consumer chain (`buildMonthlyReport` calls
    // billing's `/invoices/{}`). The HttpClientCall node carries the
    // `HttpMethod::Get` template, the `Env("BILLING_URL")` host
    // resolution matches `billing`'s declared env. The SendsHttp
    // edge anchors the call to `buildMonthlyReport` so the joiner
    // knows the caller; the Binds edge from `HttpClientCall` →
    // billing's `HttpRoute` node pins the join explicitly (the in-
    // process joiner would derive it from host+template, but we
    // stamp it directly so the test isn't sensitive to env-var
    // resolution order).
    use lain::federation::contracts::model::HostPart;
    use lain::federation::contracts::model::{
        CallVia, ConsumerFact, ContractFact, HttpMethod, MethodSpec, NormalizedUrl,
    };
    let http_call_id = GlobalId::new(
        &reports_id,
        NodeType::HttpClientCall,
        "src/index.ts",
        "buildMonthlyReport",
        Some(20),
    )
    .as_str()
    .to_string();
    let mut http_call = GraphNode::new_in(
        NodeType::HttpClientCall,
        "buildMonthlyReport".to_string(),
        "src/index.ts".to_string(),
        &ns,
    );
    http_call.id = http_call_id.clone();
    http_call.line_start = Some(20);
    http_call.line_end = Some(21);
    http_call.contract = Some(ContractFact::Consumer(ConsumerFact {
        method: MethodSpec::Known(HttpMethod::Get),
        url: NormalizedUrl {
            host: HostPart::Env(vec!["BILLING_URL".to_string()]),
            template: Some("/invoices/{}".to_string()),
        },
        via: CallVia::Library {
            name: "fetch".to_string(),
        },
        url_expr: "${process.env.BILLING_URL}/invoices/${id}".to_string(),
        reads_complete: true,
    }));
    reports_g
        .insert_nodes_batch(std::slice::from_ref(&http_call))
        .unwrap();
    // SendsHttp: caller function → HttpClientCall (so `used_by` can
    // resolve the caller via the federation's `incoming_calls`).
    reports_g
        .insert_edges_batch(&[GraphEdge::new(
            EdgeType::SendsHttp,
            build_monthly.id.clone(),
            http_call_id.clone(),
        )])
        .unwrap();
    // Binds: HttpClientCall → billing's provider node, with `Exact`
    // route_match and `Static{TreeSitter}` provenance.
    let mut binds_edge = GraphEdge::new(
        EdgeType::Binds,
        http_call_id.clone(),
        billing_route_id.clone(),
    );
    binds_edge.detail = Some(lain::schema::EdgeDetail {
        route_match: Some(lain::schema::RouteMatch::Exact),
        stripped_prefix: None,
    });
    binds_edge.provenance = Some(EdgeProvenance::Static {
        source: lain::schema::StaticSource::TreeSitter,
    });
    binds_edge.weight = Some(1.0);
    reports_g.insert_edges_batch(&[binds_edge]).unwrap();

    // The contract index tracks per-repo contract node ids so the
    // joiner knows which nodes are providers / consumers. Mark the
    // synthetic `HttpClientCall` as a contract node in the
    // reports repo, plus the billing route, so the next rejoin
    // includes them.
    fed.register_contract_node_for_test(reports_id.clone(), http_call_id.clone());
    fed.register_contract_node_for_test(billing_id.clone(), billing_route_id.clone());
    fed.mark_contracts_dirty();

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

    // get_service(billing) → reports must be listed as a consumer
    // with two `uses` entries (the scheduled job + the HTTP handler,
    // both calling `buildMonthlyReport`). Each `use`'s `used_by` walk
    // must surface the matching entry-point kind with the right
    // function name from `ground_truth.yaml` (§15.2 row 17).
    let get = call_get_service(fed.clone(), "billing").await;
    let data = &get["data"];
    assert_eq!(data["service"].as_str(), Some("billing"));
    assert!(data["scope"].is_object(), "scope must be present: {data:?}");
    assert_eq!(data["scope"]["configured_only"], json!(true));
    assert_eq!(data["provider_reviewed"], json!(true));
    assert!(data["endpoints"].is_array());

    let consumers = data["consumers"].as_array().expect("consumers array");
    let reports_consumer = consumers
        .iter()
        .find(|c| c["service"].as_str() == Some("reports"))
        .unwrap_or_else(|| panic!("reports consumer missing: {consumers:?}"));
    let uses = reports_consumer["uses"]
        .as_array()
        .expect("reports consumer uses array");
    // One caller (`buildMonthlyReport`) → one `use`; the `used_by`
    // walk surfaces both entry points reachable from that caller.
    assert_eq!(
        uses.len(),
        1,
        "reports must have exactly 1 use (single caller buildMonthlyReport), got: {uses:?}"
    );
    let u = &uses[0];
    assert_eq!(
        u["caller"]["name"].as_str(),
        Some("buildMonthlyReport"),
        "use.caller.name mismatch: {u:?}"
    );
    let used_by = u["used_by"].as_array().expect("used_by array");
    let mut found_http_handler = false;
    let mut found_scheduled = false;
    for entry in used_by {
        match entry["name"].as_str() {
            Some("getMonthlyReport") => {
                assert_eq!(entry["kind"].as_str(), Some("http_handler"));
                found_http_handler = true;
            }
            Some("scheduledMonthlyReport") => {
                assert_eq!(entry["kind"].as_str(), Some("scheduled"));
                found_scheduled = true;
            }
            other => panic!("unexpected used_by entry: {other:?}"),
        }
    }
    assert!(
        found_http_handler,
        "used_by must include http_handler entry for getMonthlyReport"
    );
    assert!(
        found_scheduled,
        "used_by must include scheduled entry for scheduledMonthlyReport"
    );
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
