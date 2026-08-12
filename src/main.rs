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
    #[arg(long, default_value = "auto", value_parser = ["auto", "owner", "sidecar"])]
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
        /// Where to write the agent's MCP config: `project` (in-repo) or
        /// `user` (global, e.g. `~/.config/...`). Currently honored
        /// by `--agent opencode` and `--agent copilot`; other agents ignore it.
        #[arg(long, default_value = "project", value_parser = ["project", "user"])]
        scope: String,
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
    /// Manage agent MCP configurations (Claude, Kimi, Cursor, etc.)
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
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
        /// Active workspace. One of:
        /// - "auto" (default): resolve via `~/.config/lain/active_workspace`
        /// - "": no workspace — load every repo in `repos.yaml` (today's behavior)
        /// - <name>: load the named workspace from `workspaces.yaml` next to `repos.yaml`
        #[arg(long, default_value = "auto")] workspace: String,
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

#[derive(Debug, Subcommand)]
enum AgentsAction {
    /// List supported agents.
    List,
    /// Install MCP config for one or all agents.
    Install {
        /// Agent id (e.g. claude, kimi). Omit with --all.
        id: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
    },
    /// Verify that installed agents can reach the Lain MCP server.
    Verify {
        /// Agent id. Omit with --all.
        id: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove an agent's MCP config.
    Remove {
        id: String,
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();

    let log_level = if args.verbose { "debug" } else { &args.log_level };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| log_level.into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    if args.workspace.as_os_str() == "auto" {
        args.workspace = lain::state::Projects::resolve_auto_workspace()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    tracing::info!(workspace = %args.workspace.display(), "Serving repo");
    if let Some(cmd) = args.command {
        match cmd {
            Commands::Init { agent, workspace, embedding_model, transport, port, yes, scope } => {
                // Propagate top-level --workspace when subcommand didn't override it.
                let workspace = workspace.unwrap_or(args.workspace);
                // Resolve workspace through the registry: --workspace wins,
                // else the active project, else cwd's .lain, else error.
                let resolved = resolve_workspace_path(&workspace);
                return cmds::run_init(&agent, Some(&resolved), embedding_model.as_deref(), &transport, port, yes, &scope);
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
            Commands::Agents { action } => match action {
                AgentsAction::List => return cmds::agents::list::run_list(),
                AgentsAction::Install { id, all, scope } => {
                    let scope = parse_install_scope(&scope)?;
                    if !all && id.is_none() {
                        anyhow::bail!("--all or <id> is required");
                    }
                    return cmds::agents::install::run_install(id.as_deref(), all, scope);
                }
                AgentsAction::Verify { id, all, json } => {
                    if !all && id.is_none() {
                        anyhow::bail!("--all or <id> is required");
                    }
                    return cmds::agents::verify::run_verify(all, id.as_deref(), json).await;
                }
                AgentsAction::Remove { id, scope } => {
                    let scope = parse_install_scope(&scope)?;
                    return cmds::agents::remove::run_remove(&id, scope);
                }
            },
            Commands::Use { name } => return cmds::projects::run_use(&name),
            Commands::Server { config, transport, port, log_level, workspace } => {
                return cmds::run_server(&config, &transport, port, &log_level, &workspace).await;
            }
        }
    }

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

    let mode: LainMode = args.mode.parse().expect("validated by clap");

    // Resolve the effective role. In auto mode we attempt to take the
    // workspace's exclusive lock; success means we become the owner, failure
    // means another owner is alive so we become a sidecar. Explicit owner
    // always tries to own and fails fast if the lock is held. Explicit
    // sidecar never takes the lock here; it will verify the owner later.
    let owner_guard = if mode == LainMode::Sidecar {
        None
    } else {
        match workspace_lock.acquire_exclusive() {
            Ok(guard) => Some(guard),
            Err(e) => {
                if mode == LainMode::Owner {
                    let existing = workspace_lock.read_owner_pid()
                        .map(|pid| format!(" (existing owner pid {pid})"))
                        .unwrap_or_default();
                    anyhow::bail!(
                        "Another Lain owner already holds the workspace lock at {:?}{}: {}",
                        workspace_lock.path(),
                        existing,
                        e
                    );
                } else {
                    // Auto mode: owner exists, fall through to sidecar path.
                    tracing::info!(
                        "Auto mode: existing owner holds workspace lock at {:?}; becoming sidecar",
                        workspace_lock.path()
                    );
                    None
                }
            }
        }
    };

    if let Some(_owner_guard) = owner_guard {
        // Owner path. The exclusive lock guard is held for the lifetime of
        // this branch, preventing another owner from starting.
        workspace_lock.write_owner_pid(std::process::id(), args.port)?;

        // Construct the server (loads graph + optional NLP models). Keep
        // this inside the Owner branch: sidecars do not need a local
        // LainServer and must start quickly.
        let server = LainServer::new(&args.workspace, &memory_path, embedder_model)?;

        // Start the MCP transport immediately. build_core_memory can take
        // minutes; agents time out waiting for initialize if we block on
        // it. The executor clone shares Arc-backed state, so tools see
        // updates as indexing progresses.
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

        // Heavy initialization runs in the background so the MCP loop
        // above can answer initialize/list_tools right away.
        let mut server_for_init = server.clone();
        tokio::spawn(async move {
            if let Err(e) = server_for_init.sync_volatile_overlay().await {
                tracing::error!("Volatile overlay sync failed: {}", e);
            }
            let mut server_for_indexing = server_for_init.clone_for_background();
            if let Err(e) = server_for_indexing.build_core_memory().await {
                tracing::error!("Indexing failed: {}", e);
            }
        });

        let watcher = FileWatcher::new();
        watcher.start(args.workspace.clone(), server.clone());

        let s_sync = server.clone();
        tokio::spawn(async move { s_sync.run_background_sync(300).await; });
        let s_window = server.clone();
        tokio::spawn(async move { s_window.run_sliding_window(30).await; });

        std::future::pending::<()>().await;
        unreachable!()
    } else {
        // Sidecar path (explicit sidecar or auto mode that found an owner).
        // Verify the owner is alive by briefly attempting a shared flock on
        // the workspace lock file. flock semantics: a shared acquire against
        // an exclusively-held file fails with WouldBlock — that contention is
        // the "owner is alive" signal. If the shared acquire succeeds, no
        // exclusive holder exists. If the lock file doesn't exist yet (no
        // owner has ever run here), bail with a clear message.
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
        let sidecar_transport = if args.transport.as_str() == "stdio" {
            lain::server::Transport::Stdio
        } else {
            lain::server::Transport::Http
        };
        let cfg = lain::sidecar::SidecarConfig {
            workspace: args.workspace.clone(),
            memory_path: args.memory_path.clone().unwrap_or_else(|| args.workspace.join(".lain/graph.bin")),
            port: args.port,
            transport: sidecar_transport,
            owner_url: std::env::var("LAIN_OWNER_URL").unwrap_or_else(|_| {
                let port = std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into());
                format!("http://localhost:{}/mcp", port)
            }),
            embedding_model: args.embedding_model.clone().map(std::path::PathBuf::from),
        };
        return lain::sidecar::run(cfg).await.map_err(anyhow::Error::from);
    }
}

fn parse_install_scope(s: &str) -> Result<cmds::agents::adapters::InstallScope> {
    use cmds::agents::adapters::InstallScope;
    match s {
        "user" => Ok(InstallScope::User),
        "project" => Ok(InstallScope::Project),
        "workspace" => Ok(InstallScope::Workspace),
        _ => Err(anyhow::anyhow!("unknown scope: {s}")),
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
