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
        Err("no active project; run `lain projects add <name> <path>` then `lain use <name>`, or pass --workspace".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    // cargo test runs tests in parallel. XDG_CONFIG_HOME is a process-global
    // env var, so parallel tests would stomp on each other. Serialize the
    // state tests through a global mutex.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

        // 3. cwd .lain (use a fresh dir with .lain inside)
        let cwd_dir = tmp("cwd");
        std::env::set_var("XDG_CONFIG_HOME", &cwd_dir);
        let lain_dir = cwd_dir.join(".lain");
        fs::create_dir_all(&lain_dir).unwrap();
        let resolved = Projects::resolve_workspace(Path::new(".")).unwrap();
        let _ = resolved;
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
}
