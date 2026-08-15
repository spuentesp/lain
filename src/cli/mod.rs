pub mod init;
pub mod query;
pub mod ask;
pub mod hook;
pub mod agents;
pub mod server;
pub mod workspaces;
pub mod repos;
pub mod dispatch;

pub use init::run_init;
pub use query::run_query;
pub use ask::run_ask;
pub use hook::run_hook_install;
pub use server::run_server;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Top-level `lain` CLI surface — kept subcommands only.
///
/// After the consolidation `Init`, `Agents`, `Hook`, `Projects`, and
/// `Use` (as a top-level verb) are gone. Federation / repo /
/// per-repo concerns are reached via `Server` + `Repos`. Workspaces
/// keep their own subcommand tree (managed in `cli::workspaces`).
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
}
