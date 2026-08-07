use crate::error::LainError;
use crate::federation::config::FederationConfig;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::manifest::{FederationManifest, RepoEntry};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

pub async fn load_federation(config_path: &Path) -> Result<Arc<FederatedIndex>, LainError> {
    let config = FederationConfig::load(config_path)?;
    let manifest_path = config.data_dir.join("federation_manifest.bin");
    let _manifest = FederationManifest::load_or_default(&manifest_path)?;

    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&config.data_dir)?);
    let fed = Arc::new(FederatedIndex::new(backend));

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
    let _ = save_manifest(&fed, &manifest_path);
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
/// TODO: once `add_repo`/`remove_repo` mutations become runtime-mutable
/// (rather than reloaded from YAML on every restart), persist the manifest
/// from those mutation sites instead of from this single end-of-load call,
/// so the on-disk manifest stays consistent with the live in-memory state.
///
/// TODO: populate `source_config` from the live `RepoSource` config — today
/// we only persist the kind label (`"workspace_dir"` etc.); round-tripping
/// the full YAML config would require plumbing the original `SourceConfig`
/// through `RepoSource`.
///
/// TODO: populate `content_hash` from `git rev-parse HEAD` (or equivalent)
/// for the repo's local checkout.
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
        let last_indexed_unix = system_time_to_unix_secs(repo.last_indexed());
        manifest.add_repo(RepoEntry {
            id: id.clone(),
            source_kind: repo.source().kind().to_string(),
            // TODO: serialize the original `SourceConfig` YAML.
            source_config: serde_yaml::Value::Null,
            last_indexed_unix,
            // TODO: hash the repo's HEAD commit (or content tree).
            content_hash: String::new(),
            health,
        });
    }
    manifest.save(path)
}

fn system_time_to_unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // A pre-epoch `SystemTime` (rare, only `SystemTime::UNIX_EPOCH`
        // itself in practice) collapses to 0 rather than underflowing.
        .unwrap_or(0)
}
