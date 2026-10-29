//! A provider the analyzer cannot see (a runtime-configured route) can be
//! declared in `provides`. The declaration enters the endpoint table, merges
//! with a detected provider of the same key, and binds a consumer.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::contracts::config::{
    ContractFederationConfig, HostAliasDecl, ProviderDecl, ServiceDecl,
};
use lain::federation::contracts::index::{ConsumerResolution, ConsumerTarget, ContractIndex};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};

fn git_init_committed(dir: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

fn service(name: &str, paths: &[&str], hosts: &[&str]) -> ServiceDecl {
    ServiceDecl {
        name: name.into(),
        repo: "nat".into(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
        provides_graphql: true,
    }
}

/// Index one repo holding `files` under `services` with `provides`, then
/// return the joined contract index.
async fn join(
    files: &[(&str, &str)],
    services: Vec<ServiceDecl>,
    provides: Vec<ProviderDecl>,
    host_aliases: Vec<HostAliasDecl>,
) -> Arc<ContractIndex> {
    let root = tempfile::tempdir().unwrap();
    let repo_dir = root.path().join("nat");
    for (rel, content) in files {
        let path = repo_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    git_init_committed(&repo_dir);

    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));

    let repo_id = RepoId::new("nat").unwrap();
    let source: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            repo_id.clone(),
            repo_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir {
                path: repo_dir.clone(),
            },
        )
        .unwrap(),
    );
    source.fetch().await.unwrap();
    fed.add_repo(source, &data_dir).await.unwrap();
    fed.set_contract_config(ContractFederationConfig {
        services,
        provides,
        host_aliases,
        ..Default::default()
    });

    let repo = fed.get_repo(&repo_id).unwrap();
    repo.index().await.unwrap();
    fed.project_repo(&repo_id).await.unwrap();
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    fed.contract_index().expect("contract index")
}

fn declared(service: &str, key: &str) -> ProviderDecl {
    ProviderDecl {
        service: service.into(),
        key: key.into(),
        source: Some("nat config.yaml".into()),
    }
}

#[tokio::test]
async fn declared_provider_lands_in_the_endpoint_table() {
    let index = join(
        &[("src/app.py", "x = 1\n")],
        vec![service("nat", &[], &["nat.svc"])],
        vec![declared("nat", "http:POST /chat/stream")],
        vec![],
    )
    .await;
    let found = index
        .endpoints
        .iter()
        .any(|(id, e)| id.0 .0 == "nat" && e.template == "/chat/stream");
    assert!(
        found,
        "the declared provider is missing from the endpoint table"
    );
}

/// Review focus #3: a declared key the analyzer also detects is one endpoint,
/// with the detected provider and the declared one both listed.
#[tokio::test]
async fn declared_and_detected_providers_merge_into_one_endpoint() {
    let app = "from fastapi import FastAPI\napp = FastAPI()\n\n@app.post(\"/chat/stream\")\ndef stream():\n    return {}\n";
    let index = join(
        &[("src/app.py", app)],
        vec![service("nat", &[], &["nat.svc"])],
        vec![declared("nat", "http:POST /chat/stream")],
        vec![],
    )
    .await;
    let matching: Vec<_> = index
        .endpoints
        .values()
        .filter(|e| e.template == "/chat/stream")
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected one endpoint, got {}",
        matching.len()
    );
    assert_eq!(
        matching[0].providers.len(),
        2,
        "detected and declared providers should both appear"
    );
}

/// A consumer that calls the declared route binds to it.
#[tokio::test]
async fn consumer_binds_to_a_declared_provider() {
    let client = "import requests\n\ndef call():\n    return requests.post(\"http://nat.svc/chat/stream\", json={})\n";
    let index = join(
        &[("client/call.py", client)],
        vec![
            service("nat", &[], &["nat.svc"]),
            service("client", &["client"], &[]),
        ],
        vec![declared("nat", "http:POST /chat/stream")],
        vec![],
    )
    .await;
    let bound = index
        .consumers
        .values()
        .flatten()
        .any(|r: &ConsumerResolution| matches!(r.target, Some(ConsumerTarget::Binds { .. })));
    assert!(
        bound,
        "the consumer did not bind to the declared provider: {:?}",
        index.consumers
    );
}

// ─── runtime write path: apply_contract_context ──────────────────────────

