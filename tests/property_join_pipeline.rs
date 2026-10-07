//! Property tests for the contract-joiner pipeline.
//!
//! Spec §9.4 pins the join pipeline's invariants I2, I5, and I6 with
//! proptest. The tests live in an integration-test binary because
//! they exercise `resolve_consumer_to_service` (the public surface
//! `src/server/federation/contracts/joiner.rs` exposes for
//! property-test consumption) and round-trip synthetic inputs through
//! the real joiner without going through a snapshot harness.
//!
//! **I2 — partition.** Every discovered call lands in exactly one
//! terminal state: `Binds` / `External` / `Unresolved { reason }`.
//! The joiner never drops a call silently and never emits two
//! verdicts for the same call. The property test pins this for
//! random `calls × services` pairs.
//!
//! **I5 — no same-service bind.** A service can never bind to a
//! route owned by itself (the §7.8 invariant). The property test
//! asserts the predicate `bound_endpoint.service != own_service`
//! for every call's resolution.
//!
//! **I6 — total-order precedence.** Adding more evidence to a
//! resolution never lowers its rank. We pin this by running the
//! joiner with one call, then with the same call plus an extra
//! matching client — the bind's confidence / provenance /
//! route_match must not regress.

use lain::federation::contracts::clients::{ClientLibrary, ClientRegistry, ClientSite, UrlPart};
use lain::federation::contracts::config::{ContractFederationConfig, HttpClientDecl, ServiceDecl};
use lain::federation::contracts::index::ConsumerTarget;
use lain::federation::contracts::joiner::{resolve_consumer_to_service, Resolution};
use lain::federation::contracts::model::{
    CallVia, ConsumerFact, HttpMethod, MethodSpec, NormalizedUrl, ServiceName,
};
use lain::schema::{NodeType, RepoNamespace};
use proptest::prelude::*;
use std::collections::BTreeMap;

// ─── Strategies ───────────────────────────────────────────────────────

/// A repo-style service id ("orders", "billing", "platform", …).
fn arb_service_name() -> BoxedStrategy<String> {
    prop::string::string_regex("[a-z][a-z0-9]{1,5}")
        .unwrap()
        .boxed()
}

fn arb_method() -> BoxedStrategy<MethodSpec> {
    prop_oneof![
        Just(MethodSpec::Known(HttpMethod::Get)),
        Just(MethodSpec::Known(HttpMethod::Post)),
        Just(MethodSpec::Known(HttpMethod::Put)),
        Just(MethodSpec::Known(HttpMethod::Patch)),
        Just(MethodSpec::Known(HttpMethod::Delete)),
        Just(MethodSpec::Unknown),
    ]
    .boxed()
}

fn arb_url() -> BoxedStrategy<NormalizedUrl> {
    let literal_hosts = prop_oneof![
        Just("orders.svc".to_string()),
        Just("billing.svc".to_string()),
        Just("reports.svc".to_string()),
        Just("api.example.com".to_string()),
    ];
    let literal_templates = prop_oneof![
        Just("/api/orders".to_string()),
        Just("/api/orders/{}".to_string()),
        Just("/health".to_string()),
        Just("/v1/invoices/{}".to_string()),
        Just("/".to_string()),
    ];
    (literal_hosts, literal_templates)
        .prop_map(|(host, template)| NormalizedUrl {
            host: lain::federation::contracts::model::HostPart::Literal(host),
            template: Some(template),
        })
        .boxed()
}

fn arb_call() -> BoxedStrategy<(String, ConsumerFact)> {
    (arb_service_name(), arb_method(), arb_service_name())
        .prop_flat_map(|(repo_name, method, via_name)| {
            arb_url().prop_map(move |url| {
                let via = CallVia::Receiver {
                    expr: via_name.clone(),
                    fn_name: "get".to_string(),
                    base: None,
                };
                let consumer = ConsumerFact {
                    method: method.clone(),
                    url,
                    via,
                    url_expr: format!("{repo_name}/call"),
                    reads_complete: true,
                };
                (repo_name.clone(), consumer)
            })
        })
        .boxed()
}

