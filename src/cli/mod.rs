pub mod query;
pub mod ask;
pub mod server;
pub mod workspaces;
pub mod repos;
pub mod dispatch;
pub mod signal;
pub mod hooks;
pub mod doctor;

pub use query::run_query;
pub use ask::run_ask;
pub use server::run_server;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Top-level `lain` CLI surface — kept subcommands only.
///
/// After the consolidation `Init`, `Agents`, `Projects`, and `Use`
/// (as a top-level verb) are gone. Federation / repo / per-repo
/// concerns are reached via `Server` + `Repos`. Workspaces keep
/// their own subcommand tree (managed in `cli::workspaces`).
/// `Hooks` is the agent pre-edit hook entry point (claim/release
/// against the server's presence registry).
#[derive(Parser, Debug)]
#[command(
    name = "lain",
    author,
    version,
    about = "Local MCP server for code analysis",
    long_about = None
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Kept subcommands. Each variant carries its own `--config`
/// (default: `./repos.yaml`) so the federation file follows the
/// subcommand instead of being a binary-level flag.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start the MCP server (the headline).
    Server {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[arg(long, default_value = "stdio", value_parser = ["stdio", "http"])]
        transport: String,
        #[arg(long, default_value = "9999")]
        port: u16,
        #[arg(long, default_value = "info")]
        log_level: String,
        /// Active workspace. One of: "auto", "", or a workspace name.
        #[arg(long, default_value = "auto")]
        workspace: String,
        /// Disable process-based attribution. lain falls back to the
        /// single-agent heuristic + git polling. Useful on systems
        /// where `/proc/<pid>/fd` or `lsof` is unreliable (some macOS
        /// configurations, containerized environments without `/proc`
        /// access) or for operators who want attribution off entirely.
        #[arg(long)]
        no_process_attribution: bool,
    },
    /// Manage `workspaces.yaml` for the project.
    Workspaces {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[command(subcommand)]
        action: crate::cli::workspaces::WorkspacesAction,
    },
    /// Manage `repos.yaml` for the project.
    Repos {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[command(subcommand)]
        action: crate::cli::repos::ReposAction,
    },
    /// Run a query against the project's persisted graph.
    Query {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        expression: String,
    },
    /// Single-user LLM-assisted query.
    Ask {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        question: String,
    },
    /// Agent pre-edit hook entry point (claim/release files).
    Hooks {
        #[command(subcommand)]
        action: crate::cli::hooks::HooksAction,
    },
    /// Run installation / version diagnostics — the
    /// "one-version-of-truth" page operators can paste into bug
    /// reports. Always exits 0 on a clean install, 1 on hard
    /// failures (missing hook script, un-creatable dirs).
    Doctor,
}
