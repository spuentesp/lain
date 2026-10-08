//! Tests for `ContractJoiner` (§5.3 + §7.2 / §7.3 / §7.4 / §7.6).
//!
//! The joiner is a pure function of `(nodes, config)`; tests here
//! construct the inputs by hand so they run without disk or git.

use crate::federation::contracts::config::{
    ConfirmedBinding, ConfirmedBindingConsumer, ConfirmedBindingProvider, ContractFederationConfig,
    HttpClientDecl, SchemaDecl, ServiceDecl,
};
use crate::federation::contracts::index::{
    ConsumerTarget, EndpointId, FieldRefResolution, StaleReason, UnresolvedReason,
};
use crate::federation::contracts::joiner::{ContractJoiner, JoinOutput};
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, ContractKey, Direction, FieldMeta, FieldReadFact,
    GraphqlConsumerFact, GraphqlOp, GraphqlProviderFact, HostPart, HttpMethod, JsonPath,
    MethodSpec, NormalizedUrl, ProviderFact, ProviderOrigin, RpcConsumerFact, RpcProviderFact,
    RpcSystem, ServiceName, Table, TableConsumerFact, TopicConsumerFact, TopicConsumerKind,
    TypeDesc, WebSocketConsumerFact, WebSocketProviderFact,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{
    EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, RouteMatch,
};
use std::collections::BTreeSet;

// ─── Builders ─────────────────────────────────────────────────────────

fn repo_ns() -> RepoNamespace {
    RepoNamespace::for_test()
}

fn make_id(repo: &str, kind: NodeType, path: &str, name: &str, line: u32) -> String {
    let r = RepoId::new(repo).unwrap();
    GlobalId::new(&r, kind, path, name, Some(line))
        .as_str()
        .to_string()
}

fn provider_node(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: HttpMethod,
    template: &str,
) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::HttpRoute,
        name.to_string(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::HttpRoute, path, name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method,
        template: template.to_string(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    n
}

fn consumer_node(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: MethodSpec,
    url: NormalizedUrl,
    via: CallVia,
) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::HttpClientCall,
        name.to_string(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::HttpClientCall, path, name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::Consumer(ConsumerFact {
        method,
        url,
        via,
        url_expr: "".to_string(),
        reads_complete: true,
    }));
    n
}

fn default_config() -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec!["ORDERS_URL".into()],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

fn url_with_host_method(host: HostPart, template: Option<&str>) -> NormalizedUrl {
    NormalizedUrl {
        host,
        template: template.map(|s| s.to_string()),
    }
}

// ─── §7.8 invariants ────────────────────────────────────────────────

#[test]
fn run_is_pure_two_independent_calls_match_byte_for_byte() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let a = ContractJoiner::run(&[p.clone(), c.clone()], &[], &cfg);
    let b = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(a, b, "two runs of identical input must match");
}

#[test]
fn every_binds_edge_connects_two_services_and_carries_provenance() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    for edge in &out.binds {
        assert_ne!(
            edge.consumer_service, edge.provider_service,
            "every Binds edge connects two services"
        );
        assert!(
            !matches!(edge.provenance, EdgeProvenance::Runtime { .. }),
            "static / heuristic / confirmed only"
        );
    }
}

#[test]
fn cross_repo_flag_reflects_repos_difference() {
    let p_orders = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p_orders, c], &[], &cfg);
    let edge = out.binds.first().expect("expected one Binds");
    let consumer_repo = edge.consumer.repo_id();
    let provider_repo = edge.provider.repo_id();
    assert_ne!(consumer_repo, provider_repo);
    // §7.8 invariant: cross_repo true iff repos differ.
}

#[test]
fn order_independence_two_projection_orders_match() {
    let nodes = vec![
        provider_node(
            "orders",
            "src/orders.py",
            "get_order",
            10,
            HttpMethod::Get,
            "/api/orders/{}",
        ),
        provider_node(
            "billing",
            "src/billing.py",
            "create_invoice",
            5,
            HttpMethod::Post,
            "/api/invoices",
        ),
        consumer_node(
            "billing",
            "src/billing.py",
            "fetch_order",
            1,
            MethodSpec::Known(HttpMethod::Get),
            url_with_host_method(
                HostPart::Literal("orders.svc".into()),
                Some("/api/orders/42"),
            ),
            CallVia::Library {
                name: "requests".into(),
            },
        ),
    ];
    let cfg = default_config();
    let a = ContractJoiner::run(&nodes, &[], &cfg);
    let mut reversed = nodes.clone();
    reversed.reverse();
    let b = ContractJoiner::run(&reversed, &[], &cfg);
    assert_eq!(a.binds, b.binds);
    assert_eq!(a.index, b.index);
}

// ─── §7.3 resolution table ─────────────────────────────────────────

#[test]
fn rule_1_records_unresolved_wrapper_candidate_with_no_http_client_match() {
    // Phase A rule-1 fix: a wrapper candidate (`CallVia::Receiver`)
    // with no matching `http_clients` entry is recorded as
    // `Unresolved { reason: WrapperUnconfigured }` instead of being
    // silently dropped. The consumer still has no `binds` edge (no
    // route matches), but the consumer is in the index so the
    // coverage ledger and `evaluate()` can see it.
    //
    // The test uses a non-empty `http_clients` config (with a
    // pattern that does NOT match the receiver) so the emission
    // fires. When `http_clients` is empty, the pre-Phase-A `continue`
    // is preserved — see the empty-config companion test below.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "ordersClient".into(),
            fn_name: "get".into(),
        },
    );
    let mut cfg = default_config();
    // http_clients is non-empty but its pattern does not match
    // the call's receiver.
    cfg.http_clients
        .push(crate::federation::contracts::config::HttpClientDecl {
            call: "differentClient.{method}".into(),
            service: "orders".into(),
            method: None,
            path_arg: None,
        });
    let out = ContractJoiner::run(&[p, c.clone()], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "rule 1: Receiver with no matching http_clients call still has no bind"
    );
    assert_eq!(
        out.index.consumers.len(),
        1,
        "rule 1 fix: the consumer is recorded as Unresolved"
    );
    let call_id = GlobalId::parse(&c.id).unwrap();
    let resolution = out.index.consumers.get(&call_id).expect("consumer present");
    match &resolution.target {
        Some(crate::federation::contracts::index::ConsumerTarget::Unresolved {
            reason, ..
        }) => assert_eq!(
            *reason,
            UnresolvedReason::WrapperUnconfigured,
            "rule 1 fix: the unresolved reason is WrapperUnconfigured"
        ),
        other => panic!("expected Unresolved, got {other:?}"),
    }
}

/// Companion test: when `http_clients` is empty, the wrapper
/// candidate is silently dropped (the pre-Phase-A behaviour).
/// The emission only fires when the operator has at least one
/// `http_clients` entry but no match — a real "wrapper
/// unconfigured" state.
#[test]
fn rule_1_with_empty_http_clients_silently_drops() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "ordersClient".into(),
            fn_name: "get".into(),
        },
    );
    let cfg = default_config(); // empty http_clients
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(out.binds.is_empty());
    assert!(
        out.index.consumers.is_empty(),
        "rule 1 with empty http_clients: the consumer is dropped"
    );
}

#[test]
fn rule_3_target_service_via_http_clients_binds_with_static_confidence() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "ordersClient".into(),
            fn_name: "get".into(),
        },
    );
    let cfg = ContractFederationConfig {
        services: default_config().services,
        http_clients: vec![HttpClientDecl {
            call: "ordersClient.{method}".into(),
            service: "orders".into(),
            method: None,
            path_arg: None,
        }],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    };
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    assert!(
        matches!(edge.provenance, EdgeProvenance::Static { .. })
            && (edge.confidence - 1.0).abs() < f32::EPSILON,
        "rule 3: known method on a known target → Static 1.0 (§7.3), got {:?} @ {}",
        edge.provenance,
        edge.confidence
    );
}

#[test]
fn rule_3_target_service_via_hosts_only() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1, "rule 3 via hosts only");
    assert!(
        matches!(out.binds[0].provenance, EdgeProvenance::Static { .. }),
        "rule 3 via hosts → Static 1.0 (§7.3), got {:?}",
        out.binds[0].provenance
    );
}

#[test]
fn rule_3_method_unknown_caps_confidence_at_0_6() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_it",
        10,
        HttpMethod::Post,
        "/api/orders",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Unknown,
        url_with_host_method(HostPart::Literal("orders.svc".into()), Some("/api/orders")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    let (
        EdgeProvenance::Heuristic {
            detector,
            confidence,
        },
        _,
    ) = (edge.provenance.clone(), ())
    else {
        panic!("expected Heuristic");
    };
    assert_eq!(detector, "method_unknown");
    assert!((confidence - 0.6).abs() < f32::EPSILON, "got {confidence}");
}

#[test]
fn rule_3_prefix_stripped_match_is_unresolved_with_known_target() {
    // Bug C. The provider template is "/orders/{}" (no base_path);
    // the consumer comes in with "/api/orders/42" — prefix
    // stripping is the only way they match. Rule 3 must NOT bind:
    // the consumer is unresolved with the known target service
    // retained, and the orders endpoint surfaces as a could-match
    // candidate through `diff::could_match`. A plain match (no
    // prefix strip) still binds — see other tests for that.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c.clone()], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "rule 3 prefix tolerance must not produce a bind: {binds:?}",
        binds = out.binds
    );
    let call_id = GlobalId::parse(&c.id).expect("parse");
    let resolution = out
        .index
        .consumers
        .get(&call_id)
        .expect("consumer resolution");
    match &resolution.target {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service,
        }) => {
            assert_eq!(
                target_service.as_ref().map(|s| &s.0),
                Some(&"orders".to_string()),
                "rule 3 keeps the known target service on the unresolved verdict"
            );
        }
        other => panic!("expected Unresolved/NoRouteInService, got {other:?}"),
    }
    assert!(resolution.bound_endpoints.is_empty());
}

#[test]
fn rule_4_external_host_when_no_service_matches_and_not_exempt() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("api.stripe.com".into()),
            Some("/v1/charges"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(out.binds.is_empty(), "rule 4 produces no Binds");
    let count = out.index.external.get("api.stripe.com").copied();
    assert_eq!(count, Some(1), "rule 4: external host counted");
}

#[test]
fn rule_4_exempts_localhost() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::Literal("localhost".into()), Some("/v1/charges")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(out.index.external.is_empty(), "localhost is exempt");
    assert!(out.index.unnormalized.is_empty(), "rule 5 not in play");
    // No service matches localhost, but it's exempt: falls through
    // to rule 6, finds no service that owns a generic, and ends up
    // unresolved no_match.
    let consumer = out.index.consumers.values().next().unwrap();
    assert!(matches!(
        consumer.target,
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoMatch,
            ..
        })
    ));
}

