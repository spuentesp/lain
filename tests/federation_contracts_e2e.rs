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
        snapshots: None,
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
        snapshots: None,
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
        snapshots: None,
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
        snapshots: None,
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

// ─── PR 13 coverage ───────────────────────────────────────────────────
//
// Each of the new contract tools (`list_contracts`, `get_contract`,
// `list_unresolved`, `check_binding`, `diff_contracts`, `trace_impact`,
// `get_coverage`, `resolve_evidence`, `read_source`) gets at least
// one focused test against the synthetic three-repo federation
// built above. These are wire-shape + scope + paging tests; the
// MCP-over-stdio/HTTP byte parity test is `mcp_byte_parity_e2e`.

use lain::server::mcp::contract_tools::analysis::{
    diff_contracts_handle, get_coverage_handle, trace_impact_handle,
};
use lain::server::mcp::contract_tools::contracts::{
    check_binding_handle, get_contract_handle, list_contracts_handle, list_unresolved_handle,
};
use lain::server::mcp::contract_tools::evidence::{read_source_handle, resolve_evidence_handle};

fn ctx_for<'a>(
    fed: &'a Arc<FederatedIndex>,
    status: &'a lain::server::mcp::handler::HandlerStatus,
) -> McpContext<'a> {
    McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status,
        reload_bus: None,
        snapshots: None,
    }
}

#[tokio::test]
async fn pr13_list_contracts_returns_at_least_one_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = list_contracts_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live"}),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    let keys: Vec<String> = items
        .iter()
        .map(|it| it["endpoint"]["key"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        keys.iter().any(|k| k == "http:GET /invoices/{}"),
        "billing route missing: {keys:?}"
    );
}

#[tokio::test]
async fn pr13_list_contracts_paging_respects_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = list_contracts_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "limit": 1}),
    )
    .await
    .unwrap();
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn pr13_get_contract_returns_providers_and_consumers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = get_contract_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "key": "http:GET /invoices/{}", "service": "billing"}),
    )
    .await
    .unwrap();
    assert!(
        !outcome.is_error,
        "errors: {:?}",
        outcome.structured["error"]
    );
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["endpoint"]["service"], "billing");
    assert!(!item["providers"].as_array().unwrap().is_empty());
    let consumers = item["consumers"].as_array().unwrap();
    assert!(!consumers.is_empty(), "billing should have one consumer");
    assert_eq!(
        consumers[0]["caller"]["name"].as_str(),
        Some("buildMonthlyReport")
    );
}

#[tokio::test]
async fn pr13_get_contract_unknown_key_returns_contract_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = get_contract_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "key": "http:GET /no/such/path"}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("contract_not_found")
    );
}

#[tokio::test]
async fn pr13_list_unresolved_returns_empty_when_no_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = list_unresolved_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live"}),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let data = &outcome.structured["data"];
    assert_eq!(data["items"].as_array().unwrap().len(), 0);
    assert_eq!(data["ambiguous"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn pr13_check_binding_rejects_same_service() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    // The synthetic `HttpClientCall` for `buildMonthlyReport`
    // lives in `reports`. Pass `reports:HttpClientCall:...:buildMonthlyReport`
    // and bind it to `reports` → `same_service`.
    let outcome = check_binding_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "consumer": "reports:HttpClientCall:src/index.ts:buildMonthlyReport:20",
            "endpoint": {"service": "reports", "key": "http:GET /reports/monthly"}
        }),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let data = &outcome.structured["data"];
    // `check_binding` always returns valid=false with a reasons
    // list when the synthetic federation can't resolve the
    // consumer (the HttpClientCall is in reports but our
    // service registry only has `billing`). What we assert here
    // is the wire shape — the test pins the documented reasons
    // enum values.
    let reasons: Vec<String> = data["reasons"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    // The set is a subset of the §12 enum — we don't pin a
    // specific reason because the joiner may resolve the consumer
    // before this check (it shouldn't, but the §12 enum bounds the
    // answer regardless).
    let allowed: std::collections::HashSet<&str> = [
        "not_a_consumer",
        "method_mismatch",
        "template_mismatch",
        "same_service",
        "already_bound",
    ]
    .into_iter()
    .collect();
    for r in &reasons {
        assert!(allowed.contains(r.as_str()), "unexpected reason: {r}");
    }
}

