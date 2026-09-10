use crate::error::LainError;
use crate::federation::config::FederationConfig;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::manifest::{FederationManifest, RepoEntry};
use crate::federation::workspace::{WorkspacesFile, WorkspaceIndex, filter_repos_by_workspace};
use crate::server::time;
use crate::state::resolve_active_workspace;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub async fn load_federation(config_path: &Path) -> Result<Arc<FederatedIndex>, LainError> {
    let config = FederationConfig::load(config_path)?;
    let manifest_path = config.data_dir.join("federation_manifest.bin");
    let _manifest = FederationManifest::load_or_default(&manifest_path)?;

    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&config.data_dir)?);
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_ready_threshold(config.ready_threshold);
    // Wire the manifest path before any add_repo so a runtime add_repo
    // / remove_repo persists its mutation. Without this, only the
    // end-of-load save_manifest below sees the file written.
    fed.set_manifest_path(Some(manifest_path.clone()));

    let sources = config.build_sources()?;
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_indexers));

    // Spawn per-repo indexers up to `max_concurrent_indexers` in flight, then
    // await them all. The semaphore is acquired *before* spawn so the limit
    // applies to the in-flight count, not the spawn count; each task holds
    // its permit until completion (the `_permit` binding), so permits are
    // released by drop when the task end.
    //
    // For each source we first run `fetch()` (clones the repo if needed).
    // `WorkspaceDirSource::fetch` is a no-op so this is cheap for in-tree
    // repos; for `ShallowCloneSource` it materializes the on-disk checkout
    // that `RepoIndex::new` (via `GitSensor::new`) requires to exist.
    let mut handles = Vec::with_capacity(sources.len());
    for src in sources {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| LainError::Other(format!("semaphore: {e}")))?;
        let fed_clone = fed.clone();
        let data_dir = config.data_dir.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            src.fetch().await?;
            let repo_id = src.id().clone();
            fed_clone.add_repo(src, &data_dir).await?;
            fed_clone.project_repo(&repo_id).await?;
            // Wire the federation as this repo's cross-repo resolver
            // (wishlist #13) so a subsequent `repo.index()` can
            // materialize cross-repo `Calls` edges.
            if let Some(repo) = fed_clone.get_repo(&repo_id) {
                repo.set_cross_repo_resolver(fed_clone.clone());
            }
            Ok::<(), LainError>(())
        }));
    }
    for h in handles {
        h.await.map_err(|e| LainError::Other(format!("join: {e}")))??;
    }

    // Persist the manifest on a best-effort basis: a save failure must not
    // tear down a federation that successfully loaded.
    //
    // Per the spec, cold restart loads the manifest first and then
    // re-attaches each repo's bincode. Today the authoritative repo list
    // still comes from `repos.yaml` (re-read on every load), so the
    // manifest is an observability snapshot of what *was* loaded rather
    // than the source of truth for repo membership — see the inline notes
    // in `save_manifest` for the full rationale.
    // Discarding this hid a failed save entirely: the federation came up
    // with no persisted snapshot and nothing said so. The manifest is
    // observability rather than source of truth, so a failure must not
    // abort startup — but it must be visible.
    if let Err(e) = save_manifest(&fed, &manifest_path) {
        tracing::warn!("federation manifest not saved to {manifest_path:?}: {e}");
    }
    Ok(fed)
}

