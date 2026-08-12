//! Project registry — single-user multi-project support.
//!
//! Each project is a separate git repo with its own `.lain/graph.bin`.
//! The registry keeps a list of known projects so the user can switch
//! between them without typing `--workspace` every time.
//!
//! Files (XDG-style):
//! - `~/.config/lain/projects.toml` — registry of known projects
//! - `~/.config/lain/current` — single line, the active project name
//!
//! Workspace resolution priority (when `--workspace` is not given):
//! 1. `--workspace` flag (always wins)
//! 2. `current` file (set by `lain use <name>`)
//! 3. `.lain/` in current working directory
//! 4. Error: "no active project"

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One project in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// Display name. Must be unique within the registry.
    pub name: String,
    /// Absolute path to the repo root.
    pub path: PathBuf,
    /// Last time this project was used (RFC3339). Optional for back-compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

/// On-disk registry shape. Always `version = 1` so we can migrate later.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    projects: Vec<Project>,
}

fn default_version() -> u32 { 1 }

impl RegistryFile {
    fn to_map(&self) -> BTreeMap<String, Project> {
        self.projects.iter().map(|p| (p.name.clone(), p.clone())).collect()
    }
    fn from_map(m: BTreeMap<String, Project>) -> Self {
        let mut projects: Vec<Project> = m.into_values().collect();
        // Sort by name for stable display
        projects.sort_by(|a, b| a.name.cmp(&b.name));
        Self { version: 1, projects }
    }
}

/// Return the path to the user config dir (`~/.config/lain`).
pub fn config_dir() -> PathBuf {
    // XDG: $XDG_CONFIG_HOME or ~/.config. lain doesn't have other XDG-aware
    // code yet, so we follow the standard directly.
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("lain");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lain")
}

fn projects_file() -> PathBuf { config_dir().join("projects.toml") }
fn current_file() -> PathBuf { config_dir().join("current") }

fn read_registry() -> RegistryFile {
    match fs::read_to_string(projects_file()) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => RegistryFile::default(),
    }
}

fn write_registry(reg: &RegistryFile) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let s = toml::to_string_pretty(reg).map_err(std::io::Error::other)?;
    fs::write(projects_file(), s)
}

/// Errors from the registry API. Kept simple — the CLI maps these to
/// user-friendly messages.
#[derive(Debug)]
pub enum RegistryError {
    AlreadyExists(String),
    NotFound(String),
    /// Tried to register a project at a path that's already registered
    /// under a different name. Includes the existing name so the CLI
    /// can tell the user what to do.
    PathAlreadyRegistered { path: String, existing_name: String },
    Io(std::io::Error),
    Parse(String),
    NotAbsolutePath(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyExists(n) => write!(f, "project '{}' already exists in registry", n),
            RegistryError::NotFound(n) => write!(f, "project '{}' not found in registry", n),
            RegistryError::PathAlreadyRegistered { path, existing_name } => write!(
                f,
                "path '{}' is already registered as '{}'; use that name or run `lain projects forget {}` first",
                path, existing_name, existing_name
            ),
            RegistryError::Io(e) => write!(f, "I/O error: {}", e),
            RegistryError::Parse(s) => write!(f, "parse error: {}", s),
            RegistryError::NotAbsolutePath(s) => write!(f, "path is not absolute: {}", s),
        }
    }
}

impl std::error::Error for RegistryError {}
impl From<std::io::Error> for RegistryError { fn from(e: std::io::Error) -> Self { Self::Io(e) } }

/// High-level API. All methods are best-effort: a corrupt registry file
/// becomes an empty registry rather than failing the whole command.
pub struct Projects;

