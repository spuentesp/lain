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
pub fn run_init(workspace: Option<&Path>, force: bool, print: bool) -> Result<()> {
    let workspace = match workspace {
        Some(p) => p.to_path_buf(),
        None => find_git_workspace(None)?.ok_or_else(|| {
            anyhow!(
                "no `.git` found in any parent directory and no --workspace given; \
                 pass --workspace PATH or run from inside a clone"
            )
        })?,
    };
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "{} has no .git — pass --workspace PATH or run from inside a clone",
            workspace.display()
        ));
    }
    // Repo id: the basename of the workspace dir, sanitized. It used to go
    // in verbatim: `proj #1` became id `proj` (the rest read as a YAML
    // comment) and `a: b` made the file invalid.
    let id = repo_id_from_dir_name(
        workspace
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repo"),
    );
    // The path as a double-quoted YAML scalar (JSON string syntax is valid
    // YAML), so `#`, `: ` and quotes in it survive.
    let quoted_path =
        serde_json::to_string(&workspace.display().to_string()).context("quote workspace path")?;
    let body = REPOS_TEMPLATE
        .replace("{id}", &id)
        .replace("{path}", &quoted_path);

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

/// A valid repo id from a directory name: characters outside
/// `[A-Za-z0-9._-]` become `-`.
fn repo_id_from_dir_name(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-').to_string();
    if id.is_empty() || crate::federation::repo_id::RepoId::new(&id).is_err() {
        "repo".to_string()
    } else {
        id
    }
}

#[cfg(test)]
mod id_tests {
    use super::*;

    #[test]
    fn directory_names_become_valid_ids_and_paths_are_quoted() {
        assert_eq!(repo_id_from_dir_name("proj #1"), "proj--1");
        assert_eq!(repo_id_from_dir_name("a: b"), "a--b");
        assert_eq!(repo_id_from_dir_name("###"), "repo");
        let body = REPOS_TEMPLATE
            .replace("{id}", &repo_id_from_dir_name("proj #1"))
            .replace("{path}", &serde_json::to_string("/x/proj #1").unwrap());
        let cfg: crate::federation::config::FederationConfig =
            serde_yaml::from_str(&body).expect("valid YAML");
        assert_eq!(cfg.repos[0].id, "proj--1");
        assert!(body.contains("\"/x/proj #1\""), "{body}");
    }
}