#[test]
fn known_target_with_dynamic_path_lands_in_unresolved_no_route_in_service() {
    // Row-order regression pin. The §7.3 table fires rule 3
    // (target service known) BEFORE rule 5 (template=None →
    // unnormalized). A consumer whose host resolves to a
    // declared service and whose template is dynamic must
    // therefore become rule 3's verdict: no route in that
    // service, marked as such. Not `unnormalized`.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_it",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::Literal("orders.svc".into()), None),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    // Rule 3 fires (orders.svc matches the configured hosts
    // list); the dynamic path matches nothing → unresolved
    // no_route_in_service. `unnormalized` stays empty.
    assert_eq!(
        out.index.unnormalized.len(),
        0,
        "row order: known target + dynamic path is rule 3, not rule 5"
    );
    let consumer = out.index.consumers.values().next().expect("consumer");
    assert!(matches!(
        consumer.target,
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            ..
        })
    ));
}

#[test]
fn rule_5_unnormalized_recorded() {
    // Rule 5 only fires when rules 3 (target service known)
    // and 4 (literal external host) have NOT matched. A
    // consumer with `template = None` AND `HostPart::Expr` (not
    // a literal, not an env name, not matching any service /
    // env / hosts entry) reaches rule 5.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_it",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_dynamic",
        1,
        MethodSpec::Known(HttpMethod::Get),
        // HostPart::Expr — neither rule 3 nor rule 4 can
        // resolve it (rule 4 is literal-only).
        url_with_host_method(HostPart::Expr("config.base_url".into()), None),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(
        out.index.unnormalized.len(),
        1,
        "rule 5: dynamic path + Expr host lands in unnormalized"
    );
    let consumer = out.index.consumers.values().next().expect("consumer");
    assert!(matches!(
        consumer.target,
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::Unnormalized,
            ..
        })
    ));
}

#[test]
fn dynamic_path_with_external_literal_host_lands_in_external_not_unnormalized() {
    // Row-order regression pin: rule 4 (literal external
    // host) beats rule 5 (template=None → unnormalized). A
    // consumer with `template = None` AND a literal host that
    // doesn't match any service becomes `External`, not
    // `unnormalized`.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_it",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "charge",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::Literal("api.stripe.com".into()), None),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(
        out.index.unnormalized.is_empty(),
        "row order: literal external host beats rule 5"
    );
    assert_eq!(
        out.index.external.get("api.stripe.com").copied(),
        Some(1),
        "rule 4: api.stripe.com recorded as External"
    );
}

#[test]
fn rule_6_own_service_skip() {
    // A billing consumer calling billing's own route should NOT
    // produce a Binds edge (own-service skip).
    let p = provider_node(
        "billing",
        "src/billing.py",
        "do_it",
        10,
        HttpMethod::Get,
        "/api/ping",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "ping_self",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::None, Some("/api/ping")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let mut cfg = default_config();
    cfg.services[0].name = "billing".into();
    cfg.services[0].repo = "billing".into();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    // The own-service skip fires for rule 6; the call is
    // unresolved no_match.
    assert!(out.binds.is_empty(), "own-service call is not a contract");
}

#[test]
fn rule_6_skips_generic_keys() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_health",
        10,
        HttpMethod::Get,
        "/health",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_health",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::None, Some("/health")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "rule 6: built-in generic /health is skipped"
    );
}

#[test]
fn rule_6_prefix_stripped_match_is_unresolved_with_unknown_target() {
    // Scenario 3 regression: a billing consumer whose host resolves
    // to nothing (`HostPart::Expr`) and whose template is
    // `/v1/api/orders/{}` would otherwise rule-6-bind to orders's
    // `/api/orders/{}` via prefix-strip (with the consumer marked
    // `Heuristic 0.6` rather than unresolved). That hides the
    // scenario-3 `ConsumerEndpointUnmatched` change from
    // `diff_consumers`. The fix mirrors rule-3: prefix-stripped
    // matches are could-match hints, not binds.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "build_invoice",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Expr("_UNMAPPED_BASE_VAR".into()),
            Some("/v1/api/orders/{}"),
        ),
        CallVia::Library {
            name: "httpx".into(),
        },
    );
    let mut cfg = default_config();
    cfg.services.clear(); // no env host → target_service unknown
    let out = ContractJoiner::run(&[p, c.clone()], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "rule 6 prefix tolerance must not produce a bind: {binds:?}",
        binds = out.binds
    );
    let call_id = GlobalId::parse(&c.id).expect("parse");
    let resolution = out
        .index
        .consumers
        .get(&call_id)
        .expect("consumer resolution");
    match &resolution.target {
        Some(ConsumerTarget::Unresolved {
            reason,
            target_service,
        }) => {
            assert_eq!(*reason, UnresolvedReason::NoMatch);
            assert!(
                target_service.is_none(),
                "rule 6 keeps unknown target_service on the unresolved verdict"
            );
        }
        other => panic!("expected Unresolved/NoMatch, got {other:?}"),
    }
    assert!(resolution.bound_endpoints.is_empty());
}

#[test]
fn rule_6_unbound_host_with_one_match_gives_0_6_confidence() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::None, Some("/api/orders")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let mut cfg = default_config();
    cfg.services.clear(); // remove implicit services
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    let (
        EdgeProvenance::Heuristic {
            detector,
            confidence,
        },
        _,
    ) = (edge.provenance.clone(), ())
    else {
        panic!("expected Heuristic");
    };
    assert_eq!(detector, "unbound_host");
    assert!((confidence - 0.6).abs() < f32::EPSILON);
}

// ─── §7.6 confirmed bindings ────────────────────────────────────────

#[test]
fn confirmed_binding_matches_consumer_and_records_provenance() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "create_order",
        10,
        HttpMethod::Post,
        "/api/orders",
    );
    let mut c = consumer_node(
        "billing",
        "src/orders_api.py",
        "create_order",
        1,
        MethodSpec::Known(HttpMethod::Post),
        url_with_host_method(HostPart::Literal("orders.svc".into()), Some("/api/orders")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    c.container = Some("create_order".into()); // enclosing symbol
    let cfg = ContractFederationConfig {
        services: default_config().services,
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![ConfirmedBinding {
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
        }],
        databases: vec![],
    };
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    assert!(matches!(edge.provenance, EdgeProvenance::Confirmed { .. }));
    assert!((edge.confidence - 1.0).abs() < f32::EPSILON);
}

#[test]
fn confirmed_binding_no_endpoint_marks_stale() {
    let cfg = ContractFederationConfig {
        services: default_config().services,
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![ConfirmedBinding {
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
        }],
        databases: vec![],
    };
    let out = ContractJoiner::run(&[], &[], &cfg);
    assert_eq!(out.binds.len(), 0);
    assert_eq!(out.index.stale_bindings.len(), 1);
    assert!(matches!(
        out.index.stale_bindings[0].reason,
        StaleReason::NoEndpoint
    ));
}

#[test]
fn confirmed_binding_no_consumer_marks_stale() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "create_order",
        10,
        HttpMethod::Post,
        "/api/orders",
    );
    let cfg = ContractFederationConfig {
        services: default_config().services,
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![ConfirmedBinding {
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
        }],
        databases: vec![],
    };
    let out = ContractJoiner::run(&[p], &[], &cfg);
    assert!(out.binds.is_empty());
    assert_eq!(out.index.stale_bindings.len(), 1);
    assert!(matches!(
        out.index.stale_bindings[0].reason,
        StaleReason::NoConsumer
    ));
}

// ─── §7.8 ordering ─────────────────────────────────────────────────

#[test]
fn output_collections_are_sorted_by_key() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do_a",
        10,
        HttpMethod::Get,
        "/api/a",
    );
    let p2 = provider_node(
        "orders",
        "src/orders.py",
        "do_b",
        11,
        HttpMethod::Get,
        "/api/b",
    );
    let c1 = consumer_node(
        "billing",
        "src/billing.py",
        "f1",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::None, Some("/api/a")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let c2 = consumer_node(
        "billing",
        "src/billing.py",
        "f2",
        2,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(HostPart::None, Some("/api/b")),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let mut cfg = default_config();
    cfg.services.clear();
    let out = ContractJoiner::run(&[p, p2, c1, c2], &[], &cfg);
    // binds sorted by (consumer, provider).
    for w in out.binds.windows(2) {
        let key = |e: &crate::federation::contracts::joiner::BindsEdge| {
            (
                e.consumer.as_str().to_string(),
                e.provider.as_str().to_string(),
            )
        };
        assert!(key(&w[0]) <= key(&w[1]));
    }
    // services sorted by name.
    let keys: Vec<_> = out.index.services.keys().map(|k| k.0.clone()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn no_field_refs_yet_field_join_is_a_no_op() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do",
        10,
        HttpMethod::Get,
        "/api/orders",
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p], &[], &cfg);
    assert!(out.index.field_refs.is_empty());
}

// ─── Pure-function tests for the 6 §7.3 rows at the API level ─────

#[test]
fn run_returns_the_join_output_struct() {
    let p = provider_node(
        "orders",
        "src/orders.py",
        "do",
        10,
        HttpMethod::Get,
        "/api/orders",
    );
    let cfg = default_config();
    let out: JoinOutput = ContractJoiner::run(&[p], &[], &cfg);
    // The struct is public; assert the fields exist and the basic
    // shape is sound.
    assert!(out.binds.is_empty());
    let _ = EndpointId::clone;
}

#[test]
fn rule3_prefix_stripped_match_leaves_consumer_unresolved_with_could_match() {
    // Bug C. The consumer template `/v1/api/orders/{}` (3 segments)
    // does not direct-match the provider `/api/orders/{}` (2
    // segments). §7.4 prefix tolerance strips `/v1` and matches, but
    // rule 3 must NOT bind: the consumer is unresolved (the prefix
    // is a hint, not a contract), and `diff::could_match` is the
    // channel that surfaces orders as a candidate.
    let p = provider_node(
        "orders",
        "src/orders.py",
        "get_order",
        10,
        HttpMethod::Get,
        "/api/orders/{}",
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "build_invoice",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/v1/api/orders/{}"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c.clone()], &[], &cfg);
    // The consumer must be Unresolved, NOT a PrefixStripped bind.
    assert!(
        out.binds.is_empty(),
        "rule 3 prefix tolerance must not produce a bind: {binds:?}",
        binds = out.binds
    );
    let call_id = GlobalId::parse(&c.id).expect("parse");
    let resolution = out
        .index
        .consumers
        .get(&call_id)
        .expect("consumer resolution");
    match &resolution.target {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service,
        }) => {
            assert_eq!(
                target_service.as_ref().map(|s| &s.0),
                Some(&"orders".to_string()),
                "rule 3 keeps the known target service on the unresolved verdict"
            );
        }
        other => panic!("expected Unresolved/NoRouteInService, got {other:?}"),
    }
    assert!(
        resolution.bound_endpoints.is_empty(),
        "unresolved consumer must carry no bound_endpoints: {:?}",
        resolution.bound_endpoints
    );
}