impl Projects {
    /// Add or replace a project by name. Refuses if the path is already
    /// registered under a different name (prevents the "monitor" vs
    /// "monitor_dm_system" duplicate the user hit during stress testing).
    /// If the same name already exists with the same path, just updates
    /// last_used.
    pub fn add(name: &str, path: &Path) -> Result<(), RegistryError> {
        let canon = std::fs::canonicalize(path)
            .map_err(|e| RegistryError::Io(e))?;
        if !canon.is_absolute() {
            return Err(RegistryError::NotAbsolutePath(path.display().to_string()));
        }
        let reg = read_registry();
        let mut map = reg.to_map();

        // Reject if a different name already points at the same path.
        // Same name + same path is a no-op (just touch last_used).
        if let Some((existing_name, _)) = map.iter().find(|(_, p)| p.path == canon) {
            if existing_name != name {
                return Err(RegistryError::PathAlreadyRegistered {
                    path: canon.display().to_string(),
                    existing_name: existing_name.clone(),
                });
            }
        }

        let now = chrono_like_now();
        let entry = Project { name: name.to_string(), path: canon, last_used: Some(now) };
        map.insert(name.to_string(), entry);
        let reg = RegistryFile::from_map(map);
        write_registry(&reg)?;
        Ok(())
    }

    /// Register a project, or update its last_used if the path is
    /// already registered. Used by `lain init`'s auto-register so
    /// re-running init doesn't create duplicates. Returns true if a
    /// new entry was created, false if an existing one was updated.
    pub fn register_or_touch(path: &Path) -> Result<bool, RegistryError> {
        let canon = std::fs::canonicalize(path)
            .map_err(|e| RegistryError::Io(e))?;
        let reg = read_registry();
        let mut map = reg.to_map();

        // Path already registered — just touch it, don't create a new
        // entry. This is the core of the "no double register" behavior.
        if let Some((_, existing)) = map.iter_mut().find(|(_, p)| p.path == canon) {
            existing.last_used = Some(chrono_like_now());
            let reg = RegistryFile::from_map(map);
            write_registry(&reg)?;
            return Ok(false);
        }

        // New path. Use the directory basename as the default name.
        let name = canon
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();
        let now = chrono_like_now();
        map.insert(
            name.clone(),
            Project { name, path: canon, last_used: Some(now) },
        );
        let reg = RegistryFile::from_map(map);
        write_registry(&reg)?;
        Ok(true)
    }

    /// Remove a project by name. Errors if not found.
    pub fn forget(name: &str) -> Result<(), RegistryError> {
        let reg = read_registry();
        let mut map = reg.to_map();
        if map.remove(name).is_none() {
            return Err(RegistryError::NotFound(name.to_string()));
        }
        write_registry(&RegistryFile::from_map(map))?;
        // If the forgotten project was the active one, clear current
        let cur = fs::read_to_string(current_file()).ok();
        if cur.as_deref().map(|s| s.trim()) == Some(name) {
            let _ = fs::remove_file(current_file());
        }
        Ok(())
    }

    /// Touch a project's last_used timestamp. Silent no-op if missing.
    pub fn touch(name: &str) {
        let reg = read_registry();
        let mut map = reg.to_map();
        if let Some(p) = map.get_mut(name) {
            p.last_used = Some(chrono_like_now());
            let _ = write_registry(&RegistryFile::from_map(map));
        }
    }

