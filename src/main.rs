//! `lain` — local MCP server for cross-repo and per-repo code analysis.
//!
//! This binary is a thin dispatcher over the clap-derived [`Args`] /
//! [`Commands`] enum in [`lain::cli`]. The kept subcommands are:
//! `server`, `workspaces`, `repos`, `query`, `ask`, `hooks`, `doctor`.
//! `Init`, `Agents`, `Projects`, and the old top-level `Use` are gone
//! after the consolidation. `hooks` is the agent pre-edit hook entry
//! point (claim/release against the server's presence registry).
//! `doctor` is the "one version of truth" diagnostic for the
//! installation — binary version + git sha + on-disk state.
//!
//! `main` is sync. Only the `server` subcommand needs a tokio runtime,
//! and we build a fresh one for it on demand rather than wrapping the
//! whole binary in `#[tokio::main]`. Running every subcommand inside a
//! tokio runtime was masking a reqwest-blocking panic: `reqwest::blocking`
//! builds its own internal runtime, and dropping a nested runtime from
//! inside the outer `#[tokio::main]` context aborts the process. Hooks
//! and `doctor` are pure sync code; they don't need (and shouldn't have)
//! a tokio runtime in scope.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use lain::cli::{Args, Commands};

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Commands::Server {
            config,
            transport,
            port,
            log_level,
            workspace,
            no_process_attribution,
            embedding_model,
        }) => {
            // The server is the only subcommand that needs a tokio
            // runtime. Build one on demand rather than wrapping the
            // whole binary.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("build tokio runtime for server subcommand")?;
            rt.block_on(lain::cli::server::run_server(
                &config,
                &transport,
                port,
                &log_level,
                &workspace,
                no_process_attribution,
                embedding_model.as_deref(),
            ))
        }
        Some(Commands::Workspaces { config, action }) => {
            // workspaces is sync; wrap the single async call.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build tokio runtime for workspaces subcommand")?;
            rt.block_on(lain::cli::workspaces::run(action, &config))
        }
        Some(Commands::Repos { config, action }) => lain::cli::repos::run(action, &config),
        Some(Commands::Query { workspace, expression }) => {
            // `query` reads `<workspace>/.lain/graph.bin`; without
            // `--workspace` it walks up for `.git` exactly like
            // `lain mcp` (see cli::query::run_query).
            lain::cli::query::run_query(&expression, workspace.as_deref())
        }
        Some(Commands::Ask { config: _, question: _ }) => {
            // NOTE: `cli::ask::run_ask` is the PreToolUse hook handler
            // — it reads JSON from stdin and outputs a permission
            // decision. The `--config` / `--question` flags on the
            // new `Ask` variant are forward-looking; PR 2 will wire
            // them through (likely by serializing into stdin or by
            // adding an interactive prompt). For now the args are
            // accepted for surface parity and ignored at dispatch.
            lain::cli::ask::run_ask()
        }
        Some(Commands::Mcp {
            workspace,
            embedding_model,
            reindex_timeout,
        }) => {
            // `lain mcp` — single-repo MCP server on stdio. Walks up
            // for `.git` if `--workspace` is not given, then serves
            // the per-repo tool surface (no federation). Wishlist #11
            // (option A): stable MCP config
            // `{"command":"lain","args":["mcp"]}` that doesn't
            // depend on `repos.yaml`.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build tokio runtime for mcp subcommand")?;
            rt.block_on(lain::cli::mcp::run_mcp(
                workspace.as_deref(),
                embedding_model.as_deref(),
                reindex_timeout.map(std::time::Duration::from_secs),
            ))
        }
        Some(Commands::Hooks { action }) => lain::cli::dispatch::run(action),
        Some(Commands::Doctor) => {
            // `doctor` returns its own exit code (0 clean, 1 hard
            // failure). Anything else (e.g. a network error from
            // reqwest) collapses to 2 so we don't silently lie about
            // a clean install.
            std::process::exit(lain::cli::doctor::run_doctor().unwrap_or(2));
        }
        None => {
            // No subcommand: print help.
            let mut cmd = Args::command();
            cmd.print_help().ok();
            println!();
            Ok(())
        }
    }
}
