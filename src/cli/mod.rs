pub mod ask;
pub mod dispatch;
pub mod doctor;
pub mod hooks;
pub mod init;
pub mod io;
pub mod mcp;
pub mod mcp_client;
pub mod oneshot;
pub mod query;
pub mod repos;
pub mod server;
pub mod signal;
pub mod workspace;
pub mod workspaces;

pub use query::run_query;
pub use ask::run_ask;
pub use server::run_server;
pub use crate::resolve_repos_config;

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
    /// Run a query against the project's persisted graph
    /// (`<workspace>/.lain/graph.bin`). Without `--workspace`, walks
    /// up from the current directory for `.git` like `lain mcp`.
    Query {
        #[arg(long)]
        workspace: Option<PathBuf>,
        expression: String,
    },
    /// One-shot MCP query: boots a transient `lain mcp` server
    /// (stdin/stdout), sends a single `tools/call` for the named
    /// tool, prints the result as a pretty table, and exits. The
    /// ergonomic shortcut for "I just want to grep the symbols
    /// without keeping a server alive".
    ///
    /// Built-in shortcuts:
    /// - `find_anchors` — top symbols by anchor score (deduped)
    /// - `get_blast_radius <symbol>` — incoming callers of `<symbol>`
    /// - `find_dead_code` — symbols with no incoming call edges
    /// - `get_call_chain <from> <to>` — call path between two symbols
    ///
    /// Any tool name registered in the stdio MCP server works.
    /// Arguments after the tool name are passed as the tool's
    /// `arguments` object (positional, in the order the tool
    /// declares them).
    Oneshot {
        /// Workspace root (default: walk up from cwd for `.git`).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Tool name, e.g. `find_anchors` or `get_blast_radius`.
        tool: String,
        /// Positional arguments forwarded as the tool's `arguments`.
        /// Numbers are parsed as integers; everything else as strings.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    /// Scaffold a `repos.yaml` for the current directory. Walks up for
    /// `.git` (same as `lain mcp`), then writes a minimal
    /// `repos.yaml` pointing the only repo at the discovered workspace
    /// and a per-repo `data_dir` at `./.lain/data`. The ergonomic
    /// onboarding: `cd` into any clone, run `lain init`, run
    /// `lain server` — no hand-written YAML.
    ///
    /// Fails if `./repos.yaml` already exists (refuse to clobber).
    /// Add `--force` to overwrite. With `--print` print the would-be
    /// file and exit (useful for piping into `tee` or for CI).
    Init {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        print: bool,
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

#[cfg(test)]
mod tests {
    use super::resolve_repos_config;
    use std::path::Path;

    #[test]
    fn reexport_resolves_to_canonical_implementation() {
        // Locks the property the duplicate violated. With both copies
        // present this passes (they're byte-identical); after Step 3
        // the CLI re-export and the crate-root canonical must remain
        // the same function pointer, and any future re-introduction
        // of the duplicate breaks the build.
        assert!(std::ptr::eq(
            resolve_repos_config as *const (),
            crate::resolve_repos_config as *const (),
        ));
        // The unused import silencer — both items must resolve to the
        // same signature for the cast above to be well-typed.
        let _: fn(&Path) -> std::path::PathBuf = resolve_repos_config;
    }
}
