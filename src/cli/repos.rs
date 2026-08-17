//! `lain repos` subcommand — manage the `repos.yaml` federation registry.
//!
//! PR 3 lands the `add` / `list` / `remove` operations against the
//! project's `repos.yaml`. The file is rewritten atomically
//! (write-temp-then-rename) so the watcher either sees the old
//! contents or the new contents, never a partial write.

use anyhow::{Context, Result};
use clap::Subcommand;
use crate::server::federation::config::{FederationConfig, RepoConfig, SourceConfig};
use std::path::Path;

/// Subcommands for `lain repos`.
#[derive(Debug, Subcommand)]
pub enum ReposAction {
    /// Register a new repo in `repos.yaml` (cloned from `url` at `ref`).
    Add {
        name: String,
        url: String,
        #[arg(long, default_value = "main")]
        ref_: String,
    },
    /// List all repos registered in `repos.yaml`.
    List,
    /// Remove a repo from `repos.yaml`.
    Remove { name: String },
}

/// Dispatch a `lain repos <action>` invocation.
pub fn run(action: ReposAction, config_path: &Path) -> Result<()> {
    match action {
        ReposAction::Add { name, url, ref_ } => add(config_path, &name, &url, &ref_),
        ReposAction::List => list(config_path),
        ReposAction::Remove { name } => remove(config_path, &name),
    }
}

/// `lain repos add <name> <url> [--ref <branch>]`
fn add(config_path: &Path, name: &str, url: &str, ref_: &str) -> Result<()> {
    let mut file = FederationConfig::load(config_path).unwrap_or_default();
    if file.repos.iter().any(|r| r.id == name) {
        anyhow::bail!("repo '{name}' already exists in {}", config_path.display());
    }
    file.repos.push(RepoConfig {
        id: name.to_string(),
        source: SourceConfig::LocalClone {
            url: url.to_string(),
            r#ref: ref_.to_string(),
        },
    });
    write_atomic(config_path, &file)?;
    crate::cli::signal::signal_reload(config_path)
        .with_context(|| format!("signal reload after adding '{name}'"))?;
    Ok(())
}

/// `lain repos list`
fn list(config_path: &Path) -> Result<()> {
    let file = FederationConfig::load(config_path).unwrap_or_default();
    if file.repos.is_empty() {
        println!("(no repos registered in {})", config_path.display());
        return Ok(());
    }
    for r in &file.repos {
        println!("{}\t{:?}", r.id, r.source);
    }
    Ok(())
}

/// `lain repos remove <name>`
fn remove(config_path: &Path, name: &str) -> Result<()> {
    let mut file = FederationConfig::load(config_path)
        .with_context(|| format!("load {}", config_path.display()))?;
    let before = file.repos.len();
    file.repos.retain(|r| r.id != name);
    if file.repos.len() == before {
        anyhow::bail!("repo '{name}' not found in {}", config_path.display());
    }
    write_atomic(config_path, &file)?;
    crate::cli::signal::signal_reload(config_path)
        .with_context(|| format!("signal reload after removing '{name}'"))?;
    Ok(())
}

/// Write YAML atomically: write to a sibling temp file, then rename.
fn write_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repos.yaml".to_string())
    ));
    let yaml = serde_yaml::to_string(value).context("serialize yaml")?;
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_repos(dir: &Path) -> PathBuf {
        let path = dir.join("repos.yaml");
        let yaml = r#"
data_dir: ./federation
max_concurrent_indexers: 8
ready_threshold: 0.8
repos:
  - id: existing
    source:
      type: local_clone
      url: https://example.com/existing.git
      ref: main
"#;
        fs::write(&path, yaml).unwrap();
        path
    }

    #[test]
    fn add_appends_new_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        add(&path, "new-repo", "https://example.com/new.git", "main").unwrap();
        let file = FederationConfig::load(&path).unwrap();
        assert!(file.repos.iter().any(|r| r.id == "new-repo"));
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        let err = add(&path, "existing", "https://example.com/x.git", "main").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn remove_drops_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        remove(&path, "existing").unwrap();
        let file = FederationConfig::load(&path).unwrap();
        assert!(file.repos.is_empty());
    }

    #[test]
    fn remove_unknown_id_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        let err = remove(&path, "nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