use lain::server::mcp::contract_tools::context::apply_contract_context_handle;
use lain::server::mcp::handler::{HandlerStatus, McpContext};
use serde_json::{json, Value};

/// A federation over one repo `nat` with a `repos.yaml` on disk the write path
/// can edit. Returns the federation, the file path, and the directory guard.
async fn write_fixture(
    initial_yaml: &str,
) -> (Arc<FederatedIndex>, std::path::PathBuf, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let repo_dir = root.path().join("nat");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(repo_dir.join("src/app.py"), "x = 1\n").unwrap();
    git_init_committed(&repo_dir);

    let config_path = root.path().join("repos.yaml");
    std::fs::write(&config_path, initial_yaml).unwrap();

    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    let repo_id = RepoId::new("nat").unwrap();
    let source: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            repo_id.clone(),
            repo_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir {
                path: repo_dir.clone(),
            },
        )
        .unwrap(),
    );
    source.fetch().await.unwrap();
    fed.add_repo(source, &data_dir).await.unwrap();
    fed.set_contract_config(ContractFederationConfig {
        services: vec![service("nat", &[], &["nat.svc"])],
        ..Default::default()
    });
    fed.set_config_path(config_path.clone());
    let repo = fed.get_repo(&repo_id).unwrap();
    repo.index().await.unwrap();
    fed.project_repo(&repo_id).await.unwrap();
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    (fed, config_path, root)
}

async fn call_apply(fed: &Arc<FederatedIndex>, args: Value) -> Value {
    let status = HandlerStatus::for_test();
    let ctx = McpContext {
        server: None,
        federation: Some(fed.as_ref()),
        workspaces: None,
        status: &status,
        reload_bus: None,
        snapshots: None,
    };
    let outcome = apply_contract_context_handle(&ctx, args).await.unwrap();
    outcome.structured
}

const INITIAL: &str = "repos:\n  - id: nat\n    source: {type: workspace_dir, path: /nowhere}\n";

#[tokio::test]
async fn dry_run_returns_the_yaml_and_touches_nothing() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "provides": [{"service": "nat", "key": "http:POST /chat/stream"}]}),
    )
    .await;
    assert_eq!(env["data"]["dry_run"], json!(true));
    assert_eq!(env["data"]["applied"], json!(false));
    assert!(env["data"]["yaml"]
        .as_str()
        .unwrap()
        .contains("/chat/stream"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        INITIAL,
        "a dry run must not write"
    );
    assert!(!lain::federation::contracts::context::backup_path(&path).exists());
}

#[tokio::test]
async fn apply_writes_backs_up_and_serves_the_declaration() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "provides": [{"service": "nat", "key": "http:POST /chat/stream"}]}),
    )
    .await;
    assert_eq!(env["data"]["applied"], json!(true), "{env}");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("/chat/stream"));
    assert_eq!(
        std::fs::read_to_string(lain::federation::contracts::context::backup_path(&path)).unwrap(),
        INITIAL
    );
    let served = fed
        .contract_index()
        .unwrap()
        .endpoints
        .values()
        .any(|e| e.template == "/chat/stream");
    assert!(served, "the re-joined index must serve the declaration");
}

/// A host alias is contract context too: the runtime tool declares it, the
/// file carries it, and the re-joined index runs against it.
#[tokio::test]
async fn apply_declares_a_host_alias_at_runtime() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "host_aliases": [{"host": "UPSTREAM_HTTP_ORIGIN", "service": "nat",
                                 "source": "agent"}]}),
    )
    .await;
    assert_eq!(env["data"]["applied"], json!(true), "{env}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("host_aliases"), "{text}");
    assert!(text.contains("UPSTREAM_HTTP_ORIGIN"), "{text}");
    // The live contract config carries the alias the joiner ran against.
    let live = fed.contract_config().unwrap();
    assert!(
        live.host_aliases
            .iter()
            .any(|a| { a.host.as_deref() == Some("UPSTREAM_HTTP_ORIGIN") && a.service == "nat" }),
        "the alias must be live in the contract config"
    );
}

/// An alias naming an undeclared service is refused, and the file stays
/// byte-identical.
#[tokio::test]
async fn a_host_alias_for_an_unknown_service_is_refused() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "host_aliases": [{"host": "UPSTREAM_HTTP_ORIGIN", "service": "nowhere"}]}),
    )
    .await;
    assert_eq!(env["error"]["code"], json!("invalid_argument"), "{env}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), INITIAL);
    assert!(!lain::federation::contracts::context::backup_path(&path).exists());
}

