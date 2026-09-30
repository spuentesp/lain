//! `lain workspaces` subcommand — manage the workspace registry.
//!
//! Workspaces are named groups of repos declared in `workspaces.yaml`. The
//! CLI edits that file directly; the active workspace pointer at
//! `~/.config/lain/active_workspace` is read by `lain server --workspace
//! auto` to pick which workspace the federation loads.

use crate::error::LainError;
use crate::federation::workspace::{
    WorkspaceSource, WorkspaceSourceConfig, WorkspaceSpec, WorkspacesFile,
};
use crate::state::ActiveWorkspace;
use anyhow::{anyhow, Result};
use clap::Subcommand;
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
        /// Branch or tag. Defaults to the remote's default branch.
        #[arg(long)]
        ref_: Option<String>,
    },
    /// List all known workspaces.
    List,
    /// Show full spec of one workspace.
    Show { name: String },
    /// Set the active workspace (writes ~/.config/lain/active_workspace).
    Use { name: String },
    /// Print the active workspace.
    Current,
    /// Remove a workspace from workspaces.yaml.
    Forget { name: String },
}

/// Dispatch a `lain workspaces <action>` invocation. `config` is the
/// resolved `--config` path (defaults to `./repos.yaml`); the
/// individual `run_*` helpers each take `Option<&Path>` and resolve
/// from there.
pub async fn run(action: WorkspacesAction, config: &Path) -> Result<()> {
    let workspaces_file = workspaces_file_for(config);
    let config = Some(workspaces_file.as_path());
    match action {
        WorkspacesAction::Create {
            name,
            description,
            members,
        } => run_create(&name, description, members, config),
        WorkspacesAction::Add { name, repo } => run_add(&name, &repo, config),
        WorkspacesAction::Remove { name, repo } => run_remove(&name, &repo, config),
        WorkspacesAction::Import { name, from } => run_import(&name, &from, config),
        WorkspacesAction::Init { name, from, ref_ } => run_init(&name, &from, ref_, config).await,
        WorkspacesAction::List => run_list(config),
        WorkspacesAction::Show { name } => run_show(&name, config),
        WorkspacesAction::Use { name } => run_use(&name, config),
        WorkspacesAction::Current => run_current(),
        WorkspacesAction::Forget { name } => run_forget(&name, config),
    }
}

/// `--config` names the project's `repos.yaml` (the flag and its default
/// are shared with `lain repos`); workspaces live beside it in
/// `workspaces.yaml`, which is where `lain server` reads them. Writing the
/// workspaces file *to* the `repos.yaml` path replaced every registered
/// repo with the workspace list. A path already naming a workspaces file
/// is used as given.
fn workspaces_file_for(config: &Path) -> PathBuf {
    if config
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with("workspaces.yaml"))
    {
        config.to_path_buf()
    } else {
        config.with_file_name("workspaces.yaml")
    }
}

/// Which config path should carry the reload signal.
///
/// Workspaces mutations live in `workspaces.yaml`, but `signal_reload`
/// names the socket after the file it is given, so signal through the
/// sibling `repos.yaml` when there is one; otherwise fall back to the
/// given path so a standalone `workspaces.yaml` still works.
///
/// Limitation: the sibling is matched by the hardcoded `repos.yaml`
/// basename. A server started with `--config myrepos.yaml` listens on a
/// socket named after `myrepos`, so these commands dial a socket nobody
/// owns and the reload is silently missed. The same trap applies to
/// `--config myworkspaces.yaml` when a sibling `repos.yaml` exists:
/// `reload_target` resolves to `repos.sock` (the sibling) while the
/// server listens on the path-canonicalized socket for
/// `myworkspaces.yaml`. Sockets are path-hashed, so the misfire cannot
/// reach another project's server either.
fn reload_target(workspaces_yaml: &Path) -> PathBuf {
    let sibling = workspaces_yaml
        .parent()
        .map(|p| p.join("repos.yaml"))
        .filter(|p| p.is_file());
    sibling.unwrap_or_else(|| workspaces_yaml.to_path_buf())
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
    // Temp-then-rename, as `repos.yaml` is written: a watcher-triggered
    // rebuild must never read a torn `workspaces.yaml`.
    crate::cli::io::write_file_atomic(path, text.as_bytes())?;
    Ok(())
}