fn arb_service_decl() -> BoxedStrategy<ServiceDecl> {
    (
        arb_service_name(),
        arb_service_name(),
        prop::collection::vec(arb_service_name(), 0..3),
        prop::collection::vec(arb_service_name(), 0..3),
    )
        .prop_map(|(name, repo, hosts, env)| ServiceDecl {
            name,
            repo,
            paths: Vec::new(),
            hosts: hosts.into_iter().map(|h| format!("{h}.svc")).collect(),
            env,
            base_path: None,
            route_prefixes: Vec::new(),
        })
        .boxed()
}

fn arb_config(services: Vec<ServiceDecl>) -> ContractFederationConfig {
    ContractFederationConfig {
        services,
        http_clients: Vec::new(),
        generic_keys: Vec::new(),
        schemas: Vec::new(),
        bindings: Vec::new(),
        databases: Vec::new(),
    }
}

// ─── I2 — partition ────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every discovered call lands in exactly one terminal state:
    /// `Binds` / `External` / `Unresolved { reason }`. The joiner
    /// never drops a call silently and never emits two verdicts for
    /// the same call (spec §3 invariant I2).
    #[test]
    fn every_call_lands_in_exactly_one_terminal_state(
        services in prop::collection::vec(arb_service_decl(), 1..5),
        call in arb_call(),
    ) {
        let cfg = arb_config(services);
        let own_service = ServiceName(call.0.clone());
        let endpoints: BTreeMap<_, _> = BTreeMap::new();
        let http_clients = Vec::new();
        let mut external = BTreeMap::new();
        let mut binds = Vec::new();
        let registry = ClientRegistry::new();
        let compiled = lain::federation::contracts::joiner::CompiledHttpClients::compile_for_tests(&http_clients);
        let resolution = resolve_consumer_to_service(
            &call.1,
            &own_service,
            &cfg,
            &endpoints,
            &compiled,
            &mut external,
            &mut binds,
            &registry,
        );
        let terminal = match &resolution.target {
            Some(ConsumerTarget::Binds { .. }) => 1,
            Some(ConsumerTarget::External { .. }) => 1,
            Some(ConsumerTarget::Unresolved { .. }) => 1,
            None => 0,
        };
        prop_assert_eq!(
            terminal, 1,
            "every call must land in exactly one terminal state"
        );
    }
}

// ─── I2-WS / I2-Topic — protocol-specific partition coverage
//
//     The I2 proptest only exercises the HTTP ladder via
//     `resolve_consumer_to_service`. The WebSocket and Topic
//     resolvers live on a different code path
//     (`resolve_by_key` in `consumer_protocol.rs`); the partition
//     invariant must hold for them too. The cases below drive
//     each protocol end-to-end through `ContractJoiner::run` —
//     the public surface that calls the per-protocol resolvers
//     — with representative inputs and assert the same
//     terminal-state contract.
//
//     The inputs are deliberately small (a handful of cases
//     per protocol) so the test stays fast; coverage is over
//     *protocol shape* (zero / one / two candidates,
//     same-service skip), not over all possible URL/host
//     values.

fn ws_node(service: &str, host: &str, route: &str, line: u32) -> lain::schema::GraphNode {
    use lain::federation::contracts::model::{
        ContractFact, HostPart, NormalizedUrl, WebSocketConsumerFact, WebSocketProviderFact,
    };
    let is_provider = host.is_empty();
    let id_name = if is_provider {
        format!("ws:server:{route}")
    } else {
        format!("ws:client:{service}:{host}:{route}")
    };
    let path = if is_provider {
        "src/server.js"
    } else {
        "src/socket.js"
    };
    let node_type = if is_provider {
        NodeType::HttpRoute
    } else {
        NodeType::HttpClientCall
    };
    let mut n = lain::schema::GraphNode::new_in(
        node_type.clone(),
        id_name.clone(),
        path.to_string(),
        &RepoNamespace::for_test(),
    );
    n.repo_id = Some(service.to_string());
    n.id = lain::federation::repo_id::GlobalId::new(
        &lain::federation::repo_id::RepoId::new(service).unwrap(),
        node_type.clone(),
        path,
        &id_name,
        Some(line),
    )
    .as_str()
    .to_string();
    n.line_start = Some(line);
    if is_provider {
        n.contract = Some(ContractFact::WebSocketProvider(WebSocketProviderFact {
            route: route.to_string(),
            handler: None,
        }));
    } else {
        n.contract = Some(ContractFact::WebSocketConsumer(WebSocketConsumerFact {
            url: NormalizedUrl {
                host: HostPart::Literal(host.to_string()),
                template: Some(route.to_string()),
            },
            route: route.to_string(),
        }));
    }
    n
}