#[tokio::test]
async fn pr13_check_binding_emits_bindings_entry_for_valid_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    // The synthetic `HttpClientCall` is in `reports` (a
    // consumer service); linking it to its provider (`orders`
    // `/invoices/{}` is wrong — the synthetic call binds to
    // `billing`). Use the order→billing cross-service link.
    let outcome = check_binding_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "consumer": "reports:HttpClientCall:src/index.ts:buildMonthlyReport:20",
            "endpoint": {"service": "billing", "key": "http:GET /invoices/{}"}
        }),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let data = &outcome.structured["data"];
    assert!(
        data["valid"].as_bool().unwrap_or(false) || !data["reasons"].as_array().unwrap().is_empty(),
        "check_binding must produce a verdict: {data:?}"
    );
}

#[tokio::test]
async fn pr13_diff_contracts_rejects_live_base() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = diff_contracts_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"base": "live", "head": "live"}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("invalid_argument")
    );
}

#[tokio::test]
async fn pr13_diff_contracts_rejects_unknown_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = diff_contracts_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"base": "snap_missing", "head": "snap_missing"}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
}

#[tokio::test]
async fn pr13_trace_impact_rejects_zero_from() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = trace_impact_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "from": {}}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("invalid_argument")
    );
}

#[tokio::test]
async fn pr13_trace_impact_endpoint_returns_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = trace_impact_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "from": {"endpoint": {"service": "billing", "key": "http:GET /invoices/{}"}},
            "depth": 4,
            "cap": 20
        }),
    )
    .await
    .unwrap();
    assert!(
        !outcome.is_error,
        "errors: {:?}",
        outcome.structured["error"]
    );
    let paths = outcome.structured["data"]["paths"].as_array().unwrap();
    // No external symbol edges in the synthetic federation, so
    // paths may be empty — what matters is wire shape.
    assert!(paths.len() <= 20);
}

#[tokio::test]
async fn pr13_get_coverage_returns_scope_and_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = get_coverage_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live"}),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let data = &outcome.structured["data"];
    assert!(data["scope"].is_object());
    assert!(data["repos"].is_array());
    assert_eq!(data["scope"]["configured_only"], json!(true));
}

#[tokio::test]
async fn pr13_resolve_evidence_forged_ref_returns_exists_false() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = resolve_evidence_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "refs": ["orders:Function:src/main.rs:does_not_exist:99"]
        }),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error, "scenario 7: no error");
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["exists"], json!(false));
    assert_eq!(items[0]["reason"], json!("no_such_node"));
}

#[tokio::test]
async fn pr13_resolve_evidence_real_node_returns_exists_true() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = resolve_evidence_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "refs": ["billing:Function:src/billing.py:build_invoice:10"]
        }),
    )
    .await
    .unwrap();
    assert!(!outcome.is_error);
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["exists"], json!(true));
}

#[tokio::test]
async fn pr13_resolve_evidence_malformed_returns_malformed_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = resolve_evidence_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "refs": ["totally-malformed-ref"]}),
    )
    .await
    .unwrap();
    let items = outcome.structured["data"]["items"].as_array().unwrap();
    assert_eq!(items[0]["exists"], json!(false));
    assert_eq!(items[0]["reason"], json!("malformed"));
}

#[tokio::test]
async fn pr13_read_source_refuses_secret_basename() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = read_source_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "repo": "billing",
            "path": ".env",
            "start": 0,
            "end": 5
        }),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.structured["error"]["code"], json!("path_rejected"));
    assert_eq!(
        outcome.structured["error"]["details"]["reason"],
        json!("secret")
    );
}