    /// List registered projects sorted by name.
    pub fn list() -> Vec<Project> {
        let reg = read_registry();
        let map = reg.to_map();
        let mut out: Vec<Project> = map.into_values().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Mark a project as active (writes the `current` file).
    pub fn set_active(name: &str) -> Result<(), RegistryError> {
        let reg = read_registry();
        let map = reg.to_map();
        if !map.contains_key(name) {
            return Err(RegistryError::NotFound(name.to_string()));
        }
        let dir = config_dir();
        fs::create_dir_all(&dir)?;
        fs::write(current_file(), name)?;
        Projects::touch(name);
        Ok(())
    }
    // (rest of the impl follows)

    /// Clear the active project.
    pub fn clear_active() -> std::io::Result<()> {
        fs::remove_file(current_file())
    }

    /// Get the active project name (the contents of `current`), or None.
    pub fn active_name() -> Option<String> {
        fs::read_to_string(current_file())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Resolve workspace path. Priority:
    /// 1. Explicit --workspace (if not the clap default ".")
    /// 2. `lain use <name>`-set active project from registry
    /// 3. `.lain/` in current working directory
    /// 4. Error
    pub fn resolve_workspace(explicit: &Path) -> Result<PathBuf, String> {
        // 1. Explicit non-default workspace wins
        if explicit != Path::new(".") {
            return Ok(explicit.to_path_buf());
        }
        // 2. Active project from registry
        if let Some(name) = Projects::active_name() {
            for p in Projects::list() {
                if p.name == name {
                    return Ok(p.path);
                }
            }
        }
        // 3. .lain in cwd
        let cwd_lain = std::env::current_dir()
            .map(|d| d.join(".lain"))
            .unwrap_or_else(|_| PathBuf::from(".lain"));
        if cwd_lain.exists() {
            // Walk up to the repo root from the .lain dir
            if let Some(repo_root) = cwd_lain.parent() {
                return Ok(repo_root.to_path_buf());
            }
        }
        Err("no active project; run `lain projects add <name> <path>` then `lain use <name>`, pass --workspace, or run inside a git repo to auto-discover it".to_string())
    }

    /// Resolve the workspace when the user passed `--workspace auto`.
    ///
    /// Walks up from the current working directory to find the nearest
    /// enclosing git repository and returns its workdir. Returns a clear
    /// user-facing error when no repository is found.
    pub fn resolve_auto_workspace() -> Result<PathBuf, crate::error::LainError> {
        let repo = git2::Repository::discover(".").map_err(|e| {
            crate::error::LainError::Workspace(format!(
                "--workspace auto requires a git repository, but none was found from {}: {e}. \
                 Pass an explicit --workspace <path> or run inside a git repo.",
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<unknown cwd>".into())
            ))
        })?;
        let path = repo.workdir().ok_or_else(|| {
            crate::error::LainError::Workspace(
                "--workspace auto: bare repositories are not supported. \
                 Pass an explicit --workspace <path>."
                    .to_string(),
            )
        })?;
        Ok(path.to_path_buf())
    }
}

/// Tiny inline replacement for chrono so we don't pull in a date dep.
/// Format: RFC3339-ish `YYYY-MM-DDTHH:MM:SSZ`. Good enough for sorting.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let mut year = 1970;
    let mut month = 1;
    let mut day = days as i64;
    // Days from 1970-01-01
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if day < year_days { break; }
        day -= year_days;
        year += 1;
    }
    let mdays = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for &dm in &mdays {
        if day < dm { break; }
        day -= dm;
        month += 1;
    }
    day += 1;
    let tod = secs % 86400;
    let h = tod / 3600;
    let m = (tod / 60) % 60;
    let s = tod % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, h, m, s)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// Test-only mutex shared by all `mod tests` and `mod active_workspace_tests`
