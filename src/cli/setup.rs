//! `lain setup` — guided onboarding (AGENT_UX_ROADMAP.md Milestone 2).
//!
//! Detects the repository, reuses `doctor::build_report` for the shared
//! capability/readiness contract (never recomputes it independently),
//! optionally installs the bi-encoder embedding model, configures one
//! MCP client, and verifies the result with a real `initialize` +
//! `tools/list` round trip against the exact command it just wrote.
//!
//! Two adapters exist today: `generic` (writes `.mcp.json`, the format
//! documented in `docs/COOKBOOK.md`) and `claude-code` (shells out to
//! the `claude` CLI's own `mcp add`/`get`/`remove`, which already owns
//! safe JSON editing for its config — reimplementing that here would be
//! a second, competing writer for a file this binary doesn't otherwise
//! touch). Codex/Cursor/VS Code/Continue adapters are not implemented
//! yet; `--agent` rejects anything but `generic` and `claude-code`.

use crate::cli::doctor;
use crate::cli::io::write_file_atomic;
use crate::server::nlp::NlpEmbedder;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct SetupOptions {
    pub workspace: Option<PathBuf>,
    /// `None` means "ask interactively if stdin is a TTY, else default
    /// to generic" — generic always works and needs no external CLI.
    pub agent: Option<String>,
    pub json: bool,
    pub dry_run: bool,
    pub print_config: bool,
    /// Skip every interactive prompt and assume "yes" (model download,
    /// overwrite confirmations). Required for any non-interactive run
    /// that wants the model installed.
    pub yes: bool,
    /// Never attempt the model download, don't ask, don't report it as
    /// actionable — just note it's absent.
    pub no_model: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticModelState {
    Ready,
    Installed,
    NotInstalled,
    DownloadFailed,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct SemanticModelStatus {
    pub state: SemanticModelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Shared installed-models directory. Matches the cross-encoder default
/// in `server/ingest/constructors.rs` and `install.sh --download-model`'s
/// destination — one place every optional model lands, regardless of
/// which tool put it there.
fn shared_models_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/lain/models")
}

const MODEL_FILE_NAME: &str = "all-MiniLM-L6-v2.onnx";
const TOKENIZER_FILE_NAME: &str = "tokenizer.json";
const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";

/// Find an already-usable model without downloading anything. Checks,
/// in order: `LAIN_EMBEDDING_MODEL` (the explicit override every other
/// entry point already honors), the CWD-relative default `NlpEmbedder`
/// falls back to, then the shared install directory `install.sh
/// --download-model` and this command both write to.
fn locate_existing_model() -> Option<(PathBuf, PathBuf)> {
    if let Some(env) = std::env::var_os("LAIN_EMBEDDING_MODEL") {
        let (model, tokenizer) = NlpEmbedder::resolve_model_paths(Path::new(&env));
        if model.is_file() && tokenizer.is_file() {
            return Some((model, tokenizer));
        }
    }
    let default = (
        PathBuf::from("models").join(MODEL_FILE_NAME),
        PathBuf::from("models").join(TOKENIZER_FILE_NAME),
    );
    if default.0.is_file() && default.1.is_file() {
        return Some(default);
    }
    let shared_dir = shared_models_dir();
    let shared = (
        shared_dir.join(MODEL_FILE_NAME),
        shared_dir.join(TOKENIZER_FILE_NAME),
    );
    if shared.0.is_file() && shared.1.is_file() {
        return Some(shared);
    }
    None
}

/// Download both files to the shared install directory. Mirrors
/// `install.sh`'s `download_onnx_model`: same destination, same
/// sources, same all-or-nothing cleanup on failure. No checksum
/// verification — Hugging Face publishes no `SHA256SUMS` for this
/// asset, and the manual `curl` instructions in the README carry the
/// same trust model.
fn download_model() -> Result<(PathBuf, PathBuf)> {
    let dir = shared_models_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create model directory {}", dir.display()))?;
    let model_path = dir.join(MODEL_FILE_NAME);
    let tokenizer_path = dir.join(TOKENIZER_FILE_NAME);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .context("build HTTP client for model download")?;

    let fetch = |url: &str, dest: &Path| -> Result<()> {
        let mut resp = client
            .get(url)
            .send()
            .with_context(|| format!("request {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("{url} returned HTTP {}", resp.status()));
        }
        let tmp = dest.with_extension("part");
        {
            let mut file =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            resp.copy_to(&mut file)
                .with_context(|| format!("write body from {url}"))?;
        }
        std::fs::rename(&tmp, dest)
            .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
        Ok(())
    };

    let result = fetch(MODEL_URL, &model_path).and_then(|_| fetch(TOKENIZER_URL, &tokenizer_path));
    if let Err(e) = result {
        // Don't leave a half-downloaded model behind — the same
        // all-or-nothing rule `install.sh` follows.
        let _ = std::fs::remove_file(&model_path);
        let _ = std::fs::remove_file(&tokenizer_path);
        let _ = std::fs::remove_file(model_path.with_extension("part"));
        let _ = std::fs::remove_file(tokenizer_path.with_extension("part"));
        return Err(e);
    }
    Ok((model_path, tokenizer_path))
}

/// Resolve the semantic model status for this run: find an existing
/// one, or offer to download one, respecting `--no-model` / `--yes` /
/// interactivity. Never downloads in `--json` mode without `--yes` —
/// a prompt would corrupt the JSON output on stdout.
fn resolve_semantic_model(opts: &SetupOptions) -> SemanticModelStatus {
    if let Some((model, _)) = locate_existing_model() {
        return SemanticModelStatus {
            state: SemanticModelState::Ready,
            model_path: Some(model),
            detail: None,
        };
    }
    if opts.no_model {
        return SemanticModelStatus {
            state: SemanticModelState::Skipped,
            model_path: None,
            detail: Some("--no-model passed; semantic_search will be unavailable_optional".into()),
        };
    }
    let should_download = if opts.yes {
        true
    } else if opts.json || !is_stdin_tty() {
        false
    } else {
        prompt_yes_no(
            "Optional semantic model (~90MB, sentence-transformers/all-MiniLM-L6-v2) \
             is not installed. Download it now?",
        )
    };
    if !should_download {
        return SemanticModelStatus {
            state: SemanticModelState::NotInstalled,
            model_path: None,
            detail: Some(
                "Run `lain setup --yes` or download manually (see README's \
                 \"Setting Up Semantic Search\" section)."
                    .into(),
            ),
        };
    }
    match download_model() {
        Ok((model, _)) => SemanticModelStatus {
            state: SemanticModelState::Installed,
            model_path: Some(model),
            detail: None,
        },
        Err(e) => SemanticModelStatus {
            state: SemanticModelState::DownloadFailed,
            model_path: None,
            detail: Some(format!("{e:#}")),
        },
    }
}

fn is_stdin_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn prompt_yes_no(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes")
}

/// Detect languages by root-level manifest presence. Deliberately
/// shallow — a one-level check, not a repo walk — this is a display
/// hint for the setup transcript, not a build-system integration.
fn detect_languages(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let checks: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("go.mod", "Go"),
        ("pyproject.toml", "Python"),
        ("setup.py", "Python"),
        ("requirements.txt", "Python"),
        ("pom.xml", "Java"),
        ("build.gradle", "Java/Kotlin"),
        ("build.gradle.kts", "Kotlin"),
        ("Gemfile", "Ruby"),
        ("composer.json", "PHP"),
    ];
    for (file, lang) in checks {
        if root.join(file).is_file() && !found.contains(&lang.to_string()) {
            found.push(lang.to_string());
        }
    }
    if root.join("package.json").is_file() {
        let lang = if root.join("tsconfig.json").is_file() {
            "TypeScript"
        } else {
            "JavaScript"
        };
        found.push(lang.to_string());
    }
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationState {
    Configured,
    WouldConfigure,
    Printed,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct ConfigurationOutcome {
    pub agent: String,
    pub state: ConfigurationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Build the `.mcp.json` / `claude mcp add` server entry. Both adapters
/// describe the exact same command: the current binary's own absolute
/// path (not the bare name `lain`, which may not be what a
/// generic-MCP-writing caller has on `PATH`) invoked with `mcp`.
fn build_mcp_server_entry(exe: &Path, model: Option<&Path>) -> Value {
    let mut entry = json!({
        "command": exe.display().to_string(),
        "args": ["mcp"],
    });
    if let Some(model) = model {
        entry["env"] = json!({ "LAIN_EMBEDDING_MODEL": model.display().to_string() });
    }
    entry
}

/// Merge `entry` into `path`'s `"mcpServers"` object under `server_name`,
/// preserving every other key. Refuses (rather than overwrites) a file
/// that isn't valid JSON or isn't shaped as expected — "nothing was
/// changed" must actually be true, not just claimed.
fn merge_mcp_json(path: &Path, server_name: &str, entry: Value) -> Result<Value> {
    let mut root: Value = if path.is_file() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "{} contains invalid JSON; nothing was changed",
                path.display()
            )
        })?
    } else {
        json!({})
    };
    let Some(root_obj) = root.as_object_mut() else {
        return Err(anyhow!(
            "{} does not contain a JSON object at the top level; nothing was changed",
            path.display()
        ));
    };
    let servers = root_obj.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers_obj) = servers.as_object_mut() else {
        return Err(anyhow!(
            "{}'s \"mcpServers\" key is not an object; nothing was changed",
            path.display()
        ));
    };
    servers_obj.insert(server_name.to_string(), entry);
    Ok(root)
}