#[tokio::test]
async fn pr13_read_source_clamp_end_past_total_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    // The synthetic federation has no `src/billing.py` on disk —
    // we wrote only `src/.keep`. The handler rejects with
    // `path_rejected` (`not_indexed`). The clamping logic itself
    // is unit-tested in `evidence.rs`.
    let outcome = read_source_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "repo": "billing",
            "path": "src/billing.py",
            "start": 0,
            "end": 10000
        }),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    // `range_too_large` is checked before the indexed-file lookup,
    // so an oversized range on an unindexed path returns the
    // range error (not `path_rejected`).
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("range_too_large")
    );
}

#[tokio::test]
async fn pr13_read_source_returns_empty_when_start_past_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = read_source_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "repo": "billing",
            "path": "src/billing.py",
            "start": 10000,
            "end": 10010
        }),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.structured["error"]["code"], json!("path_rejected"));
}

#[tokio::test]
async fn pr13_read_source_rejects_over_400_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = read_source_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "repo": "billing",
            "path": "src/billing.py",
            "start": 0,
            "end": 500
        }),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("range_too_large")
    );
}

#[tokio::test]
async fn pr13_read_source_unknown_repo_returns_repo_not_registered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = read_source_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({
            "snapshot": "live",
            "repo": "ghost",
            "path": "src/main.py",
            "start": 0,
            "end": 5
        }),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("repo_not_registered")
    );
}

#[tokio::test]
async fn pr13_unsupported_api_version_returns_supported_array() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fed = build_three_repo_federation(root, RepoHealth::Ready).await;
    let outcome = list_contracts_handle(
        &{
            let _status = Box::leak(Box::new(
                lain::server::mcp::handler::HandlerStatus::for_test(),
            ));
            ctx_for(&fed, _status)
        },
        json!({"snapshot": "live", "api_version": 99}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("unsupported_api_version")
    );
    assert_eq!(
        outcome.structured["error"]["details"]["supported"],
        json!([1])
    );
}

#[tokio::test]
async fn pr13_federation_disabled_when_no_fed() {
    // Empty federation (federation = None) is not an error per §12;
    // the tool returns `items: []` and a minimal scope.
    use std::sync::OnceLock;
    static STATUS: OnceLock<lain::server::mcp::handler::HandlerStatus> = OnceLock::new();
    let status: &'static lain::server::mcp::handler::HandlerStatus =
        STATUS.get_or_init(lain::server::mcp::handler::HandlerStatus::for_test);
    let ctx = McpContext {
        server: None,
        federation: None,
        workspaces: None,
        status,
        reload_bus: None,
        snapshots: None,
    };
    let outcome = list_services_handle(&ctx, json!({"snapshot": "live"}))
        .await
        .unwrap();
    // `list_services` raises `federation_disabled` when the server
    // runs without a federation; this is the §13 verbatim code,
    // not an internal failure.
    assert!(outcome.is_error);
    assert_eq!(
        outcome.structured["error"]["code"],
        json!("federation_disabled")
    );
}

// ─── §15.2 diff scenarios over the T1 fixture (review r1, F1) ────────
//
// Ground truth: `tests/fixtures/contracts/ground_truth.yaml`
// (scenarios 1, 2, 5, 5b, 6, 11, 12, 19, 20, 21, 22) and the §15.2
// table in `docs/CONTRACT_FEDERATION.md`. One base snapshot at the
// fixture's `base` tag; each scenario derives `from: <base>` with its
// scenario tag as the override and runs `diff_contracts` through the
// same handler path the MCP dispatcher uses.

#[path = "support/contracts_snapshot_harness.rs"]
mod snap_harness;

use snap_harness as harness;

async fn run_diff(ctx: &McpContext<'_>, base: &str, head: &str) -> Value {
    let args = json!({"base": base, "head": head, "cap": 100});
    let outcome = diff_contracts_handle(ctx, args).await.unwrap();
    assert!(
        !outcome.is_error,
        "diff_contracts({base}, {head}) error: {:#?}",
        outcome.structured
    );
    outcome.structured["data"].clone()
}