// blocks in this file. cargo test runs tests in parallel; XDG_CONFIG_HOME
// and cwd are process-global state, so parallel tests would stomp on each
// other without serialization. Defined at file scope so both test mods
// (which are sibling test sub-modules) can see it.
#[cfg(test)]
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    struct DirGuard(PathBuf);
    impl DirGuard {
        fn new(p: PathBuf) -> Self { Self(p) }
    }
    #[allow(dead_code)]
    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    /// Save and restore the `XDG_CONFIG_HOME` env var so test mutations
    /// don't leak into other tests sharing the same process.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join(format!("lain-state-{}-{}", std::process::id(), tag))
            .join(format!("{:?}", std::thread::current().id()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn add_then_list() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("add");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p1 = dir.join("repo1");
        let p2 = dir.join("repo2");
        fs::create_dir_all(&p1).unwrap();
        fs::create_dir_all(&p2).unwrap();

        Projects::add("alpha", &p1).unwrap();
        Projects::add("beta", &p2).unwrap();

        let list = Projects::list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|x| x.name == "alpha"));
        assert!(list.iter().any(|x| x.name == "beta"));
    }

    #[test]
    fn resolve_workspace_priority() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("resolve");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p = dir.join("repo");
        fs::create_dir_all(&p).unwrap();

        // 1. explicit non-default wins
        assert_eq!(
            Projects::resolve_workspace(Path::new("/anywhere")).unwrap(),
            PathBuf::from("/anywhere"),
        );

        // 2. active project
        Projects::add("alpha", &p).unwrap();
        Projects::set_active("alpha").unwrap();
        let resolved = Projects::resolve_workspace(Path::new(".")).unwrap();
        assert_eq!(resolved, p.canonicalize().unwrap());

        // 3. cwd .lain fallback is tested separately below because the
        // main implementation reads std::env::current_dir() which is a
        // process-global we can't safely mutate under parallel test
        // execution (would race other tests that may also read cwd).
    }

    /// Verify the cwd-with-.lain fallback. This test must run serially
    /// (TEST_LOCK) because it uses set_current_dir, which is a process-
    /// wide mutation. Other tests reading cwd concurrently would see
    /// the changed value and either fail or behave non-deterministically.
    #[test]
    fn resolve_workspace_cwd_fallback() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("resolve_cwd");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        fs::create_dir_all(dir.join(".lain")).unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let result = Projects::resolve_workspace(Path::new(".")).unwrap();
        std::env::set_current_dir(&original_cwd).unwrap();
        assert_eq!(result.canonicalize().unwrap(), dir.canonicalize().unwrap());
    }

    #[test]
    fn forget_clears_active() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("forget");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p = dir.join("repo");
        fs::create_dir_all(&p).unwrap();
        Projects::add("alpha", &p).unwrap();
        Projects::set_active("alpha").unwrap();
        assert_eq!(Projects::active_name().as_deref(), Some("alpha"));
        Projects::forget("alpha").unwrap();
        assert!(Projects::active_name().is_none());
    }

    #[test]
    fn add_rejects_path_already_registered_under_different_name() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("pathdup");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p = dir.join("repo");
        fs::create_dir_all(&p).unwrap();

        // First add under one name works.
        Projects::add("alpha", &p).unwrap();
        // Same path, different name — must refuse, not silently overwrite.
        let err = Projects::add("beta", &p).unwrap_err();
        match err {
            RegistryError::PathAlreadyRegistered { existing_name, .. } => {
                assert_eq!(existing_name, "alpha");
            }
            other => panic!("expected PathAlreadyRegistered, got {:?}", other),
        }
        // And list still has only one entry.
        assert_eq!(Projects::list().len(), 1);
        // Re-adding under the same name is a no-op (just touches last_used).
        Projects::add("alpha", &p).unwrap();
        assert_eq!(Projects::list().len(), 1);
    }

    #[test]
    fn register_or_touch_no_duplicate() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("touch");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p = dir.join("repo");
        fs::create_dir_all(&p).unwrap();

        // First call creates with directory basename.
        let created = Projects::register_or_touch(&p).unwrap();
        assert!(created);
        let list = Projects::list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "repo");

        // Second call doesn't create a duplicate.
        let created = Projects::register_or_touch(&p).unwrap();
        assert!(!created);
        assert_eq!(Projects::list().len(), 1);
        // But last_used is updated.
    }

    #[test]
    fn resolve_auto_workspace_finds_repo_root() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-root");
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", &dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git2::Repository::init(&repo).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let expected = std::fs::canonicalize(&repo).unwrap();

        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&repo).unwrap();
        let resolved = Projects::resolve_auto_workspace().expect("resolve");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_auto_workspace_walks_up_to_repo_root() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-subdir");
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", &dir);
        let repo = dir.join("repo");
        let sub = repo.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        git2::Repository::init(&repo).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let expected = std::fs::canonicalize(&repo).unwrap();

        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&sub).unwrap();
        let resolved = Projects::resolve_auto_workspace().expect("resolve");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_auto_workspace_errors_outside_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Use a freshly-created tempdir that is itself NOT inside any git
        // repo, so the "no-repo" subdir is guaranteed to live outside of
        // a discovered repository. This keeps the test deterministic
        // regardless of where the build sandbox places /tmp.
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let dir = tmpdir.path().to_path_buf();
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", &dir);
        let outside = dir.join("no-repo");
        std::fs::create_dir_all(&outside).unwrap();

        // Sanity: confirm the chosen directory has no enclosing repo, so
        // the assertion below is meaningful.
        assert!(
            git2::Repository::discover(&outside).is_err(),
            "test setup: expected no enclosing git repo at {}",
            outside.display()
        );

        let cwd = std::env::current_dir().unwrap();
        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&outside).unwrap();
        let err = Projects::resolve_auto_workspace().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--workspace auto"),
            "error should mention --workspace auto, got: {msg}"
        );
    }

    #[test]
    fn resolve_auto_workspace_rejects_bare_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tmp("auto-bare");
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", &dir);
        let bare = dir.join("bare.git");
        std::fs::create_dir_all(&bare).unwrap();
        git2::Repository::init_bare(&bare).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let _restore = DirGuard::new(cwd);
        std::env::set_current_dir(&bare).unwrap();
        let err = Projects::resolve_auto_workspace().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("bare") || msg.contains("--workspace auto"),
            "error should mention bare repo or --workspace auto, got: {msg}"
        );
    }
}

