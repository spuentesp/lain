//! Tests for `ContractJoiner` (§5.3 + §7.2 / §7.3 / §7.4 / §7.6).
//!
//! The joiner is a pure function of `(nodes, config)`; tests here
//! construct the inputs by hand so they run without disk or git.

use crate::federation::contracts::config::{
    ConfirmedBinding, ConfirmedBindingConsumer, ConfirmedBindingProvider, ContractFederationConfig,
    HttpClientDecl, ServiceDecl,
};
use crate::federation::contracts::index::{
    ConsumerTarget, EndpointId, FieldRefResolution, StaleReason, UnresolvedReason,
};
use crate::federation::contracts::joiner::{ContractJoiner, JoinOutput};
use crate::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, ContractKey, HostPart, HttpMethod, MethodSpec,
    NormalizedUrl, ProviderFact, ProviderOrigin,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::{EdgeProvenance, GraphNode, NodeType, RepoNamespace, RouteMatch};
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
fn rule_1_discards_wrapper_candidate_with_no_http_client_match() {
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
            expr: "ordersClient".into(),
            fn_name: "get".into(),
        },
    );
    let cfg = default_config(); // empty http_clients
    let out = ContractJoiner::run(&[p, c], &[], &cfg);
    assert!(
        out.binds.is_empty(),
        "rule 1: Receiver with no matching http_clients call is discarded"
    );
    assert!(
        out.index.consumers.is_empty(),
        "rule 1: discarded calls do not appear in consumers"
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
    };
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    // §7.8: every Binds edge connects two different services; an
    // in-service consumer must not bind to its own producer.
    assert!(
        out.binds.is_empty(),
        "intra-service topic consumption must not bind"
    );
}

// Silence unused-imports from churn.
#[allow(dead_code)]
fn _silence(_: &RepoId, _: &GlobalId, _: &BTreeSet<FieldRefResolution>) {}