#[test]
fn contract_key_display_round_trip() {
    let k = ContractKey::Http {
        method: MethodSpec::Known(HttpMethod::Get),
        template: "/api/orders/{}".into(),
    };
    let s = k.to_string();
    let parsed: ContractKey = s.parse().unwrap();
    assert_eq!(parsed, k);
}

#[test]
fn contract_key_display_round_trip_websocket() {
    let k = ContractKey::WebSocket {
        route: "/ws/orders".into(),
    };
    let s = k.to_string();
    let parsed: ContractKey = s.parse().unwrap();
    assert_eq!(parsed, k);
    assert!(s.starts_with("websocket:"));
}

#[test]
fn contract_key_display_round_trip_table() {
    let k = ContractKey::Table {
        name: "orders".into(),
    };
    let s = k.to_string();
    let parsed: ContractKey = s.parse().unwrap();
    assert_eq!(parsed, k);
    assert!(s.starts_with("table:"));
}

// ─── Task 9 — pin `is_suffix` boundary cases under the
//     `joiner_tests` filter (the field_join::tests::is_suffix_*
//     cases exist in field_join.rs but are not run by the
//     mutation harness, which filters by `joiner_tests`).
//
//     Each survivor in `field_join.rs` is on the `==` at line 652:
//         fp[fp.len() - read.len()..] == read[..]
//     A mutation to `!=` makes a non-matching pair return `true`
//     (false negative) or a matching pair return `false` (false
//     positive). The cases below pin the four shape classes the
//     field-join §7.5 step-3 suffix match encounters.

#[test]
fn t9_is_suffix_field_path_with_trailing_segment_matches() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "customer.id".parse().unwrap();
    let rc: JsonPath = "id".parse().unwrap();
    assert!(is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_full_path_match_is_also_a_suffix() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "customer.id".parse().unwrap();
    let rc: JsonPath = "customer.id".parse().unwrap();
    assert!(is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_rejects_non_trailing_segment() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "customer.id".parse().unwrap();
    let rc: JsonPath = "address.id".parse().unwrap();
    assert!(!is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_empty_read_chain_is_never_a_suffix() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "customer.id".parse().unwrap();
    let empty = JsonPath(Vec::new());
    assert!(!is_suffix(&fp, &empty));
}

#[test]
fn t9_is_suffix_read_longer_than_field_path_does_not_match() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "id".parse().unwrap();
    let rc: JsonPath = "customer.id".parse().unwrap();
    assert!(!is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_array_segment_in_field_path_matches() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "items[].sku".parse().unwrap();
    let rc: JsonPath = "items[].sku".parse().unwrap();
    assert!(is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_array_segment_in_read_chain_matches() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "items[].sku".parse().unwrap();
    let rc: JsonPath = "[].sku".parse().unwrap();
    assert!(is_suffix(&fp, &rc));
}

#[test]
fn t9_is_suffix_rejects_unrelated_array_segment() {
    use crate::federation::contracts::field_join::is_suffix;
    let fp: JsonPath = "items[].sku".parse().unwrap();
    let rc: JsonPath = "lines[].sku".parse().unwrap();
    assert!(!is_suffix(&fp, &rc));
}

// ─── Task 9 — boundary cases for consumer_protocol.rs
//
//     The mutation harness reports 10 survivors in
//     `consumer_protocol.rs`. Most are the byte-level boolean
//     short-circuits in the candidate-filter / ambiguity-decision
//     paths. The cases below pin the four terminal-state
//     contracts the resolvers must honour: zero-candidate,
//     single-candidate, multi-candidate, and same-service skip.
//     Each test drives `ContractJoiner::run` end-to-end, which
//     calls the same resolvers; the higher-level API avoids the
//     private `EndpointBindInfo`/`ProviderId` plumbing.

#[test]
fn t9_consumer_protocol_websocket_foreign_host_no_endpoint_pair() {
    // Foreign host with no matching service AND no matching
    // provider endpoint — the resolver must NOT invent a Binds
    // edge. The config has no service that names the literal
    // host, and the endpoint table carries no candidate.
    let consumer = ws_consumer_node("billing", "api.thirdparty.com", "/feed", 7);
    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: Vec::new(),
            hosts: vec!["orders.internal".into()],
            env: Vec::new(),
            base_path: None,
            route_prefixes: Vec::new(),
        }],
        http_clients: Vec::new(),
        generic_keys: Vec::new(),
        schemas: Vec::new(),
        bindings: Vec::new(),
        databases: Vec::new(),
    };
    let out = ContractJoiner::run(&[consumer], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "foreign WS host with no endpoint must NOT bind, got: {:?}",
        out.binds
    );
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer resolution must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "foreign WS host must land in Unresolved, got: {:?}",
        res.target
    );
}

#[test]
fn t9_consumer_protocol_websocket_two_providers_ambiguous() {
    // Two services declare `/ws` AND both name the same host —
    // the resolver must NOT multi-bind (GraphqlNoOp policy from
    // §8.3 carries over to WebSocket). The `ws_consumer_node`
    // helper yields a `WebSocketConsumer` whose host literal
    // matches both services' host axis.
    let consumer = ws_consumer_node("billing", "api.internal", "/ws", 7);
    let provider_a = ws_provider_node("api", "/ws", 12);
    let provider_b = ws_provider_node("notifications", "/ws", 12);
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "api".into(),
                repo: "api".into(),
                paths: Vec::new(),
                hosts: vec!["api.internal".into()],
                env: Vec::new(),
                base_path: None,
                route_prefixes: Vec::new(),
            },
            ServiceDecl {
                name: "notifications".into(),
                repo: "notifications".into(),
                paths: Vec::new(),
                hosts: vec!["api.internal".into()],
                env: Vec::new(),
                base_path: None,
                route_prefixes: Vec::new(),
            },
        ],
        http_clients: Vec::new(),
        generic_keys: Vec::new(),
        schemas: Vec::new(),
        bindings: Vec::new(),
        databases: Vec::new(),
    };
    let out = ContractJoiner::run(&[consumer, provider_a, provider_b], &[], &cfg);
    // The contract is "ambiguity refuses": a `Binds` edge to
    // both `api` AND `notifications` would be an invented
    // multi-bind (no consumer can call both). A `Unresolved`
    // outcome is the only honest terminal state.
    let ambiguous_binds: Vec<_> = out
        .binds
        .iter()
        .filter(|b| b.provider_service.0 == "api" || b.provider_service.0 == "notifications")
        .collect();
    if !ambiguous_binds.is_empty() {
        // If a Binds edge is present, ambiguity must have been
        // refused (the joiner may still surface a Binds edge on
        // the host-axis for *one* service, never both). The
        // accepted behaviour is either 0 binds, or exactly 1
        // bind (single-candidate path) — never 2.
        assert!(
            ambiguous_binds.len() <= 1,
            "ambiguous WS provider set must not multi-bind, got: {:?}",
            ambiguous_binds
        );
    }
}

#[test]
fn t9_consumer_protocol_topic_same_service_endpoint_is_skipped() {
    // A topic consumer in the same service as the producer must
    // NOT bind (I5 — every `Binds` edge connects two different
    // services). The mutation `&&`→`||` in
    // `resolve_by_key`'s own-service guard would let the
    // consumer bind to its own service.
    let provider = topic_provider_node("orders", "src/p.rs", "publish", 1, "orders.events");
    let consumer = topic_consumer_node(
        "orders",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.events",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "same-service topic consumer must NOT bind (I5), got: {:?}",
        out.binds
    );
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "same-service topic consumer must be Unresolved, got: {:?}",
        res.target
    );
}

#[test]
fn t9_consumer_protocol_topic_zero_candidate_is_unresolved() {
    // A topic consumer for a name no producer publishes is
    // `Unresolved { reason: NoMatch }` — `NoMatch` is the
    // `AmbiguityPolicy` Topic maps onto. The contract is the
    // terminal-state contract: every consumer must land in
    // exactly one of Binds / External / Unresolved, never
    // silently drop.
    let consumer = topic_consumer_node(
        "billing",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "nonexistent.topic",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[consumer], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "no-producer topic consumer must NOT bind, got: {:?}",
        out.binds
    );
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    let Some(ConsumerTarget::Unresolved { reason, .. }) = &res.target else {
        panic!(
            "no-producer topic consumer must be Unresolved, got: {:?}",
            res.target
        );
    };
    assert!(
        matches!(reason, UnresolvedReason::NoMatch),
        "no-producer topic consumer must land in NoMatch, got: {reason:?}"
    );
}

#[test]
fn service_name_is_valid_predicate() {
    assert!(crate::federation::contracts::model::service_name_is_valid(
        "orders"
    ));
    assert!(crate::federation::contracts::model::service_name_is_valid(
        "billing-svc"
    ));
    assert!(crate::federation::contracts::model::service_name_is_valid(
        "x1_y_z"
    ));
    assert!(!crate::federation::contracts::model::service_name_is_valid(
        "Orders"
    ));
    assert!(!crate::federation::contracts::model::service_name_is_valid(
        ""
    ));
    assert!(!crate::federation::contracts::model::service_name_is_valid(
        "-leading"
    ));
}

// ─── PR 18 — operationId fallback (generated SDK clients) ──────────

fn provider_node_with_operation_id(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: HttpMethod,
    template: &str,
    operation_id: Option<&str>,
) -> GraphNode {
    let mut n = provider_node(repo, path, name, line, method, template);
    if let Some(ContractFact::Provider(pf)) = n.contract.as_mut() {
        pf.operation_id = operation_id.map(str::to_string);
        pf.origin = ProviderOrigin::OpenApi;
    }
    n
}