// =============================================================================
// Federation workspace pointer
// =============================================================================
//
// A federation workspace is a named subset of repos declared in
// `workspaces.yaml`. The active workspace pointer lives at
// `~/.config/lain/active_workspace` (separate from the single-workspace
// `~/.config/lain/current` file because the two registries are
// independent — a user can be working on a single-workspace project AND
// have an active federation workspace at the same time).
//
// File format: two whitespace-separated tokens on one or more lines:
//   <workspace-name>  <path-to-workspaces.yaml>
// Example:
//   backend-team  /home/user/code/lain/workspaces.yaml
//
// The path is stored so the server can resolve the active workspace's
// definition without needing `--workspace` to be passed explicitly.

/// Pointer to the active federation workspace: name + path to the
/// `workspaces.yaml` it was sourced from. Lives at
/// `~/.config/lain/active_workspace`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWorkspace {
    pub name: String,
    pub source_path: PathBuf,
}

fn active_workspace_file() -> PathBuf {
    config_dir().join("active_workspace")
}

impl ActiveWorkspace {
    /// Load the active workspace pointer from disk. Returns `Ok(None)` if
    /// the file does not exist (no workspace ever set). Returns `Err` if
    /// the file exists but is malformed.
    pub fn load() -> Result<Option<Self>, crate::error::LainError> {
        let path = active_workspace_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::LainError::Io(e.to_string())),
        };
        let mut parts = text.split_whitespace();
        let name = parts.next()
            .ok_or_else(|| crate::error::LainError::Config(format!(
                "active_workspace file empty: {}", path.display()
            )))?
            .to_string();
        let source_path = PathBuf::from(parts.next().ok_or_else(|| crate::error::LainError::Config(format!(
            "active_workspace missing source path: {}", path.display()
        )))?);
        if name.is_empty() {
            return Err(crate::error::LainError::Config(format!(
                "active_workspace name is empty: {}", path.display()
            )));
        }
        Ok(Some(Self { name, source_path }))
    }

    /// Save this pointer to disk atomically (write to .tmp, rename).
    pub fn save(&self) -> Result<(), crate::error::LainError> {
        if self.name.is_empty() {
            return Err(crate::error::LainError::Config("active workspace name cannot be empty".into()));
        }
        let dir = config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
        let path = active_workspace_file();
        let text = format!("{}\n{}\n", self.name, self.source_path.display());
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
        Ok(())
    }

    /// Remove the active workspace pointer file. No-op if it doesn't
    /// exist.
    pub fn clear() -> Result<(), crate::error::LainError> {
        let path = active_workspace_file();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::error::LainError::Io(e.to_string())),
        }
    }
}

