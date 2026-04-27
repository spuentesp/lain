//! Execution domain handlers - run commands, build, test, lint
//!
//! These handlers use the decoration pattern: command output -> parser -> enricher -> report
//!
//! Configuration is loaded from toolchains/*.toml files. To add or modify a language's
//! build/test commands or parser, edit the corresponding .toml file.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::tuning::RuntimeConfig;
use crate::toolchains::{detect_toolchains, load_toolchain_profiles};
use crate::tools::handlers::decoration::{decorate_output, get_parser, GraphEnricher};
use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Parse a command string like "cargo build --message-format=json" into a Command
fn parse_command(cmd_str: &str) -> Command {
    let mut parts = cmd_str.split_whitespace();
    let program = parts.next().unwrap_or("echo");
    let mut cmd = Command::new(program);
    for arg in parts {
        cmd.arg(arg);
    }
    cmd
}

pub async fn run_build(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    cwd: Option<&str>,
    release: bool,
) -> Result<String, LainError> {
    let work_dir = cwd.map(Path::new).unwrap_or(Path::new("."));

    // Detect toolchain
    let detected = detect_toolchains(work_dir, None);
    let toolchain_name = detected.first().map(|s| s.as_str()).unwrap_or("unknown");

    // Load profile and get build command + parser
    let profiles = load_toolchain_profiles(None);
    let profile = match profiles.get(toolchain_name) {
        Some(p) => p,
        None => {
            return Err(LainError::NotFound(format!(
                "No profile found for toolchain: {}. Add a toolchains/{}.toml file.",
                toolchain_name, toolchain_name
            )));
        }
    };

    let mut cmd = parse_command(&profile.build_cmd());
    // Inject --release if requested (for rust)
    if release && (toolchain_name == "rust" || toolchain_name == "cargo") {
        // Inject --release into the build command for rust
        let base_cmd = profile.build_command.clone().unwrap_or_default();
        let cmd_str = if !base_cmd.contains("--release") {
            base_cmd.replace("cargo build", "cargo build --release")
        } else {
            base_cmd
        };
        cmd = parse_command(&cmd_str);
    }
    cmd.current_dir(work_dir);

    let output = cmd.output().await.map_err(LainError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);

    // Combine stdout and stderr for parsing
    let combined = if stdout.is_empty() {
        stderr.to_string()
    } else if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{}\n{}", stdout, stderr)
    };

    let mut response = format!("Running `{}` in {:?} (toolchain: {})\n", profile.build_cmd(), work_dir, toolchain_name);
    response.push_str(&format!("Exit code: {}\n", exit_code));

    if exit_code == 0 {
        response.push_str("\n✅ Build successful\n");
    } else {
        response.push_str(&format!("\n❌ Build failed with exit code {}\n", exit_code));
        // Use decoration with toolchain-specific parser
        if let Some(parser) = get_parser(profile.build_parser_id()) {
            let enriched = decorate_output(&combined, parser, &GraphEnricher, graph, overlay);
            if !enriched.is_empty() && enriched != combined {
                response.push_str(&enriched);
            } else {
                response.push_str(&stderr);
            }
        } else {
            response.push_str(&format!("\n⚠️  Unknown parser '{}' — raw output:\n{}", profile.build_parser_id(), stderr));
        }
    }

    Ok(response)
}