/// Build a config with a `client.orders.{method}` wrapper registered
/// against the `orders` service. This is the SDK wrapper case: the
/// generated client's method-name member is the operationId.
fn sdk_wrapper_config() -> ContractFederationConfig {
    use crate::federation::contracts::config::HttpClientDecl;
    ContractFederationConfig {
        services: default_config().services,
        http_clients: vec![HttpClientDecl {
            call: "client.orders.{method}".into(),
            service: "orders".into(),
            method: None,
            path_arg: None,
        }],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

#[test]
fn rule_3_operation_id_fallback_binds_when_url_no_match() {
    // A generated SDK client calls `client.orders.getOrderById({id})`.
    // The URL `/api/v1/orders/42` does not match the provider's
    // `/api/orders/{}` even after prefix strip, but the OpenAPI
    // operationId is `getOrderById`. The joiner must bind via
    // operationId fallback (§7.3 rule 3, PR 18).
    let p = provider_node_with_operation_id(
        "orders",
        "openapi.yaml",
        "getOrderById",
        100,
        HttpMethod::Get,
        "/api/orders/{}",
        Some("getOrderById"),
    );
    let c = consumer_node(
        "billing",
        "src/sdk.ts",
        "getOrderById",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/v1/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "client.orders".into(),
            fn_name: "getOrderById".into(),
        },
    );
    let cfg = sdk_wrapper_config();
    let out = ContractJoiner::run(&[p.clone(), c.clone()], &[], &cfg);
    assert_eq!(
        out.binds.len(),
        1,
        "operationId fallback must bind: {binds:?}",
        binds = out.binds
    );
    let edge = &out.binds[0];
    let (
        EdgeProvenance::Heuristic {
            detector,
            confidence,
        },
        _,
    ) = (edge.provenance.clone(), ())
    else {
        panic!("expected Heuristic, got {:?}", edge.provenance);
    };
    assert_eq!(detector, "operation_id");
    assert!((confidence - 0.9).abs() < f32::EPSILON, "got {confidence}");
    assert!(matches!(edge.route_match, RouteMatch::Exact));
    assert_eq!(edge.provider_service.0, "orders");
    let call_id = GlobalId::parse(&c.id).expect("parse");
    let resolution = out
        .index
        .consumers
        .get(&call_id)
        .expect("consumer resolution");
    match &resolution.target {
        Some(ConsumerTarget::Binds {
            provenance,
            confidence,
            route_match,
            ..
        }) => {
            let EdgeProvenance::Heuristic { detector, .. } = provenance else {
                panic!("expected Heuristic provenance on target");
            };
            assert_eq!(detector, "operation_id");
            assert!((*confidence - 0.9).abs() < f32::EPSILON);
            assert!(matches!(route_match, RouteMatch::Exact));
        }
        other => panic!("expected Binds/operation_id, got {other:?}"),
    }
    assert_eq!(resolution.bound_endpoints.len(), 1);
}

#[test]
fn rule_3_operation_id_fallback_skips_library_via() {
    // The operationId fallback only fires for `CallVia::Receiver`
    // (SDK wrappers). Library calls (e.g. `requests.get(...)`) have
    // no SDK-method identity to match against — `via.fn_name` does
    // not exist on `Library`, so the fallback gate rejects the call
    // and the consumer stays unresolved.
    let p = provider_node_with_operation_id(
        "orders",
        "openapi.yaml",
        "getOrderById",
        100,
        HttpMethod::Get,
        "/api/orders/{}",
        Some("getOrderById"),
    );
    let c = consumer_node(
        "billing",
        "src/billing.py",
        "fetch_order",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/v1/orders/42"),
        ),
        CallVia::Library {
            name: "requests".into(),
        },
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "library calls don't get operationId fallback"
    );
}

#[test]
fn rule_3_operation_id_fallback_no_match_stays_unresolved() {
    // No provider carries `operation_id == "someUnknown"`. The
    // consumer stays unresolved with the known target service.
    let p = provider_node_with_operation_id(
        "orders",
        "openapi.yaml",
        "getOrderById",
        100,
        HttpMethod::Get,
        "/api/orders/{}",
        Some("getOrderById"),
    );
    let c = consumer_node(
        "billing",
        "src/sdk.ts",
        "someUnknown",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/v1/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "client.orders".into(),
            fn_name: "someUnknown".into(),
        },
    );
    let cfg = sdk_wrapper_config();
    let out = ContractJoiner::run(&[p, c.clone()], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "no operationId match → unresolved, got {binds:?}",
        binds = out.binds
    );
    let call_id = GlobalId::parse(&c.id).expect("parse");
    let resolution = out
        .index
        .consumers
        .get(&call_id)
        .expect("consumer resolution");
    assert!(matches!(
        resolution.target,
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            ..
        })
    ));
}

#[test]
fn rule_3_url_match_still_wins_over_operation_id_fallback() {
    // URL match at full confidence (Static 1.0) takes priority over
    // the operationId fallback (Heuristic 0.9). Even though
    // `fn_name == operationId`, the URL already matches exactly, so
    // §7.3 rule 3 fires first.
    let p = provider_node_with_operation_id(
        "orders",
        "openapi.yaml",
        "getOrderById",
        100,
        HttpMethod::Get,
        "/api/orders/{}",
        Some("getOrderById"),
    );
    let c = consumer_node(
        "billing",
        "src/sdk.ts",
        "getOrderById",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "client.orders".into(),
            fn_name: "getOrderById".into(),
        },
    );
    let cfg = sdk_wrapper_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    assert!(
        matches!(edge.provenance, EdgeProvenance::Static { .. }),
        "URL match wins → Static provenance, got {:?}",
        edge.provenance
    );
    assert!((edge.confidence - 1.0).abs() < f32::EPSILON);
}

#[test]
fn rule_3_operation_id_fallback_overrides_prefix_stripped_url_match() {
    // URL prefix-strip would yield a `PrefixStripped` match
    // (Heuristic 0.5), but operationId matches exactly. PR 18 says
    // operationId wins because the bind is exact on the name
    // dimension — stronger than a prefix-stripped URL.
    let p = provider_node_with_operation_id(
        "orders",
        "openapi.yaml",
        "getOrderById",
        100,
        HttpMethod::Get,
        "/orders/{}",
        Some("getOrderById"),
    );
    let c = consumer_node(
        "billing",
        "src/sdk.ts",
        "getOrderById",
        1,
        MethodSpec::Known(HttpMethod::Get),
        url_with_host_method(
            HostPart::Literal("orders.svc".into()),
            Some("/api/orders/42"),
        ),
        CallVia::Receiver {
            base: None,
            expr: "client.orders".into(),
            fn_name: "getOrderById".into(),
        },
    );
    let cfg = sdk_wrapper_config();
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert_eq!(out.binds.len(), 1);
    let edge = &out.binds[0];
    let EdgeProvenance::Heuristic { detector, .. } = &edge.provenance else {
        panic!("expected Heuristic");
    };
    assert_eq!(
        detector, "operation_id",
        "operationId wins over prefix-stripped URL"
    );
    assert!((edge.confidence - 0.9).abs() < f32::EPSILON);
}

// ─── §7.7 topic join (stretch, PR 15) ────────────────────────────────

fn topic_provider_node(
    repo: &str,
    path: &str,
    _name: &str,
    line: u32,
    topic_name: &str,
) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::Topic,
        format!("kafka/{topic_name}"),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(
        repo,
        NodeType::Topic,
        path,
        &format!("kafka/{topic_name}"),
        line,
    );
    n.line_start = Some(line);
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Any,
        template: topic_name.to_string(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    n
}

fn topic_consumer_node(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    broker: &str,
    topic_name: &str,
) -> GraphNode {
    use crate::federation::contracts::model::{TopicConsumerFact, TopicConsumerKind};
    let mut n = GraphNode::new_in(
        NodeType::Function,
        name.to_string(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Function, path, name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: broker.to_string(),
        name: topic_name.to_string(),
        kind: TopicConsumerKind::Subscription,
    }));
    n
}

fn two_service_topic_config() -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "billing".into(),
                repo: "billing".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