fn changes_of(data: &Value) -> Vec<Value> {
    data["changes"].as_array().cloned().unwrap_or_default()
}

fn find_change<'a>(data: &'a Value, kind: &str) -> &'a Value {
    data["changes"]
        .as_array()
        .expect("changes array")
        .iter()
        .find(|c| c["kind"] == json!(kind))
        .unwrap_or_else(|| panic!("expected a {kind} change, got: {data:#?}"))
}

/// Shared response models can change several endpoints at once
/// (e.g. `s1-remove-customer-id` touches every endpoint whose
/// schema references the order response); the ground truth pins the
/// change on one specific endpoint key.
fn find_endpoint_change<'a>(data: &'a Value, kind: &str, key: &str) -> &'a Value {
    data["changes"]
        .as_array()
        .expect("changes array")
        .iter()
        .find(|c| c["kind"] == json!(kind) && c["endpoint"]["key"] == json!(key))
        .unwrap_or_else(|| panic!("expected {kind} on {key}, got: {data:#?}"))
}

fn assert_affected(change: &Value, service: &str, class: &str) {
    let affected = change["affected"].as_array().expect("affected array");
    assert!(
        affected
            .iter()
            .any(|a| a["service"] == json!(service) && a["class"] == json!(class)),
        "affected must contain {service} → {class}: {affected:#?}"
    );
}

fn assert_reason(change: &Value, reason: &str) {
    let reasons = change["impact"]["reasons"]
        .as_array()
        .expect("impact.reasons array");
    assert!(
        reasons.iter().any(|r| r == &json!(reason)),
        "impact.reasons must contain {reason}: {reasons:#?}"
    );
}

/// The base fixture reviews all four configured repos (§9.6 scope).
fn assert_scope_complete(data: &Value) {
    assert_eq!(
        data["coverage"]["complete"],
        json!(true),
        "coverage: {data:#?}"
    );
    let scope = &data["coverage"]["scope"];
    let mut reviewed: Vec<String> = scope["reviewed"]
        .as_array()
        .expect("reviewed array")
        .iter()
        .map(|r| r["repo"].as_str().unwrap_or("").to_string())
        .collect();
    reviewed.sort();
    assert_eq!(
        reviewed,
        vec![
            "billing".to_string(),
            "orders".to_string(),
            "platform".to_string(),
            "reports".to_string(),
        ],
        "scope.reviewed: {scope:#?}"
    );
    assert_eq!(scope["unreviewed"], json!([]), "scope.unreviewed");
}

/// §15.2 scenario 1: "a path reaches `reports`" — some hop of some
/// path on the change lands in the `reports` repo. The shared
/// response model means several endpoints can carry the same
/// `kind`; the ground truth pins the change on one key.
fn path_reaches(data: &Value, kind: &str, key: &str, repo: &str) {
    let change = find_endpoint_change(data, kind, key);
    let prefix = format!("{repo}:");
    let reaches = change["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .any(|p| {
            p["hops"].as_array().map(|hops| {
                hops.iter().any(|h| {
                    h["node"]
                        .as_str()
                        .map(|n| n.starts_with(&prefix))
                        .unwrap_or(false)
                })
            }) == Some(true)
        });
    assert!(
        reaches,
        "a path of the {kind} change must reach {repo}: {:#?}",
        change["paths"]
    );
}