fn ws_config(svc: &str, host: &str) -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: svc.into(),
            repo: svc.into(),
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

fn topic_node(
    service: &str,
    kind: &str,
    broker: &str,
    topic: &str,
    line: u32,
) -> lain::schema::GraphNode {
    use lain::federation::contracts::model::{
        ContractFact, HttpMethod, ProviderFact, ProviderOrigin, TopicConsumerFact,
        TopicConsumerKind,
    };
    let is_producer = kind == "producer";
    let id_name = format!("{broker}/{topic}");
    let path = if is_producer {
        "src/events.py"
    } else {
        "src/handlers/orders.py"
    };
    let node_type = if is_producer {
        NodeType::Topic
    } else {
        NodeType::Function
    };
    let mut n = lain::schema::GraphNode::new_in(
        node_type.clone(),
        id_name.clone(),
        path.to_string(),
        &RepoNamespace::for_test(),
    );
    n.repo_id = Some(service.to_string());
    n.id = lain::federation::repo_id::GlobalId::new(
        &lain::federation::repo_id::RepoId::new(service).unwrap(),
        node_type.clone(),
        path,
        &id_name,
        Some(line),
    )
    .as_str()
    .to_string();
    n.line_start = Some(line);
    if is_producer {
        n.contract = Some(ContractFact::Provider(ProviderFact {
            method: HttpMethod::Any,
            template: topic.to_string(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        }));
    } else {
        n.contract = Some(ContractFact::TopicConsumer(TopicConsumerFact {
            broker: broker.to_string(),
            name: topic.to_string(),
            kind: TopicConsumerKind::Subscription,
        }));
    }
    n
}

#[test]
fn every_websocket_call_lands_in_exactly_one_terminal_state() {
    use lain::federation::contracts::joiner::ContractJoiner;

    // (a) zero matching candidates — Unresolved.
    let provider = ws_node("orders", "", "/feed", 10);
    let consumer = ws_node("billing", "thirdparty.com", "/feed", 1);
    let cfg = ws_config("orders", "orders.internal");
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "(a) zero WS candidates must be Unresolved, got: {:?}",
        res.target
    );

    // (b) single matching candidate — Binds.
    let provider = ws_node("orders", "", "/feed", 10);
    let consumer = ws_node("billing", "orders.internal", "/feed", 1);
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Binds { .. })),
        "(b) one matching WS candidate must Binds, got: {:?}",
        res.target
    );
    assert_eq!(out.binds.len(), 1, "exactly one Binds edge expected");

    // (c) same-service skip — Unresolved (I5).
    let provider = ws_node("orders", "", "/feed", 10);
    let consumer = ws_node("orders", "orders.internal", "/feed", 1);
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "(c) same-service WS consumer must be Unresolved (I5), got: {:?}",
        res.target
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edge may be emitted for same-service, got: {:?}",
        out.binds
    );
}

