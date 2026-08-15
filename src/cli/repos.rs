//! `lain repos` subcommand — manage the `repos.yaml` federation registry.
//!
//! PR 3 will land the full implementation (list/add/remove/show for
//! the repos declared in `repos.yaml`). For Task 1.9 we only need
//! the clap-derived `ReposAction` enum and a single `run` dispatcher
//! so the new top-level `Repos` variant in `src/main.rs` compiles.

use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

/// Subcommands for `lain repos`.
#[derive(Debug, Subcommand)]
pub enum ReposAction {
    /// List all repos registered in `repos.yaml`.
    List,
    /// Register a new repo in `repos.yaml`.
    Add {
        #[arg(long)] id: String,
        #[arg(long)] path: std::path::PathBuf,
    },
    /// Remove a repo from `repos.yaml`.
    Remove {
        #[arg(long)] id: String,
    },
    /// Show the resolved spec for one repo.
    Show {
        id: String,
    },
}

/// Dispatch a `lain repos <action>` invocation.
///
/// Stubbed until PR 3 — the body must compile so the new top-level
/// `Repos` variant wires up cleanly.
pub fn run(_action: ReposAction, _config: &Path) -> Result<()> {
    anyhow::bail!("`lain repos` is not implemented yet; PR 3 will land it")
}
