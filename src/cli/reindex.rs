//! `lain reindex` — re-index the workspace from scratch.
//!
//! Backs up the existing `<data_dir>/federated_graph.bin` to
//! `<data_dir>/federated_graph.bin.bak` and rebuilds every repo's
//! per-repo graph plus the federation backend. Required after a
//! federation schema version bump.
//!
//! Idempotent — running it twice produces the same graph and the same
//! backup file (overwritten with a fresh copy from the first run).
//!
//! Failure-tolerance: an `index_forced` failure aborts the whole run
//! (a half-rebuilt graph is harder to debug than "reindex failed: ..."),
//! but a `project_repo` failure after a successful `index_forced` is
//! logged and skipped — the per-repo graph is already fresh and the
//! next `lain reindex` (or `lain server` startup) re-projects for free.

use crate::cli::resolve_repos_config;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub async fn run_reindex(
    config: PathBuf,
    workspace: Option<String>,
    verbose: bool,
) -> Result<()> {
    let config_path = resolve_repos_config(&config);
    let federation_cfg = crate::server::federation::config::FederationConfig::load(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    let data_dir = federation_cfg.data_dir.clone();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;

    let graph_bin = data_dir.join("federated_graph.bin");
    let graph_bin_bak = data_dir.join("federated_graph.bin.bak");

    // Step 1: backup the existing graph so an operator can recover if
    // the rebuild misbehaves. `std::fs::rename` is atomic on the same
    // filesystem, which is what we want — a half-copied file would be
    // useless.
    if graph_bin.exists() {
        if verbose {
            eprintln!(
                "backing up {} → {}",
                graph_bin.display(),
                graph_bin_bak.display()
            );
        }
        std::fs::rename(&graph_bin, &graph_bin_bak).with_context(|| {
            format!(
                "backup {} → {}",
                graph_bin.display(),
                graph_bin_bak.display()
            )
        })?;
    } else if verbose {
        eprintln!("no existing {} to back up", graph_bin.display());
    }

    // Step 2: load the federation. The loader's Phase 0/1/2
    // orchestration (Task 4) registers every repo, projects every node,
    // then projects every edge — but it does NOT call `repo.index()`.
    // That is the caller's responsibility (see the docstring on
    // `federation::loader::load_federation` at
    // `src/server/federation/loader.rs:91-94`).
    //
    // When `--workspace` is set we use the workspace-scoped loader so
    // the federation matches the scope `lain server --workspace` would
    // pick up. Without `--workspace` we re-index the entire federation
    // — every repo in `repos.yaml`.
    let fed = if let Some(ws_name) = workspace.as_deref() {
        let workspaces_path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("workspaces.yaml");
        crate::server::federation::loader::load_federation_with_workspace(
            &config_path,
            &workspaces_path,
            ws_name,
        )
        .await
        .map_err(|e| anyhow::anyhow!("load workspace '{ws_name}': {e}"))?
    } else {
        crate::server::federation::loader::load_federation(&config_path)
            .await
            .map_err(|e| anyhow::anyhow!("load federation: {e}"))?
    };

    // Step 3: re-index every repo and re-project. Pattern transcribed
    // from `src/cli/server.rs:62-85` with `index()` swapped for
    // `index_forced()`. Stops on the first `index_forced` failure —
    // a partial re-index would leave the operator looking at a
    // half-rebuilt graph that's harder to debug than "reindex failed:
    // <repo>: <reason>".
    for (repo_id, _) in fed.list_repos() {
        let Some(repo) = fed.get_repo(&repo_id) else {
            continue;
        };
        if verbose {
            eprintln!("re-indexing repo {}", repo_id);
        }
        repo.index_forced()
            .await
            .map_err(|e| anyhow::anyhow!("re-indexing repo '{repo_id}': {e}"))?;
        // After a successful `index_forced`, re-project so the
        // federated backend sees the freshly-extracted nodes/edges.
        // (This mirrors `server.rs`'s `fed.project_repo(&id)` call.)
        // Failures here are logged but not propagated — the per-repo
        // graph is already fresh and a re-run picks up the projection
        // for free.
        if let Err(e) = fed.project_repo(&repo_id).await {
            tracing::warn!(
                "project_repo for '{repo_id}' after re-indexing failed: {e}"
            );
        }
    }

    if verbose {
        eprintln!("re-index complete");
    }
    Ok(())
}
