//! Lain

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// The CLI subcommands live in the `lain` library crate (`src/cli/`).
// Re-export the call sites here so the binary can dispatch.
use lain::cli;

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
    /// Manage agent MCP configurations (Claude, Kimi, Cursor, etc.)
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },
    /// Manage the federation workspace registry (`workspaces.yaml`).
    /// Use `lain workspaces use <name>` to set the active workspace.
    Workspaces {
        #[command(subcommand)]
        action: WorkspacesAction,
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
enum WorkspacesAction {
    /// Create a new workspace.
    Create {
        name: String,
        #[arg(long)] description: Option<String>,
        #[arg(long, value_delimiter = ',')] members: Vec<String>,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Add a repo to a workspace's members.
    Add {
        name: String,
        #[arg(long)] repo: String,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Remove a repo from a workspace's members.
    Remove {
        name: String,
        #[arg(long)] repo: String,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Import a workspace from another workspaces.yaml.
    Import {
        name: String,
        #[arg(long)] from: std::path::PathBuf,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Clone a workspace definition repo and register it.
    Init {
        name: String,
        #[arg(long)] from: String,            // git url
        #[arg(long, default_value = "main")] ref_: Option<String>,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// List all known workspaces.
    List {
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Show full spec of one workspace.
    Show {
        name: String,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Set the active workspace (writes ~/.config/lain/active_workspace).
    Use {
        name: String,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
    /// Print the active workspace.
    Current,
    /// Remove a workspace from workspaces.yaml.
    Forget {
        name: String,
        #[arg(long)] config: Option<std::path::PathBuf>,
    },
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
    let args = Args::parse();

    let log_level = if args.verbose { "debug" } else { &args.log_level };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| log_level.into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    tracing::info!(workspace = %args.workspace.display(), "Serving repo");
    if let Some(cmd) = args.command {
        match cmd {
            Commands::Init { agent, workspace, embedding_model, transport, port, yes, scope } => {
                let workspace = workspace.unwrap_or(args.workspace);
                return cli::run_init(&agent, Some(&workspace), embedding_model.as_deref(), &transport, port, yes, &scope);
            }
            Commands::Query { expression, workspace } => {
                let workspace = if workspace == std::path::Path::new(".") && args.workspace != std::path::Path::new(".") {
                    args.workspace
                } else {
                    workspace
                };
                return cli::run_query(&expression, &workspace);
            }
            Commands::Hook { agent, uninstall } => return cli::run_hook_install(agent, uninstall),
            Commands::Ask => return cli::run_ask(),
            Commands::Workspaces { action } => match action {
                WorkspacesAction::Create { name, description, members, config } => {
                    return cli::workspaces::run_create(&name, description, members, config.as_deref());
                }
                WorkspacesAction::Add { name, repo, config } => {
                    return cli::workspaces::run_add(&name, &repo, config.as_deref());
                }
                WorkspacesAction::Remove { name, repo, config } => {
                    return cli::workspaces::run_remove(&name, &repo, config.as_deref());
                }
                WorkspacesAction::Import { name, from, config } => {
                    return cli::workspaces::run_import(&name, &from, config.as_deref());
                }
                WorkspacesAction::Init { name, from, ref_, config } => {
                    return cli::workspaces::run_init(&name, &from, ref_, config.as_deref()).await;
                }
                WorkspacesAction::List { config } => {
                    return cli::workspaces::run_list(config.as_deref());
                }
                WorkspacesAction::Show { name, config } => {
                    return cli::workspaces::run_show(&name, config.as_deref());
                }
                WorkspacesAction::Use { name, config } => {
                    return cli::workspaces::run_use(&name, config.as_deref());
                }
                WorkspacesAction::Current => return cli::workspaces::run_current(),
                WorkspacesAction::Forget { name, config } => {
                    return cli::workspaces::run_forget(&name, config.as_deref());
                }
            },
            Commands::Agents { action } => match action {
                AgentsAction::List => return cli::agents::list::run_list(),
                AgentsAction::Install { id, all, scope } => {
                    let scope = parse_install_scope(&scope)?;
                    if !all && id.is_none() {
                        anyhow::bail!("--all or <id> is required");
                    }
                    return cli::agents::install::run_install(id.as_deref(), all, scope);
                }
                AgentsAction::Verify { id, all, json } => {
                    if !all && id.is_none() {
                        anyhow::bail!("--all or <id> is required");
                    }
                    return cli::agents::verify::run_verify(all, id.as_deref(), json).await;
                }
                AgentsAction::Remove { id, scope } => {
                    let scope = parse_install_scope(&scope)?;
                    return cli::agents::remove::run_remove(&id, scope);
                }
            },
            Commands::Server { config, transport, port, log_level, workspace } => {
                return cli::run_server(&config, &transport, port, &log_level, &workspace).await;
            }
        }
    }

    anyhow::bail!("no command given; see `lain --help`");
}

fn parse_install_scope(s: &str) -> Result<lain::cli::agents::adapters::InstallScope> {
    use lain::cli::agents::adapters::InstallScope;
    match s {
        "user" => Ok(InstallScope::User),
        "project" => Ok(InstallScope::Project),
        "workspace" => Ok(InstallScope::Workspace),
        _ => Err(anyhow::anyhow!("unknown scope: {s}")),
    }
}
