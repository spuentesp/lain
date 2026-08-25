//! `lain init` — scaffold a minimal `repos.yaml` for the current
//! directory. The onboarding shortcut: from inside any clone
//! `lain init && lain server` and you have a working MCP server with
//! zero hand-written YAML.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

const REPOS_TEMPLATE: &str = "\
data_dir: ./.lain/data
repos:
  - id: {id}
    source:
      type: workspace_dir
      path: {path}
";

/// Run `lain init`. Walks up for `.git` (unless `--workspace` is
/// given), then writes `./repos.yaml` plus a `.gitignore`-friendly
/// `data_dir` hint. With `--print`, render to stdout instead of
/// writing — useful for piping or for CI sanity checks. With
/// `--force`, overwrite an existing `./repos.yaml`.
pub fn run_init(
    workspace: Option<&Path>,
    force: bool,
    print: bool,
) -> Result<()> {
    let workspace = match workspace {
        Some(p) => p.to_path_buf(),
        None => find_git_workspace(None)?
            .ok_or_else(|| anyhow!(
                "no `.git` found in any parent directory and no --workspace given; \
                 pass --workspace PATH or run from inside a clone"
            ))?,
    };
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "{} has no .git — pass --workspace PATH or run from inside a clone",
            workspace.display()
        ));
    }
    // Repo id: the basename of the workspace dir, sanitized.
    let id = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();
    let body = REPOS_TEMPLATE
        .replace("{id}", &id)
        .replace("{path}", &workspace.display().to_string());

    if print {
        print!("{body}");
        return Ok(());
    }
    let target = std::env::current_dir()
        .context("get current dir")?
        .join("repos.yaml");
    if target.exists() && !force {
        return Err(anyhow!(
            "{} already exists; pass --force to overwrite",
            target.display()
        ));
    }
    std::fs::write(&target, body).context("write repos.yaml")?;
    println!("wrote {}", target.display());
    println!("next: lain server");
    Ok(())
}

pub(crate) use crate::cli::workspace::find_git_workspace_root as find_git_workspace;