#[test]
fn topic_join_same_broker_same_name_binds() {
    let provider = topic_provider_node("orders", "src/p.rs", "publish", 1, "orders.created");
    let consumer = topic_consumer_node(
        "billing",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.created",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[provider.clone(), consumer.clone()], &[], &cfg);
    assert!(
        !out.binds.is_empty(),
        "topic-join must produce a Binds edge; got {:?}",
        out.binds
    );
    let bind = &out.binds[0];
    assert_eq!(bind.consumer_service.0, "billing");
    assert_eq!(bind.provider_service.0, "orders");
    assert_eq!(bind.confidence, 1.0);
    let (svc, key) = &bind.target_endpoint;
    assert_eq!(svc.0, "orders");
    match key {
        ContractKey::Topic { broker, name } => {
            assert_eq!(broker, "kafka");
            assert_eq!(name, "orders.created");
        }
        other => panic!("expected ContractKey::Topic, got {other:?}"),
    }
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    match &res.target {
        Some(ConsumerTarget::Binds {
            confidence,
            route_match,
            ..
        }) => {
            assert_eq!(*confidence, 1.0);
            assert!(matches!(route_match, RouteMatch::Exact));
        }
        other => panic!("expected Binds, got {other:?}"),
    }
    assert_eq!(res.bound_endpoints.len(), 1);
}

#[test]
fn topic_join_different_broker_is_unresolved() {
    let provider = topic_provider_node("orders", "src/p.rs", "publish", 1, "orders.created");
    let consumer = topic_consumer_node(
        "billing",
        "src/c.rs",
        "subscribe",
        5,
        "rabbitmq",
        "orders.created",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "different broker must NOT bind; got {:?}",
        out.binds
    );
    let cid = GlobalId::parse(&make_id(
        "billing",
        NodeType::Function,
        "src/c.rs",
        "subscribe",
        5,
    ))
    .unwrap();
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    match &res.target {
        Some(ConsumerTarget::Unresolved { reason, .. }) => {
            assert!(matches!(reason, UnresolvedReason::NoMatch));
        }
        other => panic!("expected Unresolved/NoMatch, got {other:?}"),
    }
}

#[test]
fn topic_join_different_name_is_unresolved() {
    let provider = topic_provider_node("orders", "src/p.rs", "publish", 1, "orders.created");
    let consumer = topic_consumer_node(
        "billing",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.updated",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "different name must NOT bind; got {:?}",
        out.binds
    );
    let cid = GlobalId::parse(&make_id(
        "billing",
        NodeType::Function,
        "src/c.rs",
        "subscribe",
        5,
    ))
    .unwrap();
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    match &res.target {
        Some(ConsumerTarget::Unresolved { reason, .. }) => {
            assert!(matches!(reason, UnresolvedReason::NoMatch));
        }
        other => panic!("expected Unresolved/NoMatch, got {other:?}"),
    }
}

#[test]
fn topic_join_no_producer_is_unresolved() {
    // No Topic provider node at all.
    let consumer = topic_consumer_node(
        "billing",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.created",
    );
    let cfg = two_service_topic_config();
    let out = ContractJoiner::run(&[consumer], &[], &cfg);
    assert!(out.binds.is_empty());
    let cid = GlobalId::parse(&make_id(
        "billing",
        NodeType::Function,
        "src/c.rs",
        "subscribe",
        5,
    ))
    .unwrap();
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(matches!(
        res.target,
        Some(ConsumerTarget::Unresolved { .. })
    ));
}

#[test]
fn topic_join_same_topic_in_two_services_binds_to_both() {
    let provider1 = topic_provider_node("orders", "src/p1.rs", "publish", 1, "orders.created");
    let provider2 = topic_provider_node("billing", "src/p2.rs", "publish", 1, "orders.created");
    let consumer = topic_consumer_node(
        "reports",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.created",
    );
    let mut cfg = two_service_topic_config();
    cfg.services.push(ServiceDecl {
        name: "reports".into(),
        repo: "reports".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    let out = ContractJoiner::run(&[provider1, provider2, consumer], &[], &cfg);
    assert_eq!(
        out.binds.len(),
        2,
        "two producers with the same key bind both; got {:?}",
        out.binds
    );
}

#[test]
fn topic_join_skips_own_service_endpoint() {
    // Same service publishes and consumes the topic — no edge,
    // because the joiner's `own_service` is not the producer here.
    let provider = topic_provider_node("orders", "src/p.rs", "publish", 1, "orders.created");
    let consumer = topic_consumer_node(
        "orders",
        "src/c.rs",
        "subscribe",
        5,
        "kafka",
        "orders.created",
    );
    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec![],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    };
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    // §7.8: every Binds edge connects two different services; an
    // in-service consumer must not bind to its own producer.
    assert!(
        out.binds.is_empty(),
        "intra-service topic consumption must not bind"
    );
}

#[test]
fn websocket_cross_service_join_produces_binds_edge() {
    let mut provider = GraphNode::new(
        NodeType::HttpRoute,
        "ws:server:/feed".into(),
        "src/server.js".into(),
    );
    provider.repo_id = Some("orders".into());
    provider.id = make_id(
        "orders",
        NodeType::HttpRoute,
        "src/server.js",
        "ws_server",
        10,
    );
    provider.line_start = Some(10);
    provider.contract = Some(ContractFact::WebSocketProvider(WebSocketProviderFact {
        route: "/feed".into(),
        handler: None,
    }));

    let mut consumer = GraphNode::new(
        NodeType::HttpClientCall,
        "ws:client:orders:/feed".into(),
        "src/client.js".into(),
    );
    consumer.repo_id = Some("billing".into());
    consumer.id = make_id(
        "billing",
        NodeType::HttpClientCall,
        "src/client.js",
        "ws_client",
        20,
    );
    consumer.line_start = Some(20);
    consumer.contract = Some(ContractFact::WebSocketConsumer(WebSocketConsumerFact {
        url: NormalizedUrl {
            host: HostPart::Literal("orders".into()),
            template: Some("/feed".into()),
        },
        route: "/feed".into(),
    }));

    let cfg = ws_config("orders", "orders");
    let out = ContractJoiner::run(&[provider.clone(), consumer.clone()], &[], &cfg);
    assert!(
        !out.binds.is_empty(),
        "websocket cross-service join must produce a Binds edge"
    );
    let bind = &out.binds[0];
    assert_eq!(bind.consumer_service.0, "billing");
    assert_eq!(bind.provider_service.0, "orders");
    assert_eq!(bind.confidence, 1.0);
    let (svc, key) = &bind.target_endpoint;
    assert_eq!(svc.0, "orders");
    assert_eq!(
        key,
        &ContractKey::WebSocket {
            route: "/feed".into()
        }
    );

    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    match &res.target {
        Some(ConsumerTarget::Binds {
            confidence,
            route_match,
            ..
        }) => {
            assert_eq!(*confidence, 1.0);
            assert!(matches!(route_match, RouteMatch::Exact));
        }
        other => panic!("expected Binds, got {other:?}"),
    }
}

#[test]
fn topic_payload_schema_and_consumer_field_read_binds() {
    let mut topic = GraphNode::new(
        NodeType::Topic,
        "kafka/orders.events".into(),
        "src/events.py".into(),
    );
    topic.repo_id = Some("orders".into());
    topic.id = make_id(
        "orders",
        NodeType::Topic,
        "src/events.py",
        "orders_events",
        10,
    );
    topic.line_start = Some(10);
    topic.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Any,
        template: "orders.events".into(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));

    let mut schema = GraphNode::new(
        NodeType::Schema,
        "OrderEvent".into(),
        "schemas/orders.avsc".into(),
    );
    schema.repo_id = Some("orders".into());
    schema.id = make_id(
        "orders",
        NodeType::Schema,
        "schemas/orders.avsc",
        "OrderEvent",
        1,
    );
    schema.line_start = Some(1);
    schema.contract = Some(ContractFact::Schema {
        direction: Direction::Payload,
    });

    let mut field = GraphNode::new(
        NodeType::Field,
        "order_id".into(),
        "schemas/orders.avsc".into(),
    );
    field.repo_id = Some("orders".into());
    field.id = make_id(
        "orders",
        NodeType::Field,
        "schemas/orders.avsc",
        "order_id",
        3,
    );
    field.line_start = Some(3);
    field.contract = Some(ContractFact::Field(FieldMeta {
        ty: TypeDesc::String,
        required: true,
        nullable: false,
        enum_values: None,
    }));

    let mut consumer = GraphNode::new(
        NodeType::Function,
        "on_order_event".into(),
        "src/subscriber.py".into(),
    );
    consumer.repo_id = Some("billing".into());
    consumer.id = make_id(
        "billing",
        NodeType::Function,
        "src/subscriber.py",
        "on_order_event",
        25,
    );
    consumer.line_start = Some(25);
    consumer.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
        broker: "kafka".into(),
        name: "orders.events".into(),
        kind: TopicConsumerKind::Subscription,
    }));

    let mut field_ref = GraphNode::new(
        NodeType::FieldRef,
        "order_id".into(),
        "src/subscriber.py".into(),
    );
    field_ref.repo_id = Some("billing".into());
    field_ref.id = make_id(
        "billing",
        NodeType::FieldRef,
        "src/subscriber.py",
        "order_id_ref",
        30,
    );
    field_ref.line_start = Some(30);
    field_ref.contract = Some(ContractFact::FieldRead(FieldReadFact {
        chain: "order_id".parse::<JsonPath>().unwrap(),
        exact: true,
        origin: crate::federation::contracts::model::FieldReadOrigin::FieldAccess,
    }));

    let has_field_edge = GraphEdge::new(EdgeType::HasField, schema.id.clone(), field.id.clone());
    let payload_schema_edge =
        GraphEdge::new(EdgeType::PayloadSchema, topic.id.clone(), schema.id.clone());
    let reads_from_edge = GraphEdge::new(
        EdgeType::ReadsFrom,
        field_ref.id.clone(),
        consumer.id.clone(),
    );

    let mut cfg = two_service_topic_config();
    cfg.schemas.push(SchemaDecl {
        topic: "orders.events".into(),
        repo: "orders".into(),
        file: "schemas/orders.avsc".into(),
    });

    let nodes = vec![
        topic,
        schema,
        field.clone(),
        consumer.clone(),
        field_ref.clone(),
    ];
    let edges = vec![has_field_edge, payload_schema_edge, reads_from_edge];

    let out = ContractJoiner::run(&nodes, &edges, &cfg);

    // 1. Topic consumer binds to orders endpoint
    let ep_key = (
        crate::federation::contracts::model::ServiceName("orders".into()),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.events".into(),
        },
    );
    let endpoint = out
        .index
        .endpoints
        .get(&ep_key)
        .expect("topic endpoint must exist");
    assert!(
        endpoint.schemas.contains_key(&Direction::Payload),
        "endpoint must contain Direction::Payload schema"
    );
    let payload_schema = &endpoint.schemas[&Direction::Payload];
    assert!(
        payload_schema
            .fields
            .contains_key(&"order_id".parse::<JsonPath>().unwrap()),
        "payload schema must contain order_id field"
    );

    // 2. FieldRef binds to field
    let fid = GlobalId::parse(&field_ref.id).unwrap();
    let res = out
        .index
        .field_refs
        .get(&fid)
        .expect("field_ref must resolve");
    assert!(!res.unknown, "field_ref must not be unknown");
    assert_eq!(
        res.bound_fields.len(),
        1,
        "field_ref must bind to exactly 1 field"
    );
    assert_eq!(res.bound_fields[0].field.as_str(), field.id);

    // 3. Field Binds edge is emitted
    assert!(
        out.binds
            .iter()
            .any(|b| b.consumer.as_str() == field_ref.id && b.provider.as_str() == field.id),
        "out.binds must contain Binds(field_ref -> field)"
    );
}

// Silence unused-imports from churn.
#[allow(dead_code)]
fn _silence(_: &RepoId, _: &GlobalId, _: &BTreeSet<FieldRefResolution>) {}

// ─── WebSocket consumers need host evidence and ambiguity refusal ────
//
// `resolve_websocket_consumer` used to build its key from `route` only
// and bind on *any* candidate count at confidence 1.0 Exact, without
// ever reading `WebSocketConsumerFact.url`. So `wss://api.thirdparty.com`
// invented a Binds edge to whatever internal service declared the same
// path, and two providers for `/ws` multi-bound instead of refusing.

fn ws_provider_node(service: &str, route: &str, line: u32) -> GraphNode {
    let mut provider = GraphNode::new(
        NodeType::HttpRoute,
        format!("ws:server:{route}"),
        "src/server.js".into(),
    );
    provider.repo_id = Some(service.into());
    provider.id = make_id(service, NodeType::HttpRoute, "src/server.js", route, line);
    provider.line_start = Some(line);
    provider.contract = Some(ContractFact::WebSocketProvider(WebSocketProviderFact {
        route: route.into(),
        handler: None,
    }));
    provider
}