/// Timestamped backup of a user-owned file before this command
/// modifies it. `with_file_name` (not `with_extension`) so a dotfile
/// like `.mcp.json` keeps its whole name intact in the backup name.
fn backup_file(path: &Path) -> Result<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".to_string());
    let backup = path.with_file_name(format!("{file_name}.bak-{ts}"));
    std::fs::copy(path, &backup)
        .with_context(|| format!("back up {} to {}", path.display(), backup.display()))?;
    Ok(backup)
}

fn configure_generic(
    root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let config_path = root.join(".mcp.json");
    let target = config_path.display().to_string();
    let entry = build_mcp_server_entry(exe, model);
    let merged = match merge_mcp_json(&config_path, "lain", entry) {
        Ok(v) => v,
        Err(e) => {
            return ConfigurationOutcome {
                agent: "generic".into(),
                state: ConfigurationState::Failed,
                target: Some(target),
                detail: Some(format!("{e:#}")),
            }
        }
    };
    let pretty = serde_json::to_string_pretty(&merged).unwrap_or_default();

    if opts.print_config {
        println!("{pretty}");
        return ConfigurationOutcome {
            agent: "generic".into(),
            state: ConfigurationState::Printed,
            target: Some(target),
            detail: None,
        };
    }
    if opts.dry_run {
        return ConfigurationOutcome {
            agent: "generic".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some(target),
            detail: Some(pretty),
        };
    }
    if config_path.is_file() {
        if let Err(e) = backup_file(&config_path) {
            return ConfigurationOutcome {
                agent: "generic".into(),
                state: ConfigurationState::Failed,
                target: Some(target),
                detail: Some(format!(
                    "backup before write failed: {e:#}; nothing was changed"
                )),
            };
        }
    }
    match write_file_atomic(&config_path, format!("{pretty}\n")) {
        Ok(()) => ConfigurationOutcome {
            agent: "generic".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: None,
        },
        Err(e) => ConfigurationOutcome {
            agent: "generic".into(),
            state: ConfigurationState::Failed,
            target: Some(target),
            detail: Some(e.to_string()),
        },
    }
}

