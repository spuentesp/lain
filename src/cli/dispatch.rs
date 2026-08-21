//! CLI dispatch re-exports.
//!
//! `main_command_factory` constructs the top-level `lain` clap
//! `Command` so tests can inspect the rendered help text without
//! running the binary. The binary in `src/main.rs` imports
//! `lain::cli::{Args, Commands}` and dispatches.

use clap::CommandFactory;

use crate::cli::hooks::HooksAction;

/// Return the top-level `lain` clap `Command`. Used by the
/// `top_level_help_lists_only_kept_subcommands` test to verify the
/// subcommand surface, and by `src/main.rs` when no subcommand is
/// given (default action: print help).
///
/// Exposed at the crate root as `lain::main_command_factory` via the
/// `pub use` in `src/lib.rs`.
pub fn main_command_factory() -> clap::Command {
    crate::cli::Args::command()
}

/// Dispatch a `lain hooks <claim|release|overlap-check|lock|unlock>`
/// invocation. Each arm maps the clap-shaped struct straight onto the
/// free function in `crate::cli::hooks`, so this file stays a thin
/// routing layer.
pub fn run(action: HooksAction) -> anyhow::Result<()> {
    match action {
        HooksAction::Claim {
            url,
            path,
            symbol,
            intent,
            agent_name,
            agent_kind,
            parent_session_id,
        } => crate::cli::hooks::claim(
            &url,
            &path,
            &symbol,
            &intent,
            &agent_name,
            &agent_kind,
            &parent_session_id,
        )
        .map_err(|e| anyhow::anyhow!("{e}")),
        HooksAction::Release {
            url,
            path,
            symbol,
            agent_name,
            agent_kind,
            parent_session_id,
        } => crate::cli::hooks::release(
            &url,
            &path,
            &symbol,
            &agent_name,
            &agent_kind,
            &parent_session_id,
        )
        .map_err(|e| anyhow::anyhow!("{e}")),
        HooksAction::OverlapCheck {
            url,
            base,
            head,
            workspace,
        } => crate::cli::hooks::overlap_check(
            &url,
            &base,
            head.as_deref(),
            &workspace,
        )
        .map_err(|e| anyhow::anyhow!("{e}")),
        HooksAction::Lock {
            workspace_root,
            path,
            agent_name,
            agent_kind,
            intent,
        } => crate::cli::hooks::lock(
            &workspace_root,
            &path,
            &agent_name,
            &agent_kind,
            &intent,
        )
        .map_err(|e| anyhow::anyhow!("{e}")),
        HooksAction::Unlock {
            workspace_root,
            path,
            agent_name,
        } => crate::cli::hooks::unlock(&workspace_root, &path, &agent_name)
            .map_err(|e| anyhow::anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    /// The top-level help text must list the kept subcommands
    /// (`server`, `workspaces`, `repos`, `query`, `ask`, `hooks`,
    /// `init`) and must NOT list the removed ones (`init` as the
    /// old top-level `Use`, `agents`, `projects`). `hooks` is now
    /// a kept subcommand (agent pre-edit hook entry point — see
    /// `cli::hooks`), so the pre-consolidation guard against the
    /// bare `hook` token is dropped. This is still a regression
    /// guard against re-introducing the old multi-command surface.
    #[test]
    fn top_level_help_lists_only_kept_subcommands() {
        let mut cmd = crate::main_command_factory();
        // `render_help` returns a `StyledStr`; convert to a String so
        // `.contains(...)` works (StyledStr has no `.contains`).
        let help = clap::builder::Command::render_help(&mut cmd).to_string();
        // Kept subcommands must appear.
        assert!(help.contains("server"), "help must list `server`: {help}");
        assert!(help.contains("workspaces"), "help must list `workspaces`: {help}");
        assert!(help.contains("repos"), "help must list `repos`: {help}");
        assert!(help.contains("query"), "help must list `query`: {help}");
        assert!(help.contains("ask"), "help must list `ask`: {help}");
        assert!(help.contains("hooks"), "help must list `hooks`: {help}");
        // Removed subcommands must not appear.
        // `init` was reintroduced in B (the ergonomic shortcut commit)
        // as a kept subcommand. Make sure the help string reflects that.
        assert!(help.contains("init"), "help must list `init`: {help}");
        assert!(!help.contains("agents"), "help must not list `agents`: {help}");
        assert!(!help.contains("projects"), "help must not list `projects`: {help}");
    }
}
