//! Read-only repository diagnosis. The report describes a persisted snapshot and
//! a separate MCP transport probe, not the readiness of a running indexer.

use crate::config::{config_dir, hooks_dir, lain_git_sha};
use crate::server::graph::{inspect_persisted_graph, GraphInspectionError};
use crate::server::readiness::{Capabilities, Capability, CapabilityState, SCHEMA_VERSION};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct Problem {
    pub code: String,
    pub message: String,
    pub remediation: String,
    pub retryable: bool,
}

#[derive(Debug, Serialize)]
pub struct RepositoryReport {
    pub root: PathBuf,
    pub head: Option<String>,
    pub indexed_commit: Option<String>,
    /// This command never constructs or observes a live working-tree overlay.
    pub working_tree_overlay: bool,
    pub working_tree_dirty: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct TransportReport {
    pub kind: &'static str,
    pub healthy: bool,
    pub tools_count: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct InstallationReport {
    pub config_dir: PathBuf,
    pub config_dir_state: &'static str,
    pub hooks_dir: PathBuf,
    pub hooks_dir_state: &'static str,
    pub hook_script: Option<PathBuf>,
}

/// Active wire-level tool profile. Surfaced in `doctor --json` so an
/// operator checking an offline server can see whether `tools/list`
/// will return the curated 14-tool semantic surface or the full
/// 79-tool legacy surface. The advertised count is what the next
/// `tools/list` round-trip will report — lower than the registry
/// total under `semantic`, equal to the registry total under
/// `full`.
#[derive(Debug, Serialize)]
pub struct ToolProfileReport {
    pub name: &'static str,
    pub advertised_count: usize,
}

/// LSP cold-boot prewarm knobs. Defaulted from
/// `IngestionConfig::default()`; the operator can override per-server
/// via `.lain/tuning.toml` (not currently read by `doctor` — the
/// snapshot stays default-only so a non-workspace `doctor`
/// invocation is meaningful). The `env_opt_out` line tells the
/// operator whether `LAIN_LSP_PREWARM=false` has disabled the
/// prewarm entirely.
#[derive(Debug, Serialize)]
pub struct LspPrewarmKnobs {
    pub timeout_secs: u64,
    pub max_files: usize,
    pub opt_out: bool,
    pub env_opt_out: bool,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub server_version: &'static str,
    pub build_commit: &'static str,
    pub assessment: &'static str,
    pub agent_ready: bool,
    pub repository: Option<RepositoryReport>,
    pub capabilities: Capabilities,
    pub transport: TransportReport,
    pub installation: InstallationReport,
    pub tool_profile: ToolProfileReport,
    pub lsp_prewarm: LspPrewarmKnobs,
    pub problems: Vec<Problem>,
}

impl DoctorReport {
    pub fn exit_code(&self) -> i32 {
        self.capabilities
            .readiness(self.transport.healthy)
            .exit_code()
    }

    fn problem(
        &mut self,
        code: &str,
        message: impl Into<String>,
        remediation: &str,
        retryable: bool,
    ) {
        self.problems.push(Problem {
            code: code.into(),
            message: message.into(),
            remediation: remediation.into(),
            retryable,
        });
    }

    fn structural(&mut self, state: CapabilityState, reason: &str, remediation: &str) {
        let mut capability = Capability::new(state, false);
        capability.reason = Some(reason.into());
        capability.remediation = Some(remediation.into());
        self.capabilities.symbols = capability.clone();
        self.capabilities.call_graph = capability;
    }
}

fn directory_state(path: &Path) -> &'static str {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => "present",
        Ok(_) => "not_directory",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "unreadable",
    }
}

fn installation() -> InstallationReport {
    let mut candidates =
        vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/claude-code/pre-edit.sh")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("../share/lain/hooks/claude-code/pre-edit.sh"));
            candidates.push(parent.join("hooks/claude-code/pre-edit.sh"));
        }
    }
    let config_dir = config_dir();
    let hooks_dir = hooks_dir();
    InstallationReport {
        config_dir_state: directory_state(&config_dir),
        hooks_dir_state: directory_state(&hooks_dir),
        config_dir,
        hooks_dir,
        hook_script: candidates.into_iter().find(|path| path.is_file()),
    }
}

fn dirty(repo: &git2::Repository) -> Result<bool> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .update_index(false);
    Ok(repo.statuses(Some(&mut options))?.iter().any(|entry| {
        // LAIN's own cache cannot make an otherwise clean source tree stale.
        !entry
            .path()
            .ok()
            .is_some_and(|path| path == ".lain" || path.starts_with(".lain/"))
    }))
}