/// `true` if the `claude` CLI is invocable on `PATH`.
fn claude_cli_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn claude_mcp_configured(name: &str) -> bool {
    Command::new("claude")
        .args(["mcp", "get", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn configure_claude_code(
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    if !claude_cli_available() {
        return ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Failed,
            target: None,
            detail: Some(
                "`claude` CLI not found on PATH; install Claude Code, or run with \
                 `--agent generic` to write `.mcp.json` directly."
                    .into(),
            ),
        };
    }

    let mut add_args: Vec<String> = vec!["mcp".into(), "add".into(), "lain".into()];
    if let Some(model) = model {
        add_args.push("-e".into());
        add_args.push(format!("LAIN_EMBEDDING_MODEL={}", model.display()));
    }
    add_args.push("--".into());
    add_args.push(exe.display().to_string());
    add_args.push("mcp".into());
    let command_line = format!("claude {}", add_args.join(" "));

    if opts.print_config {
        println!("{command_line}");
        return ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Printed,
            target: Some("claude mcp".into()),
            detail: Some(command_line),
        };
    }

    let already_configured = claude_mcp_configured("lain");
    if opts.dry_run {
        let plan = if already_configured {
            format!("claude mcp remove lain && {command_line}")
        } else {
            command_line
        };
        return ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some("claude mcp".into()),
            detail: Some(plan),
        };
    }

    // Idempotent re-run: `claude mcp add` fails outright if the name
    // already exists, so refresh rather than duplicate or silently
    // keep stale settings (e.g. an embedding model path from a
    // previous run that no longer applies).
    if already_configured {
        let _ = Command::new("claude")
            .args(["mcp", "remove", "lain"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    match Command::new("claude").args(&add_args).output() {
        Ok(out) if out.status.success() => ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Configured,
            target: Some("claude mcp".into()),
            detail: None,
        },
        Ok(out) => ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Failed,
            target: Some("claude mcp".into()),
            detail: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        },
        Err(e) => ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Failed,
            target: Some("claude mcp".into()),
            detail: Some(e.to_string()),
        },
    }
}

