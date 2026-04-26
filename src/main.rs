//! Lain - Language-Augmented Ingestion Network

use anyhow::Result;
use clap::Parser;
use lain::LainServer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, required = true)]
    workspace: std::path::PathBuf,

    #[arg(long)]
    memory_path: Option<std::path::PathBuf>,

    #[arg(long, default_value = "info")]
    log_level: String,

    #[arg(long, short)]
    verbose: bool,
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
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Initializing Lain - Language-Augmented Ingestion Network");
    tracing::info!("Workspace: {:?}", args.workspace);

    if !args.workspace.exists() {
        anyhow::bail!("Workspace does not exist: {:?}", args.workspace);
    }

    let memory_path = args
        .memory_path
        .unwrap_or_else(|| args.workspace.join(".lain/kuzu"));

    tracing::info!("Memory path: {:?}", memory_path);

    let mut server = LainServer::new(&args.workspace, &memory_path)?;

    if !server.is_git_repo() {
        anyhow::bail!("Fatal: No Git repository found at workspace. Lain requires a .git folder.");
    }

    tracing::info!("Git repository validated");

    server.build_core_memory().await?;
    tracing::info!("Core memory built successfully");

    server.run_mcp_server().await?;

    Ok(())
}