/// Look up a workspace by name in a `WorkspacesFile`. Returns
/// `LainError::Config` with a clear message if the name is not present.
pub fn resolve_active_workspace<'a>(
    spec: &'a crate::federation::workspace::WorkspacesFile,
    name: &str,
) -> Result<&'a crate::federation::workspace::WorkspaceSpec, crate::error::LainError> {
    spec.workspaces.iter()
        .find(|w| w.name == name)
        .ok_or_else(|| crate::error::LainError::Config(format!(
            "workspace '{name}' not found in workspaces.yaml"
        )))
}

#[cfg(test)]
mod active_workspace_tests {
    use super::*;

    /// Run a closure with XDG_CONFIG_HOME pointed at a tempdir, restoring
    /// the original env var on drop. Used so tests don't touch the user's
    /// real `~/.config/lain/active_workspace`.
    struct XdgGuard { prev: Option<String> }
    impl XdgGuard {
        fn new(dir: &Path) -> Self {
            let prev = std::env::var("XDG_CONFIG_HOME").ok();
            std::env::set_var("XDG_CONFIG_HOME", dir);
            Self { prev }
        }
    }
    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let r = ActiveWorkspace::load().expect("load should not error on missing file");
        assert!(r.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let aw = ActiveWorkspace {
            name: "backend-team".into(),
            source_path: PathBuf::from("/srv/workspaces.yaml"),
        };
        aw.save().expect("save should succeed");
        let loaded = ActiveWorkspace::load().unwrap().expect("load should return Some");
        assert_eq!(loaded, aw);
    }

    #[test]
    fn clear_removes_the_file() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        let aw = ActiveWorkspace {
            name: "team".into(),
            source_path: PathBuf::from("/srv/ws.yaml"),
        };
        aw.save().unwrap();
        assert!(active_workspace_file().exists());
        ActiveWorkspace::clear().unwrap();
        assert!(!active_workspace_file().exists());
        // Idempotent: clear on missing file is OK.
        ActiveWorkspace::clear().unwrap();
    }

    #[test]
    fn load_errors_on_malformed_file() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = XdgGuard::new(tmp.path());
        std::fs::create_dir_all(active_workspace_file().parent().unwrap()).unwrap();
        // Only the name, no path — malformed.
        std::fs::write(active_workspace_file(), "just-a-name\n").unwrap();
        let r = ActiveWorkspace::load();
        assert!(r.is_err());
    }

    #[test]
    fn resolve_active_workspace_finds_known() {
        let f = crate::federation::workspace::WorkspacesFile {
            default: None,
            workspaces: vec![
                crate::federation::workspace::WorkspaceSpec {
                    name: "alpha".into(),
                    description: None,
                    source: None,
                    members: vec!["a".into(), "b".into()],
                },
                crate::federation::workspace::WorkspaceSpec {
                    name: "beta".into(),
                    description: None,
                    source: None,
                    members: vec!["c".into(), "d".into()],
                },
            ],
        };
        let ws = resolve_active_workspace(&f, "beta").unwrap();
        assert_eq!(ws.name, "beta");
    }

    #[test]
    fn resolve_active_workspace_errors_on_unknown() {
        let f = crate::federation::workspace::WorkspacesFile::default();
        let r = resolve_active_workspace(&f, "ghost");
        assert!(r.is_err());
    }
}
