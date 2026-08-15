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
        }),
        ("go".to_string(), ToolchainProfile {
            name: "go".to_string(),
            marker: "go.mod".to_string(),
            priority: 90,
            build_command: Some("go build".to_string()),
            test_command: Some("go test".to_string()),
            build_parser: Some("go-build".to_string()),
            test_parser: Some("go-test".to_string()),
        }),
        ("javascript".to_string(), ToolchainProfile {
            name: "javascript".to_string(),
            marker: "package.json".to_string(),
            priority: 80,
            build_command: Some("npm run build".to_string()),
            test_command: Some("npm test".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("jest".to_string()),
        }),
        ("typescript".to_string(), ToolchainProfile {
            name: "typescript".to_string(),
            marker: "tsconfig.json".to_string(),
            priority: 85,
            build_command: Some("npm run build".to_string()),
            test_command: Some("npm test".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("jest".to_string()),
        }),
        ("python".to_string(), ToolchainProfile {
            name: "python".to_string(),
            marker: "pyproject.toml".to_string(),
            priority: 80,
            build_command: Some("python -m build".to_string()),
            test_command: Some("pytest".to_string()),
            build_parser: Some("text".to_string()),
            test_parser: Some("pytest".to_string()),
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