fn observe_repository(report: &mut DoctorReport, root: &Path) -> Result<()> {
    let repo = git2::Repository::open(root)?;
    let head = repo.head()?.peel_to_commit()?.id().to_string();
    let was_dirty = dirty(&repo)?;
    let mut repository = RepositoryReport {
        root: root.to_path_buf(),
        head: Some(head.clone()),
        indexed_commit: None,
        working_tree_overlay: false,
        working_tree_dirty: Some(was_dirty),
    };
    report.capabilities.git_history = Capability::new(CapabilityState::Ready, false);
    let refresh = "Run `lain mcp` in this repository to build or refresh the index.";
    match inspect_persisted_graph(&root.join(".lain/graph.bin")) {
        Ok(commit) => {
            repository.indexed_commit = commit.clone();
            match commit {
                None => {
                    report.structural(
                        CapabilityState::UnavailableError,
                        "Graph has no indexed commit.",
                        refresh,
                    );
                    report.problem(
                        "graph_unindexed",
                        "Graph has no indexed commit.",
                        refresh,
                        true,
                    );
                }
                Some(commit) if commit != head || was_dirty => {
                    let reason = "Persisted graph is an intact stale snapshot; working-tree changes are not included.";
                    report.structural(CapabilityState::StaleUsable, reason, refresh);
                    report.problem("graph_stale", reason, refresh, true);
                }
                Some(_) => {
                    report.capabilities.symbols = Capability::new(CapabilityState::Ready, false);
                    report.capabilities.call_graph = Capability::new(CapabilityState::Ready, false);
                }
            }
        }
        Err(error) => {
            let (code, action) = match &error {
                GraphInspectionError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => ("graph_missing", refresh),
                GraphInspectionError::Io(_) => ("graph_unreadable", "Check read permissions on .lain/graph.bin and its parent directories."),
                GraphInspectionError::Incompatible(_) => ("graph_incompatible", "Stop LAIN processes using this repository, back up .lain/graph.bin, then run `lain mcp` to rebuild."),
                _ => ("graph_corrupt", "Stop LAIN processes using this repository, move .lain/graph.bin to a backup, then run `lain mcp` to rebuild."),
            };
            report.structural(
                CapabilityState::UnavailableError,
                &error.to_string(),
                action,
            );
            report.problem(code, error.to_string(), action, true);
        }
    }
    // Do not label a snapshot current if Git changed during inspection.
    let final_head = repo.head()?.peel_to_commit()?.id().to_string();
    let final_dirty = dirty(&repo)?;
    if final_head != head || final_dirty != was_dirty {
        let message = "Repository changed during diagnosis.";
        report.structural(
            CapabilityState::UnavailableError,
            message,
            "Run `lain doctor` again after edits settle.",
        );
        report.problem(
            "repository_changed",
            message,
            "Run `lain doctor` again after edits settle.",
            true,
        );
    }
    report.repository = Some(repository);
    Ok(())
}

fn observe_semantic(report: &mut DoctorReport) {
    let configured = std::env::var_os("LAIN_EMBEDDING_MODEL");
    let (model, tokenizer) = configured
        .as_deref()
        .map(Path::new)
        .map(crate::server::nlp::NlpEmbedder::resolve_model_paths)
        .unwrap_or_else(|| {
            (
                PathBuf::from("models/all-MiniLM-L6-v2.onnx"),
                PathBuf::from("models/tokenizer.json"),
            )
        });
    if configured.is_none() && !model.exists() && !tokenizer.exists() {
        let mut capability = Capability::new(CapabilityState::UnavailableOptional, true);
        capability.reason = Some("Optional semantic model is not installed.".into());
        report.capabilities.semantic_search = capability;
        return;
    }
    // File presence cannot prove model validity or semantic index coverage.
    let reason = if !model.is_file() || !tokenizer.is_file() {
        "Configured semantic model or tokenizer is missing."
    } else {
        "Model files are present; semantic runtime and index coverage have not been verified."
    };
    let action = "Check LAIN_EMBEDDING_MODEL and inspect `get_health` on the running MCP server.";
    let mut capability = Capability::new(CapabilityState::UnavailableError, true);
    capability.reason = Some(reason.into());
    capability.remediation = Some(action.into());
    report.capabilities.semantic_search = capability;
    report.problem("semantic_unverified", reason, action, true);
}

