//! `lain workspaces` subcommand — manage the workspace registry.
//!
//! Workspaces are named groups of repos declared in `workspaces.yaml`. The
//! CLI edits that file directly; the active workspace pointer at
//! `~/.config/lain/active_workspace` is read by `lain server --workspace
//! auto` to pick which workspace the federation loads.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use crate::error::LainError;
use crate::federation::workspace::{WorkspaceSource, WorkspaceSourceConfig, WorkspaceSpec, WorkspacesFile};
use crate::state::ActiveWorkspace;
use std::path::{Path, PathBuf};

/// Subcommands for `lain workspaces`. Mirrors the actions available
/// before the consolidation; the dispatcher in [`run`] routes each
/// variant to the matching `run_*` function below.
#[derive(Debug, Subcommand)]
pub enum WorkspacesAction {
    /// Create a new workspace.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
    },
    /// Add a repo to a workspace's members.
    Add {
        name: String,
        #[arg(long)]
        repo: String,
    },
    /// Remove a repo from a workspace's members.
    Remove {
        name: String,
        #[arg(long)]
        repo: String,
    },
    /// Import a workspace from another workspaces.yaml.
    Import {
        name: String,
        #[arg(long)]
        from: PathBuf,
    },
    /// Clone a workspace definition repo and register it.
    Init {
        name: String,
        #[arg(long)]
        from: String,
        #[arg(long, default_value = "main")]
        ref_: Option<String>,
    },
    /// List all known workspaces.
    List,
    /// Show full spec of one workspace.
    Show {
        name: String,
    },
    /// Set the active workspace (writes ~/.config/lain/active_workspace).
    Use {
        name: String,
    },
    /// Print the active workspace.
    Current,
    /// Remove a workspace from workspaces.yaml.
    Forget {
        name: String,
    },
}

/// Dispatch a `lain workspaces <action>` invocation. `config` is the
/// resolved `--config` path (defaults to `./repos.yaml`); the
/// individual `run_*` helpers each take `Option<&Path>` and resolve
/// from there.
pub async fn run(action: WorkspacesAction, config: &Path) -> Result<()> {
    let config = Some(config);
    match action {
        WorkspacesAction::Create { name, description, members } => {
            run_create(&name, description, members, config)
        }
        WorkspacesAction::Add { name, repo } => run_add(&name, &repo, config),
        WorkspacesAction::Remove { name, repo } => run_remove(&name, &repo, config),
        WorkspacesAction::Import { name, from } => run_import(&name, &from, config),
        WorkspacesAction::Init { name, from, ref_ } => {
            run_init(&name, &from, ref_, config).await
        }
        WorkspacesAction::List => run_list(config),
        WorkspacesAction::Show { name } => run_show(&name, config),
        WorkspacesAction::Use { name } => run_use(&name, config),
        WorkspacesAction::Current => run_current(),
        WorkspacesAction::Forget { name } => run_forget(&name, config),
    }
}

/// Resolve a `workspaces.yaml` path. The CLI accepts an explicit `--config`
/// flag; if absent, walk up from cwd looking for a `workspaces.yaml` next
/// to a `repos.yaml`, then a standalone `workspaces.yaml`. Fall back to
/// `./workspaces.yaml` if nothing is found (the create/import commands
/// will then create it).
fn resolve_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if cwd.join("workspaces.yaml").is_file() {
            return cwd.join("workspaces.yaml");
        }
        if !cwd.pop() {
            break;
        }
    }
    PathBuf::from("workspaces.yaml")
}

fn load_or_default(path: &Path) -> Result<WorkspacesFile> {
    if path.exists() {
        WorkspacesFile::load(path).map_err(|e| anyhow!("load {}: {e}", path.display()))
    } else {
        Ok(WorkspacesFile::default())
    }
}

fn save(path: &Path, f: &WorkspacesFile) -> Result<()> {
    let text = serde_yaml::to_string(f).map_err(|e| anyhow!("serialize: {e}"))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn err_already_exists(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' already exists"))
}

fn err_not_found(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' not found"))
}

/// `lain workspaces create <name> [--description <text>] [--members repo,repo,...]`
pub fn run_create(
    name: &str,
    description: Option<String>,
    members: Vec<String>,
    config: Option<&Path>,
) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("workspace name cannot be empty");
    }
    let path = resolve_config_path(config);
    let mut f = load_or_default(&path)?;
    if f.workspaces.iter().any(|w| w.name == name) {
        return Err(err_already_exists(name).into());
    }
    f.workspaces.push(WorkspaceSpec {
        name: name.to_string(),
        description,
        source: None,
        members,
    });
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Created workspace '{name}' in {}", path.display());
    Ok(())
}

/// `lain workspaces add <name> --repo <repo-id>`
pub fn run_add(name: &str, repo: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let mut f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f.workspaces.iter_mut().find(|w| w.name == name).ok_or_else(|| anyhow!("{}", err_not_found(name)))?;
    if !ws.members.iter().any(|m| m == repo) {
        ws.members.push(repo.to_string());
    }
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Added repo '{repo}' to workspace '{name}'");
    Ok(())
}

/// `lain workspaces remove <name> --repo <repo-id>`
pub fn run_remove(name: &str, repo: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let mut f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f.workspaces.iter_mut().find(|w| w.name == name).ok_or_else(|| anyhow!("{}", err_not_found(name)))?;
    ws.members.retain(|m| m != repo);
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Removed repo '{repo}' from workspace '{name}'");
    Ok(())
}

