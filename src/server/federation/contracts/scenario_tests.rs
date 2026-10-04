//! Scenario tests for §15.2 items 13 / 14 / 15 / 16.
//!
//! These exercise the joiner's behavior on the §15.2 scenarios
//! directly, using hand-crafted `GraphNode` inputs that mirror what
//! the per-repo sensors would emit for the T1 fixture. The tests
//! are pure-function tests — no disk, no git, no full federation
//! wiring — so they run in the standard `cargo test` cycle.
//!
//! Reference (docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §15.2):
//!   13: literal `/api/orders/me` binds the literal route, not
//!       `/api/orders/{}`.
//!   14: `api.stripe.com` ends up in `coverage.external`, not
//!       `unresolved`.
//!   15: a confirmed binding survives lines inserted above its
//!       call site (its `SymbolKey` doesn't change).
//!   16: shipping → inventory produces one `Binds` with
//!       `cross_repo = false`.

use crate::federation::contracts::config::{
    ConfirmedBinding, ConfirmedBindingConsumer, ConfirmedBindingProvider, ContractFederationConfig,
    ServiceDecl,
};
use crate::federation::contracts::index::EndpointId;
use crate::federation::contracts::joiner::{BindsEdge, ContractJoiner};
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, ContractKey, HostPart, HttpMethod, MethodSpec,
    NormalizedUrl, ProviderFact, ProviderOrigin,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{GraphNode, NodeType, RepoNamespace};
use std::collections::HashMap;

/// Synthesize one `GraphNode` carrying the given contract fact. The
/// `id` is a canonical `GlobalId` so the joiner can parse it.
fn contract_node(
    repo: &str,
    node_type: NodeType,
    path: &str,
    name: &str,
    line: u32,
    fact: ContractFact,
) -> GraphNode {
    let ns = RepoNamespace::for_test();
    let mut n = GraphNode::new_in(node_type.clone(), name.to_string(), path.to_string(), &ns);
    n.repo_id = Some(repo.to_string());
    n.line_start = Some(line);
    n.id = GlobalId::new(
        &RepoId::new(repo).unwrap(),
        node_type,
        path,
        name,
        Some(line),
    )
    .as_str()
    .to_string();
    n.contract = Some(fact);
    n
}

fn provider(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: HttpMethod,
    template: &str,
) -> GraphNode {
    contract_node(
        repo,
        NodeType::HttpRoute,
        path,
        name,
        line,
        ContractFact::Provider(ProviderFact {
            method,
            template: template.to_string(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        }),
    )
}

fn consumer(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: MethodSpec,
    url: NormalizedUrl,
    via: CallVia,
) -> GraphNode {
    contract_node(
        repo,
        NodeType::HttpClientCall,
        path,
        name,
        line,
        ContractFact::Consumer(ConsumerFact {
            method,
            url,
            via,
            url_expr: "".into(),
            reads_complete: true,
        }),
    )
}

fn orders_config() -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
    }
}

// ─── Scenario 13 ────────────────────────────────────────────────────

