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

    for source in sources {
        let _permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| LainError::Other(format!("semaphore: {e}")))?;
        let repo_id = source.id().clone();
        fed.add_repo(source, &config.data_dir).await?;
        fed.project_repo(&repo_id).await?;
        drop(_permit);
    }

    save_manifest(&fed, &manifest_path)?;
    Ok(fed)
}

fn save_manifest(_fed: &FederatedIndex, _path: &Path) -> Result<(), LainError> {
    Ok(())
}