fn prompt_agent_choice() -> String {
    println!();
    println!("  Choose an agent");
    println!("  › 1) Claude Code");
    println!("    2) Generic MCP");
    print!("> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_ok() && line.trim() == "1" {
        return "claude-code".to_string();
    }
    "generic".to_string()
}

#[derive(Debug, Serialize)]
pub struct VerificationOutcome {
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Spawn the exact command just configured, perform `initialize` +
/// `tools/list`, and confirm both succeed. This is the roadmap's
/// "final verification" step — it must exercise the real command, not
/// a stand-in, so a misconfigured adapter is caught here rather than
/// on the agent's first real turn.
fn verify_mcp_command(exe: &Path, root: &Path, model: Option<&Path>) -> VerificationOutcome {
    let mut cmd = Command::new(exe);
    cmd.arg("mcp")
        .arg("--workspace")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(model) = model {
        cmd.env("LAIN_EMBEDDING_MODEL", model);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return VerificationOutcome {
                healthy: false,
                tools_count: None,
                detail: Some(format!("spawn failed: {e}")),
            }
        }
    };
    let write_result = (|| -> std::io::Result<()> {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        let protocol_version = rust_mcp_schema::ProtocolVersion::latest().to_string();
        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "lain-setup", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
        writeln!(stdin, "{init}")?;
        writeln!(stdin, "{list}")?;
        stdin.flush()
    })();
    if let Err(e) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return VerificationOutcome {
            healthy: false,
            tools_count: None,
            detail: Some(format!("failed to write to lain mcp stdin: {e}")),
        };
    }

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").is_some() && tx.send(v).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut init_ok = false;
    let mut tools_count = None;
    let mut detail = None;
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            detail = Some("timed out waiting for a response from lain mcp".to_string());
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(v) => match v.get("id").and_then(|i| i.as_i64()) {
                Some(1) => init_ok = v.get("result").is_some(),
                Some(2) => {
                    tools_count = v
                        .pointer("/result/tools")
                        .and_then(|t| t.as_array())
                        .map(|a| a.len())
                }
                _ => {}
            },
            Err(_) => {
                detail = Some("lain mcp closed its connection without answering".to_string());
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    VerificationOutcome {
        healthy: init_ok && tools_count.is_some(),
        tools_count,
        detail,
    }
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub schema_version: u32,
    pub server_version: &'static str,
    pub repository: PathBuf,
    pub languages: Vec<String>,
    pub capabilities: crate::server::readiness::Capabilities,
    pub semantic_model: SemanticModelStatus,
    pub configuration: ConfigurationOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationOutcome>,
    pub ready: bool,
}