#[test]
fn every_topic_call_lands_in_exactly_one_terminal_state() {
    use lain::federation::contracts::joiner::ContractJoiner;

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
    };

    // (a) zero candidates — Unresolved.
    let consumer = topic_node("billing", "consumer", "kafka", "missing.topic", 1);
    let out = ContractJoiner::run(&[consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "(a) zero topic candidates must be Unresolved, got: {:?}",
        res.target
    );

    // (b) one matching candidate — Binds.
    let producer = topic_node("orders", "producer", "kafka", "orders.events", 10);
    let consumer = topic_node("billing", "consumer", "kafka", "orders.events", 1);
    let out = ContractJoiner::run(&[producer, consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Binds { .. })),
        "(b) one matching topic candidate must Binds, got: {:?}",
        res.target
    );
    assert_eq!(out.binds.len(), 1);

    // (c) same-service skip — Unresolved (I5).
    let producer = topic_node("orders", "producer", "kafka", "orders.events", 10);
    let consumer = topic_node("orders", "consumer", "kafka", "orders.events", 1);
    let out = ContractJoiner::run(&[producer, consumer], &[], &cfg);
    let res = out
        .index
        .consumers
        .values()
        .next()
        .expect("consumer must be recorded");
    assert!(
        matches!(res.target, Some(ConsumerTarget::Unresolved { .. })),
        "(c) same-service topic consumer must be Unresolved (I5), got: {:?}",
        res.target
    );
    assert!(out.binds.is_empty());
}

// ─── I5 — no same-service bind ────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// No service binds to a route owned by itself (spec §3
    /// invariant I5, §7.8). The `own_service` parameter is the
    /// service the call originates from; every `bound_endpoint`
    /// must reference a different service.
    #[test]
    fn no_service_binds_to_itself(
        services in prop::collection::vec(arb_service_decl(), 1..5),
        call in arb_call(),
    ) {
        let cfg = arb_config(services.clone());
        let own_service = ServiceName(call.0.clone());
        let endpoints: BTreeMap<_, _> = BTreeMap::new();
        let http_clients = Vec::new();
        let mut external = BTreeMap::new();
        let mut binds = Vec::new();
        let registry = ClientRegistry::new();
        let compiled = lain::federation::contracts::joiner::CompiledHttpClients::compile_for_tests(&http_clients);
        let resolution = resolve_consumer_to_service(
            &call.1,
            &own_service,
            &cfg,
            &endpoints,
            &compiled,
            &mut external,
            &mut binds,
            &registry,
        );
        for (svc, _) in &resolution.bound_endpoints {
            prop_assert_ne!(
                svc, &own_service,
                "I5: a service must not bind to a route it owns"
            );
        }
    }
}

