//! The protocol sensors must actually run during ingestion.
//!
//! `server::sensors` — five sensors, ~1k lines — had no caller anywhere
//! in `ingest/`. `http_sensor.rs` was not even declared in
//! `sensors/mod.rs`, so it never compiled: it carried two malformed raw
//! string literals and a call to `GraphDatabase::find_nodes_by_name`, a
//! method that does not exist. Meanwhile `describe_schema` advertised
//! `HttpRoute`, `CallsHttp` and `Implements` as queryable and
//! `get_cross_runtime_callers` — whose only job is reading them —
//! answered "No HTTP routes call this handler" for every symbol in every
//! codebase, which is a claim about the user's code rather than about
//! lain's blind spot.
//!
//! A unit test of `sensors::run_all` proves the sensors work; it cannot
//! prove ingestion calls them. This drives the real pipeline.

use lain::server::federation::repo_id::RepoId;
use lain::server::federation::repo_index::RepoIndex;
use lain::server::federation::repo_source::WorkspaceDirSource;
use lain::server::schema::{EdgeType, NodeType};
use std::process::Command;
use std::sync::Arc;

fn init_repo(dir: &std::path::Path) {
    let ok = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .arg(dir)
        .status()
        .expect("git init")
        .success();
    assert!(ok);
    for args in [
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Sensor Test"],
        vec!["add", "-A"],
        vec!["commit", "--quiet", "-m", "initial"],
    ] {
        assert!(Command::new("git")
            .current_dir(dir)
            .args(&args)
            .status()
            .expect("git")
            .success());
    }
}

#[tokio::test]
async fn ingestion_produces_http_route_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    std::fs::create_dir_all(repo.join("api")).unwrap();

    // Gin-style: method, path and handler on one line, which is what the
    // line-oriented extractor can see.
    std::fs::write(
        repo.join("api").join("routes.go"),
        "package api\n\nfunc Register(r *Router) {\n\tr.GET(\"/api/users\", listUsers)\n}\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("sensors").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    ri.index().await.expect("index");

    let routes: Vec<_> = ri
        .nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpRoute)
        .collect();

    assert!(
        !routes.is_empty(),
        "ingestion must run the protocol sensors: no HttpRoute node was \
         produced for `r.GET(\"/api/users\", listUsers)`. If this fails, \
         `sensors::run_all` is no longer called from the ingest pipeline \
         and `describe_schema` is advertising a type nothing can produce."
    );
    assert!(
        routes.iter().any(|n| n.name.contains("/api/users")),
        "the route node should name its path; got {:?}",
        routes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );

    // Node paths must be workspace-relative like every other node. When
    // they were absolute the orphan sweep — which compares against
    // `graph_path`-reduced tracked files — pruned every route node in the
    // same pass that created it, so the sensor reported success and the
    // graph ended up empty.
    for n in &routes {
        assert!(
            !n.path.starts_with('/') || !n.path.starts_with(repo.to_string_lossy().as_ref()),
            "route node path must be workspace-relative, got {:?}",
            n.path
        );
    }
}

/// An OpenAPI spec is the other route producer, and it went through a
/// different code path (`enrich_with_openapi`) with the same absolute
/// path bug.
#[tokio::test]
async fn ingestion_indexes_an_openapi_spec() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    std::fs::create_dir_all(repo.join("api")).unwrap();
    std::fs::write(
        repo.join("api").join("openapi.yaml"),
        "openapi: 3.0.0\ninfo:\n  title: probe\n  version: \"1\"\npaths:\n  /api/widgets:\n    get:\n      operationId: listWidgets\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("spec").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());
    ri.index().await.expect("index");

    let routes: Vec<_> = ri
        .nodes()
        .into_iter()
        .filter(|n| n.node_type == NodeType::HttpRoute)
        .collect();
    assert!(
        routes.iter().any(|n| n.name.contains("/api/widgets")),
        "the OpenAPI spec's operation should become an HttpRoute; got {:?}",
        routes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

/// A repo with no protocol surface must index exactly as before — the
/// sensors are additive, not a new failure mode.
#[tokio::test]
async fn a_repo_with_no_routes_still_indexes_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src").join("lib.rs"),
        "pub fn helper() -> u32 { 1 }\npub fn caller() -> u32 { helper() + 1 }\n",
    )
    .unwrap();
    init_repo(&repo);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source =
        Box::new(WorkspaceDirSource::new(RepoId::new("plain").unwrap(), repo.clone()).unwrap());
    let ri = Arc::new(RepoIndex::new(source, &data_dir).unwrap());

    ri.index().await.expect("index must still succeed");

    let nodes = ri.nodes();
    assert!(nodes.iter().any(|n| n.name == "helper"), "symbols still indexed");
    assert_eq!(
        nodes
            .iter()
            .filter(|n| n.node_type == NodeType::HttpRoute)
            .count(),
        0,
        "no routes in this repo, so no HttpRoute nodes"
    );
    assert_eq!(
        ri.db()
            .all_edges()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::CallsHttp)
            .count(),
        0
    );
}
