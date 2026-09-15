//! `lain` — local MCP server for cross-repo and per-repo code analysis.
//!
//! This binary is a thin dispatcher over the clap-derived [`Args`] /
//! [`Commands`] enum in [`lain::cli`]. The kept subcommands are:
//! `server`, `workspaces`, `repos`, `query`, `ask`, `hooks`, `doctor`.
//! `Init`, `Agents`, `Projects`, and the old top-level `Use` are gone
//! after the consolidation. `hooks` is the agent pre-edit hook entry
//! point (claim/release against the server's presence registry).
//! `doctor` is the read-only repository and MCP readiness diagnostic.
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
    // Stamp the executable's mtime before anything can rebuild
    // underneath a long-running server, so `get_health` /
    // `get_server_status` can tell an agent when the binary answering
    // its calls has been superseded on disk.
    lain::server::build_info::record_startup_exe_mtime();
    let args = Args::parse();
    if args.print_mcp_protocol_version {
        // Single source of truth: rust-mcp-schema's ProtocolVersion
        // enum. The 2025_11_25 feature in Cargo.toml selects this
        // version; bumping the feature flag bumps this output.
        println!("{}", rust_mcp_schema::ProtocolVersion::latest());
        return Ok(());
    }
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
                &lain::cli::resolve_repos_config(&config),
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
            rt.block_on(lain::cli::workspaces::run(
                action,
                &lain::cli::resolve_repos_config(&config),
            ))
        }
        Some(Commands::Repos { config, action }) => {
            lain::cli::repos::run(action, &lain::cli::resolve_repos_config(&config))
        }
        Some(Commands::Query {
            workspace,
            expression,
        }) => {
            // `query` reads `<workspace>/.lain/graph.bin`; without
            // `--workspace` it walks up for `.git` exactly like
            // `lain mcp` (see cli::query::run_query).
            lain::cli::query::run_query(&expression, workspace.as_deref())
        }
        Some(Commands::Ask {
            config: _,
            question: _,
        }) => {
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
            owner_url,
        }) => {
            // `lain mcp` — MCP server on stdio. Resolves the workspace
            // list from `--workspace` (repeatable), the `LAIN_WORKSPACE`
            // env var (comma-separated), or the agent-harness cwd
            // walk-up. See `cli::mcp::resolve_workspaces` for the
            // full resolution policy.
            // Milestone 4 (AGENT_UX_ROADMAP.md): the startup re-index now
            // runs as a background task alongside the MCP protocol loop
            // (see `LainMcpServer::run_stdio`/`run_http`) rather than
            // being awaited before it starts. A single-thread runtime
            // would still let a long synchronous git/parser call in that
            // background task starve `initialize`/`ping`/`tools/list` —
            // both tasks would be cooperatively scheduled on the same one
            // OS thread. `max(2, available_parallelism)` guarantees the
            // protocol loop always has its own thread to run on, even on
            // a single-core sandbox where `available_parallelism()` could
            // report 1.
            let worker_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .max(2);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .build()
                .context("build tokio runtime for mcp subcommand")?;
            match owner_url {
                Some(url) => rt.block_on(lain::cli::mcp::run_sidecar(&workspace, &url)),
                None => rt.block_on(lain::cli::mcp::run_mcp(
                    &workspace,
                    embedding_model.as_deref(),
                    reindex_timeout.map(std::time::Duration::from_secs),
                )),
            }
        }
        Some(Commands::Init {
            workspace,
            force,
            print,
        }) => lain::cli::init::run_init(workspace.as_deref(), force, print),
        Some(Commands::Hooks { action }) => lain::cli::dispatch::run(action),
        Some(Commands::Schema { action }) => lain::cli::schema::run(action),
        Some(Commands::Oneshot {
            workspace,
            tool,
            args,
        }) => lain::cli::oneshot::run_oneshot(workspace.as_deref(), &tool, &args),
        Some(Commands::Doctor {
            json,
            workspace,
            probe_mcp,
        }) => {
            if probe_mcp {
                return tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(lain::cli::doctor::run_probe(
                        workspace.as_deref().context("probe workspace")?,
                    ));
            }
            let code = match lain::cli::doctor::run_doctor(json, workspace.as_deref()) {
                Ok(code) => code,
                Err(error) => {
                    eprintln!("doctor failed: {error:#}");
                    2
                }
            };
            std::process::exit(code);
        }
        Some(Commands::Capabilities { json, workspace }) => {
            let code = lain::cli::readiness::capabilities(json, workspace.as_deref())
                .unwrap_or_else(|error| {
                    eprintln!("capabilities failed: {error:#}");
                    2
                });
            std::process::exit(code);
        }
        Some(Commands::Status { json, workspace }) => {
            let code =
                lain::cli::readiness::status(json, workspace.as_deref()).unwrap_or_else(|error| {
                    eprintln!("status failed: {error:#}");
                    2
                });
            std::process::exit(code);
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
