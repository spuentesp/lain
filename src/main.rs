//! Lain

use anyhow::Result;
use clap::Parser;
use lain::{LainMcpServer, LainServer};
use lain::watcher::FileWatcher;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let log_level = if args.verbose { "debug" } else { &args.log_level };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    tracing::info!("Initializing Lain");
    tracing::info!("Workspace: {:?}", args.workspace);

    if !args.workspace.exists() {
        anyhow::bail!("Workspace does not exist: {:?}", args.workspace);
    }

    let memory_path = args
        .memory_path
        .unwrap_or_else(|| args.workspace.join(".lain/graph.bin"));

    tracing::info!("Memory path: {:?}", memory_path);

    // Single-instance guard: check for existing lock
    let lock_path = args.workspace.join(".lain/server.lock");
    if let Ok(contents) = std::fs::read_to_string(&lock_path) {
        if let Some((pid_str, port_str)) = contents.split_once(':') {
            let pid: u32 = pid_str.parse().unwrap_or(0);
            let port: u16 = port_str.parse().unwrap_or(9999);
            // Check if process is still running
            if pid != 0 && pid != std::process::id() {
                #[cfg(unix)]
                if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                    eprintln!("ERROR: Another Lain instance is already running (pid {}), listening on port {}.", pid, port);
                    eprintln!("Stop it first or remove .lain/server.lock to override.");
                    std::process::exit(1);
                }
                #[cfg(windows)]
                {
                    use std::process::Command;
                    let output = Command::new("tasklist").arg("/FI").arg(&format!("PID eq {}", pid)).output();
                    if output.map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false) {
                        eprintln!("ERROR: Another Lain instance is already running (pid {}), listening on port {}.", pid, port);
                        eprintln!("Stop it first or remove .lain/server.lock to override.");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
    // Write our lock file
    std::fs::create_dir_all(args.workspace.join(".lain"))?;
    std::fs::write(&lock_path, format!("{}:{}", std::process::id(), args.port))?;
    tracing::info!("Single-instance lock acquired at {:?}", lock_path);

    // Register shutdown to remove lock file
    let cleanup_lock_path = lock_path.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        let _ = std::fs::remove_file(&cleanup_lock_path);
    });

    let embedder_model = args.embedding_model.as_deref();
    if let Some(path) = embedder_model {
        tracing::info!("Embedding model: {:?}", path);
    }

    let mut server = LainServer::new(&args.workspace, &memory_path, embedder_model)?;

    if !server.is_git_repo() {
        anyhow::bail!("Fatal: No Git repository found at workspace. Lain requires a .git folder.");
    }

    tracing::info!("Git repository validated");

    tracing::info!("Syncing volatile overlay...");
    server.sync_volatile_overlay().await?;
    tracing::info!("Volatile overlay synced");

    // CQRS Pattern: Run command (build_core_memory) on main task FIRST
    // This ensures bulk indexing completes before we start serving queries
    tracing::info!("Starting background indexing (Command)...");
    let mut server_for_indexing = server.clone_for_background();
    if let Err(e) = server_for_indexing.build_core_memory().await {
        tracing::error!("Background indexing failed: {}", e);
    } else {
        tracing::info!("Background indexing completed successfully");
    }

    // Start file watcher for real-time overlay updates
    tracing::info!("Starting file watcher for real-time updates...");
    let watcher = FileWatcher::new();
    watcher.start(args.workspace.clone(), server.clone());

    // Start background jobs (Sync and Sliding Window)
    let s_sync = server.clone();
    tokio::spawn(async move {
        s_sync.run_background_sync(300).await;
    });

    let s_window = server.clone();
    tokio::spawn(async move {
        s_window.run_sliding_window(30).await;
    });

    // Now spawn query server as background - LAIN is now responsive
    tracing::info!("Starting MCP server (Query)...");

    let mcp_server = LainMcpServer::new(server.tool_executor.clone());

    match args.transport.as_str() {
        "both" => {
            let mcp_http = mcp_server.clone();
            let mcp_std = mcp_server.clone();
            tokio::spawn(async move {
                tracing::info!("Starting MCP HTTP server on port {}", args.port);
                if let Err(e) = mcp_http.run_http(args.port).await {
                    tracing::error!("MCP HTTP server exited: {}", e);
                }
            });
            tokio::spawn(async move {
                tracing::info!("Starting MCP stdio server");
                if let Err(e) = mcp_std.run_stdio().await {
                    tracing::error!("MCP stdio server exited: {}", e);
                }
            });
        }
        "http" => {
            tokio::spawn(async move {
                if let Err(e) = mcp_server.run_http(args.port).await {
                    tracing::error!("MCP HTTP server exited with error: {}", e);
                }
            });
        }
        _ => {
            tokio::spawn(async move {
                if let Err(e) = mcp_server.run_stdio().await {
                    tracing::error!("MCP stdio server exited with error: {}", e);
                }
            });
        }
    };

    // Keep main task alive forever
    std::future::pending::<()>().await;
    unreachable!()
}
