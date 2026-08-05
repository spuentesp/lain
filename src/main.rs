//! Lain

use anyhow::Result;
use clap::{Parser, Subcommand};
use lain::{LainMcpServer, LainServer};
use lain::watcher::FileWatcher;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cmds;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(long, default_value = ".")]
    workspace: std::path::PathBuf,
    #[arg(long)]
    memory_path: Option<std::path::PathBuf>,
    #[arg(long)]
    embedding_model: Option<std::path::PathBuf>,
    #[arg(long, default_value = "info")]
    log_level: String,
    #[arg(long, short)]
    verbose: bool,
    #[arg(long, default_value = "stdio")]
    transport: String,
    #[arg(long, default_value = "9999")]
    port: u16,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Init {
        #[arg(long, default_value = "auto")] agent: String,
        #[arg(long)] workspace: Option<std::path::PathBuf>,
        #[arg(long)] embedding_model: Option<std::path::PathBuf>,
        #[arg(long, default_value = "stdio")] transport: String,
        #[arg(long, default_value = "9999")] port: u16,
        #[arg(long, short)] yes: bool,
    },
    Query {
        #[arg(required = true)] expression: String,
        #[arg(long, default_value = ".")] workspace: std::path::PathBuf,
    },
    Hook {
        #[arg(long)] agent: Option<String>,
        #[arg(long)] uninstall: bool,
    },
    Ask,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(cmd) = args.command {
        match cmd {
            Commands::Init { agent, workspace, embedding_model, transport, port, yes } => {
                // Propagate top-level --workspace when subcommand didn't override it.
                let workspace = workspace.unwrap_or(args.workspace);
                return cmds::run_init(&agent, Some(&workspace), embedding_model.as_deref(), &transport, port, yes);
            }
            Commands::Query { expression, workspace } => {
                // Top-level --workspace is the documented invocation form
                // (e.g. `lain --workspace /path query "..."`). Honor it when
                // the subcommand's --workspace is the clap default (".").
                let workspace = if workspace == std::path::Path::new(".") && args.workspace != std::path::Path::new(".") {
                    args.workspace
                } else {
                    workspace
                };
                return cmds::run_query(&expression, &workspace);
            }
            Commands::Hook { agent, uninstall } => return cmds::run_hook_install(agent, uninstall),
            Commands::Ask => return cmds::run_ask(),
        }
    }

    let log_level = if args.verbose { "debug" } else { &args.log_level };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| log_level.into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    tracing::info!("Initializing Lain");
    if !args.workspace.exists() { anyhow::bail!("Workspace does not exist: {:?}", args.workspace); }
    let memory_path = args.memory_path.unwrap_or_else(|| args.workspace.join(".lain/graph.bin"));

    let lock_path = args.workspace.join(".lain/server.lock");
    if let Ok(contents) = std::fs::read_to_string(&lock_path) {
        if let Some((pid_str, _port_str)) = contents.split_once(':') {
            let pid: u32 = pid_str.parse().unwrap_or(0);
            if pid != 0 && pid != std::process::id() {
                #[cfg(unix)]
                if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                    eprintln!("ERROR: Another Lain instance is running (pid {}). Stop it or remove .lain/server.lock.", pid);
                    std::process::exit(1);
                }
            }
        }
    }
    std::fs::create_dir_all(args.workspace.join(".lain"))?;
    std::fs::write(&lock_path, format!("{}:{}", std::process::id(), args.port))?;

    let cleanup_lock_path = lock_path.clone();
    tokio::spawn(async move { tokio::signal::ctrl_c().await.ok(); let _ = std::fs::remove_file(&cleanup_lock_path); });

    let embedder_model = args.embedding_model.as_deref();
    // Check git repo FIRST so the user sees a clean error, not a libgit2 panic.
    if !args.workspace.join(".git").exists() {
        anyhow::bail!(
            "No Git repository found at {:?}. Lain requires a .git folder.\n\
             Run `git init` (or open an existing repo) and try again.",
            args.workspace
        );
    }
    let mut server = LainServer::new(&args.workspace, &memory_path, embedder_model)?;

    server.sync_volatile_overlay().await?;
    let mut server_for_indexing = server.clone_for_background();
    if let Err(e) = server_for_indexing.build_core_memory().await { tracing::error!("Indexing failed: {}", e); }

    let watcher = FileWatcher::new();
    watcher.start(args.workspace.clone(), server.clone());

    let s_sync = server.clone();
    tokio::spawn(async move { s_sync.run_background_sync(300).await; });
    let s_window = server.clone();
    tokio::spawn(async move { s_window.run_sliding_window(30).await; });

    let mcp_server = LainMcpServer::new(server.tool_executor.clone());
    match args.transport.as_str() {
        "both" => {
            let h = mcp_server.clone(); let s = mcp_server;
            tokio::spawn(async move { if let Err(e) = h.run_http(args.port).await { tracing::error!("HTTP: {}", e); } });
            tokio::spawn(async move { if let Err(e) = s.run_stdio().await { tracing::error!("Stdio: {}", e); } });
        }
        "http" => { tokio::spawn(async move { if let Err(e) = mcp_server.run_http(args.port).await { tracing::error!("HTTP: {}", e); } }); }
        _ => { tokio::spawn(async move { if let Err(e) = mcp_server.run_stdio().await { tracing::error!("Stdio: {}", e); } }); }
    };

    std::future::pending::<()>().await;
    unreachable!()
}
