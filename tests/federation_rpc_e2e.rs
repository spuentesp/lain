//! A gRPC provider (proto) and a gRPC consumer (python SDK) in the
//! same repo, split into two services by `services[].paths`, must
//! produce one `Binds` edge — and the provider's package-qualified
//! fallback must join a bare `OpenShell` to `openshell.v1.OpenShell`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::{ConsumerResolution, ConsumerTarget};
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

fn service(name: &str, paths: &[&str], hosts: &[&str], env: &[&str]) -> ServiceDecl {
    ServiceDecl {
        name: name.into(),
        repo: "openshell".into(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        env: env.iter().map(|e| e.to_string()).collect(),
        base_path: None,
        route_prefixes: vec![],
        provides_graphql: true,
    }
}

const PROTO: &str = "syntax = \"proto3\";\n\
package openshell.v1;\n\
service OpenShell {\n\
\x20 rpc Health(HealthRequest) returns (HealthResponse);\n\
}\n\
message HealthRequest {}\n\
message HealthResponse {}\n";

const SDK: &str = "import os\n\
import grpc\n\
import openshell_pb2_grpc\n\
\n\
env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\n\
\n\
class SandboxClient:\n\
\x20   def __init__(self, endpoint):\n\
\x20       self._channel = grpc.insecure_channel(endpoint)\n\
\x20       self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)\n\
\n\
\x20   def health(self):\n\
\x20       return self._stub.Health(request)\n";

/// Index one repo holding `files` (repo-relative path → content) under
/// `services`, then return the joined contract index.
async fn join_repo(
    files: &[(&str, &str)],
    services: Vec<ServiceDecl>,
) -> Arc<lain::federation::contracts::index::ContractIndex> {
    let root = tempfile::tempdir().unwrap();
    let repo_dir = root.path().join("openshell");
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

    let repo_id = RepoId::new("openshell").unwrap();
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
        ..Default::default()
    });

    let repo = fed.get_repo(&repo_id).unwrap();
    repo.index().await.unwrap();
    fed.project_repo(&repo_id).await.unwrap();
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    fed.contract_index().expect("contract index")
}

fn gateway_and_sdk() -> Vec<ServiceDecl> {
    vec![
        service(
            "openshell-gateway",
            &["proto"],
            &["localhost"],
            &["OPENSHELL_GATEWAY"],
        ),
        service("openshell-sdk", &["python"], &[], &[]),
    ]
}

#[tokio::test]
async fn proto_provider_binds_the_python_sdk_consumer_across_two_services() {
    let index = join_repo(
        &[
            ("proto/openshell.proto", PROTO),
            ("python/openshell/sandbox.py", SDK),
        ],
        gateway_and_sdk(),
    )
    .await;

    // The provider is the proto surface: the method is the endpoint template.
    let health_endpoints = index
        .endpoints
        .values()
        .filter(|e| e.template == "Health")
        .count();
    assert_eq!(
        health_endpoints, 1,
        "expected the proto's Health method as an endpoint"
    );

    // Exactly one consumer resolution for the one SDK call site, and it binds.
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert!(
        matches!(resolutions[0].target, Some(ConsumerTarget::Binds { .. })),
        "expected a bind, got {:?}",
        resolutions[0].target
    );
}

/// Review focus #4: a stub handle whose constructor names a service
/// that owns no matching provider must stay unbound, never bind to
/// the gateway's `Health` because the method name happens to match.
#[tokio::test]
async fn stub_handle_for_a_non_candidate_service_stays_unbound() {
    let sdk = SDK.replace("OpenShellStub", "OtherStub");
    let index = join_repo(
        &[
            ("proto/openshell.proto", PROTO),
            ("python/openshell/sandbox.py", &sdk),
        ],
        gateway_and_sdk(),
    )
    .await;

    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert!(
        !matches!(resolutions[0].target, Some(ConsumerTarget::Binds { .. })),
        "a stub for an unknown service must not bind, got {:?}",
        resolutions[0].target
    );
}

/// Review focus #5: a service whose `paths` prefix (`proto/`) matches
/// no file must not claim `python/...`. Without a python service the
/// SDK call belongs to the repo's implicit service, not the gateway.
#[tokio::test]
async fn service_paths_do_not_claim_files_outside_their_prefix() {
    let index = join_repo(
        &[
            ("proto/openshell.proto", PROTO),
            ("python/openshell/sandbox.py", SDK),
        ],
        vec![service(
            "openshell-gateway",
            &["proto/"],
            &["localhost"],
            &["OPENSHELL_GATEWAY"],
        )],
    )
    .await;

    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert_ne!(
        resolutions[0].service.0, "openshell-gateway",
        "the gateway's proto/ prefix must not own the python SDK file"
    );
}

/// The real OpenShell `sandbox.py` reads two env vars. Only one is
/// declared in `services[].env`, so the call binds through that one.
#[tokio::test]
async fn a_declared_env_candidate_picks_the_dial_var_among_two_reads() {
    let sdk = SDK.replace(
        "env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\n",
        "env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\nconfigured = os.environ.get(\"XDG_CONFIG_HOME\")\n",
    );
    let index = join_repo(
        &[
            ("proto/openshell.proto", PROTO),
            ("python/openshell/sandbox.py", &sdk),
        ],
        gateway_and_sdk(),
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert!(
        matches!(resolutions[0].target, Some(ConsumerTarget::Binds { .. })),
        "expected a bind through the declared env var, got {:?}",
        resolutions[0].target
    );
}

/// When two declared env candidates each lead to a service that exposes
/// the same provider, the joiner cannot tell which one dials the
/// gateway, so the call stays unbound.
#[tokio::test]
async fn two_declared_env_candidates_stay_unbound() {
    let sdk = SDK.replace(
        "env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\n",
        "env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\nconfigured = os.environ.get(\"XDG_CONFIG_HOME\")\n",
    );
    let services = vec![
        service(
            "openshell-gateway",
            &["proto"],
            &["localhost"],
            &["OPENSHELL_GATEWAY"],
        ),
        service("openshell-mirror", &["mirror"], &[], &["XDG_CONFIG_HOME"]),
        service("openshell-sdk", &["python"], &[], &[]),
    ];
    let index = join_repo(
        &[
            ("proto/openshell.proto", PROTO),
            ("mirror/openshell.proto", PROTO),
            ("python/openshell/sandbox.py", &sdk),
        ],
        services,
    )
    .await;
    let resolutions: Vec<&ConsumerResolution> = index.consumers.values().flatten().collect();
    assert_eq!(
        resolutions.len(),
        1,
        "expected one consumer, got {resolutions:?}"
    );
    assert!(
        !matches!(resolutions[0].target, Some(ConsumerTarget::Binds { .. })),
        "two declared candidates must not bind, got {:?}",
        resolutions[0].target
    );
}