/// Review focus #1 and #5: an undeclared service is refused, and the file stays
/// byte-identical with no backup left behind.
#[tokio::test]
async fn a_refused_declaration_leaves_the_file_untouched() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "provides": [{"service": "nowhere", "key": "http:POST /x"}]}),
    )
    .await;
    assert_eq!(env["error"]["code"], json!("invalid_argument"), "{env}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), INITIAL);
    assert!(!lain::federation::contracts::context::backup_path(&path).exists());
}

/// Review focus #2: a malformed key is refused.
#[tokio::test]
async fn a_malformed_key_is_refused() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "provides": [{"service": "nat", "key": "http:POST"}]}),
    )
    .await;
    assert_eq!(env["error"]["code"], json!("invalid_argument"), "{env}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), INITIAL);
}

/// A second identical declaration adds nothing and writes nothing — but
/// the declaration is still in effect, so the tool reports it as applied.
/// `applied` answers "is the declaration in effect", not "did this call
/// write something".
#[tokio::test]
async fn reapplying_the_same_declaration_writes_nothing_and_reports_it_in_effect() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let args = json!({"snapshot": "live", "dry_run": false,
                      "provides": [{"service": "nat", "key": "http:POST /chat"}]});
    assert_eq!(
        call_apply(&fed, args.clone()).await["data"]["applied"],
        json!(true)
    );
    let after_first = std::fs::read_to_string(&path).unwrap();
    let env = call_apply(&fed, args).await;
    assert_eq!(env["data"]["applied"], json!(true), "{env}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);
}

