//! Extensible toolchain detection and build/test configuration — dead simple
//!
//! Drop a file into `toolchains/` directory and it's detected.
//! Filename = language name. File content = detection markers (one per line).
//!
//! Example:
//!   toolchains/rust      → detects Rust projects (marker: "Cargo.toml")
//!   toolchains/zig       → detects Zig projects (marker: "build.zig")
//!
//! For full configuration, use TOML: toolchains/rust.toml
//! ```toml
//! name = "rust"
//! marker = "Cargo.toml"
//! build_command = "cargo build --message-format=json"
//! test_command = "cargo test --message-format=short"
//! build_parser = "cargo-json"
//! test_parser = "cargo-test"
//! ```

use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

/// Detect toolchains in a directory.
/// Returns list of detected toolchain names.
pub fn detect_toolchains(cwd: &Path, toolchains_dir: Option<&Path>) -> Vec<String> {
    let markers = load_toolchain_markers(toolchains_dir);
    if markers.is_empty() {
        return default_markers().into_keys().collect();
    }

    let mut detected = Vec::new();
    for (name, marker) in &markers {
        if cwd.join(marker).exists() {
            detected.push(name.clone());
        }
    }
    detected
}

/// Load toolchain markers from directory.
/// Simple files: filename = language, content = marker file to look for.
/// TOML files: { name, marker, priority }
fn load_toolchain_markers(dir: Option<&Path>) -> HashMap<String, String> {
    let dir = match dir {
        Some(d) => d,
        None => return default_markers(),
    };

    let mut markers = HashMap::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return default_markers(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_lowercase(),
            None => continue,
        };

        // TOML file: explicit config
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(config) = toml::from_str::<ToolchainProfile>(&content) {
                    markers.insert(config.name.clone(), config.marker);
                    continue;
                }
            }
        }

        // Plain file: filename = language, content = marker file
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let marker = content.trim().to_string();
                if !marker.is_empty() {
                    markers.insert(name, marker);
                }
            }
        }
    }

    if markers.is_empty() {
        return default_markers();
    }

    markers
}

/// Full toolchain profile loaded from TOML config files
#[derive(Debug, Clone, Deserialize)]
pub struct ToolchainProfile {
    pub name: String,
    pub marker: String,
    #[serde(default)]
    pub priority: u32,
    #[serde(default)]
    pub build_command: Option<String>,
    #[serde(default)]
    pub test_command: Option<String>,
    #[serde(default)]
    pub build_parser: Option<String>,
    #[serde(default)]
    pub test_parser: Option<String>,
    /// Directories to search for this toolchain's binaries when `PATH`
    /// doesn't have them, in priority order. `~` expands to the home
    /// directory, and a `*` component matches any single directory name
    /// — `~/.nvm/versions/node/*/bin` is the shape version managers
    /// actually install into.
    #[serde(default)]
    pub program_dirs: Vec<String>,
    /// A command that answers "where is this program?", invoked as
    /// `<resolver> <program>` with the path expected on stdout.
    /// `"rustup which"` and `"pyenv which"` are the canonical examples;
    /// version managers whose install directory is dynamic can only be
    /// asked, not guessed.
    #[serde(default)]
    pub program_resolver: Option<String>,
}

impl ToolchainProfile {
    /// Get the effective build command, falling back to defaults
    pub fn build_cmd(&self) -> String {
        self.build_command.clone().unwrap_or_else(|| {
            match self.name.as_str() {
                "rust" => "cargo build --message-format=json".to_string(),
                "go" => "go build".to_string(),
                "javascript" | "typescript" => "npm run build".to_string(),
                "python" => "python -m build".to_string(),
                _ => format!("echo 'no build command for {}'", self.name),
            }
        })
    }

    /// Get the effective test command, falling back to defaults
    pub fn test_cmd(&self) -> String {
        self.test_command.clone().unwrap_or_else(|| {
            match self.name.as_str() {
                "rust" => "cargo test --message-format=short".to_string(),
                "go" => "go test".to_string(),
                "javascript" | "typescript" => "npm test".to_string(),
                "python" => "pytest".to_string(),
                _ => format!("echo 'no test command for {}'", self.name),
            }
        })
    }

    /// Get the build parser ID, falling back to "text" (generic fallback)
    pub fn build_parser_id(&self) -> &str {
        self.build_parser.as_deref().unwrap_or("text")
    }