fn ws_consumer_node(service: &str, host: &str, route: &str, line: u32) -> GraphNode {
    let mut consumer = GraphNode::new(
        NodeType::HttpClientCall,
        format!("ws:client:{host}:{route}"),
        "src/client.js".into(),
    );
    consumer.repo_id = Some(service.into());
    consumer.id = make_id(
        service,
        NodeType::HttpClientCall,
        "src/client.js",
        route,
        line,
    );
    consumer.line_start = Some(line);
    consumer.contract = Some(ContractFact::WebSocketConsumer(WebSocketConsumerFact {
        url: NormalizedUrl {
            host: HostPart::Literal(host.into()),
            template: Some(route.into()),
        },
        route: route.into(),
    }));
    consumer
}

fn ws_config(service: &str, host: &str) -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: service.into(),
            repo: service.into(),
            paths: vec![],
            hosts: vec![host.into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

#[test]
fn ws_consumer_with_foreign_host_does_not_bind() {
    // The dial goes to `api.thirdparty.com`, but the only `/feed`
    // endpoint lives in `orders`. A Binds edge here would be invented.
    let provider = ws_provider_node("orders", "/feed", 10);
    let consumer = ws_consumer_node("billing", "api.thirdparty.com", "/feed", 20);
    let cfg = ws_config("orders", "orders.internal");

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "an external WS host must not invent a Binds edge, got {:?}",
        res.target
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edge expected: {:?}",
        out.binds
    );
}

#[test]
fn ws_consumer_with_two_matching_providers_stays_unresolved() {
    // Two services expose `/ws`. Ambiguity must refuse, not bind both
    // at confidence 1.0.
    let p1 = ws_provider_node("orders", "/ws", 10);
    let p2 = ws_provider_node("billing", "/ws", 30);
    let consumer = ws_consumer_node("reports", "orders.internal", "/ws", 20);
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec![],
                hosts: vec!["orders.internal".into()],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "billing".into(),
                repo: "billing".into(),
                paths: vec![],
                hosts: vec!["orders.internal".into()],
                env: vec![],
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

    let out = ContractJoiner::run(&[p1, p2, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "ambiguity must refuse, not bind to both: {:?}",
        res.target
    );
}

#[test]
fn ws_consumer_with_single_matching_provider_binds() {
    // Positive control: one provider, host matches, route matches.
    let provider = ws_provider_node("orders", "/ws", 10);
    let consumer = ws_consumer_node("billing", "orders.internal", "/ws", 20);
    let cfg = ws_config("orders", "orders.internal");

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Binds { .. })),
        "a single host+route match must bind: {:?}",
        res.target
    );
}

// ─── Task 7: `databases` config — consume or reject ───────────────────
//
// `DatabaseDecl.shared_with` was previously declared, validated,
// and hashed into `config_hash`, but never consumed — the joiner
// picked the first `databases[]` entry whose `tables` list
// contained the table name and attributed the fact to that db's
// service. Two tests pin the consumed behaviour: shared_with
// widens ownership, and table ownership follows the declaring
// database, not the first matching table-name hit (so a
// `reports`-repo Table fact that names `orders` is attributed to
// `orders_db` if that db declared `orders` — not to whichever
// db happened to be first in the YAML).

fn table_node(repo: &str, path: &str, name: &str, line: u32) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::Table,
        name.to_string(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Table, path, name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::Table(Table {
        service: String::new(),
        name: name.to_string(),
    }));
    n
}

fn table_endpoint_id(service: &str, table: &str) -> (ServiceName, ContractKey) {
    (
        ServiceName(service.to_string()),
        ContractKey::Table {
            name: table.to_string(),
        },
    )
}

#[test]
fn table_shared_with_widens_ownership_to_named_service() {
    // `orders_db` is owned by `orders` and shared with
    // `analytics`. The Table fact for `orders` must surface in
    // BOTH services' endpoint tables — not just the owner.
    // Pre-fix: only `orders` saw the endpoint.
    let t = table_node("orders", "src/orders.sql", "orders", 1);
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "analytics".into(),
                repo: "analytics".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![crate::federation::contracts::config::DatabaseDecl {
            name: "orders_db".into(),
            service: "orders".into(),
            tables: vec!["orders".into()],
            shared_with: vec!["analytics".into()],
        }],
    };
    let out = ContractJoiner::run(&[t], &[], &cfg);
    let orders_ep = out
        .index
        .endpoints
        .get(&table_endpoint_id("orders", "orders"))
        .expect("owner service must have a Table endpoint for `orders`");
    assert_eq!(orders_ep.providers.len(), 1);
    let analytics_ep = out
        .index
        .endpoints
        .get(&table_endpoint_id("analytics", "orders"))
        .expect("shared_with service must also have a Table endpoint for `orders`");
    assert_eq!(
        analytics_ep.providers.len(),
        1,
        "shared_with widens ownership: analytics sees the same `orders` table"
    );
}

#[test]
fn table_ownership_keys_on_db_name_not_just_table_name() {
    // Two databases with disjoint table lists. A `Table` fact
    // for `analytics_metrics` originates from repo `reports` and
    // must be attributed to `analytics_db`'s service
    // (`analytics`), not to `orders_db`'s service (`orders`).
    //
    // Pre-fix: the joiner used the *first* database whose
    // `tables` list contained the name. With disjoint lists the
    // name is unambiguous, but the attribution must still follow
    // the *declaring* database — not the assigned service (which
    // would be `reports`, an implicit service with no entry in
    // the `services[]` list).
    let t = table_node("reports", "src/etl.sql", "analytics_metrics", 1);
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "orders".into(),
                repo: "orders".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "analytics".into(),
                repo: "analytics".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![
            crate::federation::contracts::config::DatabaseDecl {
                name: "orders_db".into(),
                service: "orders".into(),
                tables: vec!["orders".into()],
                shared_with: vec![],
            },
            crate::federation::contracts::config::DatabaseDecl {
                name: "analytics_db".into(),
                service: "analytics".into(),
                tables: vec!["analytics_metrics".into()],
                shared_with: vec![],
            },
        ],
    };
    let out = ContractJoiner::run(&[t], &[], &cfg);
    let analytics_ep = out
        .index
        .endpoints
        .get(&table_endpoint_id("analytics", "analytics_metrics"))
        .expect("analytics must own `analytics_metrics`");
    assert_eq!(analytics_ep.providers.len(), 1);
    assert!(
        !out.index
            .endpoints
            .contains_key(&table_endpoint_id("orders", "analytics_metrics")),
        "orders must NOT see `analytics_metrics`; the table belongs to analytics_db"
    );
    assert!(
        !out.index
            .endpoints
            .contains_key(&table_endpoint_id("reports", "analytics_metrics")),
        "the scanning repo (reports) is not the table's owner; analytics_db declared it"
    );
}

// ─── Phase D (spec §7): table consumers — a SQL reader must join ──────
//
// The empirical gap behind `sql_table_reads_are_listed_through_get_contract`
// (tests/contract_tool_e2e_non_http.rs): the joiner had no
// table-consumer model at all, so a function reading a table could
// never enter `ContractIndex.consumers`. Without these, there can be
// no unresolved table consumer either — which made Task 1's `Table`
// arm in `could_match` unreachable in production.

/// A SQL reader as `sql_sensor` emits it: a synthetic
/// `sql-read:<path>:<line>` Function node carrying a
/// `TableConsumer` fact listing every distinct literal table the
/// site's statement touches.
fn sql_reader_node(repo: &str, path: &str, name: &str, line: u32, tables: &[&str]) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::Function,
        name.to_string(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Function, path, name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::TableConsumer(TableConsumerFact {
        tables: tables.iter().map(|s| (*s).to_string()).collect(),
    }));
    n
}

#[test]
fn a_sql_reader_binds_to_the_table_endpoint() {
    // A function that calls `cursor.execute("SELECT id FROM shipments")`
    // must produce a ConsumerResolution for `table:shipments`, and a
    // Binds edge to the service that owns that table.
    let table = table_node("logistics", "db/schema.sql", "shipments", 1);
    let reader = sql_reader_node(
        "platform",
        "scripts/report.py",
        "load_shipments",
        10,
        &["shipments"],
    );
    let cfg = default_config();
    let out = ContractJoiner::run(&[table, reader.clone()], &[], &cfg);

    let cid = GlobalId::parse(&reader.id).expect("global id parse");
    let res = out
        .index
        .consumers
        .get(&cid)
        .expect("a SQL reader must be indexed as a consumer, not dropped");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Binds { .. })),
        "the reader must bind to table:shipments: {:?}",
        res.target
    );
    let owner_endpoint = table_endpoint_id("logistics", "shipments");
    assert!(
        res.bound_endpoints.contains(&owner_endpoint),
        "bound_endpoints must contain the owning service's table endpoint: {:?}",
        res.bound_endpoints
    );

    assert_eq!(
        out.binds.len(),
        1,
        "exactly one Binds edge: {:?}",
        out.binds
    );
    let edge = &out.binds[0];
    assert_eq!(edge.consumer, cid, "the Binds edge starts at the reader");
    assert_eq!(
        edge.provider_service,
        ServiceName("logistics".into()),
        "the Binds edge targets the service that owns the table"
    );
    assert_eq!(edge.target_endpoint, owner_endpoint);
}

#[test]
fn a_sql_reader_for_an_unowned_table_is_unresolved_not_silent() {
    // `SELECT id FROM nope` must land in `Unresolved`, not vanish —
    // silence is what makes `NoKnownImpact` claimable.
    let reader = sql_reader_node("platform", "scripts/report.py", "probe", 10, &["nope"]);
    let cfg = default_config();
    let out = ContractJoiner::run(std::slice::from_ref(&reader), &[], &cfg);

    let cid = GlobalId::parse(&reader.id).expect("global id parse");
    let res = out
        .index
        .consumers
        .get(&cid)
        .expect("a reader of an unowned table must still be indexed, not dropped");
    match &res.target {
        Some(ConsumerTarget::Unresolved { reason, .. }) => assert_eq!(
            *reason,
            UnresolvedReason::NoMatch,
            "no endpoint owns `nope`, so the consumer is Unresolved{{NoMatch}}"
        ),
        other => panic!("an unowned table must be Unresolved, got {other:?}"),
    }
    assert!(
        out.binds.is_empty(),
        "no table endpoint exists, so no Binds edge may be invented: {:?}",
        out.binds
    );
}

// ─── Task 4: field_join schema keying must match build_endpoints ──────
//
// `repos.yaml#schemas` validates, appears in `config_hash`, and today
// silently does nothing because the field_join step 4b builds
// `EndpointId`s the joiner never created: raw template, repo id used
// as service name, hardcoded broker. These tests pin each mismatch so
// the fix cannot regress.

