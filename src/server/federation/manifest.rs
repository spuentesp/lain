//! Versioned, bincode-persisted registry of the federation's repo set.
//!
//! `FederationManifest` is the cold-start anchor: Task 13's loader reads it
//! to reconstruct the list of repos the server knows about before any
//! remote handshakes. The on-disk format is bincode with a leading
//! `version: u32` so older binaries can be migrated when the schema
//! breaks.

use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_id::RepoId;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current manifest schema version. Bump when the bincode layout breaks.
pub const CURRENT_VERSION: u32 = 1;

/// `serde_yaml::Value` is not self-describing, so the default `serde::Deserialize`
/// impl calls `deserialize_any` which bincode rejects. For the bincode wire
/// format we serialize the value as a YAML string and parse it back on load.
/// The public field type stays `serde_yaml::Value` so callers don't need to
/// know about the transport.
mod yaml_value_as_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &serde_yaml::Value,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let yaml = serde_yaml::to_string(value).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&yaml)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<serde_yaml::Value, D::Error> {
        let yaml = String::deserialize(deserializer)?;
        serde_yaml::from_str(&yaml).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoEntry {
    pub id: RepoId,
    pub source_kind: String,
    #[serde(with = "yaml_value_as_string")]
    pub source_config: serde_yaml::Value,
    pub last_indexed_unix: i64,
    pub content_hash: String,
    pub health: RepoHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationManifest {
    pub version: u32,
    pub repos: Vec<RepoEntry>,
}

impl Default for FederationManifest {
    fn default() -> Self {
        Self { version: CURRENT_VERSION, repos: Vec::new() }
    }
}

impl FederationManifest {
    /// Load a manifest from `path`, or return an empty default when the file
    /// does not exist. Any read/deserialize error is surfaced as `LainError`.
    /// A deserialized manifest with `version > CURRENT_VERSION` is rejected
    /// with `LainError::UnsupportedManifestVersion` so a newer binary cannot
    /// silently downgrade an older one's data.
    pub fn load_or_default(path: &Path) -> Result<Self, LainError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path)
            .map_err(|e| LainError::Io(format!("read manifest: {e}")))?;
        let m: Self = bincode::deserialize(&bytes)
            .map_err(|e| LainError::Serialization(format!("bincode: {e}")))?;
        if m.version > CURRENT_VERSION {
            return Err(LainError::UnsupportedManifestVersion(m.version));
        }
        Ok(m)
    }

    /// Persist the manifest to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<(), LainError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LainError::Io(format!("mkdir: {e}")))?;
        }
        let bytes = bincode::serialize(self)
            .map_err(|e| LainError::Serialization(format!("bincode: {e}")))?;
        // Write through a temp file and rename. A plain `fs::write`
        // truncates the destination first, so a crash — or a reader
        // arriving mid-write — sees a half-length bincode blob, and
        // `Manifest::load` rejects it. That loses the federation's whole
        // repo registry to an interrupted save. `graph::save_to_disk`
        // already went through this helper; the manifest did not.
        crate::cli::io::write_file_atomic(path, bytes)
            .map_err(|e| LainError::Io(format!("write manifest: {e}")))?;
        Ok(())
    }

    pub fn add_repo(&mut self, entry: RepoEntry) {
        self.repos.push(entry);
    }

    pub fn remove_repo(&mut self, id: &RepoId) {
        self.repos.retain(|r| r.id != *id);
    }
}

#[cfg(test)]
mod atomic_save_tests {
    use super::*;

    /// `save` must never leave a partially-written manifest where the
    /// real one was. A plain `fs::write` truncates the destination
    /// first, so an interrupted save destroys the federation's repo
    /// registry — `load` then rejects the short bincode blob.
    #[test]
    fn save_replaces_the_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("manifest.bin");

        let entry = |id: &str| RepoEntry {
            id: crate::federation::repo_id::RepoId::new(id).unwrap(),
            source_kind: "workspace_dir".to_string(),
            source_config: serde_yaml::Value::Null,
            last_indexed_unix: 0,
            content_hash: String::new(),
            health: crate::federation::health::RepoHealth::Ready,
        };

        let mut first = FederationManifest::default();
        first.add_repo(entry("alpha"));
        first.save(&path).expect("first save");
        assert!(path.exists(), "save creates parent directories");

        // A second save must land whole, and must not leave the temp
        // file behind for the next reader to trip over.
        let mut second = FederationManifest::default();
        second.add_repo(entry("beta"));
        second.save(&path).expect("second save");

        let loaded = FederationManifest::load_or_default(&path).expect("manifest still parses");
        assert_eq!(loaded.repos.len(), 1);
        assert_eq!(loaded.repos[0].id.as_str(), "beta");

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp file left behind: {leftovers:?}");
    }
}
