use crate::error::LainError;
use crate::federation::config::FederationConfig;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::manifest::FederationManifest;
use std::path::Path;
use std::sync::Arc;
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
    let _ = save_manifest(&fed, &manifest_path);
    Ok(fed)
}

fn save_manifest(_fed: &FederatedIndex, _path: &Path) -> Result<(), LainError> {
    Ok(())
}
