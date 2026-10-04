//! Phase C acceptance scenarios (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §6).
//!
//! Spec §6 pins the acceptance criteria for Phase C (env aliases):
//!
//! - **C1**: `process.env.ORDERS_API_URL` with
//!   `ORDERS_API_URL=http://orders:8080` in docker-compose binds
//!   to the Orders service whose `services[].hosts` contains
//!   `orders`.
//! - **C2**: same call but no env file → the call appears in
//!   `unresolved` with reason `EnvUnmapped`.
//! - **C3**: same var defined with conflicting values (`.env`
//!   says `http://a:80`, `docker-compose.yml` says `http://b:80`) →
//!   `unresolved` with reason `EnvAmbiguous`.
//! - **C4**: env vars in helm `values.yaml` are picked up.
//! - **C5**: env vars in k8s manifests are picked up.
//!
//! The tests exercise the joiner's public
//! `run_with_registry_and_env` surface so the full I6 total-order
//! precedence path runs end-to-end. The `EnvBindingIndex` is built
//! by hand from a `Vec<EnvBinding>` (mirroring what the env_sensor
//! would produce for a real workspace). The orchestrator path
//! (`FederatedIndex::rejoin_contracts`) is exercised by the
//! existing `pr13_hermetic_precision_recall_over_t1_fixture`
//! test — that fixture uses an operator `repos.yaml` and does
//! not depend on the env_sensor directly, so it stays at 1.000.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use lain::federation::contracts::clients::{
    ClientDef, ClientLibrary, ClientRegistry, ClientSite, UrlPart,
};
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::{ConsumerTarget, UnresolvedReason};
use lain::federation::contracts::joiner::ContractJoiner;
use lain::federation::contracts::model::{
    CallVia, ConsumerFact, ContractFact, HttpMethod, MethodSpec, NormalizedUrl, ProviderFact,
    ProviderOrigin,
};
use lain::schema::{GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::env_sensor::{scan as env_scan, EnvBinding, EnvBindingIndex, EnvSource};

// ─── Builders ────────────────────────────────────────────────────────

fn repo_ns() -> RepoNamespace {
    RepoNamespace::for_test()
}

fn make_id(repo: &str, kind: NodeType, path: &str, name: &str, line: u32) -> String {
    use lain::federation::repo_id::{GlobalId, RepoId};
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

fn library_consumer_node(
    repo: &str,
    path: &str,
    name: &str,
    line: u32,
    method: MethodSpec,
    url: NormalizedUrl,
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
        via: CallVia::Library {
            name: "axios".into(),
        },
        url_expr: format!("{name}(...)"),
        reads_complete: true,
    }));
    n
}

fn orders_config() -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: vec![],
            hosts: vec!["orders".into()],
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

fn env_index(bindings: Vec<EnvBinding>) -> EnvBindingIndex {
    let mut by_var: BTreeMap<String, Vec<EnvBinding>> = BTreeMap::new();
    for b in bindings {
        by_var.entry(b.var.clone()).or_default().push(b);
    }
    EnvBindingIndex { by_var }
}

fn make_call(repo: &str, path: &str, name: &str, line: u32, var: &str) -> GraphNode {
    library_consumer_node(
        repo,
        path,
        name,
        line,
        MethodSpec::Known(HttpMethod::Get),
        NormalizedUrl {
            host: lain::federation::contracts::model::HostPart::Env(vec![var.to_string()]),
            template: Some("/users".into()),
        },
    )
}

fn fixed_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_env_aliases_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ─── C1 — docker-compose env var binds to a service ─────────────────

