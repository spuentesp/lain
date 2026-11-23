//! `lain prewarm` — build the completed-federation startup checkpoint
//! without serving anything.
//!
//! `lain server` publishes `startup-complete.json` itself, once a full cold
//! index has finished; every later start with unchanged inputs restores the
//! saved graph instead of indexing. That is the wrong shape for an image
//! build or a deploy hook, which wants the checkpoint made *ahead of time*
//! and no listener left running. `lain reindex` cannot stand in: it rebuilds
//! the graph but skips the cross-repo relink and contract join the server
//! runs before publishing, so it never writes the checkpoint.
//!
//! Prewarm runs the server's own startup pipeline (`complete_startup`) and
//! exits. It fails — non-zero — when no checkpoint was written, naming the
//! gate that stopped it, because a bake that silently leaves the next boot to
//! re-index is the failure it exists to prevent.
//!
//! What the checkpoint is keyed on, and so what the runtime must match:
//! the `lain` build, the whole `repos.yaml` (including `data_dir`), run
//! params, the `LAIN_DISABLE_LSP` / `LAIN_TEST_NO_LSP` / `LAIN_LSP_LEAN` /
//! `LAIN_LSP_PREWARM` environment, and for every repo its absolute path,
//! commit and tracked-file contents (a dirty worktree never publishes).

use crate::cli::server::{
    complete_startup, init_tracing, load_federation_for_workspace, StartupCache,
};
use anyhow::{bail, Context, Result};
use std::path::Path;

/// How a prewarm ended when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrewarmOutcome {
    /// The checkpoint was already valid; nothing was indexed.
    AlreadyWarm,
    /// This run indexed everything and published the checkpoint.
    Published,
}

pub async fn run_prewarm(
    config_path: &Path,
    workspace_arg: &str,
    log_level: &str,
) -> Result<PrewarmOutcome> {
    init_tracing(log_level);
    let (fed, _scope) = load_federation_for_workspace(config_path, workspace_arg)
        .await
        .context("federation load")?;

    // A valid checkpoint was restored by the loader. Do not take the
    // server's restored path: it starts source watchers, and there is
    // nothing left to index.
    if fed.startup_restored() && fed.startup_inputs_unchanged().unwrap_or(false) {
        eprintln!("prewarm: the startup checkpoint is already valid; nothing to do");
        return Ok(PrewarmOutcome::AlreadyWarm);
    }

    match complete_startup(&fed).await {
        StartupCache::Published | StartupCache::Restored => {
            eprintln!("prewarm: startup checkpoint published");
            Ok(PrewarmOutcome::Published)
        }
        StartupCache::NotPublished(why) => bail!(
            "indexing finished but no startup checkpoint was published: {why}. \
             `lain server` would index again on every start."
        ),
    }
}
