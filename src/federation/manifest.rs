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
        std::fs::write(path, bytes)
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
