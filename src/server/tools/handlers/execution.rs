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
use crate::server::tools::handlers::decoration::{decorate_output, get_parser, GraphEnricher};
use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Resolve a toolchain binary that may not be on the server's `PATH`.
///
/// MCP servers inherit the environment of whatever spawned them, and an
/// editor-launched process routinely has no toolchain shims: on the
/// machine this was found, `cargo` lived only at
/// `~/.rustup/toolchains/<triple>/bin/cargo`, so every `run_build` /
/// `run_tests` / `run_clippy` call failed. Falls back to the bare name
/// so a `PATH`-resolvable binary keeps working unchanged.
fn resolve_program(program: &str) -> String {
    if program != "cargo" && program != "rustc" {
        return program.to_string();
    }
    // Explicit override wins.
    if let Ok(p) = std::env::var(program.to_uppercase()) {
        if !p.is_empty() && Path::new(&p).exists() {
            return p;
        }
    }
    // Ask rustup where the active toolchain's binary lives.
    if let Ok(out) = std::process::Command::new("rustup").args(["which", program]).output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && Path::new(&p).exists() {
                return p;
            }
        }
    }
    // The conventional shim directory.
    if let Ok(home) = std::env::var("HOME") {
        let p = Path::new(&home).join(".cargo/bin").join(program);
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    program.to_string()
}

/// Turn a failed spawn into an error an agent can act on.
///
/// `cmd.output()` on a missing binary yields `No such file or directory
/// (os error 2)` and nothing else — no program, no directory, no path.
/// An agent reading that has no way to tell a missing toolchain from a
/// bad `cwd`, and both are one-line fixes once named.
fn spawn_error(program: &str, work_dir: &Path, e: std::io::Error) -> LainError {
    if e.kind() == std::io::ErrorKind::NotFound {
        let path = std::env::var("PATH").unwrap_or_else(|_| "<unset>".into());
        return LainError::NotFound(format!(
            "`{program}` was not found. Tried to run it in {} with PATH={path}. \
             The lain server inherits the environment of whatever launched it, which \
             often lacks toolchain shims — install {program}, add it to PATH, or set \
             the {} environment variable to its absolute path.",
            work_dir.display(),
            program.to_uppercase()
        ));
    }
    LainError::Io(format!(
        "running `{program}` in {}: {e}",
        work_dir.display()
    ))
}

/// Parse a command string like "cargo build --message-format=json" into
/// a Command, plus the program name for error reporting.
fn parse_command(cmd_str: &str) -> (Command, String) {
    let mut parts = cmd_str.split_whitespace();
    let program = parts.next().unwrap_or("echo");
    let resolved = resolve_program(program);
    let mut cmd = Command::new(&resolved);
    for arg in parts {
        cmd.arg(arg);
    }
    (cmd, program.to_string())
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

    let (mut cmd, mut program) = parse_command(&profile.build_cmd());
    // Inject --release if requested (for rust)
    if release && (toolchain_name == "rust" || toolchain_name == "cargo") {
        // Inject --release into the build command for rust
        let base_cmd = profile.build_command.clone().unwrap_or_default();
        let cmd_str = if !base_cmd.contains("--release") {
            base_cmd.replace("cargo build", "cargo build --release")
        } else {
            base_cmd
        };
        let (c, p) = parse_command(&cmd_str);
        cmd = c;
        program = p;
    }
    cmd.current_dir(work_dir);

    let output = cmd
        .output()
        .await
        .map_err(|e| spawn_error(&program, work_dir, e))?;
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

    let (mut cmd, program) = parse_command(&profile.test_cmd());
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
        .map_err(|e| spawn_error(&program, work_dir, e))?;

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

    let mut cmd = Command::new(resolve_program("cargo"));
    cmd.arg("clippy");
    if fix {
        cmd.arg("--fix");
        cmd.arg("--allow-dirty");
        cmd.arg("--allow-staged");
    }
    cmd.arg("--message-format=json");
    cmd.current_dir(work_dir);

    let output = cmd
        .output()
        .await
        .map_err(|e| spawn_error("cargo", work_dir, e))?;
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

#[cfg(test)]
mod spawn_tests {
    use super::*;

    /// A missing binary used to surface as bare
    /// `IO error: No such file or directory (os error 2)` — no program,
    /// no directory, no PATH. An agent reading that cannot tell a
    /// missing toolchain from a bad `cwd`.
    #[test]
    fn missing_program_error_names_the_program_and_where_it_looked() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = spawn_error("cargo", Path::new("/ws/project"), e);
        let msg = err.to_string();
        assert!(msg.contains("cargo"), "must name the program: {msg}");
        assert!(msg.contains("/ws/project"), "must name the directory: {msg}");
        assert!(msg.contains("PATH="), "must show the PATH it searched: {msg}");
    }

    #[test]
    fn other_spawn_errors_keep_their_cause_and_context() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = spawn_error("cargo", Path::new("/ws"), e).to_string();
        assert!(msg.contains("cargo") && msg.contains("/ws"), "{msg}");
    }

    #[test]
    fn non_toolchain_programs_are_left_alone() {
        // Only cargo/rustc get rustup resolution; everything else must
        // resolve through PATH exactly as before.
        assert_eq!(resolve_program("npm"), "npm");
        assert_eq!(resolve_program("go"), "go");
    }

    #[test]
    fn parse_command_reports_the_program_name_not_the_resolved_path() {
        // The error message should say `cargo`, not a long absolute
        // path the user never typed.
        let (_cmd, program) = parse_command("cargo build --message-format=json");
        assert_eq!(program, "cargo");
    }
}