pub fn run_setup(opts: SetupOptions) -> Result<i32> {
    let root = opts
        .workspace
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(anyhow::Error::from)
        .and_then(|start| crate::cli::workspace::walk_up_for_git(&start))
        .and_then(|found| {
            found.ok_or_else(|| {
                anyhow!("No Git repository found; pass --workspace PATH or run inside a clone.")
            })
        })?;

    let doctor_report = doctor::build_report(Some(&root))?;
    let languages = detect_languages(&root);
    let semantic = resolve_semantic_model(&opts);
    let exe = std::env::current_exe().context("locate current lain binary")?;

    let agent = match &opts.agent {
        Some(a) => a.clone(),
        None if !opts.json && is_stdin_tty() => prompt_agent_choice(),
        None => "generic".to_string(),
    };
    if agent != "generic" && agent != "claude-code" {
        return Err(anyhow!(
            "unknown --agent '{agent}'; supported values: generic, claude-code"
        ));
    }

    let configuration = if agent == "claude-code" {
        configure_claude_code(&exe, semantic.model_path.as_deref(), &opts)
    } else {
        configure_generic(&root, &exe, semantic.model_path.as_deref(), &opts)
    };

    let verification =
        if opts.dry_run || opts.print_config || configuration.state == ConfigurationState::Failed {
            None
        } else {
            Some(verify_mcp_command(
                &exe,
                &root,
                semantic.model_path.as_deref(),
            ))
        };

    let ready = configuration.state != ConfigurationState::Failed
        && verification.as_ref().map(|v| v.healthy).unwrap_or(true);

    let report = SetupReport {
        schema_version: crate::server::readiness::SCHEMA_VERSION,
        server_version: env!("CARGO_PKG_VERSION"),
        repository: root,
        languages,
        capabilities: doctor_report.capabilities,
        semantic_model: semantic,
        configuration,
        verification,
        ready,
    };

    // `--print-config` means "stdout is exactly the config, nothing
    // else" — pipeable (`lain setup --print-config > .mcp.json`) and
    // safe for a script to embed. The adapter already printed it above;
    // printing the full report on top of that isn't wrong information,
    // just no longer that contract.
    if !opts.print_config {
        if opts.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_human(&report);
        }
    }
    Ok(if report.ready { 0 } else { 1 })
}