/// `lain workspaces import <name> --from <dir>`
pub fn run_import(name: &str, from: &Path, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let from_path = from.join("workspaces.yaml");
    let from_file = WorkspacesFile::load(&from_path).map_err(|e| anyhow!("load {}: {e}", from_path.display()))?;
    let imported = from_file.workspaces.iter().find(|w| w.name == name)
        .ok_or_else(|| anyhow!("workspace '{name}' not found in {}", from_path.display()))?
        .clone();
    let mut f = load_or_default(&path)?;
    if f.workspaces.iter().any(|w| w.name == name) {
        return Err(err_already_exists(name).into());
    }
    f.workspaces.push(imported);
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Imported workspace '{name}' into {}", path.display());
    Ok(())
}

/// `lain workspaces init <name> --from <git-url> [--ref <branch>]`
///
/// Clones a workspace definition repo and registers a workspace_clone
/// source. (The actual clone requires network + a configured
/// WorkspaceCloneSource; the test gap is that we don't run network
/// tests in this env. Operators running locally get a clone.)
pub async fn run_init(
    name: &str,
    from_url: &str,
    ref_: Option<String>,
    config: Option<&Path>,
) -> Result<()> {
    if from_url.is_empty() {
        anyhow::bail!("--from url cannot be empty");
    }
    let path = resolve_config_path(config);
    let local_root = std::env::var_os("LAIN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/lain"))
                .unwrap_or_else(|| PathBuf::from(".local/lain"))
        });
    let source = crate::federation::workspace::WorkspaceCloneSource::new(
        name.to_string(),
        from_url.to_string(),
        ref_.clone(),
        None,
        local_root,
    )?;
    // Best-effort fetch (clones on first run, fetch+reset otherwise).
    // Skip if no network — operator can re-run later.
    if let Err(e) = source.fetch().await {
        eprintln!("warning: initial fetch failed (will retry on first server start): {e}");
    }
    let mut f = load_or_default(&path)?;
    if f.workspaces.iter().any(|w| w.name == name) {
        return Err(err_already_exists(name).into());
    }
    f.workspaces.push(WorkspaceSpec {
        name: name.to_string(),
        description: None,
        source: Some(WorkspaceSourceConfig::WorkspaceClone {
            url: from_url.to_string(),
            ref_,
            refresh_interval_secs: None,
        }),
        members: vec![],  // populate via `lain workspaces add` afterward
    });
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Initialized workspace '{name}' from {from_url}");
    Ok(())
}

/// `lain workspaces list`
pub fn run_list(config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let f = load_or_default(&path)?;
    let active = ActiveWorkspace::load().ok().flatten().map(|a| a.name);
    if f.workspaces.is_empty() {
        println!("(no workspaces defined in {})", path.display());
        return Ok(());
    }
    for ws in &f.workspaces {
        let marker = if active.as_deref() == Some(&ws.name) { "* " } else { "  " };
        println!("{}{:<24} {} repos", marker, ws.name, ws.members.len());
    }
    Ok(())
}

/// `lain workspaces show <name>`
pub fn run_show(name: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f.workspaces.iter().find(|w| w.name == name)
        .ok_or_else(|| anyhow!("{}", err_not_found(name)))?;
    println!("name: {}", ws.name);
    if let Some(d) = &ws.description {
        println!("description: {d}");
    }
    println!("members ({}):", ws.members.len());
    for m in &ws.members {
        println!("  - {m}");
    }
    match &ws.source {
        Some(WorkspaceSourceConfig::WorkspaceDir { path }) => println!("source: workspace_dir ({})", path.display()),
        Some(WorkspaceSourceConfig::WorkspaceClone { url, ref_, refresh_interval_secs }) => {
            let r = ref_.clone().unwrap_or_else(|| "main".to_string());
            let ri = refresh_interval_secs.map(|n| format!(" refresh={n}s")).unwrap_or_default();
            println!("source: workspace_clone ({url} @ {r}{ri})");
        }
        None => println!("source: (none)"),
    }
    Ok(())
}

/// `lain workspaces use <name>` — set the active workspace.
pub fn run_use(name: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    if !f.workspaces.iter().any(|w| w.name == name) {
        return Err(anyhow!("{}", LainError::Config(format!(
            "workspace '{name}' not found in {}", path.display()
        ))).into());
    }
    ActiveWorkspace { name: name.to_string(), config_path: Some(path.clone()) }.save()
        .map_err(|e| anyhow!("save active workspace: {e}"))?;
    println!("Active workspace set to '{name}' (from {}). Restart `lain server` to pick it up.", path.display());
    Ok(())
}

/// `lain workspaces current` — print the active workspace.
pub fn run_current() -> Result<()> {
    match ActiveWorkspace::load().map_err(|e| anyhow!("load: {e}"))? {
        Some(a) => println!("{}", a.name),
        None => {
            eprintln!("no active workspace; use `lain workspaces use <name>`");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// `lain workspaces forget <name>` — remove a workspace from workspaces.yaml.
pub fn run_forget(name: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let mut f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let before = f.workspaces.len();
    f.workspaces.retain(|w| w.name != name);
    if f.workspaces.len() == before {
        return Err(err_not_found(name).into());
    }
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    println!("Forgot workspace '{name}'");
    Ok(())
}