    /// Get the test parser ID, falling back to "text"
    pub fn test_parser_id(&self) -> &str {
        self.test_parser.as_deref().unwrap_or("text")
    }
}

/// Load full toolchain profiles from a directory.
/// Reads all .toml files and returns a map of name -> ToolchainProfile.
/// Falls back to defaults for built-in toolchains.
pub fn load_toolchain_profiles(dir: Option<&Path>) -> HashMap<String, ToolchainProfile> {
    let dir = match dir {
        Some(d) => d,
        None => return default_profiles(),
    };

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return default_profiles(),
    };

    let mut profiles = HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(profile) = toml::from_str::<ToolchainProfile>(&content) {
                profiles.insert(profile.name.clone(), profile);
            }
        }
    }

    // Merge with defaults for any built-in toolchains not explicitly configured
    let defaults = default_profiles();
    for (name, default_profile) in defaults {
        if !profiles.contains_key(&name) {
            profiles.insert(name, default_profile);
        }
    }

    if profiles.is_empty() {
        return default_profiles();
    }

    profiles
}

/// Get a single toolchain profile by name
pub fn get_toolchain_profile(name: &str) -> Option<ToolchainProfile> {
    let profiles = load_toolchain_profiles(None);
    profiles.get(name).cloned()
}

/// Default toolchain profiles — shipped with Lain.
/// To override or extend, drop TOML files in the toolchains/ directory.
/// See toolchains/rust.toml for the full format.
fn default_profiles() -> HashMap<String, ToolchainProfile> {
    HashMap::from([
        ("rust".to_string(), ToolchainProfile {
            name: "rust".to_string(),
            marker: "Cargo.toml".to_string(),
            priority: 100,
            build_command: Some("cargo build --message-format=json".to_string()),
            test_command: Some("cargo test --message-format=short".to_string()),
            build_parser: Some("cargo-json".to_string()),
            test_parser: Some("cargo-test".to_string()),
            program_resolver: Some("rustup which".to_string()),
            program_dirs: vec!["~/.cargo/bin".to_string(), "~/.rustup/toolchains/*/bin".to_string()],
        }),
        ("go".to_string(), ToolchainProfile {
            name: "go".to_string(),
            marker: "go.mod".to_string(),
            priority: 90,
            build_command: Some("go build".to_string()),
            test_command: Some("go test".to_string()),
            build_parser: Some("go-build".to_string()),
            test_parser: Some("go-test".to_string()),
            program_resolver: None,
            program_dirs: vec!["/usr/local/go/bin".to_string(), "~/go/bin".to_string(), "~/.local/go/bin".to_string(), "~/sdk/*/bin".to_string()],
        }),
        ("javascript".to_string(), ToolchainProfile {
            name: "javascript".to_string(),
            marker: "package.json".to_string(),
            priority: 80,
            build_command: Some("npm run build".to_string()),
            test_command: Some("npm test".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("jest".to_string()),
            program_resolver: None,
            program_dirs: vec!["~/.volta/bin".to_string(), "~/.bun/bin".to_string(), "~/.yarn/bin".to_string(), "~/.local/share/pnpm".to_string(), "~/.nvm/versions/node/*/bin".to_string(), "~/.fnm/aliases/default/bin".to_string()],
        }),
        ("typescript".to_string(), ToolchainProfile {
            name: "typescript".to_string(),
            marker: "tsconfig.json".to_string(),
            priority: 85,
            build_command: Some("npm run build".to_string()),
            test_command: Some("npm test".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("jest".to_string()),
            program_resolver: None,
            program_dirs: vec!["~/.volta/bin".to_string(), "~/.bun/bin".to_string(), "~/.yarn/bin".to_string(), "~/.local/share/pnpm".to_string(), "~/.nvm/versions/node/*/bin".to_string(), "~/.fnm/aliases/default/bin".to_string()],
        }),
        ("python".to_string(), ToolchainProfile {
            name: "python".to_string(),
            marker: "pyproject.toml".to_string(),
            priority: 80,
            build_command: Some("python -m build".to_string()),
            test_command: Some("pytest".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("pytest".to_string()),
            program_resolver: Some("pyenv which".to_string()),
            program_dirs: vec!["~/.pyenv/shims".to_string(), "~/.rye/shims".to_string(), "~/.local/pipx/venvs/*/bin".to_string()],
        }),
    ])
}

/// Default toolchain markers — shipped with Lain
fn default_markers() -> HashMap<String, String> {
    HashMap::from([
        ("rust".to_string(), "Cargo.toml".to_string()),
        ("go".to_string(), "go.mod".to_string()),
        ("python".to_string(), "pyproject.toml".to_string()),
        ("javascript".to_string(), "package.json".to_string()),
        ("typescript".to_string(), "tsconfig.json".to_string()),
        ("java".to_string(), "pom.xml".to_string()),
        ("csharp".to_string(), "*.csproj".to_string()),
        ("ruby".to_string(), "Gemfile".to_string()),
        ("php".to_string(), "composer.json".to_string()),
        ("cpp".to_string(), "CMakeLists.txt".to_string()),
        ("c".to_string(), "Makefile".to_string()),
        ("zig".to_string(), "build.zig".to_string()),
        ("swift".to_string(), "Package.swift".to_string()),
        ("kotlin".to_string(), "build.gradle.kts".to_string()),
        ("scala".to_string(), "build.sbt".to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_detect_rust() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();

        let detected = detect_toolchains(tmp.path(), None);
        assert!(detected.contains(&"rust".to_string()));
    }

    #[test]
    fn test_default_markers_have_rust() {
        let markers = default_markers();
        assert!(markers.contains_key("rust"));
        assert_eq!(markers.get("rust"), Some(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_detect_custom_language_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let toolchains_dir = tempfile::tempdir().unwrap();

        // Create a custom "foobar" language that detects "foobar.txt"
        fs::write(toolchains_dir.path().join("foobar"), "foobar.txt").unwrap();

        // Create the marker file in the project
        fs::write(tmp.path().join("foobar.txt"), "").unwrap();

        let detected = detect_toolchains(tmp.path(), Some(toolchains_dir.path()));
        assert!(detected.contains(&"foobar".to_string()));
    }

    #[test]
    fn test_toml_config_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let toolchains_dir = tempfile::tempdir().unwrap();

        // Create TOML config for custom language
        fs::write(
            toolchains_dir.path().join("nim.toml"),
            r#"name = "nim"
marker = "nim.cfg"
priority = 30
"#,
        )
        .unwrap();

        // Create the marker file in the project
        fs::write(tmp.path().join("nim.cfg"), "").unwrap();

        let detected = detect_toolchains(tmp.path(), Some(toolchains_dir.path()));
        assert!(detected.contains(&"nim".to_string()));
    }
}

// ── Program resolution ─────────────────────────────────────────────────────
//
// An MCP server inherits the environment of whatever launched it, and an
// editor-launched process routinely has no toolchain shims on `PATH`.
// The symptom is identical in every ecosystem: `run_build` fails with
// `No such file or directory (os error 2)` while the same command works
// in the developer's terminal. It was found with `cargo` living only
// under `~/.rustup/toolchains/<triple>/bin`, but nvm, pyenv, sdkman,
// rbenv, volta, mise and asdf all install outside a bare `PATH` too.
//
// Resolution is declared as data on the toolchain profile rather than
// branched on in Rust, so a user can teach lain about a manager we've
// never heard of by editing a TOML file.

/// Directories searched for every toolchain, after the profile's own.
/// These managers front many ecosystems at once, so listing them per
/// profile would repeat them a dozen times.
const UNIVERSAL_PROGRAM_DIRS: &[&str] =
    &["~/.local/share/mise/shims", "~/.asdf/shims", "~/.local/bin"];

/// Resolvers consulted for every toolchain, after the profile's own.
const UNIVERSAL_RESOLVERS: &[&str] = &["mise which", "asdf which"];

/// Everything program resolution consults, gathered so the search can be
/// exercised without a real version manager installed.
pub struct ProgramLookup {
    /// `$PATH`, split. Searched before anything is hunted for: if the
    /// environment already resolves the program, that is the user's
    /// choice and it wins.
    pub path_dirs: Vec<std::path::PathBuf>,
    /// Profile-declared directories, already expanded.
    pub profile_dirs: Vec<std::path::PathBuf>,
    /// Universal directories, already expanded.
    pub universal_dirs: Vec<std::path::PathBuf>,
    /// Resolver commands, profile-specific first. Each is a whole
    /// argv prefix — `["rustup", "which"]`.
    pub resolvers: Vec<Vec<String>>,
    /// Value of `$<PROGRAM>` (e.g. `$CARGO`), when set to a real file.
    pub env_override: Option<std::path::PathBuf>,
}

/// Ask a resolver command where a program lives. Separated so tests can
/// substitute one without installing rustup or mise.
pub type RunResolver = dyn Fn(&[String], &str) -> Option<std::path::PathBuf>;

/// Search `lookup` for `program`, in priority order.
///
/// Returns `None` when nothing matched, which the caller renders as the
/// bare program name so the spawn failure names what was actually
/// missing rather than a path we invented.
pub fn resolve_in(lookup: &ProgramLookup, program: &str, run: &RunResolver) -> Option<std::path::PathBuf> {
    // 1. An explicit `$CARGO` / `$GO` beats everything: the user said so.
    if let Some(p) = &lookup.env_override {
        return Some(p.clone());
    }
    // 2. `PATH` as configured. Only when this fails do we go hunting —
    //    the bug being fixed is precisely "PATH lacks it".
    if let Some(hit) = first_executable(&lookup.path_dirs, program) {
        return Some(hit);
    }
    // 3. Ask the version managers. This costs a subprocess, but a
    //    manager reporting which toolchain is *selected* beats guessing
    //    a version out of a directory glob — asked for `npm` with a
    //    stripped PATH, the glob answered with the oldest installed
    //    node on the machine.
    for argv in &lookup.resolvers {
        if let Some(hit) = run(argv, program) {
            if hit.is_file() {
                return Some(hit);
            }
        }
    }
    // 4. Fall back to guessing from the declared install locations,
    //    newest-looking first.
    if let Some(hit) = first_executable(&lookup.profile_dirs, program) {
        return Some(hit);
    }
    first_executable(&lookup.universal_dirs, program)
}

fn first_executable(dirs: &[std::path::PathBuf], program: &str) -> Option<std::path::PathBuf> {
    dirs.iter()
        .map(|d| d.join(program))
        .find(|candidate| candidate.is_file())
}

/// Expand `~` and a single `*` path component against the filesystem.
///
/// Version managers install into paths that are only knowable at
/// runtime — `~/.nvm/versions/node/v24.14.1/bin` — so a literal list
/// cannot express them. Any `*` component matches every directory at
/// that level, newest name last for stable ordering.
pub fn expand_program_dir(spec: &str) -> Vec<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let spec = if let Some(rest) = spec.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        spec.to_string()
    };
    if !spec.contains('*') {
        return vec![std::path::PathBuf::from(spec)];
    }
    let (before, after) = match spec.split_once('*') {
        Some(parts) => parts,
        None => return vec![std::path::PathBuf::from(spec)],
    };
    let base = std::path::Path::new(before);
    let suffix = after.trim_start_matches('/');
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut hits: Vec<(Vec<VersionPart>, std::path::PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let key = version_key(&e.file_name().to_string_lossy());
            let path = if suffix.is_empty() { e.path() } else { e.path().join(suffix) };
            (key, path)
        })
        .collect();
    // Newest first. A glob is a guess at *which version*, and guessing
    // the oldest installed toolchain is the worst available answer.
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.into_iter().map(|(_, p)| p).collect()
}

/// One component of a directory name, split so digit runs compare
/// numerically: `v10` must sort above `v9`, which plain lexical
/// ordering gets backwards.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum VersionPart {
    Text(String),
    Number(u64),
}

fn version_key(name: &str) -> Vec<VersionPart> {
    let mut parts = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    n.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            parts.push(VersionPart::Number(n.parse().unwrap_or(0)));
        } else {
            let mut t = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    break;
                }
                t.push(d);
                chars.next();
            }
            parts.push(VersionPart::Text(t));
        }
    }
    parts
}