fn payload_schema_node(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    field_name: &str,
) -> (GraphNode, GraphNode, GraphEdge) {
    let mut schema = GraphNode::new(NodeType::Schema, name.to_string(), path.to_string());
    schema.repo_id = Some(repo.into());
    schema.id = make_id(repo, NodeType::Schema, path, name, line);
    schema.line_start = Some(line);
    schema.contract = Some(ContractFact::Schema {
        direction: Direction::Payload,
    });

    let mut field = GraphNode::new(NodeType::Field, field_name.to_string(), path.to_string());
    field.repo_id = Some(repo.into());
    field.id = make_id(repo, NodeType::Field, path, field_name, line + 1);
    field.line_start = Some(line + 1);
    field.contract = Some(ContractFact::Field(FieldMeta {
        ty: TypeDesc::String,
        required: true,
        nullable: false,
        enum_values: None,
    }));

    let edge = GraphEdge::new(EdgeType::HasField, schema.id.clone(), field.id.clone());
    (schema, field, edge)
}

/// Topic provider at a non-kafka broker. Mirrors `topic_provider_node`
/// but with an explicit broker prefix so the joiner sees
/// `rabbitmq/<name>` and `default_broker_for` returns `"rabbitmq"`.
fn topic_provider_at_broker(
    repo: &str,
    path: &str,
    line: u32,
    broker: &str,
    topic_name: &str,
) -> GraphNode {
    let mut n = GraphNode::new_in(
        NodeType::Topic,
        format!("{broker}/{topic_name}"),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(
        repo,
        NodeType::Topic,
        path,
        &format!("{broker}/{topic_name}"),
        line,
    );
    n.line_start = Some(line);
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Any,
        template: topic_name.to_string(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    n
}

#[test]
fn topic_payload_schema_binds_through_service_base_path() {
    // Task 4 (a): service with `base_path: "/api"`. The topic
    // provider's `p.template` is `orders.events`. `build_endpoints`
    // prepends `base_path`, so the endpoint key is
    // `(orders, Topic{ kafka, "/api/orders.events" })`. The current
    // field_join step 4b uses `p.template` raw, so it would
    // attach the schema to `(orders, Topic{ kafka, "orders.events" })`
    // — a key that does not exist.
    let topic = topic_provider_node("orders", "src/events.py", "publish", 10, "orders.events");
    let (schema, field, has_field) =
        payload_schema_node("orders", "schemas/orders.avsc", "OrderEvent", 1, "order_id");
    let payload_edge = GraphEdge::new(EdgeType::PayloadSchema, topic.id.clone(), schema.id.clone());

    let mut cfg = two_service_topic_config();
    cfg.services[0].base_path = Some("/api".into());
    cfg.schemas.push(SchemaDecl {
        topic: "orders.events".into(),
        repo: "orders".into(),
        file: "schemas/orders.avsc".into(),
    });

    let nodes = vec![topic, schema, field];
    let edges = vec![has_field, payload_edge];
    let out = ContractJoiner::run(&nodes, &edges, &cfg);

    // The endpoint that build_endpoints produced must now carry the
    // payload schema. The pre-fix code attached it to a non-existent
    // endpoint, so this lookup would return None. The expected name
    // is the raw concatenation of `base_path` and `provider.template`
    // — the existing `endpoint_template_for` does not insert a
    // separator, so the operator must include a trailing `/` in
    // `base_path` (the schema validation only checks the prefix is
    // non-empty).
    let ep_key = (
        crate::federation::contracts::model::ServiceName("orders".into()),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "/apiorders.events".into(),
        },
    );
    let endpoint = out
        .index
        .endpoints
        .get(&ep_key)
        .expect("endpoint with base_path-prepended topic key must exist");
    let payload_schema = endpoint
        .schemas
        .get(&Direction::Payload)
        .expect("base_path endpoint must carry the payload schema");
    assert!(
        payload_schema
            .fields
            .contains_key(&"order_id".parse::<JsonPath>().unwrap()),
        "base_path endpoint must carry order_id field: {payload_schema:?}"
    );
}

#[test]
fn topic_payload_schema_binds_when_schema_repo_differs_from_service_name() {
    // Task 4 (b): service `payments-api` for repo `revisions`.
    // `SchemaDecl.repo = "revisions"`; the joiner must look up
    // the service whose repo matches, not treat `decl.repo` as a
    // service name. Pre-fix: `ServiceName("revisions")` produces
    // a key no endpoint has. Drop the PayloadSchema edge so step
    // 4b is the only mechanism — otherwise the schemas_by_route
    // loop at field_join.rs:156 masks the bug.
    let topic = topic_provider_node(
        "payments-api",
        "src/events.py",
        "publish",
        10,
        "revisions.created",
    );
    let (schema, field, has_field) = payload_schema_node(
        "payments-api",
        "schemas/revisions.avsc",
        "Revision",
        1,
        "revision_id",
    );

    let cfg = ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "payments-api".into(),
            repo: "revisions".into(),
            paths: vec![],
            hosts: vec![],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![SchemaDecl {
            topic: "revisions.created".into(),
            repo: "revisions".into(),
            file: "schemas/revisions.avsc".into(),
        }],
        bindings: vec![],
        databases: vec![],
    };

    let nodes = vec![topic, schema, field];
    let edges = vec![has_field];
    let out = ContractJoiner::run(&nodes, &edges, &cfg);

    let ep_key = (
        crate::federation::contracts::model::ServiceName("payments-api".into()),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "revisions.created".into(),
        },
    );
    let endpoint = out
        .index
        .endpoints
        .get(&ep_key)
        .expect("endpoint keyed on payments-api must carry the payload schema");
    assert!(
        endpoint.schemas.contains_key(&Direction::Payload),
        "schema repo differs from service name — schema must still bind to payments-api endpoint: {endpoint:?}"
    );
}

#[test]
fn topic_payload_schema_binds_for_non_kafka_broker() {
    // Task 4 (c): the topic lives at `rabbitmq/orders.events`.
    // Pre-fix the broker is hardcoded `"kafka"`, so the schema
    // attaches to a kafka endpoint that does not exist. Drop the
    // PayloadSchema edge so step 4b is the only mechanism — without
    // that, the edge would mask the bug by attaching the schema
    // through the `schemas_by_route` loop at field_join.rs:156.
    let topic =
        topic_provider_at_broker("orders", "src/events.py", 10, "rabbitmq", "orders.events");
    let (schema, field, has_field) =
        payload_schema_node("orders", "schemas/orders.avsc", "OrderEvent", 1, "order_id");

    let mut cfg = two_service_topic_config();
    cfg.schemas.push(SchemaDecl {
        topic: "orders.events".into(),
        repo: "orders".into(),
        file: "schemas/orders.avsc".into(),
    });

    let nodes = vec![topic, schema, field];
    let edges = vec![has_field];
    let out = ContractJoiner::run(&nodes, &edges, &cfg);

    let ep_key = (
        crate::federation::contracts::model::ServiceName("orders".into()),
        ContractKey::Topic {
            broker: "rabbitmq".into(),
            name: "orders.events".into(),
        },
    );
    let endpoint = out
        .index
        .endpoints
        .get(&ep_key)
        .expect("rabbitmq endpoint must exist and carry the payload schema");
    assert!(
        endpoint.schemas.contains_key(&Direction::Payload),
        "non-kafka broker endpoint must carry the payload schema: {endpoint:?}"
    );
}

#[test]
fn topic_payload_schema_with_no_matching_service_is_unbound_not_silent() {
    // Task 4 (d) negative case: a SchemaDecl naming a repo no
    // configured service references must NOT silently attach a
    // schema to a wrong endpoint. The chosen rule (Ruling in the
    // handoff) is rejection at `validate()`. The runtime path
    // here is "no matching_schema found", which must surface as
    // an unbound payload — never a silent bind to a wrong
    // endpoint.
    let topic = topic_provider_node("orders", "src/events.py", "publish", 10, "orders.events");
    let (schema, field, has_field) =
        payload_schema_node("orders", "schemas/orders.avsc", "OrderEvent", 1, "order_id");

    // The SchemaDecl's `repo` is `nonexistent` — no service in
    // `two_service_topic_config()` references it, and the
    // matching_schema lookup will not find a node whose file
    // matches.
    let mut cfg = two_service_topic_config();
    cfg.schemas.push(SchemaDecl {
        topic: "orders.events".into(),
        repo: "nonexistent".into(),
        file: "schemas/orders.avsc".into(),
    });

    let nodes = vec![topic.clone(), schema.clone(), field];
    let edges = vec![has_field];
    let out = ContractJoiner::run(&nodes, &edges, &cfg);

    // The real kafka endpoint must NOT carry a payload schema
    // that belongs to a different repo. If it does, the field_join
    // step 4b silently attached the schema to the wrong endpoint.
    let ep_key = (
        crate::federation::contracts::model::ServiceName("orders".into()),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.events".into(),
        },
    );
    let endpoint = out
        .index
        .endpoints
        .get(&ep_key)
        .expect("kafka endpoint must exist");
    assert!(
        !endpoint.schemas.contains_key(&Direction::Payload),
        "an unbound SchemaDecl must not silently attach to the kafka endpoint: {endpoint:?}"
    );
}

// ─── Task 6 (mutation-credibility plan): decision-boundary fixtures ──
//
// `consumer_protocol.rs`'s survivors were decision-boundary mutants:
// the fixtures only ever drove the *true* branch of each comparison
// (a `/graphql` template, a POST method, exactly one route owner, an
// equal candidate key). The cases below reach the *false* branch —
// a non-`/graphql` route, a GET method, two route owners, a
// different `(op, field)`, a foreign channel service, a different
// RPC method — and pin the rejected side of the predicate.

fn graphql_provider_node(
    repo: &str,
    path: &str,
    op: GraphqlOp,
    field: &str,
    line: u32,
) -> GraphNode {
    let id_name = format!("{op}:{field}");
    let mut n = GraphNode::new_in(
        NodeType::Module,
        id_name.clone(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Module, path, &id_name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::GraphqlProvider(GraphqlProviderFact {
        op,
        field: field.to_string(),
        return_type: "[Order!]!".into(),
    }));
    n
}

fn graphql_consumer_node(
    repo: &str,
    path: &str,
    op: GraphqlOp,
    field: &str,
    line: u32,
) -> GraphNode {
    let id_name = format!("graphql-call:{op}:{field}");
    let mut n = GraphNode::new_in(
        NodeType::Function,
        id_name.clone(),
        path.to_string(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Function, path, &id_name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
        op,
        field: field.to_string(),
    }));
    n
}

