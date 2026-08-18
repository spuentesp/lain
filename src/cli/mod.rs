pub mod query;
pub mod ask;
pub mod server;
pub mod workspaces;
pub mod repos;
pub mod dispatch;
pub mod signal;
pub mod hooks;
pub mod doctor;
pub mod mcp;

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
        /// Path to the ONNX bi-encoder model directory (contains
        /// `model.onnx` and `tokenizer.json`). When set, the
        /// `semantic_search` tool becomes live; when unset, the
        /// embedder runs in stub mode and the tool returns an empty
        /// result with a clear `NLP Model: Not loaded` warning. The
        /// on-disk model is small (~90 MB) and is the only thing
        /// blocking semantic_search in the federation-mode launch.
        #[arg(long)]
        embedding_model: Option<PathBuf>,
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
    /// Start a single-repo MCP server on stdio. Walks up from the
    /// current directory for `.git` and serves the per-repo tool
    /// surface directly. Wishlist #11: this is the stable MCP
    /// config — `{"command":"lain","args":["mcp"]}` — that
    /// doesn't depend on a `repos.yaml` or the federation plumbing.
    /// Optional `--workspace PATH` overrides the walk-up; the
    /// `embedding_model` flag works the same as in `Server`.
    Mcp {
        /// Workspace root (the directory containing `.git/`).
        /// When omitted, walks up from the current directory until
        /// it finds a `.git` marker and uses that parent. This
        /// matches the zero-config use case (run from anywhere
        /// inside a clone and it Just Works).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Path to the ONNX bi-encoder model directory. See
        /// `lain server --help` for details.
        #[arg(long)]
        embedding_model: Option<PathBuf>,
        /// Override the startup re-index timeout (seconds). Step 1
        /// of the staleness fix. `None` means "use `LAIN_REINDEX_TIMEOUT`
        /// env, falling back to 300s (5min) — the user picks the
        /// actual default by measuring a real full index, not by
        /// guessing."
        #[arg(long)]
        reindex_timeout: Option<u64>,
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