/// Build the real lookup for `program` under `profile`.
fn lookup_for(program: &str, profile: Option<&ToolchainProfile>) -> ProgramLookup {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    let env_override = std::env::var(program.to_uppercase())
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file());

    let profile_dirs = profile
        .map(|p| p.program_dirs.iter().flat_map(|d| expand_program_dir(d)).collect())
        .unwrap_or_default();

    let mut resolvers: Vec<Vec<String>> = profile
        .and_then(|p| p.program_resolver.as_ref())
        .map(|r| vec![r.split_whitespace().map(str::to_string).collect()])
        .unwrap_or_default();
    resolvers.extend(
        UNIVERSAL_RESOLVERS
            .iter()
            .map(|r| r.split_whitespace().map(str::to_string).collect::<Vec<_>>()),
    );

    ProgramLookup {
        path_dirs,
        profile_dirs,
        universal_dirs: UNIVERSAL_PROGRAM_DIRS.iter().flat_map(|d| expand_program_dir(d)).collect(),
        resolvers,
        env_override,
    }
}

/// Run a resolver command and take the path from its stdout.
fn run_resolver_command(argv: &[String], program: &str) -> Option<std::path::PathBuf> {
    let (head, rest) = argv.split_first()?;
    let out = std::process::Command::new(head)
        .args(rest)
        .arg(program)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

/// Resolve `program` for `profile`, falling back to the bare name.
///
/// Cached per process: `PATH` and the installed toolchains don't change
/// under a running server, and the resolver steps spawn processes.
pub fn resolve_program(program: &str, profile: Option<&ToolchainProfile>) -> String {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{}::{program}", profile.map(|p| p.name.as_str()).unwrap_or(""));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let resolved = resolve_in(&lookup_for(program, profile), program, &run_resolver_command)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| program.to_string());
    if let Ok(mut c) = cache.lock() {
        c.insert(key, resolved.clone());
    }
    resolved
}

#[cfg(test)]
mod program_resolution_tests {
    //! The chain is exercised against temp directories and a stub
    //! resolver, so these pass on a machine with no version manager
    //! installed — which is also the machine most likely to hit the bug.
    use super::*;
    use std::path::PathBuf;

    fn touch_exe(dir: &std::path::Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        p
    }

    fn lookup() -> ProgramLookup {
        ProgramLookup {
            path_dirs: vec![],
            profile_dirs: vec![],
            universal_dirs: vec![],
            resolvers: vec![],
            env_override: None,
        }
    }

    fn never_resolves(_: &[String], _: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn path_wins_when_the_environment_already_resolves_it() {
        // Hunting is only for when PATH lacks the program. If it's
        // there, that's the user's configured choice.
        let tmp = tempfile::tempdir().unwrap();
        let on_path = touch_exe(&tmp.path().join("path"), "go");
        let elsewhere = touch_exe(&tmp.path().join("managed"), "go");

        let mut l = lookup();
        l.path_dirs = vec![on_path.parent().unwrap().to_path_buf()];
        l.profile_dirs = vec![elsewhere.parent().unwrap().to_path_buf()];

        assert_eq!(resolve_in(&l, "go", &never_resolves), Some(on_path));
    }

    #[test]
    fn falls_back_to_the_profile_dir_when_path_lacks_it() {
        // The actual reported failure, in every ecosystem: an
        // editor-launched server whose PATH has no toolchain shims.
        let tmp = tempfile::tempdir().unwrap();
        let managed = touch_exe(&tmp.path().join("nvm/versions/node/v24/bin"), "npm");

        let mut l = lookup();
        l.path_dirs = vec![tmp.path().join("empty")];
        l.profile_dirs = vec![managed.parent().unwrap().to_path_buf()];

        assert_eq!(resolve_in(&l, "npm", &never_resolves), Some(managed));
    }

    #[test]
    fn a_resolver_command_answers_when_the_directory_is_unguessable() {
        // rustup and pyenv install into paths only they know; asking is
        // the only option.
        let tmp = tempfile::tempdir().unwrap();
        let hidden = touch_exe(&tmp.path().join("toolchains/stable-x86_64/bin"), "cargo");
        let hidden_for_stub = hidden.clone();
        let stub = move |argv: &[String], program: &str| -> Option<PathBuf> {
            (argv == ["rustup", "which"] && program == "cargo").then(|| hidden_for_stub.clone())
        };

        let mut l = lookup();
        l.resolvers = vec![vec!["rustup".into(), "which".into()]];
        assert_eq!(resolve_in(&l, "cargo", &stub), Some(hidden));
    }

    #[test]
    fn an_env_override_beats_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let chosen = touch_exe(tmp.path(), "cargo");
        let on_path = touch_exe(&tmp.path().join("path"), "cargo");

        let mut l = lookup();
        l.env_override = Some(chosen.clone());
        l.path_dirs = vec![on_path.parent().unwrap().to_path_buf()];

        assert_eq!(resolve_in(&l, "cargo", &never_resolves), Some(chosen));
    }

    #[test]
    fn universal_shims_are_the_last_resort() {
        // mise and asdf front many ecosystems, so they're consulted for
        // every toolchain — but after the profile's own knowledge.
        let tmp = tempfile::tempdir().unwrap();
        let shim = touch_exe(&tmp.path().join("mise/shims"), "ruby");

        let mut l = lookup();
        l.universal_dirs = vec![shim.parent().unwrap().to_path_buf()];
        assert_eq!(resolve_in(&l, "ruby", &never_resolves), Some(shim));
    }

    #[test]
    fn nothing_found_is_none_so_the_error_names_the_program() {
        let l = lookup();
        assert_eq!(resolve_in(&l, "nope", &never_resolves), None);
    }

    #[test]
    fn a_star_component_expands_to_every_matching_directory() {
        // `~/.nvm/versions/node/*/bin` is the shape version managers
        // install into; a literal list can't express it.
        let tmp = tempfile::tempdir().unwrap();
        for v in ["v20.1.0", "v24.14.1"] {
            std::fs::create_dir_all(tmp.path().join("versions/node").join(v).join("bin")).unwrap();
        }
        let spec = format!("{}/versions/node/*/bin", tmp.path().display());
        let hits = expand_program_dir(&spec);
        assert_eq!(hits.len(), 2, "both installed versions must be searched: {hits:?}");
        assert!(hits.iter().all(|h| h.ends_with("bin")));
    }

    /// Found by probing a real machine: with a stripped PATH, `npm`
    /// resolved to node v20.20.0 while the developer's shell had
    /// v24.14.1. Ascending order picks the oldest toolchain installed,
    /// which is the worst available guess.
    #[test]
    fn glob_hits_are_ordered_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for v in ["v9.0.0", "v10.1.0", "v20.20.0", "v24.14.1"] {
            std::fs::create_dir_all(tmp.path().join("node").join(v).join("bin")).unwrap();
        }
        let hits = expand_program_dir(&format!("{}/node/*/bin", tmp.path().display()));
        let first = hits[0].to_string_lossy().to_string();
        assert!(first.contains("v24.14.1"), "newest must win, got {first}");
        // Digit-aware: v10 outranks v9, which lexical ordering gets
        // backwards.
        let order: Vec<String> = hits
            .iter()
            .map(|h| h.parent().unwrap().file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(order, vec!["v24.14.1", "v20.20.0", "v10.1.0", "v9.0.0"]);
    }

    /// A manager reporting the *selected* toolchain beats guessing a
    /// version out of a directory listing.
    #[test]
    fn a_resolver_answer_beats_a_glob_guess() {
        let tmp = tempfile::tempdir().unwrap();
        let guessed = touch_exe(&tmp.path().join("versions/v1/bin"), "node");
        let selected = touch_exe(&tmp.path().join("selected/bin"), "node");
        let selected_for_stub = selected.clone();
        let stub =
            move |_: &[String], _: &str| -> Option<PathBuf> { Some(selected_for_stub.clone()) };

        let mut l = lookup();
        l.profile_dirs = vec![guessed.parent().unwrap().to_path_buf()];
        l.resolvers = vec![vec!["mise".into(), "which".into()]];

        assert_eq!(resolve_in(&l, "node", &stub), Some(selected));
    }

    #[test]
    fn a_star_over_a_missing_parent_is_empty_not_a_panic() {
        assert!(expand_program_dir("/definitely/not/here/*/bin").is_empty());
    }

    #[test]
    fn every_shipped_profile_declares_where_its_toolchain_lives() {
        // The point of the change: resolution is data on the profile,
        // so a language is supported by describing it rather than by
        // adding a branch in Rust.
        for (name, profile) in default_profiles() {
            assert!(
                !profile.program_dirs.is_empty() || profile.program_resolver.is_some(),
                "{name} has no way to find its toolchain off PATH"
            );
        }
    }
}
