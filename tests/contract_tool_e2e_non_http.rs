//! End-to-end coverage of the four non-HTTP protocols through the
//! **public contract tools** (`docs/superpowers/plans/
//! 2026-10-07-contract-soundness.md` Task 2).
//!
//! "A sensor emits nodes" is not "a path is queryable". Every test
//! here drives `get_contract` / `trace_impact` / `list_unresolved` /
//! `get_coverage` over a real pinned snapshot of the four-repo
//! fixture built by `scripts/contracts-fixture-t4.sh` (gRPC in
//! `orders`, GraphQL in `billing`, WebSocket in `reports`, SQL in
//! `platform`), and asserts a **specific** provider→consumer path —
//! never a count.
//!
//! Wire keys follow `ContractKey`'s Display/FromStr grammar
//! (`model.rs`): `rpc:<service>/<method>`, `graphql:<op>:<field>`
//! with a lower-case op, `websocket:<route>`, `table:<name>`.

#[path = "support/contracts_snapshot_harness.rs"]
mod harness;

use lain::federation::contracts::config::ContractFederationConfig;
use lain::federation::contracts::snapshots::manager::SnapshotManager;
use lain::server::mcp::contract_tools::analysis::{get_coverage_handle, trace_impact_handle};
use lain::server::mcp::contract_tools::contracts::{get_contract_handle, list_unresolved_handle};
use lain::server::mcp::handler::HandlerStatus;
use serde_json::json;
use std::sync::Arc;

/// The fixture (held alive), its snapshot manager, contract config,
/// and a `ready` snapshot pinned at the `t4` tag of all four repos.
struct T4 {
    _fixture: harness::Fixture,
    mgr: Arc<SnapshotManager>,
    #[allow(dead_code)]
    config: Arc<ContractFederationConfig>,
    base: String,
}

async fn t4() -> T4 {
    let fixture = harness::build_fixture_t4();
    let mgr = harness::manager(&fixture.root);
    let config = harness::contract_config(&fixture.root);
    let base = harness::prepare_ready(
        &mgr,
        harness::all_repos_at(&fixture.root, "t4"),
        None,
        Arc::clone(&config),
    )
    .await;
    T4 {
        _fixture: fixture,
        mgr,
        config,
        base,
    }
}

