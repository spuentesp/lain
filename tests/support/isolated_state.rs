//! Keep in-process test servers out of the developer's real state
//! directory.
//!
//! `LainServer` keeps presence state and the audit log under
//! `state_dir()` (`$XDG_STATE_HOME/lain`, else `~/.local/lain/state`).
//! Tests that built servers without isolating it appended to the real
//! `audit.jsonl` on every run — 28 MB on one machine, which in turn made
//! `get_audit_log` and `get_recent_activity` slow there and only there.
//!
//! One private directory per test process, set before the first server is
//! built. Use these wrappers instead of the constructors directly.
#![allow(dead_code)]

use lain::error::LainError;
use lain::federation::federated_index::FederatedIndex;
use lain::server::{LainServer, Transport};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = tempfile::tempdir().expect("state tempdir").keep();
        std::env::set_var("XDG_STATE_HOME", dir);
    });
}

pub fn new_server(
    workspace: &Path,
    memory_path: &Path,
    embedding_model: Option<&Path>,
) -> Result<LainServer, LainError> {
    isolate();
    LainServer::new(workspace, memory_path, embedding_model)
}

pub fn with_federation(
    federation: Arc<FederatedIndex>,
    transport: Transport,
    port: u16,
    repos_yaml: Option<PathBuf>,
    embedding_model: Option<&Path>,
) -> Result<LainServer, LainError> {
    isolate();
    LainServer::with_federation(federation, transport, port, repos_yaml, embedding_model)
}

pub async fn load_federation(config_path: &Path) -> Result<Arc<FederatedIndex>, LainError> {
    isolate();
    lain::federation::loader::load_federation(config_path).await
}