/// A request that names no declaration has nothing to be in effect: the
/// tool writes nothing and reports `applied: false`.
#[tokio::test]
async fn an_empty_request_declares_nothing() {
    let (fed, path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(&fed, json!({"snapshot": "live", "dry_run": false})).await;
    assert_eq!(env["data"]["applied"], json!(false), "{env}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), INITIAL);
}

#[tokio::test]
async fn snapshots_other_than_live_are_refused() {
    let (fed, _path, _root) = write_fixture(INITIAL).await;
    let env = call_apply(&fed, json!({"snapshot": "snap_abc", "provides": []})).await;
    assert_eq!(env["error"]["code"], json!("invalid_argument"), "{env}");
}

/// If `repos.yaml` on disk was edited to include an invalid service declaration
/// before the watcher reloaded it, `apply_contract_context` validates the merged
/// on-disk document rather than the stale in-memory config, and refuses to write.
#[tokio::test]
async fn apply_validates_the_merged_document_on_disk() {
    let edited = "\
repos:
  - id: nat
    source: {type: workspace_dir, path: /nowhere}
services:
  - name: nat
    repo: nat
  - name: broken
    repo: does-not-exist
";
    let (fed, path, _root) = write_fixture(edited).await;
    let env = call_apply(
        &fed,
        json!({"snapshot": "live", "dry_run": false,
               "provides": [{"service": "nat", "key": "http:POST /chat"}]}),
    )
    .await;
    assert_eq!(env["error"]["code"], json!("invalid_argument"), "{env}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        edited,
        "an invalid on-disk config must not be overwritten"
    );
}

/// The NAT UI proxy dials `${UPSTREAM_HTTP_ORIGIN}/chat`. Static analysis sees
/// the identifier, not the value it is built from. A declared host alias says
/// which service that expression dials, so the call resolves to that service
/// instead of `no_match`.
#[tokio::test]
async fn a_declared_host_alias_routes_an_expression_host_to_its_service() {
    let proxy = "const UPSTREAM_HTTP_ORIGIN = `${upstream.protocol}//${upstream.host}`;\n\
                 const targetUrl = `${UPSTREAM_HTTP_ORIGIN}/chat`;\n\
                 const res = await fetch(targetUrl, { method: 'POST' });\n";
    let aliases = vec![HostAliasDecl {
        host: Some("UPSTREAM_HTTP_ORIGIN".into()),
        file: None,
        service: "nat".into(),
        source: Some("proxy/server.js".into()),
    }];
    let index = join(
        &[("ui/proxy/server.js", proxy)],
        vec![
            service("nat", &[], &["nat.svc"]),
            service("nat-ui", &["ui"], &[]),
        ],
        vec![declared("nat", "http:POST /chat")],
        aliases,
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    let target = &resolutions[0].target;
    assert!(
        !matches!(
            target,
            Some(ConsumerTarget::Unresolved {
                reason: lain::federation::contracts::index::UnresolvedReason::NoMatch,
                ..
            })
        ),
        "the alias should route the call to nat, got {target:?}"
    );
}

/// Without the alias the joiner's existing `unbound_host` heuristic still binds
/// a call whose path matches a single provider, at confidence 0.6. This pins
/// that behaviour: a path-only bind is weaker evidence than a declared alias.
#[tokio::test]
async fn without_the_alias_only_the_path_heuristic_binds() {
    let proxy = "const UPSTREAM_HTTP_ORIGIN = `${upstream.protocol}//${upstream.host}`;\n\
                 const targetUrl = `${UPSTREAM_HTTP_ORIGIN}/chat`;\n\
                 const res = await fetch(targetUrl, { method: 'POST' });\n";
    let index = join(
        &[("ui/proxy/server.js", proxy)],
        vec![
            service("nat", &[], &["nat.svc"]),
            service("nat-ui", &["ui"], &[]),
        ],
        vec![declared("nat", "http:POST /chat")],
        vec![],
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    match &resolutions[0].target {
        Some(ConsumerTarget::Binds {
            provenance,
            confidence,
            ..
        }) => {
            assert!(
                format!("{provenance:?}").contains("unbound_host") && *confidence < 1.0,
                "the path-only bind must be the 0.6 heuristic, got {provenance:?} at {confidence}"
            );
        }
        other => panic!("expected the unbound_host heuristic to bind, got {other:?}"),
    }
}

/// The NAT UI proxy is a transparent reverse proxy: its path is whatever the
/// client asked for. With an alias the honest verdict is "known service,
/// unknown route", not a bind.
#[tokio::test]
async fn a_transparent_proxy_with_an_alias_is_known_service_unknown_route() {
    let proxy = "const UPSTREAM_HTTP_ORIGIN = `${upstream.protocol}//${upstream.host}`;\n\
                 const backendPath = pathname.substring(3) || '/';\n\
                 const targetUrl = `${UPSTREAM_HTTP_ORIGIN}${backendPath}`;\n\
                 const res = await fetch(targetUrl, { method: 'POST' });\n";
    let index = join(
        &[("ui/proxy/server.js", proxy)],
        vec![
            service("nat", &[], &["nat.svc"]),
            service("nat-ui", &["ui"], &[]),
        ],
        vec![declared("nat", "http:POST /chat")],
        vec![HostAliasDecl {
            host: Some("UPSTREAM_HTTP_ORIGIN".into()),
            file: None,
            service: "nat".into(),
            source: None,
        }],
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert!(
        matches!(
            resolutions[0].target,
            Some(ConsumerTarget::Unresolved {
                reason: lain::federation::contracts::index::UnresolvedReason::NoRouteInService,
                ..
            })
        ),
        "expected no_route_in_service, got {:?}",
        resolutions[0].target
    );
}

/// A transparent proxy whose host the analyzer cannot follow: declaring the
/// file dials `nat` routes its calls there, with no host expression needed.
#[tokio::test]
async fn a_declared_file_routes_its_unknown_host_calls_to_its_service() {
    let proxy = "const backendPath = pathname.substring(3) || '/';\n\
                 const res = await fetch(backendPath, { method: 'POST' });\n";
    let index = join(
        &[("ui/proxy/server.js", proxy)],
        vec![
            service("nat", &[], &["nat.svc"]),
            service("nat-ui", &["ui"], &[]),
        ],
        vec![],
        vec![HostAliasDecl {
            host: None,
            file: Some("ui/proxy/server.js".into()),
            service: "nat".into(),
            source: Some("declared".into()),
        }],
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert!(
        matches!(
            resolutions[0].target,
            Some(ConsumerTarget::Unresolved {
                reason: lain::federation::contracts::index::UnresolvedReason::NoRouteInService,
                target_service: Some(_),
            })
        ),
        "a declared file should resolve to its service, got {:?}",
        resolutions[0].target
    );
}