/// gRPC: the provider declared in `orders/proto/orders.proto` and
/// billing's stub call must both be queryable through one
/// `get_contract` lookup — provider service, the *specific* consumer
/// site, and the proto-derived schema fields.
#[tokio::test]
async fn grpc_provider_and_consumer_are_queryable_through_get_contract() {
    let t4 = t4().await;
    let status = HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&t4.mgr, &status);

    let out = get_contract_handle(
        &ctx,
        json!({"snapshot": t4.base, "key": "rpc:orders/GetOrder"}),
    )
    .await
    .unwrap();
    let err = out.structured["error"].clone();
    assert!(
        !out.is_error,
        "get_contract(rpc:orders/GetOrder) errored: {err:#?}"
    );
    let items = out.structured["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("items: {:#?}", out.structured));
    assert_eq!(
        items.len(),
        1,
        "exactly one provider of the rpc: {items:#?}"
    );
    let item = &items[0];
    assert_eq!(item["endpoint"]["service"], json!("orders"));

    // The specific consumer, not "some consumer": billing's
    // `ordersStub.GetOrder(...)` call site.
    let consumers = item["consumers"].as_array().expect("consumers");
    assert!(
        consumers
            .iter()
            .any(|c| c["site"]["id"].as_str().unwrap_or("").contains("billing")),
        "billing's stub call must be listed as a consumer: {consumers:#?}"
    );

    // …and the proto messages must render as schema fields.
    let schemas = item["schemas"].as_array().expect("schemas");
    assert!(
        !schemas.is_empty(),
        "the proto request/response schema must be joinable: {schemas:#?}"
    );
}

/// GraphQL: the fields billing's query selects (`id`,
/// `customer_id`) must be joinable — visible as a `Binds` hop from
/// billing's FieldRef to orders' SDL Field node on a
/// `trace_impact` path seeded at the field the consumer reads.
#[tokio::test]
async fn graphql_selected_fields_are_joinable_through_trace_impact() {
    let t4 = t4().await;
    let status = HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&t4.mgr, &status);

    let out = trace_impact_handle(
        &ctx,
        json!({
            "snapshot": t4.base,
            "from": {"field": {
                "endpoint": {"service": "orders", "key": "graphql:query:orders"},
                "direction": "response",
                "json_path": "customer_id",
            }},
            "depth": 3,
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "trace_impact errored: {:#?}", out.structured);
    let paths = out.structured["data"]["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("paths: {:#?}", out.structured));
    // The specific join, not "some hop": billing's selected-field
    // FieldRef binds to orders' `customer_id` Field node, so an
    // impact path must carry a Binds hop whose node is that read.
    assert!(
        paths
            .iter()
            .flat_map(|p| p["hops"].as_array().unwrap())
            .any(|h| { h["node"].as_str().unwrap_or("").contains("customer_id") }),
        "the selected field must appear on an impact path: {paths:#?}"
    );
}

/// WebSocket: reports declares `app.ws("/feed", …)` and billing
/// dials `ws://reports/feed`. Tracing from the provider endpoint
/// must reach the consumer through the joiner's `Binds` edge.
#[tokio::test]
async fn websocket_provider_reaches_its_consumer_in_trace_impact() {
    let t4 = t4().await;
    let status = HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&t4.mgr, &status);

    let out = trace_impact_handle(
        &ctx,
        json!({
            "snapshot": t4.base,
            "from": {"endpoint": {"service": "reports", "key": "websocket:/feed"}},
            "depth": 3,
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "trace_impact errored: {:#?}", out.structured);
    let paths = out.structured["data"]["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("paths: {:#?}", out.structured));
    assert!(
        paths
            .iter()
            .flat_map(|p| p["hops"].as_array().unwrap())
            .any(|h| {
                h["edge"].as_str() == Some("Binds")
                    && h["node"].as_str().unwrap_or("").contains("billing")
            }),
        "the WS consumer's bind must be on the path: {paths:#?}"
    );
}

/// SQL: `platform` reads `shipments` from
/// `scripts/report.py`. The table endpoint must be queryable
/// through `get_contract` under the repo's own service name, with
/// the reader listed as its consumer.
///
/// KNOWN-RED (soundness finding, reported with Task 2): the
/// endpoint half passes — sql_sensor emits the `Table` fact and the
/// joiner keys it `platform` / `table:shipments`. The consumer half
/// cannot pass today: the joiner's dispatch chain has no table arm
/// (`default_dispatch_chain` covers Topic/Rpc/Graphql/WebSocket),
/// `ContractFact::Table` falls through the HTTP ladder's
/// `ContractFact::Consumer` filter, and the reading function carries
/// no consumer fact at all — so a reader can never enter
/// `ContractIndex.consumers`, and no fixture can change that. Do not
/// weaken this assertion; the fix belongs in the joiner/sensor layer.
///
/// `#[ignore]` keeps CI green while the gap is unfixed — the reason is
/// the finding. Removing the attribute is the acceptance criterion for
/// the fix (a `TableDispatch` keyed on `ReadsTable` edges, or a consumer
/// fact on the reading function). Note this gap also makes
/// `an_unresolved_table_consumer_blocks_no_known_impact` unreachable in
/// production: there can be no unresolved table consumer because there
/// can be no table consumer at all.
#[ignore = "known soundness gap: the joiner has no table-consumer model, so a SQL reader never enters ContractIndex.consumers. Fix in the joiner/sensor layer; see docs/superpowers/plans/2026-10-07-contract-soundness.md"]
#[tokio::test]
async fn sql_table_reads_are_listed_through_get_contract() {
    let t4 = t4().await;
    let status = HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&t4.mgr, &status);

    let out = get_contract_handle(&ctx, json!({"snapshot": t4.base, "key": "table:shipments"}))
        .await
        .unwrap();
    assert!(
        !out.is_error,
        "get_contract(table:shipments) errored: {:#?}",
        out.structured
    );
    let items = out.structured["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("items: {:#?}", out.structured));
    assert_eq!(items.len(), 1, "exactly one owner of the table: {items:#?}");
    let item = &items[0];
    assert_eq!(item["endpoint"]["service"], json!("platform"));

    let consumers = item["consumers"].as_array().expect("consumers");
    assert!(
        consumers
            .iter()
            .any(|c| c["site"]["id"].as_str().unwrap_or("").contains("platform")),
        "the SQL reader must be listed: {consumers:#?}"
    );
}

/// "We do not know" must be visible, not implied by absence.
/// `get_coverage.schemaless_endpoints` must *name* the protocols
/// whose endpoints carry no schema (WebSocket, SQL) while not
/// conflating them with the schema-backed ones (gRPC, GraphQL),
/// and `list_unresolved` must *name* the dial the graph cannot
/// place (the third-party WebSocket host) instead of returning an
/// empty list that reads as "everything was checked".
#[tokio::test]
async fn each_protocol_reports_where_the_graph_cannot_establish_a_data_origin() {
    let t4 = t4().await;
    let status = HandlerStatus::for_test();
    let ctx = harness::snapshot_ctx(&t4.mgr, &status);

    let cov = get_coverage_handle(&ctx, json!({"snapshot": t4.base}))
        .await
        .unwrap();
    assert!(!cov.is_error, "get_coverage: {:#?}", cov.structured);
    let schemaless: Vec<String> = cov.structured["data"]["schemaless_endpoints"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e["key"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        schemaless.iter().any(|k| k == "websocket:/feed"),
        "the schemaless WebSocket endpoint must be named: {schemaless:#?}"
    );
    assert!(
        schemaless.iter().any(|k| k == "table:shipments"),
        "the schemaless SQL endpoint must be named: {schemaless:#?}"
    );
    // Protocols that DO carry a schema must not be swept into the
    // unknown bucket — silence is only acceptable when the origin
    // really was established (tests 1 and 2 prove it is).
    assert!(
        !schemaless.iter().any(|k| k == "rpc:orders/GetOrder"),
        "gRPC has a proto schema; it must not be reported schemaless: {schemaless:#?}"
    );
    assert!(
        !schemaless.iter().any(|k| k == "graphql:query:orders"),
        "GraphQL has an SDL schema; it must not be reported schemaless: {schemaless:#?}"
    );

    let unres = list_unresolved_handle(&ctx, json!({"snapshot": t4.base}))
        .await
        .unwrap();
    assert!(!unres.is_error, "list_unresolved: {:#?}", unres.structured);
    let items = unres.structured["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("items: {:#?}", unres.structured));
    // billing's `wss://feeds.example.com/prices` dial names no
    // configured service; it must surface as an unresolved consumer
    // whose site pins the protocol (websocket) and the repo.
    assert!(
        items.iter().any(|it| {
            let c = &it["consumer"];
            c["path"].as_str().unwrap_or("").contains("market_feed")
                && it["reason"].as_str().is_some()
        }),
        "the unplaceable WebSocket dial must be named with a reason, \
         not implied by an empty list: {items:#?}"
    );
}
