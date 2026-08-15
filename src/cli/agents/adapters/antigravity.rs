//! Antigravity CLI (`agy`) adapter.
//!
//! Antigravity ships as the standalone `agy` binary and uses the same
//! `~/.gemini/settings.json` MCP server format that Gemini used. The
//! install path is at `binary = "agy"`, but the config target is the
//! same `gemini` config file the legacy Gemini CLI wrote, so the
//! adapter does not need to special-case the format.

use super::{
    expand_home, server_for, write_gemini_mcp_config, AdapterError, AgentAdapter, AUTO_WORKSPACE, InstallScope,
};
use crate::cli::agents::manifest::AgentEntry;
use serde_json::Value;
use std::path::Path;

pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str { "antigravity" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        let workspace = AUTO_WORKSPACE.to_string();
        let server = server_for(entry, &workspace);
        write_gemini_mcp_config(&path, &server)
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path: &Path = &expand_home(path);
        super::read_gemini_mcp_config(path)
    }
}