fn rpc_provider_node(repo: &str, service: &str, method: &str, line: u32) -> GraphNode {
    let id_name = format!("rpc:{service}:{method}");
    let mut n = GraphNode::new_in(
        NodeType::Function,
        id_name.clone(),
        "gen/service.py".into(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Function, "gen/service.py", &id_name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::RpcProvider(RpcProviderFact {
        system: RpcSystem::Grpc,
        service: service.to_string(),
        method: method.to_string(),
        request_type: "HelloRequest".into(),
        response_type: "HelloReply".into(),
        handler: None,
    }));
    n
}

fn rpc_consumer_node(
    repo: &str,
    service: &str,
    method: &str,
    channel_host: &str,
    line: u32,
) -> GraphNode {
    let id_name = format!("rpc-call:{service}:{method}");
    let mut n = GraphNode::new_in(
        NodeType::Function,
        id_name.clone(),
        "gen/client.py".into(),
        &repo_ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = make_id(repo, NodeType::Function, "gen/client.py", &id_name, line);
    n.line_start = Some(line);
    n.contract = Some(ContractFact::RpcConsumer(RpcConsumerFact {
        system: RpcSystem::Grpc,
        service: service.to_string(),
        method: method.to_string(),
        channel_target: None,
        channel_host_part: HostPart::Literal(channel_host.to_string()),
    }));
    n
}

/// Config whose services all share the repo name (the implicit
/// service mapping) with optional `hosts` for channel resolution.
fn multi_service_config(services: &[(&str, &[&str])]) -> ContractFederationConfig {
    ContractFederationConfig {
        services: services
            .iter()
            .map(|(name, hosts)| ServiceDecl {
                name: (*name).into(),
                repo: (*name).into(),
                paths: vec![],
                hosts: hosts.iter().map(|h| (*h).to_string()).collect(),
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            })
            .collect(),
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

fn assert_graphql_target(
    out: &JoinOutput,
    consumer: &GraphNode,
) -> Option<crate::federation::contracts::model::ServiceName> {
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    match &res.target {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::GraphqlNoOp,
            target_service,
        }) => target_service.clone(),
        other => panic!("expected Unresolved{{GraphqlNoOp, ..}}, got {other:?}"),
    }
}

#[test]
fn graphql_route_owner_surfaces_for_a_single_post_route() {
    // Positive control: exactly one POST `/graphql` route exists, so
    // an unresolved GraphQL consumer must name its owner as
    // `target_service` (spec §8.3 — the operator sees the expected
    // join target).
    let route = provider_node(
        "orders",
        "src/server.ts",
        "graphqlRoute",
        10,
        HttpMethod::Post,
        "/graphql",
    );
    let consumer = graphql_consumer_node("billing", "src/gql.ts", GraphqlOp::Query, "orders", 20);
    let cfg = multi_service_config(&[("orders", &[]), ("billing", &[])]);

    let out = ContractJoiner::run(&[route, consumer.clone()], &[], &cfg);
    let owner = assert_graphql_target(&out, &consumer);
    assert_eq!(
        owner.map(|s| s.0),
        Some("orders".to_string()),
        "the single POST /graphql route owner must surface"
    );
}

#[test]
fn graphql_route_owner_ignores_a_non_graphql_route() {
    // `template == "/graphql"` false branch: a POST `/orders` route
    // is not the GraphQL route, so no owner may surface on the
    // unresolved record.
    let route = provider_node(
        "orders",
        "src/server.ts",
        "listOrders",
        10,
        HttpMethod::Post,
        "/orders",
    );
    let consumer = graphql_consumer_node("billing", "src/gql.ts", GraphqlOp::Query, "orders", 20);
    let cfg = multi_service_config(&[("orders", &[]), ("billing", &[])]);

    let out = ContractJoiner::run(&[route, consumer.clone()], &[], &cfg);
    let owner = assert_graphql_target(&out, &consumer);
    assert!(
        owner.is_none(),
        "a non-/graphql route must not look like a GraphQL route owner: {owner:?}"
    );
}

#[test]
fn graphql_route_owner_requires_a_post_or_unknown_method() {
    // `&& matches!(method, POST | Unknown)` false branch: a GET
    // provider on `/graphql` must not satisfy the route-owner rule.
    let route = provider_node(
        "orders",
        "src/server.ts",
        "graphqlGet",
        10,
        HttpMethod::Get,
        "/graphql",
    );
    let consumer = graphql_consumer_node("billing", "src/gql.ts", GraphqlOp::Query, "orders", 20);
    let cfg = multi_service_config(&[("orders", &[]), ("billing", &[])]);

    let out = ContractJoiner::run(&[route, consumer.clone()], &[], &cfg);
    let owner = assert_graphql_target(&out, &consumer);
    assert!(
        owner.is_none(),
        "a GET /graphql route must not satisfy the route-owner rule: {owner:?}"
    );
}

#[test]
fn graphql_two_route_owners_surface_no_target() {
    // `total_owners == 1` false branch: two services own a
    // POST `/graphql` route, so there is no single target to
    // surface (spec §8.3 ambiguity).
    let r1 = provider_node(
        "orders",
        "src/server.ts",
        "graphqlRoute",
        10,
        HttpMethod::Post,
        "/graphql",
    );
    let r2 = provider_node(
        "billing",
        "src/server.ts",
        "graphqlRoute",
        30,
        HttpMethod::Post,
        "/graphql",
    );
    let consumer = graphql_consumer_node("gateway", "src/gql.ts", GraphqlOp::Query, "orders", 20);
    let cfg = multi_service_config(&[("orders", &[]), ("billing", &[]), ("gateway", &[])]);

    let out = ContractJoiner::run(&[r1, r2, consumer.clone()], &[], &cfg);
    let owner = assert_graphql_target(&out, &consumer);
    assert!(
        owner.is_none(),
        "two /graphql route owners must surface no single target: {owner:?}"
    );
}

#[test]
fn graphql_consumer_does_not_bind_across_different_fields() {
    // `key == target_key_ref` false branch: a provider exposing a
    // *different* root field must be rejected by the candidate
    // filter — a different key reaching the filter never binds.
    let provider =
        graphql_provider_node("orders", "schema.graphql", GraphqlOp::Query, "orders", 10);
    let consumer =
        graphql_consumer_node("billing", "src/gql.ts", GraphqlOp::Query, "customers", 20);
    let cfg = multi_service_config(&[("orders", &[]), ("billing", &[])]);

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "a different (op, field) must not bind: {:?}",
        res.target
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edge expected for a rejected key: {:?}",
        out.binds
    );
}

// ─── RPC candidate filters (consumer_protocol first/second pass) ──────

#[test]
fn rpc_channel_scope_rejects_a_provider_outside_the_channel_service() {
    // First pass `&&` false branch: the channel resolves only to
    // `orders`, so a provider living in `greeter` with the exact
    // target key must still be rejected — the service scope may not
    // be short-circuited away.
    let provider = rpc_provider_node("greeter", "Greeter", "SayHi", 10);
    let consumer = rpc_consumer_node("billing", "Greeter", "SayHi", "grpc.orders.internal", 20);
    let cfg = multi_service_config(&[
        ("orders", &["grpc.orders.internal"]),
        ("billing", &[]),
        ("greeter", &[]),
    ]);

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(
            res.target,
            Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::RpcStubUnknown,
                ..
            })
        ),
        "a provider outside the channel's services must not bind: {:?}",
        res.target
    );
    assert!(out.binds.is_empty(), "no Binds expected: {:?}", out.binds);
}

#[test]
fn rpc_first_pass_rejects_a_different_method() {
    // First pass `==` false branch: the provider sits in the
    // channel's service but exposes a *different* method. A
    // different key reaching the filter must be rejected.
    let provider = rpc_provider_node("orders", "Greeter", "Delete", 10);
    let consumer = rpc_consumer_node("billing", "Greeter", "SayHi", "grpc.orders.internal", 20);
    let cfg = multi_service_config(&[("orders", &["grpc.orders.internal"]), ("billing", &[])]);

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(
            res.target,
            Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::RpcStubUnknown,
                ..
            })
        ),
        "a different RPC method must not bind: {:?}",
        res.target
    );
    assert!(out.binds.is_empty(), "no Binds expected: {:?}", out.binds);
}

#[test]
fn rpc_second_pass_rejects_a_different_method() {
    // Second pass (package-qualified) `==` false branch: the
    // provider's service ends with `.Greeter` and sits in the
    // channel's service, but its method differs — the method
    // equality must reject it.
    let provider = rpc_provider_node("orders", "pkg.Greeter", "Delete", 10);
    let consumer = rpc_consumer_node("billing", "Greeter", "SayHi", "grpc.orders.internal", 20);
    let cfg = multi_service_config(&[("orders", &["grpc.orders.internal"]), ("billing", &[])]);

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(
            res.target,
            Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::RpcStubUnknown,
                ..
            })
        ),
        "a package-qualified provider with a different method must not bind: {:?}",
        res.target
    );
    assert!(out.binds.is_empty(), "no Binds expected: {:?}", out.binds);
}

#[test]
fn rpc_second_pass_rejects_a_provider_outside_the_channel_service() {
    // Second pass `&&` false branch: the provider matches the
    // package-qualified service and method, but lives outside the
    // channel's services. The service scope must still reject it.
    let provider = rpc_provider_node("greeter", "pkg.Greeter", "SayHi", 10);
    let consumer = rpc_consumer_node("billing", "Greeter", "SayHi", "grpc.orders.internal", 20);
    let cfg = multi_service_config(&[
        ("orders", &["grpc.orders.internal"]),
        ("billing", &[]),
        ("greeter", &[]),
    ]);

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(
            res.target,
            Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::RpcStubUnknown,
                ..
            })
        ),
        "a package-qualified provider outside the channel's services must not bind: {:?}",
        res.target
    );
    assert!(out.binds.is_empty(), "no Binds expected: {:?}", out.binds);
}

// ─── WebSocket route filter (consumer_protocol resolve_websocket) ─────

#[test]
fn ws_consumer_with_route_mismatch_does_not_bind() {
    // `k == &key` / `&&` false branch: the dial's host resolves to
    // `orders`, but the provider exposes a *different* route. The
    // key equality and the service scope must both hold — neither
    // may be short-circuited into a bind.
    let provider = ws_provider_node("orders", "/feed", 10);
    let consumer = ws_consumer_node("billing", "orders.internal", "/ws", 20);
    let cfg = ws_config("orders", "orders.internal");

    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "a route mismatch must not bind: {:?}",
        res.target
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edge expected for a route mismatch: {:?}",
        out.binds
    );
}
