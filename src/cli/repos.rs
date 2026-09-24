//! `lain repos` subcommand — manage the `repos.yaml` federation registry.
//!
//! PR 3 lands the `add` / `list` / `remove` operations against the
//! project's `repos.yaml`. The file is rewritten atomically
//! (write-temp-then-rename) so the watcher either sees the old
//! contents or the new contents, never a partial write.

use crate::server::federation::config::{FederationConfig, RepoConfig, SourceConfig};
use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

/// Subcommands for `lain repos`.
#[derive(Debug, Subcommand)]
pub enum ReposAction {
    /// Register a new repo in `repos.yaml` (cloned from `url` at `ref`).
    Add {
        name: String,
        url: String,
        /// Branch or tag to index. Defaults to the remote's default branch
        /// (`main`, `master`, …), asked of the remote at add time.
        #[arg(long)]
        ref_: Option<String>,
    },
    /// List all repos registered in `repos.yaml`.
    List,
    /// Remove a repo from `repos.yaml`.
    Remove { name: String },
}

/// Dispatch a `lain repos <action>` invocation.
pub fn run(action: ReposAction, config_path: &Path) -> Result<()> {
    match action {
        ReposAction::Add { name, url, ref_ } => {
            let ref_ = match ref_ {
                Some(r) => r,
                None => remote_default_branch(&url).unwrap_or_else(|| {
                    eprintln!(
                        "warning: could not ask {url} for its default branch; using 'main' \
                         (pass --ref to choose)"
                    );
                    "main".to_string()
                }),
            };
            add(config_path, &name, &url, &ref_)
        }
        ReposAction::List => list(config_path),
        ReposAction::Remove { name } => remove(config_path, &name),
    }
}

/// `lain repos add <name> <url> [--ref <branch>]`
fn add(config_path: &Path, name: &str, url: &str, ref_: &str) -> Result<()> {
    // F4 — if the file exists, propagate the load error; only fall
    // back to the default for the genuinely-missing case. Pre-fix,
    // `unwrap_or_default()` silently turned unreadable / invalid YAML
    // into an empty config and then the `add` path wrote a
    // brand-new file over the corrupt one — destroying the
    // operator's existing configuration with no error message. The
    // sibling `remove` already does this right (uses `?` via
    // `with_context`).
    // Validate the id now: `lain server` rejects ids like `a/b` or ``,
    // and accepting them here left a config the server would not start on.
    crate::federation::repo_id::RepoId::new(name)
        .map_err(|e| anyhow::anyhow!("invalid repo id '{name}': {e}"))?;
    let mut file = load_or_default(config_path)?;
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
    let yaml = serde_yaml::to_string(&file).context("serialize yaml")?;
    crate::cli::io::write_file_atomic(config_path, yaml.as_bytes())
        .with_context(|| format!("write {}", config_path.display()))?;
    crate::cli::signal::signal_reload(config_path)
        .with_context(|| format!("signal reload after adding '{name}'"))?;
    println!(
        "Added repo '{name}' ({url} @ {ref_}) to {}",
        config_path.display()
    );
    Ok(())
}

/// The branch a remote's `HEAD` points at. A fixed `main` default broke
/// the README's own example: `tokio-rs/bytes` and `tokio-rs/tokio` use
/// `master`, and cloning `--branch main` fails.
fn remote_default_branch(url: &str) -> Option<String> {
    use std::io::Read;
    // Never prompt for credentials, and give up after 20s: this is only a
    // default, and `--ref` always works.
    let mut child = std::process::Command::new("git")
        .args(["ls-remote", "--symref", url, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_symref_head(&out)
}

/// `ref: refs/heads/master\tHEAD` → `master`.
fn parse_symref_head(ls_remote: &str) -> Option<String> {
    ls_remote.lines().find_map(|l| {
        let target = l.strip_prefix("ref: ")?.split('\t').next()?;
        target.strip_prefix("refs/heads/").map(str::to_string)
    })
}

/// `lain repos list`
fn list(config_path: &Path) -> Result<()> {
    // F4 — propagate load errors instead of swallowing them. An
    // empty config is reported as "no repos"; a corrupt config is
    // an error the operator can act on.
    let file = load_or_default(config_path)?;
    if file.repos.is_empty() {
        println!("(no repos registered in {})", config_path.display());
        return Ok(());
    }
    for r in &file.repos {
        println!("{}\t{:?}", r.id, r.source);
    }
    Ok(())
}

/// F4 — load the federation config, propagating errors. If the file
/// doesn't exist, return the default (legitimate "no repos yet"
/// case). If the file exists but is unreadable / invalid YAML,
/// propagate the error so the caller surfaces it.
fn load_or_default(config_path: &Path) -> Result<crate::federation::config::FederationConfig> {
    if !config_path.exists() {
        return Ok(crate::federation::config::FederationConfig::default());
    }
    crate::federation::config::FederationConfig::load(config_path)
        .with_context(|| format!("load {}", config_path.display()))
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
    let yaml = serde_yaml::to_string(&file).context("serialize yaml")?;
    crate::cli::io::write_file_atomic(config_path, yaml.as_bytes())
        .with_context(|| format!("write {}", config_path.display()))?;
    crate::cli::signal::signal_reload(config_path)
        .with_context(|| format!("signal reload after removing '{name}'"))?;
    println!("Removed repo '{name}' from {}", config_path.display());
    // Workspaces that still list it would fail validation at server start.
    let ws_path = config_path.with_file_name("workspaces.yaml");
    if let Ok(ws) = crate::server::federation::workspace::WorkspacesFile::load(&ws_path) {
        for w in ws
            .workspaces
            .iter()
            .filter(|w| w.members.iter().any(|m| m == name))
        {
            eprintln!(
                "warning: workspace '{}' in {} still lists '{name}'; run `lain workspaces remove {} --repo {name}`",
                w.name,
                ws_path.display(),
                w.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn parses_the_remote_default_branch() {
        let out = "ref: refs/heads/master\tHEAD\nabc123\tHEAD\n";
        assert_eq!(parse_symref_head(out).as_deref(), Some("master"));
        assert_eq!(parse_symref_head("abc123\tHEAD\n"), None);
    }

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
    fn add_rejects_ids_the_server_would_reject() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        for bad in ["a/b", "", "x:y"] {
            assert!(
                add(&path, bad, "https://example.com/x.git", "main").is_err(),
                "{bad:?}"
            );
        }
        assert!(!path.exists(), "nothing written");
    }

    /// A config that does not parse is an error, not an empty file to
    /// overwrite.
    #[test]
    fn add_refuses_to_overwrite_an_unparsable_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        let original = "repos:\n  - id: legacy\n    source: {type: svn_checkout, url: svn://x}\n";
        std::fs::write(&path, original).unwrap();
        assert!(add(&path, "bytes", "https://example.com/b.git", "main").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(list(&path).is_err());
    }

    #[test]
    fn remove_unknown_id_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        let err = remove(&path, "nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