/// Load a federation scoped to a single workspace's repos. Same pattern as
/// `load_federation` but filters `repos.yaml` to the workspace's members
/// before adding them to the federation. Errors fast at config time if the
/// workspace references a repo id not in `repos.yaml`.
///
/// `workspaces.yaml` is loaded from `<config_path parent>/workspaces.yaml`
/// by default; pass an explicit path via the `workspaces_path` arg if it's
/// somewhere else.
///
/// Like `load_federation`, this function does NOT call `repo.index()`. The
/// per-repo indexing pass is the caller's responsibility (see
/// `src/cmds/server.rs:35-74` for the canonical pattern that handles both
/// all-repos and workspace modes uniformly).
pub async fn load_federation_with_workspace(
    config_path: &Path,
    workspaces_path: &Path,
    workspace_name: &str,
) -> Result<Arc<FederatedIndex>, LainError> {
    let config = FederationConfig::load(config_path)?;
    let manifest_path = config.data_dir.join("federation_manifest.bin");
    let _manifest = FederationManifest::load_or_default(&manifest_path)?;

    // Load + validate the workspaces file; resolve the named workspace.
    let workspaces = if workspaces_path.exists() {
        WorkspacesFile::load(workspaces_path)?
    } else {
        WorkspacesFile::default()
    };
    let ws_spec = resolve_active_workspace(&workspaces, workspace_name)?.clone();
    let workspace = WorkspaceIndex::from_spec(ws_spec);

    // Filter repos.yaml to the workspace's members. If any member id is
    // not in repos.yaml, fail with the missing ids listed.
    let picked = filter_repos_by_workspace(&config.repos, &workspace)?;

    // Build the federation.
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&config.data_dir)?);
    let fed = Arc::new(FederatedIndex::new(backend));
    fed.set_ready_threshold(config.ready_threshold);
    // Wire the manifest path before any add_repo so a runtime add_repo
    // / remove_repo persists its mutation. Without this, only the
    // end-of-load save_manifest below sees the file written.
    fed.set_manifest_path(Some(manifest_path.clone()));

    // Spawn per-repo indexers up to `max_concurrent_indexers` in flight, then
    // await them all. Mirrors `load_federation`'s per-repo loop exactly —
    // it adds each repo to the federation and projects whatever is in the
    // per-repo DB (empty on a fresh load; populated later by the indexing
    // pass in `run_server`).
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_indexers));
    let mut handles = Vec::with_capacity(picked.len());
    for repo_config in picked {
        let permit = semaphore.clone().acquire_owned().await
            .map_err(|e| LainError::Other(format!("semaphore: {e}")))?;
        let fed_clone = fed.clone();
        let data_dir = config.data_dir.clone();
        let source = config.build_source_for(repo_config)?;
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            source.fetch().await?;
            let repo_id = source.id().clone();
            fed_clone.add_repo(source, &data_dir).await?;
            fed_clone.project_repo(&repo_id).await?;
            Ok::<(), LainError>(())
        }));
    }
    for h in handles {
        h.await.map_err(|e| LainError::Other(format!("join: {e}")))??;
    }

    // Discarding this hid a failed save entirely: the federation came up
    // with no persisted snapshot and nothing said so. The manifest is
    // observability rather than source of truth, so a failure must not
    // abort startup — but it must be visible.
    if let Err(e) = save_manifest(&fed, &manifest_path) {
        tracing::warn!("federation manifest not saved to {manifest_path:?}: {e}");
    }
    Ok(fed)
}

/// Build a `FederationManifest` from the in-memory federation and persist it
/// to `path`.
///
/// Per the spec (`docs/superpowers/specs/2026-08-07-federated-indexer-design.md:244-249`)
/// the cold-restart contract is: load the manifest first, then re-attach
/// each repo's bincode. In this MVP, repo membership is still authoritative
/// in `repos.yaml` (the loader re-reads it on every cold restart), so the
/// manifest is persisted as a *snapshot* of the federation the server is
/// currently serving — useful for observability and future tooling, but not
/// (yet) the source of truth for which repos the server knows about.
///
/// `source_config` is now the verbatim `SourceConfig` the `RepoSource` was
/// constructed from (round-tripped via `serde_yaml`), and `content_hash`
/// is `git rev-parse HEAD` for git-backed sources (or `""` for sources
/// without a local checkout — `WorkspaceDirSource` over a non-repo dir,
/// sensor-backed sources, etc.). When `add_repo`/`remove_repo` mutate the
/// federation at runtime, callers should invoke `save_manifest` again so
/// the on-disk file stays in sync.
fn save_manifest(fed: &FederatedIndex, path: &Path) -> Result<(), LainError> {
    let mut manifest = FederationManifest::default();
    for (id, health) in fed.list_repos() {
        let Some(repo) = fed.get_repo(&id) else {
            // `list_repos` is sourced from the same map `get_repo` reads,
            // so this branch should be unreachable. We skip instead of
            // returning an error so a torn read (e.g. another task is
            // mid-`remove_repo`) doesn't tear down a successful load.
            continue;
        };
        let source = repo.source();
        // Serialize the original `SourceConfig` YAML the source was
        // constructed from. Storing the verbatim value rather than a
        // re-derived one means the manifest can be inspected to see
        // exactly what `repos.yaml` said at load time — useful when
        // reconciling a stale manifest against the current config.
        let source_config = serde_yaml::to_value(source.source_config())
            .map_err(|e| LainError::Serialization(format!(
                "manifest source_config for {id}: {e}"
            )))?;
        // Best-effort content fingerprint. `Err` propagates because a
        // corrupt `.git` or lock contention is exactly the case an
        // operator most wants a clear error for — silently writing an
        // empty hash would mask a real problem. Sources that don't
        // have a checkout return `Ok(None)` and we write `""`.
        let content_hash = match source.content_hash() {
            Ok(Some(hash)) => hash,
            Ok(None) => String::new(),
            Err(e) => return Err(e),
        };
        let last_indexed_unix = time::unix_secs(repo.last_indexed());
        manifest.add_repo(RepoEntry {
            id: id.clone(),
            source_kind: source.kind().to_string(),
            source_config,
            last_indexed_unix,
            content_hash,
            health,
        });
    }
    manifest.save(path)
}
