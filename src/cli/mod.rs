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
    /// Print the MCP protocol version this binary negotiates and exit.
    /// Single source of truth for the version is
    /// `rust_mcp_schema::ProtocolVersion::latest()` — driven by the
    /// `2025_11_25` feature on the `rust-mcp-schema` crate. Tests
    /// (`tests/e2e/`) source this rather than hand-rolling the string.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub print_mcp_protocol_version: bool,

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
    /// Start an MCP server on stdio.
    ///
    /// Single repo: walks up from the agent harness's cwd for `.git`
    /// and serves the per-repo tool surface directly. Wishlist #11:
    /// this is the stable MCP config — `{"command":"lain","args":["mcp"]}`
    /// — that doesn't depend on a `repos.yaml` or the federation
    /// plumbing.
    ///
    /// Multiple repos: pass `--workspace PATH` more than once (or set
    /// `LAIN_WORKSPACE=/a,/b`). `lain mcp` synthesizes an in-memory
    /// `repos.yaml` with one `workspace_dir` source per entry and
    /// delegates to `lain server --transport stdio`, which provides
    /// the federation tool surface (`list_repos`, `search_org`,
    /// `get_federation_health`) alongside the per-repo tools.
    /// Agents that want to work across multiple repos get the same
    /// federation surface as `lain server` without having to author
    /// a `repos.yaml` themselves.
    Mcp {
        /// Workspace root(s) (directories containing `.git/`).
        /// Repeatable: `lain mcp --workspace /repo/a --workspace /repo/b`.
        /// When omitted, the binary reads `LAIN_WORKSPACE` (a
        /// comma-separated list); if that's also unset, it walks up
        /// from the agent harness's cwd (via `/proc/$PPID/cwd`),
        /// falling back to the process's own cwd. That policy is
        /// what makes `lain mcp` Just Work under any agent harness,
        /// including Kimi's plugin-security cwd pinning.
        #[arg(long, value_name = "PATH")]
        workspace: Vec<PathBuf>,
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
        /// Run as a *sidecar* against an owner server instead of
        /// indexing locally: open the persisted graph read-only, then
        /// follow the owner's `/overlay/subscribe` stream so live edits
        /// it sees are answerable here too.
        ///
        /// The owner half of this shipped working — the server serves
        /// `/overlay/subscribe` and `/overlay/get_snapshot`, the client
        /// (`overlay::subscribe`) and the read-only executor both exist
        /// and are tested — but nothing could ever *start* a sidecar,
        /// because no command exposed it. This flag is that entry point.
        #[arg(long, value_name = "URL")]
        owner_url: Option<String>,
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
        // Signature lock: the cast above is well-typed only if both
        // items resolve to `fn(&Path) -> PathBuf`. Future drift in
        // `lib.rs::resolve_repos_config`'s signature breaks the build.
        let _: fn(&Path) -> std::path::PathBuf = resolve_repos_config;
    }
}