fn err_already_exists(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' already exists"))
}

fn err_not_found(name: &str) -> LainError {
    LainError::Config(format!("workspace '{name}' not found"))
}

/// `lain workspaces create <name> [--description <text>] [--members repo,repo,...]`
/// Trimmed member ids, checked against the `repos.yaml` beside the
/// workspaces file when there is one. `--members "bytes, tokio"` stored
/// `" tokio"`, and a typo such as `bytess` was accepted and only failed
/// later, at server start.
fn checked_members(workspaces_path: &Path, members: Vec<String>) -> Result<Vec<String>> {
    let members: Vec<String> = members
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    let repos_path = workspaces_path.with_file_name("repos.yaml");
    if !repos_path.is_file() {
        return Ok(members);
    }
    let repos = crate::server::federation::config::FederationConfig::load(&repos_path)
        .map_err(|e| anyhow!("load {}: {e}", repos_path.display()))?;
    let known: Vec<String> = repos.repos.iter().map(|r| r.id.to_string()).collect();
    let unknown: Vec<&String> = members.iter().filter(|m| !known.contains(m)).collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "unknown repo id(s) {unknown:?}; {} registers: {}",
            repos_path.display(),
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
    }
    Ok(members)
}

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
    let members = checked_members(&path, members)?;
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
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after creating '{name}': {e}"))?;
    println!("Created workspace '{name}' in {}", path.display());
    Ok(())
}

/// `lain workspaces add <name> --repo <repo-id>`
pub fn run_add(name: &str, repo: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let repo = checked_members(&path, vec![repo.to_string()])?
        .pop()
        .ok_or_else(|| anyhow!("repo id cannot be empty"))?;
    let repo = repo.as_str();
    let mut f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f
        .workspaces
        .iter_mut()
        .find(|w| w.name == name)
        .ok_or_else(|| anyhow!("{}", err_not_found(name)))?;
    if !ws.members.iter().any(|m| m == repo) {
        ws.members.push(repo.to_string());
    }
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after adding '{repo}' to '{name}': {e}"))?;
    println!("Added repo '{repo}' to workspace '{name}'");
    Ok(())
}

/// `lain workspaces remove <name> --repo <repo-id>`
pub fn run_remove(name: &str, repo: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let mut f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f
        .workspaces
        .iter_mut()
        .find(|w| w.name == name)
        .ok_or_else(|| anyhow!("{}", err_not_found(name)))?;
    let repo = repo.trim();
    if !ws.members.iter().any(|m| m == repo) {
        anyhow::bail!(
            "'{repo}' is not a member of workspace '{name}' (members: {})",
            ws.members.join(", ")
        );
    }
    ws.members.retain(|m| m != repo);
    // `validate` lets a *sourced* workspace hold 0 members — `lain
    // workspaces init` writes that transient state and `lain workspaces
    // add` fills it — but this command must not drive a live workspace
    // down to zero: the loader refuses to serve a 0-member workspace, so
    // saving that state breaks the next reload.
    if ws.members.is_empty() {
        anyhow::bail!(
            "workspace '{name}' must keep at least one repo — removing '{repo}' would leave it \
             empty; run 'lain workspaces add {name} --repo <repo-id>' first, or 'lain workspaces \
             forget {name}' to drop the workspace"
        );
    }
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after removing '{repo}' from '{name}': {e}"))?;
    println!("Removed repo '{repo}' from workspace '{name}'");
    Ok(())
}

/// `lain workspaces import <name> --from <dir>`
pub fn run_import(name: &str, from: &Path, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let from_path = from.join("workspaces.yaml");
    let from_file = WorkspacesFile::load(&from_path)
        .map_err(|e| anyhow!("load {}: {e}", from_path.display()))?;
    let imported = from_file
        .workspaces
        .iter()
        .find(|w| w.name == name)
        .ok_or_else(|| anyhow!("workspace '{name}' not found in {}", from_path.display()))?
        .clone();
    let mut f = load_or_default(&path)?;
    if f.workspaces.iter().any(|w| w.name == name) {
        return Err(err_already_exists(name).into());
    }
    f.workspaces.push(imported);
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after importing '{name}': {e}"))?;
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
    // Ask the remote, as `repos add` does: a hardcoded `main` broke every
    // definition repo whose default branch is `master`.
    let ref_ = ref_.or_else(|| crate::cli::repos::remote_default_branch(from_url));
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
        members: vec![], // populate via `lain workspaces add` afterward
    });
    f.validate().map_err(|e| anyhow!("validate: {e}"))?;
    save(&path, &f)?;
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after initializing '{name}': {e}"))?;
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
        let marker = if active.as_deref() == Some(&ws.name) {
            "* "
        } else {
            "  "
        };
        println!("{}{:<24} {} repos", marker, ws.name, ws.members.len());
    }
    Ok(())
}

