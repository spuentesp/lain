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

use anyhow::Result;
use clap::{CommandFactory, Parser};
use lain::cli::{Args, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Commands::Server {
            config,
            transport,
            port,
            log_level,
            workspace,
            no_process_attribution,
        }) => lain::cli::server::run_server(
            &config,
            &transport,
            port,
            &log_level,
            &workspace,
            no_process_attribution,
        )
        .await,
        Some(Commands::Workspaces { config, action }) => {
            lain::cli::workspaces::run(action, &config).await
        }
        Some(Commands::Repos { config, action }) => lain::cli::repos::run(action, &config),
        Some(Commands::Query { config, expression }) => {
            // NOTE: Task 1.9 keeps the pre-consolidation semantics
            // for `query` — `cli::query::run_query` reads the second
            // arg as the workspace directory (it joins
            // `.lain/graph.bin` onto it). The `--config` flag here
            // defaults to `./repos.yaml` for shape parity with the
            // other subcommands; PR 2 will rewire it to a real
            // workspace path. For now, the value is forwarded as-is.
            lain::cli::query::run_query(&expression, &config)
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
