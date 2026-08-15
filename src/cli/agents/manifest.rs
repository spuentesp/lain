//! Loader for the single-source-of-truth agent manifest.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntry {
    pub id: String,
    pub display_name: String,
    pub binary: String,
    #[serde(default)]
    pub detect_paths: Vec<String>,
    pub config_user: String,
    pub config_project: String,
    pub config_format: String,
    pub mcp_section: String,
    pub mcp_name: String,
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub default_args: Vec<String>,
    #[serde(default)]
    pub headless_probe: Vec<String>,
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String { "json".to_string() }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub agent: Vec<AgentEntry>,
}

fn default_version() -> u32 { 1 }

pub const DEFAULT_MANIFEST: &str = include_str!("../../../agents/manifest.toml");

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse agent manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("agent manifest is missing required id field")]
    MissingId,
    #[error("agent manifest is missing required agent rows")]
    Empty,
}

pub fn load_manifest_from_str(src: &str) -> Result<Vec<AgentEntry>, ManifestError> {
    let parsed: ManifestFile = toml::from_str(src)?;
    if parsed.agent.is_empty() {
        return Err(ManifestError::Empty);
    }
    for a in &parsed.agent {
        if a.id.is_empty() {
            return Err(ManifestError::MissingId);
        }
    }
    Ok(parsed.agent)
}

pub fn load_manifest() -> Result<Vec<AgentEntry>, ManifestError> {
    load_manifest_from_str(DEFAULT_MANIFEST)
}

#[allow(dead_code)]
pub fn write_manifest(path: &Path, agents: &[AgentEntry]) -> std::io::Result<()> {
    let body = ManifestFile { version: 1, agent: agents.to_vec() };
    let s = toml::to_string_pretty(&body).map_err(std::io::Error::other)?;
    std::fs::write(path, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_returns_format_field() {
        let agents = load_manifest().expect("manifest");
        assert!(agents.iter().any(|a| a.format == "http"));
    }
}
