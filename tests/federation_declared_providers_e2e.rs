//! A provider the analyzer cannot see (a runtime-configured route) can be
//! declared in `provides`. The declaration enters the endpoint table, merges
//! with a detected provider of the same key, and binds a consumer.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lain::federation::contracts::config::{ContractFederationConfig, ProviderDecl, ServiceDecl};
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