pub async fn run_tests(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    cwd: Option<&str>,
    filter: Option<&str>,
    timeout_secs: Option<usize>,
    runtime: &RuntimeConfig,
) -> Result<String, LainError> {
    let work_dir = cwd.map(Path::new).unwrap_or(Path::new("."));

    // Detect toolchain
    let detected = detect_toolchains(work_dir, None);
    let toolchain_name = detected.first().map(|s| s.as_str()).unwrap_or("unknown");

    // Load profile and get test command + parser
    let profiles = load_toolchain_profiles(None);
    let profile = match profiles.get(toolchain_name) {
        Some(p) => p,
        None => {
            return Err(LainError::NotFound(format!(
                "No profile found for toolchain: {}. Add a toolchains/{}.toml file.",
                toolchain_name, toolchain_name
            )));
        }
    };

    let mut cmd = parse_command(&profile.test_cmd());
    // Inject filter for rust if provided
    if toolchain_name == "rust" || toolchain_name == "cargo" {
        if let Some(f) = filter {
            cmd.arg(f);
        }
    }
    cmd.current_dir(work_dir);

    let default_timeout = runtime.default_test_timeout_secs;
    let timeout_duration = Duration::from_secs(timeout_secs.unwrap_or(default_timeout as usize) as u64);

    let result = timeout(timeout_duration, cmd.output()).await
        .map_err(|_| LainError::Mcp("Tests timed out".to_string()))?
        .map_err(LainError::Io)?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let exit_code = result.status.code().unwrap_or(-1);

    // Use stdout, fall back to stderr if empty
    let test_output = if stdout.is_empty() { &stderr } else { &stdout };

    let mut response = format!("Running `{}` in {:?} (toolchain: {})\n", profile.test_cmd(), work_dir, toolchain_name);
    if let Some(f) = filter {
        response.push_str(&format!("Filter: {}\n", f));
    }
    response.push_str(&format!("Exit code: {}\n", exit_code));

    if exit_code == 0 {
        response.push_str("\n✅ All tests passed\n");
    } else {
        response.push_str(&format!("\n❌ Tests failed with exit code {}\n", exit_code));
        // Use decoration with toolchain-specific parser
        if let Some(parser) = get_parser(profile.test_parser_id()) {
            let enriched = decorate_output(test_output, parser, &GraphEnricher, graph, overlay);
            if !enriched.is_empty() && enriched.as_str() != test_output {
                response.push_str(&enriched);
            } else {
                response.push_str(test_output);
            }
        } else {
            response.push_str(&format!("\n⚠️  Unknown parser '{}' — raw output:\n{}", profile.test_parser_id(), test_output));
        }
    }

    Ok(response)
}

pub async fn run_clippy(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    cwd: Option<&str>,
    fix: bool,
) -> Result<String, LainError> {
    let work_dir = cwd.map(Path::new).unwrap_or(Path::new("."));

    if !work_dir.join("Cargo.toml").exists() {
        return Err(LainError::NotFound("Cargo.toml not found - not a Rust project".to_string()));
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("clippy");
    if fix {
        cmd.arg("--fix");
        cmd.arg("--allow-dirty");
        cmd.arg("--allow-staged");
    }
    cmd.arg("--message-format=json");
    cmd.current_dir(work_dir);

    let output = cmd.output().await.map_err(LainError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);

    let combined = if stdout.is_empty() {
        stderr.to_string()
    } else if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{}\n{}", stdout, stderr)
    };

    let mut response = format!("Running `cargo clippy` in {:?}\n", work_dir);
    if fix {
        response.push_str("(auto-fix mode)\n");
    }
    response.push_str(&format!("Exit code: {}\n", exit_code));

    if exit_code == 0 {
        response.push_str("\n✅ Clippy passed - no issues found\n");
    } else {
        response.push_str(&format!("\n❌ Clippy found issues (exit code {})\n", exit_code));
        // Use decoration: try JSON first, fall back to text parser
        if let Some(parser) = get_parser("cargo-json") {
            let enriched = decorate_output(&combined, parser, &GraphEnricher, graph, overlay);
            if enriched != combined {
                response.push_str(&enriched);
            } else if let Some(text_parser) = get_parser("text") {
                let enriched = decorate_output(&combined, text_parser, &GraphEnricher, graph, overlay);
                if !enriched.is_empty() && enriched != combined {
                    response.push_str(&enriched);
                } else {
                    response.push_str(&stderr);
                }
            } else {
                response.push_str(&stderr);
            }
        } else {
            response.push_str(&stderr);
        }
    }

    Ok(response)
}
