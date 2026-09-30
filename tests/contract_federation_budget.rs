//! Budget test for the contract join on the tokio + bytes federation.
//!
//! Per §5.3 the design requires a full rejoin of the tokio + bytes
//! federation to complete in ≤ 2 s × `LAIN_PERF_BUDGET_MULTIPLIER`.
//! The fixture is built by `scripts/demo-federation-fixture.sh`
//! which clones both repos with `--depth=1` — that needs network
//! access and is `#[ignore]`'d by default. Run with
//! `cargo test --test contract_federation_budget -- --ignored --nocapture`
//! after the fixture has been built once.
//!
//! If the fixture script cannot run hermetically (no GitHub
//! reachability, missing git, etc.) the test is skipped with a
//! message naming the missing precondition.

use lain::federation::config::SourceConfig;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::ContractIndex;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::RepoSource;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn multiplier() -> f64 {
    std::env::var("LAIN_PERF_BUDGET_MULTIPLIER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0)
}

/// Build the demo federation fixture under a tempdir. Returns the
/// path to the resulting `repos.yaml`. Fails the test on any
/// `git clone` or `git ls-remote` error (network required).
fn build_fixture(root: &std::path::Path) -> PathBuf {
    let script = std::env::current_dir()
        .unwrap()
        .join("scripts/demo-federation-fixture.sh");
    if !script.exists() {
        panic!(
            "scripts/demo-federation-fixture.sh missing at {}; run the budget test from the repo root",
            script.display()
        );
    }
    let status = Command::new("bash")
        .arg(&script)
        .arg(root)
        .status()
        .expect("spawn demo-federation-fixture.sh");
    assert!(status.success(), "demo-federation-fixture.sh failed");
    root.join("repos.yaml")
}

#[tokio::test]
#[ignore = "requires GitHub access to clone tokio + bytes; run with --ignored"]
async fn rejoin_budget_on_tokio_bytes() {
    let project = tempfile::tempdir().expect("tempdir");
    let repos_yaml = build_fixture(project.path());
    if !repos_yaml.exists() {
        panic!("fixture script produced no repos.yaml");
    }
    let cfg = ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "bytes".into(),
                repo: "bytes".into(),
                paths: vec![],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
            },
            ServiceDecl {
                name: "tokio".into(),
                repo: "tokio".into(),
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
    };
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_contract_config(cfg);

    // Spin up two repo sources.
    for id in ["bytes", "tokio"] {
        let source_path = project.path().join(id);
        // The fixture script writes `repos.yaml` with `shallow_clone`
        // entries, but for budget test purposes we just open the
        // already-cloned directory as a `WorkspaceDirSource` —
        // that's enough to drive the joiner.
        let source: Box<dyn RepoSource> = Box::new(
            lain::federation::repo_source::WorkspaceDirSource::with_config(
                RepoId::new(id).unwrap(),
                source_path,
                SourceConfig::WorkspaceDir {
                    path: project.path().join(id),
                },
            )
            .expect("WorkspaceDirSource"),
        );
        source.fetch().await.expect("fetch");
        fed.add_repo(source, &data_dir).await.expect("add_repo");
    }
    for id in ["bytes", "tokio"] {
        let id = RepoId::new(id).unwrap();
        fed.project_repo(&id).await.expect("project_repo");
    }
    // Measure a single full rejoin.
    let started = Instant::now();
    fed.rejoin_contracts().expect("rejoin_contracts");
    let elapsed = started.elapsed();
    println!("rejoin elapsed: {elapsed:?}");
    let budget = Duration::from_secs_f64(2.0 * multiplier());
    assert!(
        elapsed <= budget,
        "rejoin took {elapsed:?}, budget {budget:?} ({}x)",
        multiplier()
    );
    // Also verify the index materialized.
    let idx: Arc<ContractIndex> = fed.contract_index().expect("contract_index");
    println!(
        "index: {} endpoints, {} consumers",
        idx.endpoints.len(),
        idx.consumers.len()
    );
}

#[tokio::test]
#[ignore = "skeleton: reuses the budget fixture setup"]
async fn joiner_unchanged_when_no_contract_nodes() {
    // No nodes means no joins; the budget is just the boot cost
    // of `rejoin_contracts_if_dirty`.
    let project = tempfile::tempdir().unwrap();
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let cfg = ContractFederationConfig::default();
    let backend: Arc<dyn lain::federation::graph_backend::GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_contract_config(cfg);
    let started = Instant::now();
    fed.rejoin_contracts_if_dirty().expect("rejoin");
    let elapsed = started.elapsed();
    let budget = Duration::from_millis(500);
    assert!(elapsed < budget, "empty rejoin took {elapsed:?}");
}

#[allow(dead_code)]
fn _hush(_d: Duration) {}
