//! Lain

use anyhow::Result;
use clap::{Parser, Subcommand};
use lain::lock::WorkspaceLock;
use lain::{LainMcpServer, LainServer};
use lain::mode::LainMode;
use lain::state::Projects;
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
    #[arg(long, default_value = "owner", value_parser = ["owner", "sidecar"])]
    mode: String,
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
    /// Manage the project registry (`~/.config/lain/projects.toml`).
    /// Use `lain projects add <name> <path>` then `lain use <name>` to
    /// switch between projects without typing --workspace every time.
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
    /// Set the active project (shorthand for `lain projects use <name>`).
    Use {
        name: String,
    },
    /// Start a federation-mode MCP server backed by a `repos.yaml` config.
    /// Federation tools (list_repos, get_federation_health, search_org,
    /// get_cross_repo_blast_radius*, get_repo_info) are exposed over the
    /// chosen transport. Existing per-repo tools are not registered in
    /// federation mode.
    Server {
        #[arg(long)] config: std::path::PathBuf,
        #[arg(long, default_value = "http")] transport: String,
        #[arg(long, default_value = "9999")] port: u16,
        #[arg(long, default_value = "info")] log_level: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectsAction {
    /// List registered projects.
    List,
    /// Register a project by name and path.
    Add {
        name: String,
        path: std::path::PathBuf,
    },
    /// Remove a project from the registry.
    Forget { name: String },
    /// Show the currently active project name.
    Current,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(cmd) = args.command {
        match cmd {
            Commands::Init { agent, workspace, embedding_model, transport, port, yes } => {
                // Propagate top-level --workspace when subcommand didn't override it.
                let workspace = workspace.unwrap_or(args.workspace);
                // Resolve workspace through the registry: --workspace wins,
                // else the active project, else cwd's .lain, else error.
                let resolved = resolve_workspace_path(&workspace);
                return cmds::run_init(&agent, Some(&resolved), embedding_model.as_deref(), &transport, port, yes);
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
                let resolved = resolve_workspace_path(&workspace);
                return cmds::run_query(&expression, &resolved);
            }
            Commands::Hook { agent, uninstall } => return cmds::run_hook_install(agent, uninstall),
            Commands::Ask => return cmds::run_ask(),
            Commands::Projects { action } => match action {
                ProjectsAction::List => return cmds::projects::run_list(),
                ProjectsAction::Add { name, path } => return cmds::projects::run_add(&name, &path),
                ProjectsAction::Forget { name } => return cmds::projects::run_forget(&name),
                ProjectsAction::Current => {
                    // Print active project. If none is set, return a
                    // custom error that main() can map to exit code 1.
                    if let Some(name) = lain::state::Projects::active_name() {
                        println!("{}", name);
                        return Ok(());
                    } else {
                        return Err(anyhow::anyhow!("no active project; use `lain use <name>`"));
                    }
                }
            },
            Commands::Use { name } => return cmds::projects::run_use(&name),
            Commands::Server { config, transport, port, log_level } => {
                return cmds::run_server(&config, &transport, port, &log_level).await;
            }
        }
    }

    let log_level = if args.verbose { "debug" } else { &args.log_level };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| log_level.into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    tracing::info!("Initializing Lain");
    if !args.workspace.exists() { anyhow::bail!("Workspace does not exist: {:?}", args.workspace); }
    let memory_path = args.memory_path.clone().unwrap_or_else(|| args.workspace.join(".lain/graph.bin"));
    std::fs::create_dir_all(args.workspace.join(".lain"))?;
    let lock_path = args.workspace.join(".lain/server.lock");
    let workspace_lock = WorkspaceLock::new(lock_path.clone());

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

    let mode: LainMode = args.mode.parse().expect("validated by clap");
    match mode {
        LainMode::Owner => {
            // Acquire the workspace's exclusive flock. A second owner
            // pointing at the same workspace must fail fast with a clear
            // message rather than clobbering the on-disk graph.
            let _owner_guard = workspace_lock.acquire_exclusive().map_err(|e| {
                let existing = workspace_lock.read_owner_pid()
                    .map(|pid| format!(" (existing owner pid {pid})"))
                    .unwrap_or_default();
                anyhow::anyhow!(
                    "Another Lain owner already holds the workspace lock at {:?}{}: {}",
                    workspace_lock.path(),
                    existing,
                    e
                )
            })?;
            // Record our pid:port for the next sidecar to read.
            workspace_lock.write_owner_pid(std::process::id(), args.port)?;

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
        LainMode::Sidecar => {
            // Verify the owner is alive by briefly attempting a shared
            // flock on the workspace lock file. flock semantics: a shared
            // acquire against an exclusively-held file fails with
            // WouldBlock — that contention is the "owner is alive" signal.
            // If the shared acquire succeeds, no exclusive holder exists.
            // If the lock file doesn't exist yet (no owner has ever run
            // here), bail with a clear message.
            if !workspace_lock.path().exists() {
                anyhow::bail!(
                    "Cannot start sidecar: no owner has ever written a workspace lock at {:?}. \
                     Start an owner first with `--mode owner`.",
                    workspace_lock.path()
                );
            }
            match workspace_lock.acquire_shared() {
                Ok(_shared) => {
                    // No exclusive holder — no owner running.
                    tracing::warn!(
                        "Sidecar started with no live owner holding the workspace lock at {:?}",
                        workspace_lock.path()
                    );
                }
                Err(e) => {
                    // Exclusive holder exists → owner is alive.
                    tracing::debug!(
                        "Sidecar observed held workspace lock ({}); owner is alive",
                        e
                    );
                }
            }
            if let Some(owner_pid) = workspace_lock.read_owner_pid() {
                if owner_pid != std::process::id() {
                    tracing::info!("Sidecar verified owner pid {} at {:?}", owner_pid, workspace_lock.path());
                }
            }
            tracing::info!("Starting Lain in sidecar mode");
            let cfg = lain::sidecar::SidecarConfig {
                workspace: args.workspace.clone(),
                memory_path: args.memory_path.clone().unwrap_or_else(|| args.workspace.join(".lain/graph.bin")),
                port: args.port,
                owner_url: std::env::var("LAIN_OWNER_URL").unwrap_or_else(|_| {
                    let port = std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into());
                    format!("http://localhost:{}/mcp", port)
                }),
                embedding_model: args.embedding_model.clone().map(std::path::PathBuf::from),
            };
            return lain::sidecar::run(cfg).await.map_err(anyhow::Error::from);
        }
    }
}

/// Resolve the workspace path used by subcommands that don't accept
/// their own --workspace flag (e.g. `lain init`, `lain query`).
///
/// Priority:
/// 1. The flag value, if it's not the clap default "."
/// 2. The active project from `lain use <name>` (if any)
/// 3. `.lain/` in the current working directory
/// 4. Error: "no active project; use `lain projects add` then `lain use`"
fn resolve_workspace_path(explicit: &std::path::Path) -> std::path::PathBuf {
    // 1. explicit non-default wins
    if explicit != std::path::Path::new(".") {
        return explicit.to_path_buf();
    }
    // 2. active project from registry
    if let Some(name) = Projects::active_name() {
        for p in Projects::list() {
            if p.name == name {
                return p.path;
            }
        }
    }
    // 3. .lain in cwd
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".lain").exists() {
            return cwd;
        }
    }
    // 4. error — clap will surface this as a non-zero exit
    eprintln!("no active project; use `lain projects add <name> <path>` then `lain use <name>`, or pass --workspace <path>");
    std::process::exit(1);
}