// ─── I6 — total-order precedence ──────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Adding more evidence never lowers a bind's rank (spec §3
    /// invariant I6). We pin this by:
    ///
    ///   1. Running the joiner once with one call → rank R1.
    ///   2. Running the joiner again with the same call plus an
    ///      extra `ClientDef` whose base resolves to the same
    ///      service → rank R2.
    ///
    /// R2's confidence must be ≥ R1's. We use the rank function
    /// from `Resolution::rank()` below — total order:
    ///   Static (1.0) > Heuristic (0.9 op_id) > 0.6 unbound > 0.3 ambiguous > 0 (unresolved).
    #[test]
    fn adding_evidence_never_lowers_rank(
        services in prop::collection::vec(arb_service_decl(), 1..5),
        call in arb_call(),
    ) {
        let cfg = arb_config(services.clone());
        let own_service = ServiceName(call.0.clone());
        let endpoints: BTreeMap<_, _> = BTreeMap::new();
        let http_clients: Vec<HttpClientDecl> = Vec::new();
        let mut external = BTreeMap::new();
        let mut binds = Vec::new();

        // Pass 1: empty registry → resolve_consumer_to_service
        // returns whatever tier 3+ yields (tier 2 is empty).
        let compiled_empty = lain::federation::contracts::joiner::CompiledHttpClients::compile_for_tests(&http_clients);
        let r1 = resolve_consumer_to_service(
            &call.1,
            &own_service,
            &cfg,
            &endpoints,
            &compiled_empty,
            &mut external,
            &mut binds,
            &ClientRegistry::new(),
        );

        // Pass 2: same call, registry now carries a `ClientDef`
        // whose base resolves to one of the configured services
        // through the env axis (the registry's compose step
        // yields `HostPart::Literal` from the base literal).
        let mut registry = ClientRegistry::new();
        if let Some(s) = services.first() {
            let base_host = format!("{}.svc", s.name);
            registry.insert(lain::federation::contracts::clients::ClientDef {
                name: "syntheticClient".to_string(),
                module: "synthetic".to_string(),
                base: vec![UrlPart::Literal(format!("https://{base_host}"))],
                library: Some(ClientLibrary::Custom),
                site: ClientSite {
                    path: "synthetic.ts".to_string(),
                    line: 1,
                },
            });
        }

        let compiled = lain::federation::contracts::joiner::CompiledHttpClients::compile_for_tests(&http_clients);
        let r2 = resolve_consumer_to_service(
            &call.1,
            &own_service,
            &cfg,
            &endpoints,
            &compiled,
            &mut external,
            &mut binds,
            &registry,
        );

        // Both resolutions must be in a terminal state.
        prop_assert!(
            matches!(
                r1.target,
                Some(ConsumerTarget::Binds { .. })
                    | Some(ConsumerTarget::External { .. })
                    | Some(ConsumerTarget::Unresolved { .. })
            ),
            "pass 1 must produce a terminal state"
        );
        prop_assert!(
            matches!(
                r2.target,
                Some(ConsumerTarget::Binds { .. })
                    | Some(ConsumerTarget::External { .. })
                    | Some(ConsumerTarget::Unresolved { .. })
            ),
            "pass 2 must produce a terminal state"
        );
        // I6 — pass 2's rank must be ≥ pass 1's. The rank
        // function lives on `Resolution` below.
        let rank1 = rank_of_resolution(&r1);
        let rank2 = rank_of_resolution(&r2);
        prop_assert!(
            rank2 >= rank1,
            "I6: pass 2 rank ({rank2}) must be ≥ pass 1 rank ({rank1})"
        );
    }
}

// ─── Rank function (I6 ordering) ──────────────────────────────────────

/// Numeric rank for the I6 total order (higher is better). Spec
/// §5.3 ties the rank to the confidence + provenance combo:
///   Static (1.0)         — confirmed / explicit match
///   Heuristic 0.9        — operationId
///   Heuristic 0.6        — unbound-host
///   Heuristic 0.3        — ambiguous
///   Unresolved           — 0
///   External             — 0 (rule 4 doesn't rank on the bind
///                          scale; it's a non-bind)
fn rank_of_resolution(res: &lain::federation::contracts::index::ConsumerResolution) -> u8 {
    match &res.target {
        Some(ConsumerTarget::Binds { confidence, .. }) => {
            // Map the float onto a discrete rank. 1.0 → 4, 0.9 → 3,
            // 0.6 → 2, 0.3 → 1, otherwise → 0.
            let c = *confidence;
            if (c - 1.0).abs() < f32::EPSILON {
                4
            } else if (c - 0.9).abs() < 0.05 {
                3
            } else if (c - 0.6).abs() < 0.05 {
                2
            } else if (c - 0.3).abs() < 0.05 {
                1
            } else {
                0
            }
        }
        Some(ConsumerTarget::External { .. }) => 0,
        Some(ConsumerTarget::Unresolved { .. }) => 0,
        None => 0,
    }
}

// ─── Resolution type round-trip (smoke test) ──────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// The `Resolution` enum (spec §5.3 total-order summary) is
    /// matchable on every variant; no panic on construction.
    #[test]
    fn resolution_type_is_exhaustive(svc_name in arb_service_name()) {
        let r: Resolution = Resolution::Unresolved {
            reason: lain::federation::contracts::index::UnresolvedReason::NoMatch,
            target_service: Some(ServiceName(svc_name)),
        };
        match r {
            Resolution::Endpoint { .. } => panic!("unexpected Endpoint"),
            Resolution::External { .. } => panic!("unexpected External"),
            Resolution::Unresolved { .. } => {}
        }
    }
}