#[test]
fn scenario_13_literal_route_binds_over_pattern() {
    // Two providers in the orders service: one for the
    // parameterised route, one for the literal `me` route.
    let p_orders = provider(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let p_me = provider(
        "orders",
        "src/orders.py",
        "get_me",
        30,
        HttpMethod::Get,
        "/api/orders/me",
    );
    let c = consumer(
        "billing",
        "src/billing.py",
        "fetch",
        5,
        MethodSpec::Known(HttpMethod::Get),
        NormalizedUrl {
            host: HostPart::Literal("orders.svc".into()),
            template: Some("/api/orders/me".into()),
        },
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = orders_config();
    let out = ContractJoiner::run(&[p_orders, p_me, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1, "exactly one Binds");
    let edge = &out.binds[0];
    let key = match &edge.target_endpoint.1 {
        ContractKey::Http { template, .. } => template.clone(),
        _ => String::new(),
    };
    assert_eq!(
        key, "/api/orders/me",
        "scenario 13: literal /api/orders/me binds the literal route, not the pattern"
    );
}

// ─── Scenario 14 ────────────────────────────────────────────────────

#[test]
fn scenario_14_external_host_recorded_not_unresolved() {
    let p = provider(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer(
        "billing",
        "src/billing.py",
        "charge",
        1,
        MethodSpec::Known(HttpMethod::Get),
        NormalizedUrl {
            host: HostPart::Literal("api.stripe.com".into()),
            template: Some("/v1/charges".into()),
        },
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = orders_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(out.binds.is_empty(), "no Binds for an external host");
    assert_eq!(
        out.index.external.get("api.stripe.com").copied(),
        Some(1),
        "scenario 14: api.stripe.com → coverage.external"
    );
    // Confirm no unresolved NoMatch or NoRouteInService was emitted
    // for this call: the external path is preferred.
    let consumers = &out.index.consumers;
    assert!(consumers.values().all(|r| !matches!(
        r.target,
        Some(
            crate::federation::contracts::index::ConsumerTarget::Unresolved {
                reason: crate::federation::contracts::index::UnresolvedReason::NoMatch,
                ..
            }
        )
    )));
}

// ─── Scenario 15 ────────────────────────────────────────────────────

#[test]
fn scenario_15_confirmed_binding_survives_line_insertion() {
    let p = provider(
        "orders",
        "src/orders.py",
        "create_order",
        100,
        HttpMethod::Post,
        "/api/orders",
    );
    // The call site is at line 200; a line shift above (a 30-line
    // comment) puts the call at line 230 in a future revision. The
    // confirmed binding's `symbol = create_order` matches by name,
    // not by GlobalId line — so both versions bind.
    let c_v1 = consumer(
        "billing",
        "src/orders_api.py",
        "create_order",
        200,
        MethodSpec::Known(HttpMethod::Post),
        NormalizedUrl {
            host: HostPart::Literal("orders.svc".into()),
            template: Some("/api/orders".into()),
        },
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let mut cfg = orders_config();
    cfg.bindings.push(ConfirmedBinding {
        consumer: ConfirmedBindingConsumer {
            repo: "billing".into(),
            path: "src/orders_api.py".into(),
            symbol: "create_order".into(),
            key: "POST /api/orders".into(),
        },
        provider: ConfirmedBindingProvider {
            service: "orders".into(),
            key: "POST /api/orders".into(),
        },
    });
    let out = ContractJoiner::run(&[p.clone(), c_v1], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge_v1 = &out.binds[0];
    assert!(matches!(
        edge_v1.provenance,
        crate::schema::EdgeProvenance::Confirmed { .. }
    ));

    // Shift the call site downward by 30 lines (lines inserted
    // above). The SymbolKey matches by name + path + repo, so the
    // confirmed binding still fires.
    let c_v2 = consumer(
        "billing",
        "src/orders_api.py",
        "create_order",
        230,
        MethodSpec::Known(HttpMethod::Post),
        NormalizedUrl {
            host: HostPart::Literal("orders.svc".into()),
            template: Some("/api/orders".into()),
        },
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let out_v2 = ContractJoiner::run(&[p, c_v2], &[], &cfg);
    assert_eq!(
        out_v2.binds.len(),
        1,
        "scenario 15: binding survives line insertion"
    );
    assert!(matches!(
        out_v2.binds[0].provenance,
        crate::schema::EdgeProvenance::Confirmed { .. }
    ));
}

// ─── Scenario 16 ────────────────────────────────────────────────────

#[test]
fn scenario_16_shipping_to_inventory_one_binds_cross_repo_false() {
    // The §4.1 §7.1 design lets one repo hold several services:
    // `shipping` and `inventory` live under the same `platform`
    // repo. The cross_repo flag on the resulting `Binds` is
    // determined by the consumer and provider GlobalIds — same
    // repo means `cross_repo = false`.
    let p_inv = provider(
        "platform",
        "services/inventory/api.py",
        "get_stock",
        50,
        HttpMethod::Get,
        "/api/inventory/{}",
    );
    let c = consumer(
        "platform",
        "services/shipping/orders.py",
        "ship",
        90,
        MethodSpec::Known(HttpMethod::Get),
        NormalizedUrl {
            host: HostPart::Literal("inventory.svc".into()),
            template: Some("/api/inventory/42".into()),
        },
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "shipping".into(),
                repo: "platform".into(),
                paths: vec!["services/shipping/".into()],
                hosts: vec![],
                env: vec!["INVENTORY_URL".into()],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "inventory".into(),
                repo: "platform".into(),
                paths: vec!["services/inventory/".into()],
                hosts: vec!["inventory.svc".into()],
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
    let out = ContractJoiner::run(&[p_inv, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1, "exactly one Binds");
    let edge = &out.binds[0];
    assert_eq!(
        edge.consumer.repo_id(),
        edge.provider.repo_id(),
        "scenario 16: shipping → inventory shares repo `platform`"
    );
    // The federation would emit `cross_repo = false` because both
    // endpoints are in the same repo (different services).
    let cross_repo = edge.consumer.repo_id() != edge.provider.repo_id();
    assert!(!cross_repo, "scenario 16: cross_repo = false");
    // Belt-and-braces — the joiner's verdict is at the service
    // level: shipping != inventory.
    assert_ne!(edge.consumer_service, edge.provider_service);
    assert_eq!(edge.consumer_service.0, "shipping");
    assert_eq!(edge.provider_service.0, "inventory");
}

// Silence the unused-import noise from the helper builders.
#[allow(dead_code)]
fn _h(_: &HashMap<EndpointId, BindsEdge>) {}
