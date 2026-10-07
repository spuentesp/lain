//! Regression tests for the command-center contract fixes.
//!
//! Each test drives the four-repository T1 fixture built by
//! `scripts/contracts-fixture.sh` (via
//! `tests/support/contracts_snapshot_harness.rs`) and asserts on the
//! wire payload an external "architecture command center" would read
//! over HTTP or MCP stdio. These are the defects a live integration
//! review found; every test failed before its fix.
//!
//! NOTE: the fixture reproduces a ContractKey collision that a
//! three-repo federation does not — `billing` and `orders` both own
//! `topic:kafka/orders.created`, which is what made
//! `get_service.uses[].endpoint` mis-attribute every consumer.

#[path = "support/contracts_snapshot_harness.rs"]
mod harness;

use lain::server::mcp::contract_tools::services::get_service_handle;
use serde_json::{json, Value};
use std::sync::Arc;

/// Snapshot the whole T1 fixture at `rev` (e.g. `"base"`) and return
/// the snapshot id.
async fn snapshot_at(
    mgr: &Arc<lain::server::federation::contracts::snapshots::SnapshotManager>,
    config: &Arc<lain::server::federation::contracts::config::ContractFederationConfig>,
    root: &std::path::Path,
    rev: &str,
) -> String {
    let commits = harness::all_repos_at(root, rev);
    harness::prepare_ready(mgr, commits, None, Arc::clone(config)).await
}

async fn get_service(
    mgr: &Arc<lain::server::federation::contracts::snapshots::SnapshotManager>,
    snapshot: &str,
    service: &str,
) -> Value {
    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(mgr, &status);
    let outcome = get_service_handle(&ctx, json!({"snapshot": snapshot, "service": service, "limit": 50}))
        .await
        .unwrap();
    assert!(
        !outcome.is_error,
        "get_service({service}) errored: {:#?}",
        outcome.structured
    );
    outcome.structured["data"].clone()
}

/// `(site_id, "service/key")` for every consumer `use` of a service.
fn use_endpoints(data: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in data["consumers"].as_array().cloned().unwrap_or_default() {
        for u in c["uses"].as_array().cloned().unwrap_or_default() {
            let site = u["site"]["id"].as_str().unwrap_or_default().to_string();
            let key = u["endpoint"]["key"].as_str().unwrap_or_default().to_string();
            let svc = u["endpoint"]["service"].as_str().unwrap_or_default().to_string();
            out.push((site, format!("{svc}/{key}")));
        }
    }
    out
}

/// `get_service` must attribute each consumer to the endpoint it
/// actually calls, not to the alphabetically-first endpoint whose
/// `ContractKey` string collides with the provider's key set.
#[tokio::test]
async fn get_service_attributes_each_consumer_to_its_real_endpoint() {
    let fix = harness::build_fixture();
    let mgr = harness::manager(&fix.root);
    let config = harness::contract_config(&fix.root);
    let base = snapshot_at(&mgr, &config, &fix.root, "base").await;

    let data = get_service(&mgr, &base, "orders").await;
    let seen = use_endpoints(&data);
    assert!(!seen.is_empty(), "orders must have consumers: {data:#?}");

    // The caller at `GET /api/orders/{}:21` is bound to orders'
    // templated route — not to billing's topic endpoint, which shares
    // the key string `topic:kafka/orders.created` with orders' own
    // topic and therefore sorts first in the Endpoint table.
    let (_, endpoint) = seen
        .iter()
        .find(|(site, _)| site.contains("GET /api/orders/{}"))
        .unwrap_or_else(|| panic!("no consumer site for GET /api/orders/{{}}, got {seen:?}"));
    assert_eq!(
        endpoint, "orders/http:GET /api/orders/{}",
        "consumer mis-attributed; full map: {seen:?}"
    );

    // No row may claim billing's topic endpoint as its target unless
    // the site really is that topic.
    for (site, endpoint) in &seen {
        if endpoint == "billing/topic:kafka/orders.created" {
            assert!(
                site.contains("kafka/orders.created"),
                "site {site} attributed to billing's topic endpoint"
            );
        }
    }
}

/// `owners` must always be present on a `used_by` entry — `[]` means
/// "CODEOWNERS declares no owner for this path", whereas a missing key
/// means "owners could not be loaded", which a client must not silently
/// read as "no owner". And it must resolve for the repo that owns the
/// entry even when the sensor scanned a worktree directory named after
/// a commit SHA.
#[tokio::test]
async fn owners_are_always_present_and_resolve_by_repo_id() {
    let fix = harness::build_fixture();
    let mgr = harness::manager(&fix.root);
    let config = harness::contract_config(&fix.root);
    let base = snapshot_at(&mgr, &config, &fix.root, "base").await;

    let data = get_service(&mgr, &base, "orders").await;
    let mut owner_values: Vec<Value> = Vec::new();
    for c in data["consumers"].as_array().cloned().unwrap_or_default() {
        for u in c["uses"].as_array().cloned().unwrap_or_default() {
            for e in u["used_by"].as_array().cloned().unwrap_or_default() {
                let owners = e
                    .get("owners")
                    .unwrap_or_else(|| panic!("used_by entry is missing 'owners': {e:#?}"));
                assert!(owners.is_array(), "owners must be an array: {e:#?}");
                owner_values.push(owners.clone());
            }
        }
    }
    assert!(!owner_values.is_empty(), "expected at least one used_by entry");

    // billing ships a CODEOWNERS in the T1 fixture; at least one entry
    // must resolve to a real owner.
    assert!(
        owner_values
            .iter()
            .any(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)),
        "billing has CODEOWNERS but no used_by entry carried an owner: {owner_values:?}"
    );
}

/// `handlers[].repo` must be the repository that owns the provider,
/// never a sensor name. (Covered at unit level in
/// `analysis::tests::handler_repo_is_the_provider_repo_not_a_sensor_name`;
/// this pins it end-to-end over the T1 fixture.)
#[tokio::test]
async fn handlers_repo_names_a_real_repository() {
    let fix = harness::build_fixture();
    let mgr = harness::manager(&fix.root);
    let config = harness::contract_config(&fix.root);
    let base = snapshot_at(&mgr, &config, &fix.root, "base").await;
    let head_repos = {
        let mut r = harness::all_repos_at(&fix.root, "base");
        r.insert(
            "orders".to_string(),
            harness::rev_parse(&fix.root, "orders", "s21-code-only-handler"),
        );
        r
    };
    let head = harness::prepare_ready(&mgr, head_repos, None, Arc::clone(&config)).await;

    let status = lain::server::mcp::handler::HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&mgr, &status);
    let outcome = lain::server::mcp::contract_tools::analysis::diff_contracts_handle(
        &ctx,
        json!({"base": base, "head": head, "cap": 100}),
    )
    .await
    .unwrap();
    let data = &outcome.structured["data"];

    let change = data["changes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|c| c["kind"] == json!("ChangedWithoutSchema"))
        .unwrap_or_else(|| panic!("no ChangedWithoutSchema change: {data:#?}"));

    let handlers = change["handlers"].as_array().expect("handlers array");
    let h = &handlers[0];
    assert_eq!(h["file"], json!("src/orders/label.rs"));
    assert_eq!(h["symbol"], json!("get_order_label"));
    assert_eq!(
        h["repo"], json!("orders"),
        "handlers[].repo must name the provider's repository"
    );
    assert_ne!(h["repo"], json!("http-sensor"));
}