/// **C1** (spec §6): `process.env.ORDERS_API_URL` with
/// `ORDERS_API_URL=http://orders:8080` in docker-compose → binds
/// to the Orders service whose `services[].hosts` contains
/// `orders`.
///
/// The test wires a `Library` consumer (the http_client_sensor
/// emits `process.env.ORDERS_API_URL` as `HostPart::Env([X])` with
/// a `Library { name: "axios" }` `via` — the pre-Phase-B form
/// predates the registry path). The env_sensor's bindings resolve
/// `ORDERS_API_URL` to host `orders`, which matches the orders
/// service's `hosts` list. The joiner emits exactly one
/// `Binds` edge.
#[test]
fn c1_docker_compose_env_var_binds_to_service() {
    let _g = test_lock();
    let root = fixed_workspace("c1");
    std::fs::write(
        root.join("docker-compose.yml"),
        "services:\n  billing:\n    environment:\n      ORDERS_API_URL: http://orders:8080\n",
    )
    .unwrap();
    let bindings = env_scan(&root);
    assert!(
        bindings
            .iter()
            .any(|b| b.var == "ORDERS_API_URL" && b.host == "orders"),
        "C1: env_scan must pick ORDERS_API_URL=http://orders:8080 from compose; got {bindings:?}"
    );

    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users",
        10,
        HttpMethod::Get,
        "/users",
    );
    let consumer = make_call("billing", "src/c.ts", "do_get", 1, "ORDERS_API_URL");

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &ClientRegistry::new(),
        &env_index(bindings),
    );

    assert_eq!(
        out.binds.len(),
        1,
        "C1: env-resolved host must produce exactly one Binds edge; got {:?}",
        out.binds
    );
    let edge = &out.binds[0];
    assert_eq!(edge.provider_service.0, "orders");
    assert_eq!(edge.consumer_service.0, "billing");
    assert!(matches!(
        edge.provenance,
        lain::schema::EdgeProvenance::Static { .. }
    ));
    assert!(
        out.unresolved_env_vars.is_empty(),
        "C1: no unresolved env vars expected; got {:?}",
        out.unresolved_env_vars
    );
}

// ─── C2 — no env file → unresolved with EnvUnmapped ────────────────

/// **C2** (spec §6): same call but no env file → the call appears
/// in `unresolved` with reason `EnvUnmapped`, and the var is
/// recorded on `JoinOutput::unresolved_env_vars` for the
/// orchestrator to fold into the coverage ledger.
///
/// The test uses a target template (`/users`) that has no
/// matching provider route in any service, so the rule-6
/// "unbound host" fallback cannot bind the call. The
/// env-name match (`services[].env`) doesn't declare
/// `ORDERS_API_URL` either. With no env file and no other
/// rule firing, the consumer lands in
/// `Unresolved { EnvUnmapped }` and the var counter is bumped
/// for the ledger.
#[test]
fn c2_no_env_file_lands_in_unresolved() {
    let _g = test_lock();
    let root = fixed_workspace("c2");
    // No env files in this workspace.
    let bindings = env_scan(&root);
    assert!(bindings.is_empty(), "C2: no env file → empty bindings");

    // The orders service has no `env: [ORDERS_API_URL]` so the
    // legacy env-name match (target_service_from_env) doesn't
    // fire; the orders provider lives at a template that does
    // not match the consumer's `/users` path, so rule 6 (route
    // fallback) doesn't bind either. Only the env_sensor can
    // resolve the call, and it has no binding for the var.
    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users_by_id",
        10,
        HttpMethod::Get,
        "/users/{}", // Different template — rule 6 won't match
    );
    let consumer = make_call("billing", "src/c.ts", "do_get", 1, "ORDERS_API_URL");

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &ClientRegistry::new(),
        &env_index(bindings),
    );

    assert!(
        out.binds.is_empty(),
        "C2: no Binds edge when the var is unmapped and no fallback matches; got {:?}",
        out.binds
    );
    assert_eq!(
        out.unresolved_env_vars.get("ORDERS_API_URL"),
        Some(&1),
        "C2: unresolved_env_vars must count ORDERS_API_URL once; got {:?}",
        out.unresolved_env_vars
    );
    let call_id = make_id("billing", NodeType::HttpClientCall, "src/c.ts", "do_get", 1);
    let gid = lain::federation::repo_id::GlobalId::from_string(&call_id);
    let resolution = out.index.consumers.get(&gid).expect("C2: consumer present");
    match &resolution.target {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::EnvUnmapped,
            target_service: None,
        }) => {}
        other => panic!("C2: expected Unresolved/EnvUnmapped; got {other:?}"),
    }
}

// ─── C3 — conflicting env values → unresolved with EnvAmbiguous ───