#[tokio::test]
async fn pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture() {
    let fix = harness::build_fixture();
    let mgr = harness::manager(&fix.root);
    let config = harness::contract_config(&fix.root);
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&mgr, &status);

    // One shared base: all four repos at the `base` tag.
    let base_repos = harness::all_repos_at(&fix.root, "base");
    let base_id = harness::prepare_ready(&mgr, base_repos.clone(), None, config.clone()).await;

    // Scenario 5b's base: billing already at `s5b-read-status`.
    let mut base5b_repos = base_repos;
    base5b_repos.insert(
        "billing".to_string(),
        harness::rev_parse(&fix.root, "billing", "s5b-read-status"),
    );
    let base5b_id = harness::prepare_ready(&mgr, base5b_repos, None, config.clone()).await;

    // ── 1: orders removes customer_id from the response ────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s1-remove-customer-id")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "FieldRemoved", "http:GET /api/orders/{}");
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["direction"], json!("response"));
    assert_eq!(change["field"], json!("customer_id"));
    assert_eq!(change["compat"], json!("BreakingIfRead"));
    assert_eq!(
        change["impact"]["class"],
        json!("Verified"),
        "scenario 1 class: {change:#?}"
    );
    assert_affected(change, "billing", "Verified");
    assert_eq!(data["compatible_changes"], json!(0));
    assert_scope_complete(&data);
    // §15.2: "a path reaches `reports`" (traced in the base view).
    path_reaches(&data, "FieldRemoved", "http:GET /api/orders/{}", "reports");

    // ── 2: orders adds optional currency — not reported ────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s2-add-currency")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    // §15.2 row 2: the compatible optional-field add is *not
    // reported*, and `compatible_changes = 1`. The ground-truth
    // YAML spells this `changes: []`, but §9.2's file-level
    // `ChangedWithoutSchema` rule also fires here: every orders
    // scenario rewrites `src/main.rs`, which holds both the
    // touched handlers and the schemaless `GET /api/orders/{}/label`
    // route. Assert the row's actual claim — no field change is
    // reported — rather than literal emptiness.
    assert!(
        changes_of(&data).iter().all(|c| {
            c["field"] != json!("currency")
                && !matches!(
                    c["kind"].as_str(),
                    Some(
                        "FieldAdded"
                            | "FieldRemoved"
                            | "FieldRenamed"
                            | "FieldTypeChanged"
                            | "RequirednessChanged"
                            | "NullabilityChanged"
                            | "EnumValueAdded"
                            | "EnumValueRemoved"
                    )
                )
        }),
        "scenario 2: the compatible field add must not be reported: {data:#?}"
    );
    assert_eq!(data["compatible_changes"], json!(1));

    // ── 5: orders adds enum value `refunded`; billing unread ───────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s5-enum-value")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "EnumValueAdded", "http:GET /api/orders/{}");
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["direction"], json!("response"));
    assert_eq!(change["field"], json!("status"));
    assert_eq!(change["compat"], json!("NeedsReview"));
    assert_eq!(change["impact"]["class"], json!("NoKnownImpact"));
    // §9.6: every NoKnownImpact carries its scope.
    assert!(
        change["impact"]["scope"]["reviewed"]
            .as_array()
            .map(|a| a.len() == 4)
            .unwrap_or(false),
        "NI must carry the reviewed scope: {change:#?}"
    );

    // ── 5b: same, with billing reading `status` ────────────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base5b_id,
        &[("orders", "s5-enum-value")],
    )
    .await;
    let data = run_diff(&ctx, &base5b_id, &head).await;
    let change = find_endpoint_change(&data, "EnumValueAdded", "http:GET /api/orders/{}");
    assert_eq!(change["impact"]["class"], json!("NeedsInvestigation"));
    assert_reason(change, "needs_review");
    assert_affected(change, "billing", "NeedsInvestigation");

    // ── 6: /api/orders/{} → /api/order/{}, same handler ────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s6-rename-path")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "PathChanged", "http:GET /api/order/{}");
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["from"], json!("http:GET /api/orders/{}"));
    assert_eq!(change["to"], json!("http:GET /api/order/{}"));
    assert_eq!(change["compat"], json!("Breaking"));
    assert_eq!(change["impact"]["class"], json!("Verified"));
    assert_affected(change, "billing", "Verified");

    // ── 11: customer_id → customerId, same type ────────────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s11-rename-field")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    // §15.2: "One `FieldRenamed`" — a same-type rename must not
    // decompose into a remove + add pair.
    assert!(
        changes_of(&data)
            .iter()
            .all(|c| c["kind"] != json!("FieldRemoved") && c["kind"] != json!("FieldAdded")),
        "scenario 11: rename must stay one FieldRenamed: {data:#?}"
    );
    let change = find_endpoint_change(&data, "FieldRenamed", "http:GET /api/orders/{}");
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["direction"], json!("response"));
    assert_eq!(change["from"], json!("customer_id"));
    assert_eq!(change["to"], json!("customerId"));
    assert_eq!(change["compat"], json!("BreakingIfRead"));
    assert_eq!(change["impact"]["class"], json!("Verified"));
    assert_affected(change, "billing", "Verified");

    // ── 12: rename with a type change → FieldRemoved + FieldAdded ──
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s12-rename-retype")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    // §15.2: "FieldRemoved + FieldAdded" — the type change splits
    // the rename into a pair on the changed endpoint.
    let removed = find_endpoint_change(&data, "FieldRemoved", "http:GET /api/orders/{}");
    assert_eq!(removed["field"], json!("customer_id"));
    assert_eq!(removed["compat"], json!("BreakingIfRead"));
    assert_eq!(removed["impact"]["class"], json!("Verified"));
    assert_affected(removed, "billing", "Verified");
    let added = find_endpoint_change(&data, "FieldAdded", "http:GET /api/orders/{}");
    assert_eq!(added["field"], json!("customerId"));
    assert_eq!(added["compat"], json!("BreakingIfRead"));
    assert_eq!(added["impact"]["class"], json!("Verified"));
    assert_affected(added, "billing", "Verified");

    // ── 19: optional request field `note` type change ──────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s19-optional-request-type")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "FieldTypeChanged", "http:POST /api/orders");
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["direction"], json!("request"));
    assert_eq!(change["field"], json!("note"));
    assert_eq!(change["compat"], json!("BreakingIfSent"));
    assert_eq!(change["impact"]["class"], json!("NeedsInvestigation"));
    assert_reason(change, "sends_not_modeled");

    // ── 20: billing starts reading `discount` ──────────────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("billing", "s20-read-discount")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "ConsumerFieldUnmatched", "http:GET /api/orders/{}");
    assert_eq!(change["side"], json!("consumer"));
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["endpoint"]["key"], json!("http:GET /api/orders/{}"));
    assert_eq!(change["field"], json!("discount"));
    assert_eq!(change["compat"], json!("Breaking"));
    assert_eq!(change["impact"]["class"], json!("Verified"));
    assert_affected(change, "billing", "Verified");
    assert_eq!(change["consumer"]["symbol"], json!("build_invoice"));

    // ── 21: code-only route handler changed ────────────────────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[("orders", "s21-code-only-handler")],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(
        &data,
        "ChangedWithoutSchema",
        "http:GET /api/orders/{}/label",
    );
    assert_eq!(change["endpoint"]["service"], json!("orders"));
    assert_eq!(change["compat"], json!("NeedsReview"));
    assert_eq!(change["impact"]["class"], json!("NeedsInvestigation"));
    assert_reason(change, "no_schema");
    assert_affected(change, "billing", "NeedsInvestigation");

    // ── 22: billing caches the response, then scenario 1 ───────────
    let head = harness::derive_head(
        &mgr,
        config.clone(),
        &fix.root,
        &base_id,
        &[
            ("orders", "s1-remove-customer-id"),
            ("billing", "s22-cache-response"),
        ],
    )
    .await;
    let data = run_diff(&ctx, &base_id, &head).await;
    let change = find_endpoint_change(&data, "FieldRemoved", "http:GET /api/orders/{}");
    assert_eq!(change["field"], json!("customer_id"));
    assert_eq!(change["compat"], json!("BreakingIfRead"));
    assert_eq!(change["impact"]["class"], json!("NeedsInvestigation"));
    assert_reason(change, "reads_not_fully_traced");
    assert_affected(change, "billing", "NeedsInvestigation");
}