/// `lain workspaces show <name>`
pub fn run_show(name: &str, config: Option<&Path>) -> Result<()> {
    let path = resolve_config_path(config);
    let f = WorkspacesFile::load(&path).map_err(|e| anyhow!("load {}: {e}", path.display()))?;
    let ws = f
        .workspaces
        .iter()
        .find(|w| w.name == name)
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
        Some(WorkspaceSourceConfig::WorkspaceDir { path }) => {
            println!("source: workspace_dir ({})", path.display())
        }
        Some(WorkspaceSourceConfig::WorkspaceClone {
            url,
            ref_,
            refresh_interval_secs,
        }) => {
            let r = ref_.clone().unwrap_or_else(|| "main".to_string());
            let ri = refresh_interval_secs
                .map(|n| format!(" refresh={n}s"))
                .unwrap_or_default();
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
        return Err(anyhow!(
            "{}",
            LainError::Config(format!(
                "workspace '{name}' not found in {}",
                path.display()
            ))
        ));
    }
    // Absolute: the pointer is global, and `lain server` compares it with
    // its own project's workspaces file to know whether it applies.
    let absolute = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
    ActiveWorkspace {
        name: name.to_string(),
        config_path: Some(absolute),
    }
    .save()
    .map_err(|e| anyhow!("save active workspace: {e}"))?;
    println!(
        "Active workspace set to '{name}' (from {}). Restart `lain server` to pick it up.",
        path.display()
    );
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

/// `lain workspaces forget` of the active workspace cannot resolve on a
/// running `lain server` scoped to it: the rebuild goes `Failed` and the
/// server keeps serving the forgotten workspace's repos until restart or
/// re-creation. The command still succeeds, so surface the consequence on
/// stderr. Returns the warning text when `name` is the active workspace.
fn forgetting_active_workspace_warning(name: &str) -> Option<String> {
    let active = ActiveWorkspace::load().ok().flatten()?;
    (active.name == name).then(|| {
        format!(
            "warning: '{name}' is the active workspace; a running `lain server` will keep serving its repos until restart or re-creation"
        )
    })
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
    if let Some(warning) = forgetting_active_workspace_warning(name) {
        eprintln!("{warning}");
    }
    crate::cli::signal::signal_reload(&reload_target(&path))
        .map_err(|e| anyhow!("signal reload after forgetting '{name}': {e}"))?;
    println!("Forgot workspace '{name}'");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `lain workspaces create` with the default `--config ./repos.yaml`
    /// must leave the repos alone and write `workspaces.yaml` beside it.
    #[tokio::test]
    async fn create_with_the_repos_config_writes_the_sibling_workspaces_file() {
        let dir = tempfile::tempdir().unwrap();
        let repos = dir.path().join("repos.yaml");
        let original = "data_dir: ./.lain/federation\nrepos:\n- id: pflag\n  source:\n    type: local_clone\n    url: https://example.invalid/pflag.git\n    ref: main\n";
        std::fs::write(&repos, original).unwrap();
        let action = WorkspacesAction::Create {
            name: "spf13".into(),
            description: None,
            members: vec!["pflag".into()],
        };
        // The reload signal may fail with no server listening; the files
        // are what matter here.
        let _ = run(action, &repos).await;
        assert_eq!(
            std::fs::read_to_string(&repos).unwrap(),
            original,
            "repos.yaml untouched"
        );
        let ws = std::fs::read_to_string(dir.path().join("workspaces.yaml")).unwrap();
        assert!(
            ws.contains("spf13"),
            "workspace written beside it; got {ws}"
        );
    }

    #[test]
    fn members_are_trimmed_and_checked_against_repos_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        // No repos.yaml beside it: trimmed, not checked.
        assert_eq!(
            checked_members(&ws, vec!["a".into(), " b ".into(), "".into()]).unwrap(),
            vec!["a", "b"]
        );
        std::fs::write(
            dir.path().join("repos.yaml"),
            "repos:\n- id: bytes\n  source:\n    type: local_clone\n    url: https://example.invalid/b.git\n    ref: main\n",
        )
        .unwrap();
        assert_eq!(
            checked_members(&ws, vec![" bytes".into()]).unwrap(),
            vec!["bytes"]
        );
        let err = checked_members(&ws, vec!["bytess".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("bytess") && err.contains("bytes"), "{err}");
    }

    #[test]
    fn an_explicit_workspaces_path_is_used_as_given() {
        let p = Path::new("/x/team-workspaces.yaml");
        assert_eq!(workspaces_file_for(p), p);
        assert_eq!(
            workspaces_file_for(Path::new("./repos.yaml")),
            Path::new("./workspaces.yaml")
        );
    }

    #[test]
    fn reload_signal_targets_the_repos_socket_not_the_workspaces_one() {
        // A server started with `--config repos.yaml` listens on
        // `repos.sock`, named after the file stem. A workspaces command
        // that signalled `workspaces.sock` would never reach it and the
        // federation would silently go stale.
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        let repos = dir.path().join("repos.yaml");
        std::fs::write(&ws, "workspaces: []\n").unwrap();
        std::fs::write(&repos, "repos: []\n").unwrap();

        assert_eq!(reload_target(&ws), repos);
    }

    #[test]
    fn reload_signal_falls_back_to_the_given_path_without_a_repos_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        std::fs::write(&ws, "workspaces: []\n").unwrap();
        assert_eq!(reload_target(&ws), ws);
    }

    /// Run `op` with the reload socket made observable: `XDG_RUNTIME_DIR`
    /// points at a tempdir (so `run_dir()` is private to this call) and a
    /// listener is bound at the socket `signal_reload` will dial. Returns
    /// the bytes that arrived. The sequence runs under a process-wide lock
    /// because the env override is process-wide state.
    #[cfg(unix)]
    fn capture_reload_signal<F>(workspaces_yaml: &Path, op: F) -> String
    where
        F: FnOnce(&Path) -> anyhow::Result<()>,
    {
        use std::io::Read;
        use std::os::unix::net::UnixListener;
        use std::sync::Mutex;

        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_dir_owner = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_RUNTIME_DIR", run_dir_owner.path());

        let sock = crate::cli::signal::socket_path_for(&reload_target(workspaces_yaml));
        let _ = std::fs::remove_file(&sock);
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&sock).expect("bind reload socket");
        listener.set_nonblocking(true).unwrap();

        let outcome = op(workspaces_yaml);
        match prev {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        outcome.expect("command under test");

        // `signal_reload` connects and writes before the command returns,
        // so a signalled connection is already queued here.
        let (stream, _) = listener
            .accept()
            .unwrap_or_else(|e| panic!("no reload signal after the command: {e}"));
        let mut stream = stream;
        stream.set_nonblocking(false).unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        buf
    }

    #[cfg(unix)]
    #[test]
    fn run_forget_signals_reload_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        std::fs::write(
            &ws,
            "workspaces:\n  - name: doomed\n    members: [repo-a]\n",
        )
        .unwrap();
        let received = capture_reload_signal(&ws, |ws_path| run_forget("doomed", Some(ws_path)));
        assert_eq!(received, "reload\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_forget_warns_when_active_workspace_is_forgotten() {
        use std::process::Command;

        // Capture stderr from `lain workspaces forget` by running it as a
        // subprocess: the codebase has no in-process stderr capture helper
        // and adding one would require `libc`/`nix` (forbidden by the
        // Scorecard dep policy). Locating the binary from `current_exe()`
        // works because `cargo test` lays the test runner and `lain`
        // under the same `<target>/<profile>/`.
        let lain_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .and_then(|p| p.parent().map(|p| p.join("lain")))
            .filter(|p| p.is_file());
        let Some(lain_bin) = lain_bin else {
            eprintln!(
                "skip: `lain` binary not found; run `cargo build --bin lain` \
                 before this test (or run `cargo test --workspace`, which \
                 builds it for you)"
            );
            return;
        };

        let _g = crate::state::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let project = tempfile::tempdir().unwrap();
        let ws = project.path().join("workspaces.yaml");
        std::fs::write(
            &ws,
            "workspaces:\n  - name: doomed\n    members: [repo-a]\n",
        )
        .unwrap();
        let cfg_home = tempfile::tempdir().unwrap();
        let lain_cfg = cfg_home.path().join("lain");
        std::fs::create_dir_all(&lain_cfg).unwrap();
        std::fs::write(lain_cfg.join("active_workspace"), "doomed\n").unwrap();

        let out = Command::new(&lain_bin)
            .args(["workspaces", "--config"])
            .arg(&ws)
            .args(["forget", "doomed"])
            .env("XDG_CONFIG_HOME", cfg_home.path())
            .env_remove("LAIN_HOME")
            .output()
            .expect("spawn lain workspaces forget");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "lain workspaces forget failed: {stderr}"
        );
        assert!(
            stderr.contains("doomed") && stderr.contains("serving"),
            "forgetting the active workspace must warn on stderr, got: {stderr}"
        );
    }

    #[test]
    fn forgetting_the_active_workspace_warns_about_the_running_server() {
        let _g = crate::state::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = crate::test_util::XdgGuard::new(tmp.path());
        ActiveWorkspace {
            name: "doomed".into(),
            config_path: None,
        }
        .save()
        .unwrap();
        let warning = forgetting_active_workspace_warning("doomed")
            .expect("forgetting the active workspace must warn");
        assert!(warning.contains("doomed"), "{warning}");
        assert!(warning.contains("serving"), "{warning}");
        assert!(forgetting_active_workspace_warning("other").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn run_create_signals_reload_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        std::fs::write(&ws, "workspaces: []\n").unwrap();
        let received = capture_reload_signal(&ws, |ws_path| {
            run_create("new", None, vec!["r".into()], Some(ws_path))
        });
        assert_eq!(received, "reload\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_add_signals_reload_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        std::fs::write(
            &ws,
            "workspaces:\n  - name: w\n    members: [r1]\n",
        )
        .unwrap();
        let received = capture_reload_signal(&ws, |ws_path| {
            run_add("w", "r2", Some(ws_path))
        });
        assert_eq!(received, "reload\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_remove_signals_reload_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        std::fs::write(
            &ws,
            "workspaces:\n  - name: w\n    members: [r1, r2]\n",
        )
        .unwrap();
        let received = capture_reload_signal(&ws, |ws_path| {
            run_remove("w", "r2", Some(ws_path))
        });
        assert_eq!(received, "reload\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_init_signals_reload_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        let received = capture_reload_signal(&ws, |ws_path| {
            // Keep the clone scratch dir inside the test's tempdir. The
            // nonexistent URL makes the best-effort fetch fail instantly
            // and offline; init must still save and signal afterwards.
            let prev = std::env::var_os("LAIN_HOME");
            std::env::set_var("LAIN_HOME", dir.path().join("lain-home"));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(run_init(
                "sig-init",
                "/nonexistent/not-a-repo",
                None,
                Some(ws_path),
            ));
            match prev {
                Some(v) => std::env::set_var("LAIN_HOME", v),
                None => std::env::remove_var("LAIN_HOME"),
            }
            outcome
        });
        assert_eq!(received, "reload\n");
    }

    /// `lain workspaces init` writes a sourced workspace with 0 members
    /// and `lain workspaces add` fills it in; `WorkspacesFile::validate`
    /// keeps accepting that transient state so init can save. `run_remove`
    /// must not drive a workspace back down to 0 members on the way: the
    /// loader refuses to serve a 0-member workspace, so saving that state
    /// breaks the next reload.
    #[test]
    fn removing_the_last_member_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspaces.yaml");
        let before = "workspaces:\n  - name: pending\n    members: [only]\n    source:\n      type: workspace_clone\n      url: https://example.com/ws.git\n";
        std::fs::write(&ws, before).unwrap();
        let err = run_remove("pending", "only", Some(&ws)).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("must keep at least one repo"),
            "error must name the invariant, got: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&ws).unwrap(),
            before,
            "a refused removal must not rewrite workspaces.yaml"
        );
    }
}