/// **C3** (spec §6): the same var defined with conflicting values
/// in two sources → `unresolved` with reason `EnvAmbiguous` and no
/// `Binds` edge. The orchestrator also sees
/// `JoinOutput::ambiguous_env_vars` populated so the operator
/// can disambiguate.
#[test]
fn c3_conflicting_env_values_are_ambiguous() {
    let _g = test_lock();
    let root = fixed_workspace("c3");
    std::fs::write(root.join(".env"), "ORDERS_API_URL=http://a:80\n").unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        "services:\n  app:\n    environment:\n      ORDERS_API_URL: http://b:80\n",
    )
    .unwrap();
    let bindings = env_scan(&root);
    let var_bindings: Vec<&EnvBinding> = bindings
        .iter()
        .filter(|b| b.var == "ORDERS_API_URL")
        .collect();
    assert_eq!(
        var_bindings.len(),
        2,
        "C3: both sources must surface; got {var_bindings:?}"
    );
    let hosts: std::collections::BTreeSet<_> =
        var_bindings.iter().map(|b| b.host.as_str()).collect();
    assert_eq!(hosts.len(), 2, "C3: hosts must be distinct (a, b)");

    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users",
        10,
        HttpMethod::Get,
        "/users",
    );
    let consumer = make_call("billing", "src/c.ts", "do_get", 1, "ORDERS_API_URL");

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &ClientRegistry::new(),
        &env_index(bindings),
    );

    assert!(
        out.binds.is_empty(),
        "C3: no Binds edge when sources disagree; got {:?}",
        out.binds
    );
    let call_id = make_id("billing", NodeType::HttpClientCall, "src/c.ts", "do_get", 1);
    let gid = lain::federation::repo_id::GlobalId::from_string(&call_id);
    let resolution = out.index.consumers.get(&gid).expect("C3: consumer present");
    match &resolution.target {
        Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::EnvAmbiguous,
            target_service: None,
        }) => {}
        other => panic!("C3: expected Unresolved/EnvAmbiguous; got {other:?}"),
    }
    assert!(
        !out.ambiguous_env_vars.is_empty(),
        "C3: ambiguous_env_vars must be populated; got {:?}",
        out.ambiguous_env_vars
    );
}

// ─── C4 — helm values.yaml env block is picked up ──────────────────

/// **C4** (spec §6): env vars in helm `values.yaml` are picked up
/// by the env_sensor and surface to the joiner.
#[test]
fn c4_helm_values_yaml_env_block_binds() {
    let _g = test_lock();
    let root = fixed_workspace("c4");
    std::fs::create_dir_all(root.join("helm")).unwrap();
    std::fs::write(
        root.join("helm/values.yaml"),
        "env:\n  ORDERS_API_URL: http://orders:8080\n",
    )
    .unwrap();
    let bindings = env_scan(&root);
    assert!(
        bindings.iter().any(|b| b.var == "ORDERS_API_URL"
            && b.host == "orders"
            && b.source == EnvSource::HelmValues),
        "C4: helm values env block must be picked up; got {bindings:?}"
    );

    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users",
        10,
        HttpMethod::Get,
        "/users",
    );
    let consumer = make_call("billing", "src/c.ts", "do_get", 1, "ORDERS_API_URL");

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &ClientRegistry::new(),
        &env_index(bindings),
    );
    assert_eq!(out.binds.len(), 1, "C4: must bind through helm");
    assert_eq!(out.binds[0].provider_service.0, "orders");
}

// ─── C5 — k8s manifests env block is picked up ─────────────────────

/// **C5** (spec §6): env vars in k8s manifests are picked up by
/// the env_sensor and surface to the joiner.
#[test]
fn c5_k8s_manifest_env_block_binds() {
    let _g = test_lock();
    let root = fixed_workspace("c5");
    std::fs::create_dir_all(root.join("k8s")).unwrap();
    std::fs::write(
        root.join("k8s/deploy.yaml"),
        r#"
spec:
  template:
    spec:
      containers:
        - name: billing
          env:
            - name: ORDERS_API_URL
              value: http://orders:8080
"#,
    )
    .unwrap();
    let bindings = env_scan(&root);
    assert!(
        bindings.iter().any(|b| b.var == "ORDERS_API_URL"
            && b.host == "orders"
            && b.source == EnvSource::K8sEnv),
        "C5: k8s env block must be picked up; got {bindings:?}"
    );

    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users",
        10,
        HttpMethod::Get,
        "/users",
    );
    let consumer = make_call("billing", "src/c.ts", "do_get", 1, "ORDERS_API_URL");

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &ClientRegistry::new(),
        &env_index(bindings),
    );
    assert_eq!(out.binds.len(), 1, "C5: must bind through k8s");
    assert_eq!(out.binds[0].provider_service.0, "orders");
}