pub fn build_report(workspace: Option<&Path>) -> Result<DoctorReport> {
    // Tool-profile report (PR-fix-1): surface the active wire-level
    // filter so an operator reading doctor.json can tell whether
    // `tools/list` is the curated 14 or the full 79. Reads `LAIN_TOOL_PROFILE`
    // via the same env-var resolution the dispatcher uses, so doctor
    // and the running server agree to the byte.
    let profile = crate::server::tools::profile::ToolProfile::from_env();
    // LSP prewarm knobs (PR-fix-1): defaults from `IngestionConfig::default()`.
    // Doctor doesn't currently read `.lain/tuning.toml` — the snapshot
    // would diverge from a server that has overrides applied, but
    // the defaults are stable and useful enough for offline triage.
    let prewarm_knobs = crate::tuning::IngestionConfig::default();
    let env_opt_out = matches!(
        std::env::var("LAIN_LSP_PREWARM").ok().as_deref(),
        Some("false") | Some("0") | Some("")
    );
    // `advertised_count` matches what `tools/list` would return under
    // this profile, approximated. We don't have a live `LspPool`
    // here (doctor runs without booting a server), so the count is:
    //   * Full profile:      registry total
    //   * Semantic profile:  (inventory tools whose name is in
    //                         SEMANTIC_PROFILE) + SERVER_STATUS.len()
    // Both approximations omit workspace-mode tools (those live on
    // `LainMcpServer`, not on this code path); operators using a
    // workspace server will see a slightly higher count in the
    // server's own `get_capabilities.tool_profile.advertised_count`.
    let registry = crate::tools::registry::ToolRegistry::definitions();
    let advertised = match profile {
        crate::server::tools::profile::ToolProfile::Full => registry.len(),
        crate::server::tools::profile::ToolProfile::Semantic => {
            let inventory_in_profile = registry
                .iter()
                .filter(|d| {
                    crate::server::tools::profile::SEMANTIC_PROFILE
                        .iter()
                        .any(|name| *name == d.name)
                })
                .count();
            inventory_in_profile
                + crate::server::tools::profile::special_advertised_count(profile, false)
        }
    };

    let mut report = DoctorReport {
        schema_version: SCHEMA_VERSION,
        server_version: env!("CARGO_PKG_VERSION"),
        build_commit: lain_git_sha(),
        assessment: "persisted_snapshot",
        agent_ready: false,
        repository: None,
        capabilities: Capabilities {
            symbols: Capability::new(CapabilityState::UnavailableError, false),
            call_graph: Capability::new(CapabilityState::UnavailableError, false),
            git_history: Capability::new(CapabilityState::UnavailableError, false),
            semantic_search: Capability::new(CapabilityState::UnavailableOptional, true),
        },
        transport: TransportReport {
            kind: "not_checked",
            healthy: false,
            tools_count: None,
        },
        installation: installation(),
        tool_profile: ToolProfileReport {
            name: profile.as_str(),
            advertised_count: advertised,
        },
        lsp_prewarm: LspPrewarmKnobs {
            timeout_secs: prewarm_knobs.lsp_prewarm_timeout_secs,
            max_files: prewarm_knobs.lsp_prewarm_max_files,
            opt_out: prewarm_knobs.lsp_prewarm_opt_out,
            env_opt_out,
        },
        problems: Vec::new(),
    };
    // Interactive CLI diagnosis follows its own cwd, not an agent parent's cwd.
    let root = workspace
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(anyhow::Error::from)
        .and_then(|start| crate::cli::workspace::walk_up_for_git(&start))
        .and_then(|root| root.ok_or_else(|| anyhow!("No Git repository found.")));
    match root {
        Err(error) => report.problem(
            "repository_not_found",
            error.to_string(),
            "Run inside a Git repository or pass `--workspace PATH`.",
            false,
        ),
        Ok(root) => {
            if let Err(error) = observe_repository(&mut report, &root) {
                let action =
                    "Check repository permissions and create an initial commit if HEAD is unborn.";
                report.structural(
                    CapabilityState::UnavailableError,
                    &error.to_string(),
                    action,
                );
                report.capabilities.git_history =
                    Capability::new(CapabilityState::UnavailableError, false);
                report.problem("repository_unreadable", error.to_string(), action, true);
            }
            report.transport.kind = "stdio_probe";
            let probe = probe_stdio(&root);
            match probe {
                Ok(count) => {
                    report.transport.healthy = true;
                    report.transport.tools_count = Some(count);
                }
                Err(error) => report.problem(
                    "mcp_probe_failed",
                    format!("{error:#}"),
                    "Run `lain mcp` directly and inspect its stderr output.",
                    true,
                ),
            }
        }
    }
    observe_semantic(&mut report);
    // An explicitly selected endpoint must work too; a healthy local probe must
    // not hide a broken endpoint used by the caller's agent.
    if let Ok(url) = std::env::var("LAIN_URL").or_else(|_| std::env::var("LAIN_SERVER_URL")) {
        report.transport.kind = "http_endpoint";
        let endpoint_probe = crate::cli::mcp_client::post_json_rpc(
            &url,
            "initialize",
            json!({
                "protocolVersion": rust_mcp_schema::ProtocolVersion::latest().to_string(),
                "capabilities": {},
                "clientInfo": {"name": "lain-doctor", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .and_then(|initialized| {
            if initialized.get("serverInfo").is_none()
                || initialized.get("protocolVersion").is_none()
            {
                return Err(anyhow!("invalid MCP initialize response"));
            }
            crate::cli::mcp_client::post_json_rpc(&url, "tools/list", json!({}))
        })
        .and_then(|value| tools_count(&value));
        match endpoint_probe {
            Ok(count) => {
                report.transport.healthy = true;
                report.transport.tools_count = Some(count);
            }
            Err(error) => {
                report.transport.healthy = false;
                report.transport.tools_count = None;
                report.problem(
                    "mcp_endpoint_failed",
                    format!("{error:#}"),
                    "Check LAIN_URL/LAIN_SERVER_URL and start the selected MCP server.",
                    true,
                );
            }
        }
    }
    report.agent_ready = report
        .capabilities
        .readiness(report.transport.healthy)
        .agent_ready();
    report
        .problems
        .sort_by(|a, b| (&a.code, &a.message).cmp(&(&b.code, &b.message)));
    Ok(report)
}

pub fn run_doctor(json_output: bool, workspace: Option<&Path>) -> Result<i32> {
    let report = build_report(workspace)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(report.exit_code())
}

fn print_human(report: &DoctorReport) {
    println!(
        "== lain doctor ==\nbinary version: {} (commit {})",
        report.server_version, report.build_commit
    );
    if let Some(repo) = &report.repository {
        println!("Repository: {}", repo.root.display());
        println!(
            "Graph commit: {}",
            repo.indexed_commit.as_deref().unwrap_or("not indexed")
        );
    }
    for (name, capability) in [
        ("Symbols", &report.capabilities.symbols),
        ("Call graph", &report.capabilities.call_graph),
        ("Git history", &report.capabilities.git_history),
        ("Semantic search", &report.capabilities.semantic_search),
    ] {
        println!(
            "{name}: {}",
            serde_json::to_value(capability.state)
                .unwrap()
                .as_str()
                .unwrap()
        );
    }
    if report.transport.healthy {
        println!(
            "MCP surface live: tools/list advertises {} tools ({})",
            report.transport.tools_count.unwrap_or(0),
            report.transport.kind
        );
    } else {
        println!("MCP transport: unavailable");
    }
    println!(
        "config dir: {} ({})",
        report.installation.config_dir.display(),
        report.installation.config_dir_state
    );
    println!(
        "hooks dir: {} ({})",
        report.installation.hooks_dir.display(),
        report.installation.hooks_dir_state
    );
    println!(
        "hook script: {}",
        report
            .installation
            .hook_script
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not installed (optional)".into())
    );
    for problem in &report.problems {
        println!(
            "{}: {}\n  Next: {}",
            problem.code, problem.message, problem.remediation
        );
    }
    println!(
        "Agent-ready: {} (persisted snapshot assessment)",
        if report.agent_ready { "YES" } else { "NO" }
    );
}

fn tools_count(value: &Value) -> Result<usize> {
    let tools = value
        .get("tools")
        .and_then(Value::as_array)
        .context("tools/list response has no tools array")?;
    if tools.is_empty() {
        return Err(anyhow!("MCP surface empty: tools/list advertises 0 tools"));
    }
    Ok(tools.len())
}

/// Internal probe endpoint: no indexing, watchers, model loading, or tool calls.
pub async fn run_probe(workspace: &Path) -> Result<()> {
    // Validate before the sidecar constructor, preventing its fallback stub repo.
    crate::server::git::GitSensor::new(workspace)?;
    let graph = crate::server::graph::GraphDatabase::empty_read_only();
    let executor = crate::server::tools::ToolExecutor::new_read_only(
        graph,
        crate::server::overlay::VolatileOverlay::new(),
        workspace.to_path_buf(),
    );
    crate::server::mcp::handler::LainMcpServer::new_read_only(executor)
        .run_stdio()
        .await
        .map_err(|error| anyhow!("{error}"))
}

fn probe_stdio(workspace: &Path) -> Result<usize> {
    use crate::cli::mcp_stdio::{initialize_request, is_valid_initialize_result, StdioSession};

    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["doctor", "--probe-mcp", "--workspace"])
        .arg(workspace);
    let mut session = StdioSession::spawn(command, false)?;
    session.send(&initialize_request(1, "lain-doctor"))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut initialized = false;
    let result = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break Err(anyhow!("MCP probe timed out after 5 seconds"));
        }
        let value = match session.recv_timeout(remaining) {
            Ok(v) => v,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                break Err(anyhow!("MCP probe timed out after 5 seconds"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(anyhow!(
                    "MCP probe exited before completing initialize/tools-list"
                ))
            }
        };
        if value.get("error").is_some() {
            break Err(anyhow!("MCP probe error: {}", value["error"]));
        }
        if value["id"] == 1 && !initialized {
            if !is_valid_initialize_result(&value["result"]) {
                break Err(anyhow!("invalid MCP initialize response"));
            }
            initialized = true;
            let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
            let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
            if let Err(e) = session
                .send(&notification)
                .and_then(|_| session.send(&list))
            {
                break Err(e);
            }
        } else if value["id"] == 2 && initialized {
            break tools_count(&value["result"]);
        }
    };
    session.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_report` runs without a workspace and without booting a
    /// real server — it defaults to the workspace tuning defaults,
    /// the registry's tool count, and a Semantic profile unless the
    /// test runner happens to have `LAIN_TOOL_PROFILE=full` in its
    /// env (it doesn't on CI). The tool-profile / prewarm fields
    /// must reflect those defaults, NOT the registry's full
    /// surface, so an operator doing `doctor --json` against an
    /// offline server sees what `tools/list` will produce.
    ///
    /// Caveat: a test that mutates `LAIN_TOOL_PROFILE` or
    /// `LAIN_LSP_PREWARM` would race siblings that read them. We
    /// don't mutate env in these tests — env-driven paths are
    /// pinned at the unit-test level (`profile::tests::from_env`)
    /// rather than here.
    #[test]
    fn report_carries_tool_profile_and_prewarm_knobs() {
        let report = build_report(None).expect("build_report without workspace");
        assert!(
            report.tool_profile.name == "semantic" || report.tool_profile.name == "full",
            "tool_profile.name must be one of the documented values, got {:?}",
            report.tool_profile.name
        );
        // The advertised count under the default Semantic profile
        // is much smaller than the registry total. We pin a
        // coarse "less than the registry" check rather than an
        // exact value — adding a tool to the registry is a normal
        // event and shouldn't break doctor tests.
        let registry = crate::tools::registry::ToolRegistry::definitions().len();
        if report.tool_profile.name == "semantic" {
            assert!(
                report.tool_profile.advertised_count < registry,
                "semantic profile advertised_count ({}) must be < registry len ({})",
                report.tool_profile.advertised_count,
                registry
            );
        } else {
            assert_eq!(
                report.tool_profile.advertised_count, registry,
                "full profile must equal registry length"
            );
        }
        // The prewarm knobs pin the documented defaults so a future
        // tuning-config change is caught here before reaching a
        // release.
        assert_eq!(report.lsp_prewarm.timeout_secs, 30);
        assert_eq!(report.lsp_prewarm.max_files, 50);
        assert!(!report.lsp_prewarm.opt_out);
    }

    /// `doctor --json` JSON-serialises a complete document
    /// regardless of whether a workspace was found. Captures the
    /// wire shape via `serde_json::to_value` so a future field
    /// rename in either `ToolProfileReport` or `LspPrewarmKnobs`
    /// surfaces as a CI break.
    #[test]
    fn report_shape_round_trips_through_serde() {
        let report = build_report(None).expect("build_report without workspace");
        let value = serde_json::to_value(&report).expect("DoctorReport must serialise");
        let obj = value.as_object().expect("top-level JSON is an object");
        for required in [
            "schema_version",
            "server_version",
            "build_commit",
            "assessment",
            "agent_ready",
            "repository",
            "capabilities",
            "transport",
            "installation",
            "tool_profile",
            "lsp_prewarm",
            "problems",
        ] {
            assert!(
                obj.contains_key(required),
                "DoctorReport JSON missing required field {required:?}; full: {value}"
            );
        }
        let profile = obj
            .get("tool_profile")
            .and_then(|v| v.as_object())
            .expect("tool_profile is an object");
        assert!(profile.contains_key("name"));
        assert!(profile.contains_key("advertised_count"));
        let prewarm = obj
            .get("lsp_prewarm")
            .and_then(|v| v.as_object())
            .expect("lsp_prewarm is an object");
        for k in ["timeout_secs", "max_files", "opt_out", "env_opt_out"] {
            assert!(
                prewarm.contains_key(k),
                "lsp_prewarm JSON missing {k:?}; full: {value}"
            );
        }
    }
}
