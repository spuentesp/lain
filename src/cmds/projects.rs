//! `lain projects` subcommand — manage the project registry.
//!
//! Each repo gets its own `.lain/graph.bin`. The registry at
//! `~/.config/lain/projects.toml` lets a single user track multiple
//! projects and switch between them with `lain use <name>` instead of
//! typing `--workspace` every time.

use lain::state::{Projects, RegistryError};
use anyhow::Result;
use std::path::Path;

/// `lain projects list` — show registered projects.
pub fn run_list() -> Result<()> {
    let list = Projects::list();
    if list.is_empty() {
        eprintln!("no projects registered; use `lain projects add <name> <path>`");
        return Ok(());
    }
    let active = Projects::active_name();
    println!("{:<20} {:<60} {:<20} {}", "NAME", "PATH", "LAST_USED", "ACTIVE");
    for p in &list {
        let mark = if active.as_deref() == Some(p.name.as_str()) {
            "*"
        } else {
            " "
        };
        let last = p.last_used.as_deref().unwrap_or("-");
        println!("{:<20} {:<60} {:<20} {}", p.name, p.path.display(), last, mark);
    }
    Ok(())
}

/// `lain projects add <name> <path>` — register a project.
pub fn run_add(name: &str, path: &Path) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("project name cannot be empty");
    }
    match Projects::add(name, path) {
        Ok(()) => {
            println!("registered '{}' -> {}", name, path.display());
            Ok(())
        }
        // Path is already registered under a different name — surface the
        // existing name so the user can run `lain projects use <name>`
        // or `lain projects forget` first. Prevents the
        // "monitor" + "monitor_dm_system" double-register trap.
        Err(RegistryError::PathAlreadyRegistered { path, existing_name }) => {
            Err(anyhow::anyhow!(
                "path '{}' is already registered as '{}'.\n\
                 Use `lain use {}` to switch to it, or run \
                 `lain projects forget {}` first to re-add under a new name.",
                path, existing_name, existing_name, existing_name
            ))
        }
        Err(RegistryError::Io(e)) => Err(anyhow::Error::from(e)),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}

/// `lain projects forget <name>` — remove a project.
pub fn run_forget(name: &str) -> Result<()> {
    match Projects::forget(name) {
        Ok(()) => {
            println!("forgot '{}'", name);
            Ok(())
        }
        Err(RegistryError::NotFound(_)) => Err(anyhow::anyhow!("project '{}' not found in registry", name)),
        Err(RegistryError::Io(e)) => Err(anyhow::Error::from(e)),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}

/// `lain use <name>` — set as the active project.
pub fn run_use(name: &str) -> Result<()> {
    match Projects::set_active(name) {
        Ok(()) => {
            println!("active project: {}", name);
            Ok(())
        }
        Err(RegistryError::NotFound(_)) => Err(anyhow::anyhow!("project '{}' not found in registry; use `lain projects add` first", name)),
        Err(RegistryError::Io(e)) => Err(anyhow::Error::from(e)),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}