// ─── Phase B + Phase C integration: env-var base URL in a wrapper ──

/// Phase B (`ClientDef` with a `process.env.X` base) + Phase C
/// (the env_sensor resolves the var) → the registry composition
/// produces a `Binds` edge to the orders service. This pins the
/// interaction of Task 2 + Task 3: a `ky.create({prefixUrl:
/// process.env.ORDERS_API_URL})` declaration populates the
/// registry with a `ClientDef` whose `base` is
/// `UrlPart::Env([ORDERS_API_URL])`; the joiner tier-2 path
/// composes the registry's base with the call's path, the env
/// index resolves the var, and the host matches the orders
/// service.
#[test]
fn c6_phase_b_registry_base_with_env_var_binds() {
    let _g = test_lock();
    let root = fixed_workspace("c6");
    std::fs::write(
        root.join("docker-compose.yml"),
        "services:\n  app:\n    environment:\n      ORDERS_API_URL: http://orders:8080\n",
    )
    .unwrap();
    let bindings = env_scan(&root);
    assert!(!bindings.is_empty());

    let provider = provider_node(
        "orders",
        "src/orders.py",
        "get_users",
        10,
        HttpMethod::Get,
        "/users",
    );
    // The consumer is a `Receiver` (Phase B's wrapper form) with
    // a path-only URL — the joiner tier-2 path composes the
    // registry's `UrlPart::Env([ORDERS_API_URL])` base with the
    // call's `/users` path.
    let mut consumer = GraphNode::new_in(
        NodeType::HttpClientCall,
        "do_get".into(),
        "src/c.ts".into(),
        &repo_ns(),
    );
    consumer.repo_id = Some("billing".to_string());
    consumer.id = make_id("billing", NodeType::HttpClientCall, "src/c.ts", "do_get", 1);
    consumer.line_start = Some(1);
    consumer.contract = Some(ContractFact::Consumer(ConsumerFact {
        method: MethodSpec::Known(HttpMethod::Get),
        url: NormalizedUrl {
            host: lain::federation::contracts::model::HostPart::None,
            template: Some("/users".into()),
        },
        via: CallVia::Receiver {
            expr: "ordersClient".into(),
            fn_name: "get".into(),
            base: None,
        },
        url_expr: "ordersClient.get(...)".into(),
        reads_complete: true,
    }));

    let mut registry = ClientRegistry::new();
    registry.insert(ClientDef {
        name: "ordersClient".into(),
        module: "src/clients".into(),
        base: vec![UrlPart::Env(vec!["ORDERS_API_URL".into()])],
        library: Some(ClientLibrary::Axios),
        site: ClientSite {
            path: "src/clients.ts".into(),
            line: 1,
        },
    });

    let out = ContractJoiner::run_with_registry_and_env(
        &[provider, consumer],
        &[],
        &orders_config(),
        &registry,
        &env_index(bindings),
    );

    assert_eq!(
        out.binds.len(),
        1,
        "C6: registry base with Env var must bind when env_sensor resolves it; got {:?}",
        out.binds
    );
    assert_eq!(out.binds[0].provider_service.0, "orders");
}

// ─── Test serialisation ─────────────────────────────────────────────

static TEST_LOCK: Mutex<()> = Mutex::new(());
fn test_lock() -> MutexGuard<'static, ()> {
    match TEST_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

// Suppress dead-code lints for the workspace helper that is
// useful for future scenarios (not all of C1-C5 build a real
// workspace — some pass an env_index directly).
#[allow(dead_code)]
fn _fixed_workspace_pin(_p: &Path) {}