fn print_human(report: &SetupReport) {
    println!();
    println!("  LAIN setup");
    println!();
    println!("  Repository        ✓ {}", report.repository.display());
    if report.languages.is_empty() {
        println!("  Languages         ○ none detected");
    } else {
        println!("  Languages         ✓ {}", report.languages.join(", "));
    }
    for (label, capability) in [
        ("Structural index", &report.capabilities.symbols),
        ("Git history", &report.capabilities.git_history),
    ] {
        use crate::server::readiness::CapabilityState::*;
        let mark = match capability.state {
            Ready | StaleUsable => "✓",
            WarmingUp => "○",
            UnavailableOptional => "○",
            UnavailableError => "×",
        };
        let state = serde_json::to_value(capability.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        println!("  {label:<17} {mark} {state}");
    }
    let (mark, label) = match report.semantic_model.state {
        SemanticModelState::Ready | SemanticModelState::Installed => ("✓", "ready".to_string()),
        SemanticModelState::NotInstalled => ("○", "optional model not installed".to_string()),
        SemanticModelState::DownloadFailed => ("×", "download failed".to_string()),
        SemanticModelState::Skipped => ("○", "skipped".to_string()),
    };
    println!("  Semantic search   {mark} {label}");
    println!();
    let agent_label = match report.configuration.agent.as_str() {
        "claude-code" => "Claude Code",
        other => other,
    };
    match report.configuration.state {
        ConfigurationState::Configured => {
            println!(
                "  Configuration     ✓ {agent_label} ({})",
                report.configuration.target.as_deref().unwrap_or("")
            );
        }
        ConfigurationState::WouldConfigure => {
            println!("  Configuration     ○ would configure {agent_label} (--dry-run)");
            if let Some(detail) = &report.configuration.detail {
                println!("{detail}");
            }
        }
        ConfigurationState::Printed => {
            println!("  Configuration     ✓ printed {agent_label} config above");
        }
        ConfigurationState::Failed => {
            println!("  Configuration     × {agent_label} failed");
            if let Some(detail) = &report.configuration.detail {
                println!("    {detail}");
            }
        }
    }
    if let Some(verification) = &report.verification {
        if verification.healthy {
            println!(
                "  Connection        ✓ verified ({} tools)",
                verification.tools_count.unwrap_or(0)
            );
        } else {
            println!("  Connection        × failed");
            if let Some(detail) = &verification.detail {
                println!("    {detail}");
            }
        }
    }
    println!();
    if report.ready {
        println!("  Ready. Ask your agent a question about this repository.");
    } else {
        println!("  Setup did not complete. See the messages above.");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_mcp_json_creates_new_file_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        let entry = json!({"command": "/usr/bin/lain", "args": ["mcp"]});
        let merged = merge_mcp_json(&path, "lain", entry.clone()).unwrap();
        assert_eq!(merged["mcpServers"]["lain"], entry);
    }

    #[test]
    fn merge_mcp_json_preserves_other_servers_and_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "mcpServers": {"other": {"command": "other-cmd"}},
                "unrelatedTopLevelKey": true
            }))
            .unwrap(),
        )
        .unwrap();
        let entry = json!({"command": "/usr/bin/lain", "args": ["mcp"]});
        let merged = merge_mcp_json(&path, "lain", entry.clone()).unwrap();
        assert_eq!(merged["mcpServers"]["lain"], entry);
        assert_eq!(merged["mcpServers"]["other"]["command"], "other-cmd");
        assert_eq!(merged["unrelatedTopLevelKey"], true);
    }

    #[test]
    fn merge_mcp_json_updates_existing_lain_entry_not_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "mcpServers": {"lain": {"command": "/old/path", "args": ["mcp"]}}
            }))
            .unwrap(),
        )
        .unwrap();
        let entry = json!({"command": "/new/path", "args": ["mcp"]});
        let merged = merge_mcp_json(&path, "lain", entry).unwrap();
        let servers = merged["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1, "must update in place, not duplicate");
        assert_eq!(servers["lain"]["command"], "/new/path");
    }

    #[test]
    fn merge_mcp_json_refuses_invalid_json_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let result = merge_mcp_json(&path, "lain", json!({"command": "x"}));
        assert!(result.is_err());
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "invalid JSON must be left untouched");
    }

    #[test]
    fn merge_mcp_json_refuses_non_object_top_level() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        assert!(merge_mcp_json(&path, "lain", json!({"command": "x"})).is_err());
    }

    #[test]
    fn build_mcp_server_entry_omits_env_when_no_model() {
        let entry = build_mcp_server_entry(Path::new("/usr/bin/lain"), None);
        assert!(entry.get("env").is_none());
        assert_eq!(entry["command"], "/usr/bin/lain");
        assert_eq!(entry["args"], json!(["mcp"]));
    }

    #[test]
    fn build_mcp_server_entry_sets_embedding_model_env_when_present() {
        let entry = build_mcp_server_entry(
            Path::new("/usr/bin/lain"),
            Some(Path::new(
                "/home/u/.local/lain/models/all-MiniLM-L6-v2.onnx",
            )),
        );
        assert_eq!(
            entry["env"]["LAIN_EMBEDDING_MODEL"],
            "/home/u/.local/lain/models/all-MiniLM-L6-v2.onnx"
        );
    }

    #[test]
    fn backup_file_copies_content_with_timestamped_dotfile_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(&path, "{}").unwrap();
        let backup = backup_file(&path).unwrap();
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".mcp.json.bak-"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "{}");
        // Original is untouched by a backup — only the write path replaces it.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }

    #[test]
    fn detect_languages_finds_rust_and_typescript() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();
        let langs = detect_languages(tmp.path());
        assert!(langs.contains(&"Rust".to_string()));
        assert!(langs.contains(&"TypeScript".to_string()));
        assert!(!langs.contains(&"JavaScript".to_string()));
    }

    #[test]
    fn detect_languages_empty_for_bare_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect_languages(tmp.path()).is_empty());
    }

    #[test]
    fn configure_generic_dry_run_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("generic".into()),
            json: false,
            dry_run: true,
            print_config: false,
            yes: false,
            no_model: true,
        };
        let outcome = configure_generic(tmp.path(), Path::new("/usr/bin/lain"), None, &opts);
        assert_eq!(outcome.state, ConfigurationState::WouldConfigure);
        assert!(!tmp.path().join(".mcp.json").exists());
    }

    #[test]
    fn configure_generic_writes_and_backs_up_on_rerun() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("generic".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: false,
            no_model: true,
        };
        let first = configure_generic(tmp.path(), Path::new("/usr/bin/lain"), None, &opts);
        assert_eq!(first.state, ConfigurationState::Configured);
        let config_path = tmp.path().join(".mcp.json");
        assert!(config_path.is_file());

        // Re-run must update in place (no duplicate) and back up the
        // previous version rather than silently discarding it.
        let second = configure_generic(tmp.path(), Path::new("/usr/bin/lain2"), None, &opts);
        assert_eq!(second.state, ConfigurationState::Configured);
        let content: Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(content["mcpServers"]["command"].is_null()); // sanity: not flattened
        assert_eq!(content["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(content["mcpServers"]["lain"]["command"], "/usr/bin/lain2");
        let backups: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".mcp.json.bak-"))
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup after one re-run");
    }
}
