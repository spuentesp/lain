//! Validated reuse of a completed live federation. This is a completion
//! certificate, not another copy of the graph. Missing or invalid certificates
//! fall back to indexing. Only clean worktrees are eligible.
use crate::error::LainError;
use crate::federation::config::FederationConfig;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::FEDERATION_GRAPH_VERSION;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

const RECORD: &str = "startup-complete.json";
const VERSION: u32 = 1;

/// Opt-in wall-clock spans for startup profiling. Nested spans are reported
/// separately; callers must not add parent and child durations together.
pub(crate) struct StartupTiming {
    stage: &'static str,
    started: Option<std::time::Instant>,
}

impl StartupTiming {
    pub(crate) fn start(stage: &'static str) -> Self {
        Self {
            stage,
            started: (std::env::var("LAIN_STARTUP_TIMING").as_deref() == Ok("1"))
                .then(std::time::Instant::now),
        }
    }
}

impl Drop for StartupTiming {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            tracing::info!(
                "federation startup timing: {} {:.3}ms",
                self.stage,
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Completion {
    version: u32,
    inputs: String,
    outputs: String,
}

fn feed_file(hash: &mut blake3::Hasher, path: &Path) -> Result<(), LainError> {
    let mut file = std::fs::File::open(path)?;
    hash.update(&file.metadata()?.len().to_le_bytes());
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(())
}

pub(crate) fn inputs(
    fed: &FederatedIndex,
    config: &FederationConfig,
) -> Result<Option<String>, LainError> {
    if fed.list_repos().is_empty() || !fed.load_errors().is_empty() {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(crate::server::build_info::INDEX_BUILD_ID.as_bytes());
    hash.update(&FEDERATION_GRAPH_VERSION.to_le_bytes());
    hash.update(&serde_json::to_vec(config)?);
    hash.update(&serde_json::to_vec(&config.effective_params())?);
    for name in [
        "LAIN_DISABLE_LSP",
        "LAIN_TEST_NO_LSP",
        "LAIN_LSP_LEAN",
        "LAIN_LSP_PREWARM",
    ] {
        hash.update(name.as_bytes());
        hash.update(std::env::var(name).unwrap_or_default().as_bytes());
        hash.update(&[0]);
    }
    for (id, _) in fed.list_repos() {
        let repo = fed.get_repo(&id).expect("registered repository");
        let Some(source) = source_key(repo.local_path(), repo.git())? else {
            return Ok(None);
        };
        let (commit, _) = repo.git().try_get_latest_commit_info()?;
        hash.update(id.as_str().as_bytes());
        hash.update(commit.as_bytes());
        hash.update(
            repo.local_path()
                .canonicalize()?
                .as_os_str()
                .as_encoded_bytes(),
        );
        hash.update(source.as_bytes());
        hash.update(analysis_environment(repo.local_path(), repo.git())?.as_bytes());
    }
    Ok(Some(hash.finalize().to_hex().to_string()))
}

/// Context-independent source identity. Relative paths let identical checkouts
/// share extracted references without sharing namespace-bound graph nodes.
pub(crate) fn source_key(
    root: &Path,
    git: &crate::git::AnyGitSensor,
) -> Result<Option<String>, LainError> {
    if !git.get_uncommitted_changes()?.is_empty() {
        return Ok(None);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(crate::server::build_info::INDEX_BUILD_ID.as_bytes());
    // The git sensor canonicalizes its workspace, so the paths it hands back
    // are canonical while `root` is whatever the config named. Stripping the
    // prefix from a non-canonical `root` fails wherever the two spell the
    // same directory differently — macOS `/var` vs `/private/var`, a Windows
    // short name, a symlinked checkout — and every tracked file then reads as
    // "outside repository". Canonicalize the root so both sides agree.
    let canonical_root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut files = git.try_get_all_tracked_files()?;
    files.sort();
    for file in files {
        let abs = if file.is_absolute() {
            file
        } else {
            canonical_root.join(file)
        };
        let rel = abs
            .strip_prefix(&canonical_root)
            .map_err(|_| LainError::Other("tracked path outside repository".into()))?;
        hash.update(&(rel.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
        hash.update(rel.as_os_str().as_encoded_bytes());
        feed_file(&mut hash, &abs)?;
    }
    // Include ignored operator overrides too. Their paths are normalized so
    // equivalent overrides in another checkout can use the same extraction.
    for path in [root.join(".lain/patterns"), root.join(".lain/tuning.toml")] {
        feed_relative_overrides(&mut hash, root, &path)?;
    }
    // Dotenv and deployment inputs can intentionally be ignored by Git while
    // still affecting emitted graph facts and environment-based bindings.
    for path in crate::server::sensors::env_sensor::analysis_input_paths(root) {
        feed_relative_overrides(&mut hash, root, &path)?;
    }
    Ok(Some(hash.finalize().to_hex().to_string()))
}

fn feed_relative_overrides(
    hash: &mut blake3::Hasher,
    root: &Path,
    path: &Path,
) -> Result<(), LainError> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() && !path.is_symlink() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            feed_relative_overrides(hash, root, &entry.path())?;
        }
    } else {
        hash.update(
            path.strip_prefix(root)
                .unwrap_or(path)
                .as_os_str()
                .as_encoded_bytes(),
        );
        feed_file(hash, path)?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ReferenceEntry {
    key: String,
    checksum: String,
    refs: Vec<crate::server::ingest::scan::StaticFileRef>,
}

// Small, immutable extraction artifacts only; parsed trees are never retained.
const REFERENCE_BUDGET: u64 = 64 * 1024 * 1024;

pub(crate) fn read_references(
    dir: &Path,
    key: &str,
) -> Option<Vec<crate::server::ingest::scan::StaticFileRef>> {
    let path = dir.join(format!("{key}.json"));
    if std::fs::metadata(&path).ok()?.len() > REFERENCE_BUDGET {
        return None;
    }
    let entry: ReferenceEntry = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if entry.key != key {
        return None;
    }
    let bytes = serde_json::to_vec(&entry.refs).ok()?;
    if blake3::hash(&bytes).to_hex().as_str() != entry.checksum {
        return None;
    }
    if let Ok(file) = std::fs::File::open(path) {
        let _ =
            file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()));
    }
    Some(entry.refs)
}

pub(crate) fn write_references(
    dir: &Path,
    key: &str,
    refs: Vec<crate::server::ingest::scan::StaticFileRef>,
) -> Result<(), LainError> {
    let checksum = blake3::hash(&serde_json::to_vec(&refs)?)
        .to_hex()
        .to_string();
    let bytes = serde_json::to_vec(&ReferenceEntry {
        key: key.into(),
        checksum,
        refs,
    })?;
    if bytes.len() as u64 > REFERENCE_BUDGET {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{key}.{}.{seq}.tmp", std::process::id()));
    let result = (|| {
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, dir.join(format!("{key}.json")))?;
        Ok::<_, LainError>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .map(|m| (m.modified().ok(), e.path(), m.len()))
        })
        .collect();
    entries.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    for (_, path, size) in entries {
        if total <= REFERENCE_BUDGET {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= size;
        }
    }
    Ok(())
}

/// Semantic context is separate from the shareable syntax extraction key.
/// Installing or replacing a language server must not keep a graph produced
/// when that server was absent, even when HEAD and Lain itself are unchanged.
pub(crate) fn analysis_environment(
    root: &Path,
    git: &crate::git::AnyGitSensor,
) -> Result<String, LainError> {
    let mut hash = blake3::Hasher::new();
    for name in [
        "LAIN_DISABLE_LSP",
        "LAIN_TEST_NO_LSP",
        "LAIN_LSP_LEAN",
        "LAIN_LSP_PREWARM",
        "PATH",
        "RUSTUP_TOOLCHAIN",
        "VIRTUAL_ENV",
        "CONDA_PREFIX",
        "JAVA_HOME",
        "NODE_PATH",
    ] {
        hash.update(name.as_bytes());
        hash.update(
            std::env::var_os(name)
                .unwrap_or_default()
                .as_encoded_bytes(),
        );
        hash.update(&[0]);
    }
    let disabled = crate::server::ingest::background::env_disables(
        std::env::var("LAIN_DISABLE_LSP").ok().as_deref(),
    ) || std::env::var("LAIN_TEST_NO_LSP").is_ok_and(|v| !v.is_empty());
    if !disabled {
        let binaries: std::collections::BTreeSet<_> = git
            .try_get_all_tracked_files()?
            .iter()
            .filter_map(|p| p.extension().and_then(|e| e.to_str()))
            .filter_map(crate::server::lsp::language_server_for)
            .map(|server| server.binary)
            .collect();
        for binary in binaries {
            hash.update(binary.as_bytes());
            if let Ok(path) = which::which(binary) {
                let path = path.canonicalize()?;
                hash.update(path.as_os_str().as_encoded_bytes());
                feed_file(&mut hash, &path)?;
                // Node launchers can stay byte-identical when their package changes.
                if let Some(package) = path
                    .ancestors()
                    .skip(1)
                    .take(4)
                    .map(|p| p.join("package.json"))
                    .find(|p| p.is_file())
                {
                    feed_file(&mut hash, &package)?;
                }
                if binary == "rust-analyzer" {
                    let rustup = std::env::var_os("RUSTUP_HOME")
                        .map(PathBuf::from)
                        .or_else(|| {
                            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".rustup"))
                        });
                    if let Some(rustup) = rustup {
                        let settings = rustup.join("settings.toml");
                        if settings.is_file() {
                            feed_file(&mut hash, &settings)?;
                        }
                        // rust-analyzer on PATH may be a rustup proxy. Observe the
                        // installed components as well as the proxy executable.
                        if let Ok(entries) = std::fs::read_dir(rustup.join("toolchains")) {
                            let mut paths: Vec<_> = entries
                                .filter_map(Result::ok)
                                .map(|e| {
                                    e.path().join(format!(
                                        "bin/rust-analyzer{}",
                                        std::env::consts::EXE_SUFFIX
                                    ))
                                })
                                .filter(|p| p.is_file())
                                .collect();
                            paths.sort();
                            for path in paths {
                                hash.update(path.as_os_str().as_encoded_bytes());
                                let metadata = path.metadata()?;
                                hash.update(&metadata.len().to_le_bytes());
                                hash.update(
                                    &metadata
                                        .modified()?
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_nanos()
                                        .to_le_bytes(),
                                );
                            }
                        }
                    }
                }
            } else {
                hash.update(b"missing");
            }
        }
    }
    // Relative external-dependency resolution and toolchain overrides can
    // differ between identical source trees in different locations.
    hash.update(root.canonicalize()?.as_os_str().as_encoded_bytes());
    Ok(hash.finalize().to_hex().to_string())
}

pub(crate) fn analysis_key(
    root: &Path,
    git: &crate::git::AnyGitSensor,
    source: &str,
) -> Result<String, LainError> {
    let environment = analysis_environment(root, git)?;
    Ok(blake3::hash(format!("{source}:{environment}").as_bytes())
        .to_hex()
        .to_string())
}

fn outputs(fed: &FederatedIndex, data_dir: &Path) -> Result<String, LainError> {
    let mut hash = blake3::Hasher::new();
    feed_file(&mut hash, &data_dir.join("federated_graph.bin"))?;
    for (id, _) in fed.list_repos() {
        let dir = data_dir.join("repos").join(id.as_str());
        // Include WALs and build stamps. A change after publication invalidates
        // the certificate even when graph.bin itself has not been checkpointed.
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<_, _>>()?;
        paths.sort();
        for path in paths.into_iter().filter(|p| p.is_file()) {
            hash.update(path.file_name().unwrap().as_encoded_bytes());
            feed_file(&mut hash, &path)?;
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub(crate) fn restore(fed: &FederatedIndex, config: &FederationConfig) -> Result<bool, LainError> {
    let result = restore_checked(fed, config);
    if !matches!(result, Ok(true)) {
        let path = config.data_dir.join(RECORD);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    result
}

fn restore_checked(fed: &FederatedIndex, config: &FederationConfig) -> Result<bool, LainError> {
    let input_timing = StartupTiming::start("cache_input_validation");
    let key = inputs(fed, config)?;
    drop(input_timing);
    fed.set_startup_inputs(key.clone());
    let path = config.data_dir.join(RECORD);
    let record = std::fs::metadata(&path)
        .ok()
        .filter(|m| m.len() <= 4096)
        .and_then(|_| std::fs::read(&path).ok())
        .and_then(|bytes| serde_json::from_slice::<Completion>(&bytes).ok());
    let Some(record) = record else {
        return Ok(false);
    };
    if record.version != VERSION || Some(record.inputs) != key {
        return Ok(false);
    }
    for (id, _) in fed.list_repos() {
        let repo = fed.get_repo(&id).unwrap();
        let commit = repo.db().get_last_commit()?;
        if commit.is_none()
            || repo.db().calls_published_for() != commit
            || !repo
                .db()
                .was_written_by_build(crate::server::build_info::INDEX_BUILD_ID)
        {
            return Ok(false);
        }
    }
    let output_timing = StartupTiming::start("cache_output_validation");
    let output_key = outputs(fed, &config.data_dir)?;
    drop(output_timing);
    if record.outputs != output_key {
        return Ok(false);
    }
    fed.restore_projection_metadata()?;
    fed.set_startup_restored(true);
    tracing::info!("federation startup cache hit: validated completed graph");
    Ok(true)
}

pub(crate) fn publish(
    fed: &FederatedIndex,
    config: &FederationConfig,
    allow_held: bool,
) -> Result<bool, LainError> {
    if fed.list_repos().iter().any(|(id, h)| {
        *h != crate::federation::health::RepoHealth::Ready
            && !(allow_held
                && *h == crate::federation::health::RepoHealth::Indexing
                && fed
                    .get_repo(id)
                    .is_some_and(|repo| repo.indexed_signal_was_fired()))
    }) {
        return Ok(false);
    }
    let Some(key) = inputs(fed, config)? else {
        return Ok(false);
    };
    if fed.startup_inputs().as_ref() != Some(&key) {
        return Ok(false);
    }
    for (id, _) in fed.list_repos() {
        let repo = fed.get_repo(&id).unwrap();
        if !repo.analysis_checkpoint_current()? {
            return Ok(false);
        }
        repo.db().save_to_disk_sync()?;
    }
    let record = Completion {
        version: VERSION,
        inputs: key,
        outputs: outputs(fed, &config.data_dir)?,
    };
    let tmp = config
        .data_dir
        .join(format!("{RECORD}.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(&record)?;
    let result = (|| {
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, config.data_dir.join(RECORD))?;
        Ok::<_, LainError>(true)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_cache_rejects_modified_payload_and_large_files() {
        let dir = tempfile::tempdir().unwrap();
        let refs = vec![crate::server::ingest::scan::StaticFileRef {
            file_path: "src/main.py".into(),
            source_line: 1,
            target_name: "callee".into(),
            edge_type: crate::schema::EdgeType::Calls,
            foreign_receiver: false,
            self_receiver: false,
            qualifier: None,
        }];
        write_references(dir.path(), "fixture", refs).unwrap();
        assert_eq!(read_references(dir.path(), "fixture").unwrap().len(), 1);
        let path = dir.path().join("fixture.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["refs"][0]["target_name"] = "changed".into();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_references(dir.path(), "fixture").is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(REFERENCE_BUDGET + 1)
            .unwrap();
        assert!(read_references(dir.path(), "fixture").is_none());
        write_references(dir.path(), "replacement", Vec::new()).unwrap();
        let bytes: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(bytes <= REFERENCE_BUDGET);
    }

    /// The git sensor canonicalizes its workspace; `root` comes from the
    /// config and is not canonical. When the two spell the same directory
    /// differently — macOS `/var` vs `/private/var`, a symlinked checkout,
    /// a Windows short name — every tracked file used to read as "tracked
    /// path outside repository", so the startup cache could never be
    /// written. This reproduces that through a symlinked root.
    #[test]
    fn source_key_accepts_a_root_that_reaches_the_repo_through_a_symlink() {
        let real = tempfile::tempdir().unwrap();
        let repo = real.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();

        let git_cmd = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git_cmd(&["init", "-q"]);
        git_cmd(&["config", "user.email", "t@example.com"]);
        git_cmd(&["config", "user.name", "t"]);
        git_cmd(&["add", "main.rs"]);
        git_cmd(&["commit", "-q", "-m", "init"]);

        // A symlinked parent, the way macOS's `/var` reaches `/private/var`.
        let link_parent = real.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(real.path(), &link_parent).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(real.path(), &link_parent).unwrap();

        let git =
            crate::git::AnyGitSensor::new(&repo, crate::git::GitSensorMode::InProcess).unwrap();
        let via_link = link_parent.join("repo");

        // The sensor reports canonical paths; `root` is the symlinked one.
        // The property `source_key` exists for is that the key is the same
        // either way — that is what lets identical checkouts share a cache
        // entry. Asserting only `is_some` would accept a key that is wrong
        // or empty, which the prefix-stripping bug produced silently.
        let via_link_key = source_key(&via_link, &git).unwrap();
        let canonical_key = source_key(&repo, &git).unwrap();
        assert!(
            via_link_key.is_some(),
            "a clean repo must produce a source key"
        );
        assert_eq!(
            via_link_key, canonical_key,
            "the source key must not depend on whether the root is reached through a symlink"
        );
    }
}
