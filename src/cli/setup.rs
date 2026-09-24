//! `lain setup` — guided onboarding (AGENT_UX_ROADMAP.md Milestone 2).
//!
//! Detects the repository, reuses `doctor::build_report` for the shared
//! capability/readiness contract (never recomputes it independently),
//! optionally installs the bi-encoder embedding model, configures one
//! MCP client, and verifies the result with a real `initialize` +
//! `tools/list` round trip against the exact command it just wrote.
//!
//! Adapters exist for generic MCP clients, Claude Code, Codex, Cursor,
//! VS Code, and Continue. The generic adapter writes `.mcp.json`; the
//! client-specific adapters update each client's native configuration.
//! Claude Code delegates mutation to its own `claude mcp` CLI so lain
//! does not become a competing writer for Claude's config file.

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
    /// Which optional language servers to install: `none`, `detected`
    /// (every missing one for the languages found), or a comma list of
    /// languages / extensions. `None` asks on a TTY and installs nothing
    /// otherwise. `--yes` deliberately does not imply any: servers are
    /// global installs the user should choose.
    pub lsp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticModelState {
    Ready,
    Installed,
    NotInstalled,
    DownloadFailed,
    Skipped,
    /// `--dry-run` or `--print-config`: a real run would download the
    /// model, but this one changes nothing on disk or over the network.
    WouldInstall,
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

/// Three-sentence protocol the agent sees on session start (PR 4 of
/// `docs/INTENT_AND_OBSERVABILITY_PLAN.md`). Installed by `lain setup
/// --agent claude` (and equivalents); the setup command writes
/// `PROMPT.md` under `.lain/` so the user can copy the snippet into
/// `CLAUDE.md` / `.cursorrules` / `AGENTS.md` (whatever their host
/// reads) without retyping it. The plain text matches the plan's
/// example verbatim — copy-paste between the docs is the source of
/// truth, and a typo here is a typo there.
pub const LAIN_INTENT_PROMPT: &str = "\
You are operating in a Lain-managed workspace. Lain coordinates
across agents via declared intent and automatic observation.

Before a substantial code change, declare your goal and the
scopes you intend to modify via `lain_intent`. Update the intent
when your scope materially changes. Do not report individual
reads or commands; Lain observes those through hooks.";

/// Filename for the prompt snippet under the workspace's `.lain/`
/// directory. Kept distinct from the existing `tuning.toml` /
/// `graph.bin` so it doesn't get clobbered by `lain init` or
/// accidentally picked up as configuration.
pub const PROMPT_FILENAME: &str = "PROMPT.md";

/// Write the intent protocol to `<workspace>/.lain/PROMPT.md` so the
/// user can copy it into their agent's startup-context file. The
/// `.lain/` directory is created if missing; the write is atomic
/// (same `write_file_atomic` helper as the state snapshot) so a
/// partial file can't be observed by a concurrent `lain mcp`
/// reading the snippet. Returns the path on success.
fn write_intent_prompt(workspace: &Path) -> Result<PathBuf, String> {
    let dir = workspace.join(".lain");
    crate::config::create_state_dir(&dir)
        .map_err(|e| format!("create_dir_all({}): {e}", dir.display()))?;
    let path = dir.join(PROMPT_FILENAME);
    crate::cli::io::write_file_atomic(&path, LAIN_INTENT_PROMPT.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

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

    // `dest.with_extension("part")` used to be a fixed path shared by
    // every invocation of `lain setup` targeting the same shared model
    // directory. Two processes downloading concurrently (a real scenario:
    // nothing serializes `lain setup --yes` runs) wrote to the same
    // `.part` file, and either one's failure cleanup could delete the
    // other's in-progress or just-completed download. Suffixing with this
    // process's PID makes the temp path unique per invocation; PIDs are
    // unique among processes actually running at the same time, which is
    // exactly the concurrency this needs to be safe against.
    let pid = std::process::id();
    let model_tmp = model_path.with_extension(format!("part-{pid}"));
    let tokenizer_tmp = tokenizer_path.with_extension(format!("part-{pid}"));

    let fetch = |url: &str, dest: &Path, tmp: &Path| -> Result<()> {
        let mut resp = client
            .get(url)
            .send()
            .with_context(|| format!("request {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!("{url} returned HTTP {}", resp.status()));
        }
        {
            let mut file =
                std::fs::File::create(tmp).with_context(|| format!("create {}", tmp.display()))?;
            resp.copy_to(&mut file)
                .with_context(|| format!("write body from {url}"))?;
        }
        std::fs::rename(tmp, dest)
            .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
        Ok(())
    };

    let result = fetch(MODEL_URL, &model_path, &model_tmp)
        .and_then(|_| fetch(TOKENIZER_URL, &tokenizer_path, &tokenizer_tmp));
    if let Err(e) = result {
        // Don't leave a half-downloaded model behind — the same
        // all-or-nothing rule `install.sh` follows. Only this
        // invocation's own files: never touch `model_path`/
        // `tokenizer_path` themselves here, since a concurrent
        // invocation may have already completed and renamed a good
        // file into place while this one was still downloading.
        let _ = std::fs::remove_file(&model_tmp);
        let _ = std::fs::remove_file(&tokenizer_tmp);
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
    // `--dry-run`/`--print-config` must change nothing, on disk or over
    // the network — checked before `opts.yes` so `--dry-run --yes` (or
    // `--print-config --yes`) can't fall through to a real ~90MB
    // download despite promising not to touch anything. This used to
    // reach `download_model()` below because neither flag was checked
    // at all here.
    if opts.dry_run || opts.print_config {
        return SemanticModelStatus {
            state: SemanticModelState::WouldInstall,
            model_path: None,
            detail: Some(
                "would download the optional semantic model (~90MB, \
                 sentence-transformers/all-MiniLM-L6-v2); re-run without \
                 --dry-run/--print-config to install it."
                    .into(),
            ),
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

/// A language found in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectedLanguage {
    name: String,
    /// Tracked files in this language; 0 when only a manifest was seen.
    files: usize,
    /// The extension used to look up its language server.
    ext: String,
}

/// Detect languages from the repository's tracked files, most files first.
/// Falls back to root-level manifests when git lists nothing (not a clone,
/// or nothing committed yet).
fn detect_languages(root: &Path) -> Vec<DetectedLanguage> {
    let tracked = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
        .unwrap_or_default();
    // language -> (files, ext -> count)
    let mut counts: std::collections::BTreeMap<
        &'static str,
        (usize, std::collections::BTreeMap<String, usize>),
    > = Default::default();
    for path in tracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path);
        let Some(ext) = Path::new(path.as_ref())
            .extension()
            .and_then(|e| e.to_str())
        else {
            continue;
        };
        let Some(name) = crate::server::treesitter::language_name(ext) else {
            continue;
        };
        let entry = counts.entry(name).or_default();
        entry.0 += 1;
        *entry.1.entry(ext.to_string()).or_default() += 1;
    }
    let mut found: Vec<DetectedLanguage> = counts
        .into_iter()
        .map(|(name, (files, exts))| DetectedLanguage {
            name: name.to_string(),
            files,
            // The most common extension, preferring one with a server.
            ext: exts
                .iter()
                .filter(|(e, _)| crate::server::lsp::language_server_for(e).is_some())
                .max_by_key(|(_, n)| **n)
                .or_else(|| exts.iter().max_by_key(|(_, n)| **n))
                .map(|(e, _)| e.clone())
                .unwrap_or_default(),
        })
        .collect();
    found.sort_by(|a, b| b.files.cmp(&a.files).then(a.name.cmp(&b.name)));
    if found.is_empty() {
        found = detect_languages_from_manifests(root);
    }
    found
}

/// Manifest presence at the root: a shallow hint for repositories git
/// cannot list yet.
fn detect_languages_from_manifests(root: &Path) -> Vec<DetectedLanguage> {
    let mut found: Vec<DetectedLanguage> = Vec::new();
    let checks: &[(&str, &str, &str)] = &[
        ("Cargo.toml", "Rust", "rs"),
        ("go.mod", "Go", "go"),
        ("pyproject.toml", "Python", "py"),
        ("setup.py", "Python", "py"),
        ("requirements.txt", "Python", "py"),
        ("pom.xml", "Java", "java"),
        ("build.gradle", "Java/Kotlin", "java"),
        ("build.gradle.kts", "Kotlin", "kt"),
        ("Gemfile", "Ruby", "rb"),
        ("composer.json", "PHP", "php"),
    ];
    for (file, lang, ext) in checks {
        if root.join(file).is_file() && !found.iter().any(|d| d.name == *lang) {
            found.push(DetectedLanguage {
                name: lang.to_string(),
                files: 0,
                ext: ext.to_string(),
            });
        }
    }
    if root.join("package.json").is_file() {
        let (name, ext) = if root.join("tsconfig.json").is_file() {
            ("TypeScript", "ts")
        } else {
            ("JavaScript", "js")
        };
        found.push(DetectedLanguage {
            name: name.to_string(),
            files: 0,
            ext: ext.to_string(),
        });
    }
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageServerState {
    /// Already on PATH.
    Installed,
    /// Not on PATH and not selected. The built-in parser still covers it.
    NotInstalled,
    /// Installed by this run.
    InstalledNow,
    /// Selected, but `--dry-run` / `--print-config` changes nothing.
    WouldInstall,
    /// Selected, and the install command failed.
    InstallFailed,
    /// Selected, but there is no command Lain can run here (no automated
    /// installer, or Homebrew off macOS).
    NoInstaller,
}

#[derive(Debug, Serialize)]
pub struct LanguageServerStatus {
    pub language: String,
    pub files: usize,
    /// Always `built_in`: every language Lain detects has a compiled-in
    /// parser, so the server below is never required.
    pub parser: &'static str,
    pub server: &'static str,
    pub state: LanguageServerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_cmd: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Report each detected language's optional server and install the ones the
/// user selects (`--lsp`, or an interactive pick). Never installs anything
/// the user did not choose.
fn resolve_language_servers(
    opts: &SetupOptions,
    detected: &[DetectedLanguage],
) -> Result<Vec<LanguageServerStatus>> {
    let mut statuses: Vec<(crate::server::lsp::LanguageServer, LanguageServerStatus)> = Vec::new();
    let mut push = |name: &str, files: usize, server: crate::server::lsp::LanguageServer| {
        if statuses.iter().any(|(s, _)| s.binary == server.binary) {
            return;
        }
        statuses.push((
            server,
            LanguageServerStatus {
                language: name.to_string(),
                files,
                parser: "built_in",
                server: server.binary,
                state: if server.is_installed() {
                    LanguageServerState::Installed
                } else {
                    LanguageServerState::NotInstalled
                },
                // Only a command that can run on this machine: offering
                // `brew install llvm` on Linux would be advice that fails.
                install_cmd: server.install_argv().ok().and(server.install_cmd),
                detail: None,
            },
        ));
    };
    for lang in detected {
        if let Some(server) = crate::server::lsp::language_server_for(&lang.ext) {
            push(&lang.name, lang.files, server);
        }
    }

    // Which binaries to install.
    let selected: Vec<&'static str> = match opts.lsp.as_deref().map(str::trim) {
        Some("none") | Some("") => Vec::new(),
        Some("detected") | Some("all") => statuses
            .iter()
            .filter(|(_, st)| st.state == LanguageServerState::NotInstalled)
            .map(|(s, _)| s.binary)
            .collect(),
        Some(list) => {
            let mut picked = Vec::new();
            for item in list.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                let server = crate::server::lsp::language_server_for(item).ok_or_else(|| {
                    anyhow!(
                        "--lsp: no language server known for '{item}'; use a language \
                         (python, go, typescript, ...) or an extension (py, go, ts, ...)"
                    )
                })?;
                // A language the repo doesn't (yet) contain is still a valid pick.
                let name = crate::server::treesitter::language_name(item.trim_start_matches('.'))
                    .unwrap_or(item);
                push(name, 0, server);
                picked.push(server.binary);
            }
            picked
        }
        // `--yes` means "ask nothing", and servers are never installed
        // unasked, so it selects none rather than stopping at the prompt.
        None if opts.json || opts.print_config || opts.yes || !is_stdin_tty() => Vec::new(),
        None => {
            let missing: Vec<&LanguageServerStatus> = statuses
                .iter()
                .map(|(_, st)| st)
                .filter(|st| st.state == LanguageServerState::NotInstalled)
                .collect();
            prompt_language_servers(&missing)
        }
    };

    for (server, status) in statuses.iter_mut() {
        if !selected.contains(&server.binary) || status.state == LanguageServerState::Installed {
            continue;
        }
        let argv = match server.install_argv() {
            Ok(argv) => argv,
            Err(e) => {
                status.state = LanguageServerState::NoInstaller;
                status.detail = Some(e.to_string());
                continue;
            }
        };
        if opts.dry_run || opts.print_config {
            status.state = LanguageServerState::WouldInstall;
            continue;
        }
        if !opts.json {
            println!("  Installing {} ({})…", server.binary, argv.join(" "));
        }
        let mut cmd = Command::new(argv[0]);
        cmd.args(&argv[1..]).stdin(Stdio::null());
        if opts.json {
            // Keep stdout for the JSON report.
            cmd.stdout(Stdio::null()).stderr(Stdio::piped());
        }
        match cmd.output() {
            Ok(out) if out.status.success() && server.is_installed() => {
                status.state = LanguageServerState::InstalledNow;
            }
            Ok(out) => {
                status.state = LanguageServerState::InstallFailed;
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
                status.detail = Some(if out.status.success() {
                    format!("install finished but {} is not on PATH", server.binary)
                } else if tail.is_empty() {
                    format!("`{}` exited with {}", argv.join(" "), out.status)
                } else {
                    tail
                });
            }
            Err(e) => {
                status.state = LanguageServerState::InstallFailed;
                status.detail = Some(format!("could not run `{}`: {e}", argv[0]));
            }
        }
    }
    Ok(statuses.into_iter().map(|(_, st)| st).collect())
}

/// Ask which missing servers to install. Enter (the default) installs none.
fn prompt_language_servers(missing: &[&LanguageServerStatus]) -> Vec<&'static str> {
    if missing.is_empty() {
        return Vec::new();
    }
    println!();
    println!("  Language servers are optional: the built-in parsers already index every");
    println!("  language below. A server adds precision on top.");
    for (i, st) in missing.iter().enumerate() {
        println!(
            "    {}) {:<12} {:<28} {}",
            i + 1,
            st.language,
            st.server,
            st.install_cmd
                .unwrap_or("(install manually with your package manager)")
        );
    }
    print!("  Install which? Numbers (e.g. 1,3), \"all\", or Enter to skip: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Vec::new();
    }
    parse_server_selection(&line, missing)
}

fn parse_server_selection(input: &str, missing: &[&LanguageServerStatus]) -> Vec<&'static str> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return missing.iter().map(|st| st.server).collect();
    }
    let mut picked = Vec::new();
    for n in input
        .split([',', ' '])
        .filter_map(|t| t.trim().parse::<usize>().ok())
    {
        if let Some(st) = n.checked_sub(1).and_then(|i| missing.get(i)) {
            if !picked.contains(&st.server) {
                picked.push(st.server);
            }
        }
    }
    picked
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
    // Second-level precision alone collides on repeated runs within the
    // same second (e.g. a test suite, or an agent retrying setup), and
    // `std::fs::copy` silently overwrites its destination -- the second
    // run's "backup" would actually destroy the first run's backup of
    // the user's original configuration. Append a numeric suffix once a
    // timestamped name is already taken, trying until a free one is
    // found, so no run's backup is ever lost.
    let mut backup = path.with_file_name(format!("{file_name}.bak-{ts}"));
    let mut suffix = 1u32;
    while backup.exists() {
        backup = path.with_file_name(format!("{file_name}.bak-{ts}-{suffix}"));
        suffix += 1;
    }
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
    // A re-run that changes nothing leaves the file (and the repo root)
    // alone instead of piling up identical backups.
    if std::fs::read_to_string(&config_path).is_ok_and(|old| old.trim_end() == pretty.trim_end()) {
        return ConfigurationOutcome {
            agent: "generic".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: Some("already configured; nothing changed".into()),
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

/// A `claude` invocation run from the workspace: local- and
/// project-scope registrations belong to the directory `claude` runs in,
/// so running it from the caller's cwd would register the wrong project
/// when `--workspace` points elsewhere.
fn claude_command(root: &Path) -> Command {
    let mut cmd = Command::new("claude");
    cmd.current_dir(root);
    cmd
}

/// The `--scope` value of an existing registration, read from `claude mcp
/// get`'s "Scope: User config (…)" line. Re-registering must keep it: the
/// installer registers at user scope, and a scope-less `add` would
/// silently narrow that to this one project.
fn claude_scope_of(get_output: &str) -> Option<&'static str> {
    let line = get_output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Scope:"))?
        .trim()
        .to_ascii_lowercase();
    if line.starts_with("user") {
        Some("user")
    } else if line.starts_with("project") {
        Some("project")
    } else if line.starts_with("local") {
        Some("local")
    } else {
        None
    }
}

fn claude_mcp_configured(root: &Path, name: &str) -> bool {
    claude_command(root)
        .args(["mcp", "get", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn configure_claude_code(
    root: &Path,
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

    let already_configured = claude_mcp_configured(root, "lain");
    // Captured before anything is removed: it carries the scope to keep,
    // and it is the only record of the old entry if the new `add` fails.
    let previous_config = if already_configured {
        claude_command(root)
            .args(["mcp", "get", "lain"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    };
    let scope = previous_config.as_deref().and_then(claude_scope_of);

    let mut add_args: Vec<String> = vec!["mcp".into(), "add".into()];
    if let Some(scope) = scope {
        add_args.push("--scope".into());
        add_args.push(scope.into());
    }
    add_args.push("lain".into());
    if let Some(model) = model {
        add_args.push("-e".into());
        add_args.push(format!("LAIN_EMBEDDING_MODEL={}", model.display()));
    }
    add_args.push("--".into());
    add_args.push(exe.display().to_string());
    add_args.push("mcp".into());
    let command_line = format!("claude {}", add_args.join(" "));

    if opts.print_config {
        // PR 4: surface the intent protocol alongside the MCP
        // command so the operator can copy both pieces into the
        // agent's startup context in one go.
        println!("# MCP command:\n{command_line}\n");
        println!("# System-prompt snippet (copy into CLAUDE.md / .cursorrules / AGENTS.md):\n{LAIN_INTENT_PROMPT}");
        return ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Printed,
            target: Some("claude mcp".into()),
            detail: Some(command_line),
        };
    }

    let remove_args: Vec<String> = match scope {
        Some(scope) => vec![
            "mcp".into(),
            "remove".into(),
            "--scope".into(),
            scope.into(),
            "lain".into(),
        ],
        None => vec!["mcp".into(), "remove".into(), "lain".into()],
    };
    if opts.dry_run {
        let plan = if already_configured {
            format!("claude {} && {command_line}", remove_args.join(" "))
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
    //
    // Not transactional the way an atomic file replace would be — the
    // `claude` CLI's own config storage is opaque, so there is no
    // "write a new file, then rename" move available here. But it must
    // not go straight from "working entry" to "no entry, and the add
    // then also failed" with the previous configuration lost: capture
    // `claude mcp get`'s output before removing, and if the replacement
    // `add` fails, surface it so the user can restore by hand instead
    // of having to remember or reconstruct what they had.
    if already_configured {
        let _ = claude_command(root)
            .args(&remove_args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    match claude_command(root).args(&add_args).output() {
        Ok(out) if out.status.success() => {
            // PR 4: drop the intent protocol into `.lain/PROMPT.md`
            // alongside the existing setup artifacts. The user copies
            // it into `CLAUDE.md` / `.cursorrules` / `AGENTS.md` (or
            // whichever file their agent host reads); we don't try to
            // write that file because the host-specific location is
            // outside Lain's purview. Best-effort: a write failure
            // here doesn't unwind the MCP registration that already
            // succeeded — the operator can re-run `--agent claude
            // --print-config` to recover the snippet.
            let prompt_detail = match write_intent_prompt(root) {
                Ok(path) => Some(format!("wrote intent protocol to {}", path.display())),
                Err(e) => Some(format!(
                    "MCP configured, but PROMPT.md write failed: {e}. \
                     Re-run with --print-config to recover the snippet."
                )),
            };
            ConfigurationOutcome {
                agent: "claude-code".into(),
                state: ConfigurationState::Configured,
                target: Some("claude mcp".into()),
                detail: prompt_detail,
            }
        }
        Ok(out) => {
            let add_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
            ConfigurationOutcome {
                agent: "claude-code".into(),
                state: ConfigurationState::Failed,
                target: Some("claude mcp".into()),
                detail: Some(failed_replacement_detail(add_error, previous_config)),
            }
        }
        Err(e) => ConfigurationOutcome {
            agent: "claude-code".into(),
            state: ConfigurationState::Failed,
            target: Some("claude mcp".into()),
            detail: Some(failed_replacement_detail(e.to_string(), previous_config)),
        },
    }
}

/// Build the failure message for a `claude mcp add` that failed after an
/// existing `lain` entry was already removed to make way for it. When
/// `previous_config` is `Some` (there was something to lose), the
/// message includes it verbatim so the user can restore it by hand —
/// silently dropping it here was the bug (PR #63 review): the CLI's own
/// config storage is opaque, so this text is the only record of what
/// used to be there. `previous_config` is `None` both when there was no
/// prior entry and when capturing it failed; either way there's nothing
/// to show.
fn failed_replacement_detail(add_error: String, previous_config: Option<String>) -> String {
    match previous_config {
        Some(prev) if !prev.is_empty() => format!(
            "{add_error}\n\nThe previous `lain` entry was removed to make way for this one \
             and could not be restored automatically. Its configuration was:\n{prev}"
        ),
        _ => add_error,
    }
}

// ─── Codex (M8) ─────────────────────────────────────────────────────────────
//
// Codex has its own CLI for safe MCP config editing
// (`codex mcp add <name> -- <command> [args...]`). Use it when
// available; fall back to a direct edit of `~/.codex/config.toml`
// when the CLI is missing so the adapter still works in CI or
// minimal installs.

/// `true` if the `codex` CLI is invocable on `PATH`.
fn codex_cli_available() -> bool {
    #[cfg(test)]
    if std::env::var_os("LAIN_TEST_DISABLE_CODEX_CLI").is_some() {
        return false;
    }
    Command::new("codex")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve `~/.codex/config.toml`. Honours `$CODEX_HOME` first so a
/// CI matrix or a sandboxed dev box can override the path without
/// mutating the user's real config dir.
fn codex_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("CODEX_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p).join("config.toml");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".codex/config.toml")
}

/// Build the `[mcp_servers.lain]` table entry Codex expects.
fn build_codex_entry(exe: &Path, model: Option<&Path>) -> Value {
    let mut entry = json!({
        "command": exe.display().to_string(),
        "args": ["mcp"],
    });
    if let Some(model) = model {
        entry["env"] = json!({ "LAIN_EMBEDDING_MODEL": model.display().to_string() });
    }
    entry
}

/// Merge `entry` into `[mcp_servers.<server_name>]` of Codex's
/// `config.toml`, returning the new file text. Edits the document in
/// place with `toml_edit`, so the user's comments, key order and
/// formatting survive; only the `lain` table is replaced. Refuses
/// (rather than silently rewrites) a file that isn't valid TOML — same
/// contract as the JSON adapters.
fn merge_codex_toml(path: &Path, server_name: &str, entry: Value) -> Result<String> {
    let text = if path.is_file() {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };
    let mut doc = text.parse::<toml_edit::DocumentMut>().with_context(|| {
        format!(
            "{} contains invalid TOML; nothing was changed",
            path.display()
        )
    })?;
    // `serde_json::Value` -> TOML goes through the `toml` crate, whose
    // output `toml_edit` then parses as a table item to splice in.
    let toml_entry: toml::Value = toml::Value::try_from(&entry)
        .map_err(|e| anyhow!("could not convert codex entry to TOML: {e}"))?;
    let mut wrapper = toml::map::Map::new();
    wrapper.insert("entry".to_string(), toml_entry);
    let fragment = toml::to_string(&toml::Value::Table(wrapper))
        .map_err(|e| anyhow!("could not render codex entry: {e}"))?;
    let mut fragment = fragment
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| anyhow!("could not render codex entry: {e}"))?;
    let entry_item = fragment
        .remove("entry")
        .ok_or_else(|| anyhow!("could not render codex entry"))?;

    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    let Some(servers_tbl) = servers.as_table_like_mut() else {
        return Err(anyhow!(
            "{}'s `mcp_servers` key is not a TOML table; nothing was changed",
            path.display()
        ));
    };
    servers_tbl.insert(server_name, entry_item);
    Ok(doc.to_string())
}

fn configure_codex(
    _root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let config_path = codex_config_path();
    let target = config_path.display().to_string();

    // Prefer the CLI when available; fall back to a direct TOML edit.
    if codex_cli_available() {
        // `--env` is Codex's option and must come before `--`; anything
        // after it is the server's own command line.
        let mut add_args: Vec<String> = vec!["mcp".into(), "add".into(), "lain".into()];
        if let Some(m) = model {
            add_args.push("--env".into());
            add_args.push(format!("LAIN_EMBEDDING_MODEL={}", m.display()));
        }
        add_args.extend(["--".into(), exe.display().to_string(), "mcp".into()]);
        let command_line = format!("codex {}", add_args.join(" "));

        if opts.print_config {
            println!("{command_line}");
            return ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Printed,
                target: Some("codex mcp".into()),
                detail: Some(command_line),
            };
        }
        if opts.dry_run {
            return ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::WouldConfigure,
                target: Some("codex mcp".into()),
                detail: Some(command_line),
            };
        }
        return match Command::new("codex").args(&add_args).output() {
            Ok(out) if out.status.success() => ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Configured,
                target: Some("codex mcp".into()),
                detail: None,
            },
            Ok(out) => ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Failed,
                target: Some("codex mcp".into()),
                detail: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            },
            Err(e) => ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Failed,
                target: Some("codex mcp".into()),
                detail: Some(e.to_string()),
            },
        };
    }

    // Fallback: direct TOML edit.
    let entry_json = build_codex_entry(exe, model);
    let merged = match merge_codex_toml(&config_path, "lain", entry_json) {
        Ok(v) => v,
        Err(e) => {
            return ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Failed,
                target: Some(target),
                detail: Some(format!("{e:#}")),
            }
        }
    };
    let pretty = merged;

    if opts.print_config {
        println!("{pretty}");
        return ConfigurationOutcome {
            agent: "codex".into(),
            state: ConfigurationState::Printed,
            target: Some(target),
            detail: None,
        };
    }
    if opts.dry_run {
        return ConfigurationOutcome {
            agent: "codex".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some(target),
            detail: Some(pretty),
        };
    }
    // A re-run that changes nothing leaves the file (and the repo root)
    // alone instead of piling up identical backups.
    if std::fs::read_to_string(&config_path).is_ok_and(|old| old.trim_end() == pretty.trim_end()) {
        return ConfigurationOutcome {
            agent: "codex".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: Some("already configured; nothing changed".into()),
        };
    }
    if config_path.is_file() {
        if let Err(e) = backup_file(&config_path) {
            return ConfigurationOutcome {
                agent: "codex".into(),
                state: ConfigurationState::Failed,
                target: Some(target),
                detail: Some(format!(
                    "backup before write failed: {e:#}; nothing was changed"
                )),
            };
        }
    }
    match write_file_atomic(&config_path, pretty) {
        Ok(()) => ConfigurationOutcome {
            agent: "codex".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: None,
        },
        Err(e) => ConfigurationOutcome {
            agent: "codex".into(),
            state: ConfigurationState::Failed,
            target: Some(target),
            detail: Some(e.to_string()),
        },
    }
}

// ─── Cursor (M8) ─────────────────────────────────────────────────────────────
//
// Cursor reads `~/.cursor/mcp.json` directly — no stable CLI. The
// JSON shape matches the generic adapter (`mcpServers` map, name-keyed
// entries); the only difference is the file path. Reusing
// `merge_mcp_json` keeps the preservation-of-other-settings contract
// consistent with the other JSON adapters.

fn cursor_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".cursor/mcp.json")
}

fn configure_cursor(
    _root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let config_path = cursor_config_path();
    let target = config_path.display().to_string();
    let entry = build_mcp_server_entry(exe, model);
    let merged = match merge_mcp_json(&config_path, "lain", entry) {
        Ok(v) => v,
        Err(e) => {
            return ConfigurationOutcome {
                agent: "cursor".into(),
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
            agent: "cursor".into(),
            state: ConfigurationState::Printed,
            target: Some(target),
            detail: None,
        };
    }
    if opts.dry_run {
        return ConfigurationOutcome {
            agent: "cursor".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some(target),
            detail: Some(pretty),
        };
    }
    // A re-run that changes nothing leaves the file (and the repo root)
    // alone instead of piling up identical backups.
    if std::fs::read_to_string(&config_path).is_ok_and(|old| old.trim_end() == pretty.trim_end()) {
        return ConfigurationOutcome {
            agent: "cursor".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: Some("already configured; nothing changed".into()),
        };
    }
    if config_path.is_file() {
        if let Err(e) = backup_file(&config_path) {
            return ConfigurationOutcome {
                agent: "cursor".into(),
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
            agent: "cursor".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: None,
        },
        Err(e) => ConfigurationOutcome {
            agent: "cursor".into(),
            state: ConfigurationState::Failed,
            target: Some(target),
            detail: Some(e.to_string()),
        },
    }
}

// ─── VS Code (M8) ───────────────────────────────────────────────────────────
//
// VS Code reads `.vscode/mcp.json` (project-scoped) or `mcp.json`
// (user-scoped). Project-scoped takes precedence when present — a
// developer might have committed `.vscode/mcp.json` deliberately,
// and silently overwriting it would surprise them.

fn vscode_user_config_path() -> Option<PathBuf> {
    // `dirs::config_dir()` returns the per-user config root
    // (`$XDG_CONFIG_HOME` / `~/Library/Application Support` /
    // `%APPDATA%`). VS Code lives in a `Code/User/` subdir on each
    // platform — see
    // https://code.visualstudio.com/docs/configs — and uses `mcp.json`
    // for the modern MCP config.
    let cfg = dirs::config_dir()?;
    Some(cfg.join("Code").join("User").join("mcp.json"))
}

fn vscode_resolve_target(workspace_root: &Path) -> (PathBuf, bool) {
    let project_path = workspace_root.join(".vscode").join("mcp.json");
    if project_path.is_file() {
        (project_path, true)
    } else if let Some(user_path) = vscode_user_config_path() {
        (user_path, false)
    } else {
        // No user dir available (extremely rare; sandbox without
        // HOME). Fall back to the project-scoped path even though
        // it doesn't exist yet — the write step will create it.
        (project_path, true)
    }
}

fn configure_vscode(
    root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let (config_path, project_scoped) = vscode_resolve_target(root);
    let target = config_path.display().to_string();

    // VS Code's modern MCP config uses `servers` (not `mcpServers`)
    // and requires an explicit `"type": "stdio"`. Reusing
    // `merge_mcp_json` works because it operates on a generic object;
    // we just point it at the `servers` key instead of
    // `mcpServers`.
    let mut entry = json!({
        "type": "stdio",
        "command": exe.display().to_string(),
        "args": ["mcp"],
    });
    if let Some(model) = model {
        entry["env"] = json!({ "LAIN_EMBEDDING_MODEL": model.display().to_string() });
    }
    let merged = match merge_vscode_json(&config_path, "lain", entry) {
        Ok(v) => v,
        Err(e) => {
            return ConfigurationOutcome {
                agent: "vscode".into(),
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
            agent: "vscode".into(),
            state: ConfigurationState::Printed,
            target: Some(target),
            detail: if project_scoped {
                Some("project-scoped".to_string())
            } else {
                None
            },
        };
    }
    if opts.dry_run {
        return ConfigurationOutcome {
            agent: "vscode".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some(target),
            detail: Some(pretty),
        };
    }
    // A re-run that changes nothing leaves the file (and the repo root)
    // alone instead of piling up identical backups.
    if std::fs::read_to_string(&config_path).is_ok_and(|old| old.trim_end() == pretty.trim_end()) {
        return ConfigurationOutcome {
            agent: "vscode".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: Some("already configured; nothing changed".into()),
        };
    }
    if config_path.is_file() {
        if let Err(e) = backup_file(&config_path) {
            return ConfigurationOutcome {
                agent: "vscode".into(),
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
            agent: "vscode".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: if project_scoped {
                Some("project-scoped".to_string())
            } else {
                None
            },
        },
        Err(e) => ConfigurationOutcome {
            agent: "vscode".into(),
            state: ConfigurationState::Failed,
            target: Some(target),
            detail: Some(e.to_string()),
        },
    }
}

/// Merge `entry` into `path`'s top-level `"servers"` object (VS
/// Code's modern MCP config location), preserving every other key.
/// Same preservation contract as the JSON adapters.
fn merge_vscode_json(path: &Path, server_name: &str, entry: Value) -> Result<Value> {
    let mut root: Value = if path.is_file() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        // VS Code's `mcp.json` is JSONC: comments and trailing commas
        // are legal there, so accept them rather than refuse the file.
        serde_json::from_str(&strip_jsonc(&text)).with_context(|| {
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
    let servers = root_obj.entry("servers").or_insert_with(|| json!({}));
    let Some(servers_obj) = servers.as_object_mut() else {
        return Err(anyhow!(
            "{}'s \"servers\" key is not an object; nothing was changed",
            path.display()
        ));
    };
    servers_obj.insert(server_name.to_string(), entry);
    Ok(root)
}

/// Plain JSON from JSONC: drops `//` and `/* */` comments outside
/// strings and commas that directly precede `}` or `]`.
fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == '"' {
                    break;
                }
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == ',' {
            // A trailing comma: the next significant character closes
            // the object or array. Comments in between are skipped by
            // the main loop, so look past whitespace and comments here.
            let mut j = i + 1;
            loop {
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                    while j < chars.len() && chars[j] != '\n' {
                        j += 1;
                    }
                } else if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                    j += 2;
                    while j < chars.len() && !(chars[j] == '*' && chars.get(j + 1) == Some(&'/')) {
                        j += 1;
                    }
                    j += 2;
                } else {
                    break;
                }
            }
            if !matches!(chars.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

// ─── Continue (M8) ───────────────────────────────────────────────────────────
//
// Continue's current config is YAML (`~/.continue/config.yaml`), and it
// also loads standalone block files from `<workspace>/.continue/mcpServers/`.
// When the user is on YAML (or has no Continue config yet) we write a
// `lain.yaml` block there — no rewrite of the user's own file, so its
// comments survive. Only a legacy `config.json` setup (no `config.yaml`)
// gets the JSON edit: `experimental.modelContextProtocolServers` is an
// array of `{ "transport": { "type": "stdio", command, args, env } }`,
// and the apply step replaces any existing `lain` entry. Same atomic-write
// / backup contract as the other JSON adapters.

fn continue_yaml_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".continue/config.yaml")
}

fn continue_block_path(root: &Path) -> PathBuf {
    root.join(".continue/mcpServers/lain.yaml")
}

/// The workspace block file. Strings are JSON-quoted, which YAML reads
/// as double-quoted scalars, so paths with spaces or colons are safe.
fn build_continue_block(exe: &Path, model: Option<&Path>) -> String {
    let q = |s: String| serde_json::to_string(&s).unwrap_or_default();
    let mut out = format!(
        "name: Lain\nversion: 0.0.1\nschema: v1\nmcpServers:\n  - name: lain\n    command: {}\n    args: [\"mcp\"]\n",
        q(exe.display().to_string())
    );
    if let Some(model) = model {
        out.push_str(&format!(
            "    env:\n      LAIN_EMBEDDING_MODEL: {}\n",
            q(model.display().to_string())
        ));
    }
    out
}

/// Whether an entry in the legacy array is Lain's: ours carry
/// `"name": "lain"`; entries written before the schema fix had the
/// command at the top level.
fn is_lain_continue_entry(s: &Value, server_name: &str) -> bool {
    s.get("name").and_then(|n| n.as_str()) == Some(server_name)
}

fn continue_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".continue/config.json")
}

fn merge_continue_json(path: &Path, server_name: &str, entry: Value) -> Result<Value> {
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
    let experimental = root_obj.entry("experimental").or_insert_with(|| json!({}));
    let Some(experimental_obj) = experimental.as_object_mut() else {
        return Err(anyhow!(
            "{}'s \"experimental\" key is not an object; nothing was changed",
            path.display()
        ));
    };
    let servers = experimental_obj
        .entry("modelContextProtocolServers")
        .or_insert_with(|| json!([]));
    let Some(servers_arr) = servers.as_array_mut() else {
        return Err(anyhow!(
            "{}'s `experimental.modelContextProtocolServers` is not an array; \
             nothing was changed",
            path.display()
        ));
    };
    // Dedup by `name`: remove any existing entry for this server
    // before appending the new one. The roadmap's preservation
    // contract is "leave every other key untouched" — we leave
    // other servers in the array intact, just remove this one's
    // prior version.
    servers_arr.retain(|s| !is_lain_continue_entry(s, server_name));
    servers_arr.push(entry);
    Ok(root)
}

fn build_continue_entry(exe: &Path, model: Option<&Path>) -> Value {
    let mut transport = json!({
        "type": "stdio",
        "command": exe.display().to_string(),
        "args": ["mcp"],
    });
    if let Some(model) = model {
        transport["env"] = json!({ "LAIN_EMBEDDING_MODEL": model.display().to_string() });
    }
    json!({ "name": "lain", "transport": transport })
}

fn configure_continue(
    root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let config_path = continue_config_path();
    if continue_yaml_path().is_file() || !config_path.is_file() {
        return configure_continue_block(root, exe, model, opts);
    }
    let target = config_path.display().to_string();
    let entry = build_continue_entry(exe, model);
    let merged = match merge_continue_json(&config_path, "lain", entry) {
        Ok(v) => v,
        Err(e) => {
            return ConfigurationOutcome {
                agent: "continue".into(),
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
            agent: "continue".into(),
            state: ConfigurationState::Printed,
            target: Some(target),
            detail: None,
        };
    }
    if opts.dry_run {
        return ConfigurationOutcome {
            agent: "continue".into(),
            state: ConfigurationState::WouldConfigure,
            target: Some(target),
            detail: Some(pretty),
        };
    }
    // A re-run that changes nothing leaves the file (and the repo root)
    // alone instead of piling up identical backups.
    if std::fs::read_to_string(&config_path).is_ok_and(|old| old.trim_end() == pretty.trim_end()) {
        return ConfigurationOutcome {
            agent: "continue".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: Some("already configured; nothing changed".into()),
        };
    }
    if config_path.is_file() {
        if let Err(e) = backup_file(&config_path) {
            return ConfigurationOutcome {
                agent: "continue".into(),
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
            agent: "continue".into(),
            state: ConfigurationState::Configured,
            target: Some(target),
            detail: None,
        },
        Err(e) => ConfigurationOutcome {
            agent: "continue".into(),
            state: ConfigurationState::Failed,
            target: Some(target),
            detail: Some(e.to_string()),
        },
    }
}

fn configure_continue_block(
    root: &Path,
    exe: &Path,
    model: Option<&Path>,
    opts: &SetupOptions,
) -> ConfigurationOutcome {
    let path = continue_block_path(root);
    let target = path.display().to_string();
    let block = build_continue_block(exe, model);
    let outcome = |state, detail| ConfigurationOutcome {
        agent: "continue".into(),
        state,
        target: Some(target.clone()),
        detail,
    };
    if opts.print_config {
        println!("{block}");
        return outcome(ConfigurationState::Printed, None);
    }
    if opts.dry_run {
        return outcome(ConfigurationState::WouldConfigure, Some(block));
    }
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return outcome(ConfigurationState::Failed, Some(e.to_string()));
        }
    }
    match write_file_atomic(&path, block) {
        Ok(()) => outcome(ConfigurationState::Configured, None),
        Err(e) => outcome(ConfigurationState::Failed, Some(e.to_string())),
    }
}

fn prompt_agent_choice() -> String {
    println!();
    println!("  Choose an agent (Enter for the marked one)");
    println!("  › 1) Claude Code");
    println!("    2) Codex");
    println!("    3) Cursor");
    println!("    4) VS Code");
    println!("    5) Continue");
    println!("    6) Generic MCP");
    // Enter picks the marked default. It used to pick Generic — as did any
    // unrecognised answer, silently.
    for _ in 0..3 {
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break; // EOF: take the default
        }
        if let Some(agent) = agent_from_answer(&line) {
            return agent.to_string();
        }
        println!("  Type 1-6 or a name (claude, codex, cursor, vscode, continue, generic).");
    }
    "claude-code".to_string()
}

fn agent_from_answer(answer: &str) -> Option<&'static str> {
    Some(match answer.trim().to_ascii_lowercase().as_str() {
        "" | "1" | "claude" | "claude-code" | "claude code" => "claude-code",
        "2" | "codex" => "codex",
        "3" | "cursor" => "cursor",
        "4" | "vscode" | "vs code" => "vscode",
        "5" | "continue" => "continue",
        "6" | "generic" | "generic mcp" => "generic",
        _ => return None,
    })
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
    use crate::cli::mcp_stdio::{initialize_request, StdioSession};

    let mut cmd = Command::new(exe);
    cmd.arg("mcp").arg("--workspace").arg(root);
    if let Some(model) = model {
        cmd.env("LAIN_EMBEDDING_MODEL", model);
    }
    let mut session = match StdioSession::spawn(cmd, false) {
        Ok(s) => s,
        Err(e) => {
            return VerificationOutcome {
                healthy: false,
                tools_count: None,
                detail: Some(format!("spawn failed: {e}")),
            }
        }
    };

    let init = initialize_request(1, "lain-setup");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
    if let Err(e) = session.send(&init).and_then(|_| session.send(&list)) {
        session.shutdown();
        return VerificationOutcome {
            healthy: false,
            tools_count: None,
            detail: Some(format!("failed to write to lain mcp stdin: {e}")),
        };
    }

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
        match session.recv_timeout(remaining) {
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
    session.shutdown();
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
    pub language_servers: Vec<LanguageServerStatus>,
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

    // Settle the agent first: an unknown `--agent` used to fail only after
    // language servers were installed and the model download started.
    let agent = match &opts.agent {
        Some(a) => a.clone(),
        None if !opts.json && is_stdin_tty() => prompt_agent_choice(),
        None => "generic".to_string(),
    };
    // Accept common shorthand names as aliases for the canonical
    // agent kind. `claude` is what most operators type (the product
    // is called "Claude Code"); `claude-code` is the canonical
    // identifier the rest of the codebase uses. Normalizing once
    // here keeps the dispatch table below uniform.
    let agent = match agent.as_str() {
        "claude" => "claude-code".to_string(),
        other => other.to_string(),
    };
    if !matches!(
        agent.as_str(),
        "generic" | "claude-code" | "codex" | "cursor" | "vscode" | "continue",
    ) {
        return Err(anyhow!(
            "unknown --agent '{agent}'; supported values: \
             generic, claude-code, codex, cursor, vscode, continue"
        ));
    }

    let doctor_report = doctor::build_report(Some(&root))?;
    let detected = detect_languages(&root);
    let semantic = resolve_semantic_model(&opts);
    let language_servers = resolve_language_servers(&opts, &detected)?;
    let languages: Vec<String> = detected.into_iter().map(|d| d.name).collect();
    let exe = std::env::current_exe().context("locate current lain binary")?;

    let configuration = match agent.as_str() {
        "claude-code" => configure_claude_code(&root, &exe, semantic.model_path.as_deref(), &opts),
        "codex" => configure_codex(&root, &exe, semantic.model_path.as_deref(), &opts),
        "cursor" => configure_cursor(&root, &exe, semantic.model_path.as_deref(), &opts),
        "vscode" => configure_vscode(&root, &exe, semantic.model_path.as_deref(), &opts),
        "continue" => configure_continue(&root, &exe, semantic.model_path.as_deref(), &opts),
        // "generic" — the fallthrough.
        _ => configure_generic(&root, &exe, semantic.model_path.as_deref(), &opts),
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
        language_servers,
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
        println!(
            "  Languages         ✓ {} (built-in parsers)",
            report.languages.join(", ")
        );
    }
    for st in &report.language_servers {
        let (mark, state) = match st.state {
            LanguageServerState::Installed => ("✓", "installed"),
            LanguageServerState::InstalledNow => ("✓", "installed now"),
            LanguageServerState::NotInstalled => ("○", "optional, not installed"),
            LanguageServerState::WouldInstall => ("○", "would install (dry run)"),
            LanguageServerState::InstallFailed => ("×", "install failed"),
            LanguageServerState::NoInstaller => ("○", "install manually"),
        };
        println!(
            "  {:<17} {mark} {} {state} ({})",
            "", st.server, st.language
        );
        if let Some(detail) = &st.detail {
            println!("  {:<19} {detail}", "");
        }
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
        SemanticModelState::WouldInstall => ("○", "would install (dry run)".to_string()),
    };
    println!("  Semantic search   {mark} {label}");
    println!();
    let agent_label = match report.configuration.agent.as_str() {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "vscode" => "VS Code",
        "continue" => "Continue",
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
    let indexed = matches!(
        report.capabilities.symbols.state,
        crate::server::readiness::CapabilityState::Ready
            | crate::server::readiness::CapabilityState::StaleUsable
    );
    if report.ready && indexed {
        println!("  Ready. Ask your agent a question about this repository.");
    } else if report.ready {
        // Setup configures the agent; the index is built when the agent
        // first starts Lain. "Ready" here read as a contradiction of
        // `lain doctor`, which says the index is missing.
        println!("  Configured. Lain indexes this repository when your agent first starts it");
        println!("  (or run `lain oneshot find_anchors` now); `lain doctor` shows progress.");
    } else {
        println!("  Setup did not complete. See the messages above.");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_answers() {
        assert_eq!(agent_from_answer("\n"), Some("claude-code"));
        assert_eq!(agent_from_answer("claude"), Some("claude-code"));
        assert_eq!(agent_from_answer(" 6 "), Some("generic"));
        assert_eq!(agent_from_answer("Codex"), Some("codex"));
        assert_eq!(agent_from_answer("bogus"), None);
    }

    #[test]
    fn strip_jsonc_drops_comments_and_trailing_commas_only() {
        let text = r#"{
  // user comment
  "servers": { "x": { "url": "http://a//b", "s": "q\"/*not*/" }, }, /* tail */
  "list": [1, 2, ],
}"#;
        let v: Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(v["servers"]["x"]["url"], "http://a//b");
        assert_eq!(v["servers"]["x"]["s"], "q\"/*not*/");
        assert_eq!(v["list"], json!([1, 2]));
    }

    #[test]
    fn merge_codex_toml_keeps_comments_and_other_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "# my settings\nmodel = \"o3\" # inline\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        let entry = build_codex_entry(Path::new("/usr/bin/lain"), Some(Path::new("/m.onnx")));
        let text = merge_codex_toml(&path, "lain", entry).unwrap();
        assert!(
            text.starts_with("# my settings\nmodel = \"o3\" # inline"),
            "{text}"
        );
        let v: toml::Value = text.parse().unwrap();
        assert_eq!(v["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(
            v["mcp_servers"]["lain"]["command"].as_str(),
            Some("/usr/bin/lain")
        );
        assert_eq!(
            v["mcp_servers"]["lain"]["env"]["LAIN_EMBEDDING_MODEL"].as_str(),
            Some("/m.onnx")
        );
        // Re-running replaces the entry instead of duplicating it.
        std::fs::write(&path, &text).unwrap();
        let again =
            merge_codex_toml(&path, "lain", build_codex_entry(Path::new("/b/lain"), None)).unwrap();
        let v: toml::Value = again.parse().unwrap();
        assert_eq!(
            v["mcp_servers"]["lain"]["command"].as_str(),
            Some("/b/lain")
        );
        assert!(v["mcp_servers"]["lain"].get("env").is_none());
        assert_eq!(again.matches("[mcp_servers.lain]").count(), 1, "{again}");
    }

    #[test]
    fn claude_scope_is_read_from_get_output() {
        let get = "lain:\n  Scope: User config (available in all your projects)\n  Type: stdio\n";
        assert_eq!(claude_scope_of(get), Some("user"));
        assert_eq!(
            claude_scope_of("  Scope: Local config (private to you in this project)"),
            Some("local")
        );
        assert_eq!(
            claude_scope_of("  Scope: Project config (shared via .mcp.json)"),
            Some("project")
        );
        assert_eq!(claude_scope_of("lain:\n  Type: stdio"), None);
    }

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

    /// Two backups of the same file within the same second (a real
    /// scenario: repeated `lain setup` runs, or a test suite) must not
    /// collide — `std::fs::copy` silently overwrites its destination,
    /// so a naive same-second timestamp would let the second run
    /// destroy the first run's backup of the user's original config.
    /// Deterministic regardless of real-time second-boundary luck: after
    /// the first real backup, a synthetic file is pre-created at exactly
    /// the base name (no suffix) a same-second second call would try
    /// first, forcing the suffix branch to fire rather than hoping two
    /// calls happen to land in the same wall-clock second.
    #[test]
    fn backup_file_does_not_collide_within_the_same_second() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".mcp.json");
        std::fs::write(&path, "first").unwrap();
        let backup1 = backup_file(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&backup1).unwrap(), "first");

        // Simulate "a same-second backup already exists" deterministically:
        // whatever timestamp the next call computes, pre-occupy its
        // un-suffixed base name with unrelated content.
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let base = path.with_file_name(format!("{file_name}.bak-{ts}"));
        std::fs::write(&base, "occupied by another run").unwrap();

        std::fs::write(&path, "second").unwrap();
        let backup2 = backup_file(&path).unwrap();

        assert_ne!(
            backup2, base,
            "backup_file must not overwrite an already-occupied same-second path"
        );
        assert_eq!(
            std::fs::read_to_string(&base).unwrap(),
            "occupied by another run",
            "the pre-existing same-second backup must survive untouched"
        );
        assert_eq!(std::fs::read_to_string(&backup2).unwrap(), "second");
    }

    /// PR #63 review finding: `configure_claude_code` removed the
    /// existing `lain` entry before attempting to add the new one, and
    /// if `add` then failed, the user's prior working configuration was
    /// simply gone. `failed_replacement_detail` is the extracted, pure
    /// piece of that fix (the shell-out to the real `claude` CLI isn't
    /// independently testable here) — it must surface the captured
    /// previous config, not swallow it.
    #[test]
    fn failed_replacement_detail_surfaces_the_lost_config() {
        let detail = failed_replacement_detail(
            "error: permission denied".into(),
            Some("command: /usr/local/bin/lain\nargs: [mcp]".into()),
        );
        assert!(detail.contains("permission denied"));
        assert!(detail.contains("/usr/local/bin/lain"));
        assert!(detail.contains("removed to make way"));
    }

    /// PR 4: `write_intent_prompt` creates `.lain/PROMPT.md` with the
    /// exact three-sentence protocol. The test pins the file content
    /// against the plan verbatim so a future tweak to the wording
    /// is a deliberate plan update, not a silent drift.
    #[test]
    fn write_intent_prompt_writes_the_protocol_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_intent_prompt(dir.path()).expect("write_intent_prompt");
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, LAIN_INTENT_PROMPT);
        assert!(body.contains("lain_intent"));
        assert!(body.contains("Lain observes"));
    }

    /// PR 4: the prompt write creates `.lain/` if it doesn't exist
    /// (the typical case for a fresh `lain setup`).
    #[test]
    fn write_intent_prompt_creates_dot_lain_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join(".lain").exists());
        write_intent_prompt(dir.path()).unwrap();
        assert!(dir.path().join(".lain").join(PROMPT_FILENAME).exists());
    }

    #[test]
    fn failed_replacement_detail_is_just_the_error_with_nothing_to_lose() {
        // No prior entry existed (fresh install) — nothing was removed,
        // so the message should not claim otherwise.
        let detail = failed_replacement_detail("error: permission denied".into(), None);
        assert_eq!(detail, "error: permission denied");

        // Capturing `claude mcp get` itself failed — still nothing to
        // show, and claiming "removed to make way for this one" would
        // be misleading if capture failed for a reason other than "no
        // prior entry" (e.g. `claude` misbehaving).
        let detail =
            failed_replacement_detail("error: permission denied".into(), Some(String::new()));
        assert_eq!(detail, "error: permission denied");
    }

    #[test]
    fn detect_languages_finds_rust_and_typescript() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();
        let langs: Vec<String> = detect_languages(tmp.path())
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(langs.contains(&"Rust".to_string()));
        assert!(langs.contains(&"TypeScript".to_string()));
        assert!(!langs.contains(&"JavaScript".to_string()));
    }

    /// Tracked files, not root manifests: a Go service and Python scripts
    /// in one repo are both found, most files first.
    #[test]
    fn detect_languages_counts_tracked_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("svc")).unwrap();
        for f in ["svc/a.go", "svc/b.go", "tool.py", "README.md"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        let langs = detect_languages(root);
        assert_eq!(
            langs
                .iter()
                .map(|d| (d.name.as_str(), d.files))
                .collect::<Vec<_>>(),
            vec![("Go", 2), ("Python", 1)]
        );
        assert_eq!(langs[0].ext, "go");
    }

    fn status(language: &str, server: &'static str) -> LanguageServerStatus {
        LanguageServerStatus {
            language: language.into(),
            files: 1,
            parser: "built_in",
            server,
            state: LanguageServerState::NotInstalled,
            install_cmd: None,
            detail: None,
        }
    }

    #[test]
    fn server_selection_defaults_to_none() {
        let (a, b) = (status("Go", "gopls"), status("Python", "pylsp"));
        let missing = vec![&a, &b];
        assert!(parse_server_selection("", &missing).is_empty());
        assert!(parse_server_selection("  \n", &missing).is_empty());
        assert_eq!(parse_server_selection("2", &missing), vec!["pylsp"]);
        assert_eq!(
            parse_server_selection("1, 2,2 9", &missing),
            vec!["gopls", "pylsp"]
        );
        assert_eq!(
            parse_server_selection("ALL", &missing),
            vec!["gopls", "pylsp"]
        );
    }

    /// Non-interactive runs, and `--yes`, never install a language server
    /// the user did not name.
    #[test]
    fn language_servers_are_never_installed_unasked() {
        let detected = vec![DetectedLanguage {
            name: "Go".into(),
            files: 3,
            ext: "go".into(),
        }];
        let opts = SetupOptions {
            workspace: None,
            agent: None,
            json: true,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let statuses = resolve_language_servers(&opts, &detected).unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(matches!(
            statuses[0].state,
            LanguageServerState::Installed | LanguageServerState::NotInstalled
        ));
    }

    #[test]
    fn lsp_flag_dry_run_reports_without_installing() {
        let opts = SetupOptions {
            workspace: None,
            agent: None,
            json: true,
            dry_run: true,
            print_config: false,
            yes: false,
            no_model: true,
            // A server that is certainly not installed on the test machine.
            lsp: Some("svelte".into()),
        };
        let statuses = resolve_language_servers(&opts, &[]).unwrap();
        let st = statuses
            .iter()
            .find(|s| s.server == "svelte-language-server")
            .unwrap();
        if !which::which("svelte-language-server").is_ok() {
            assert_eq!(st.state, LanguageServerState::WouldInstall);
        }
        assert!(resolve_language_servers(
            &SetupOptions {
                lsp: Some("klingon".into()),
                ..opts
            },
            &[]
        )
        .is_err());
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
            lsp: None,
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
            lsp: None,
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

        // A third run with the same settings changes nothing: no new backup.
        let third = configure_generic(tmp.path(), Path::new("/usr/bin/lain2"), None, &opts);
        assert_eq!(third.state, ConfigurationState::Configured);
        let backups = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".mcp.json.bak-"))
            .count();
        assert_eq!(backups, 1, "an unchanged re-run makes no backup");
    }

    // ─── M8: codex adapter ───────────────────────────────────────────────
    //
    // Each adapter gets three tests: empty-config idempotent re-run,
    // preservation of unrelated settings, malformed input. The
    // real-CLI delegation path is covered by
    // `scripts/test_client_recipes.sh` (which CI runs against
    // installed editor binaries), not here — unit tests pin the
    // file-write fallback path that runs in the absence of the
    // `codex` CLI on `PATH`.
    //
    // The four M8 adapter tests mutate process-global env vars
    // (`HOME`, `CODEX_HOME`, `XDG_CONFIG_HOME`) so each adapter's
    // path resolver picks up the fixture's tempdir. Cargo runs
    // tests in parallel by default; without serialization a
    // concurrent cursor test can blow away the codex test's
    // `HOME` mid-run. The `SERIAL` mutex below keeps the env-mutating
    // tests serial. They are fast (no I/O outside a tempdir) so
    // serialization cost is negligible.

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn codex_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::env::set_var("HOME", fake_home.as_os_str());
        std::env::set_var("CODEX_HOME", fake_home.join(".codex"));
        std::env::set_var("LAIN_TEST_DISABLE_CODEX_CLI", "1");
        (tmp, fake_home)
    }

    #[test]
    fn codex_empty_config_writes_lain_entry() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = codex_fixture();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("codex".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_codex(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let cfg = fake_home.join(".codex/config.toml");
        assert!(cfg.is_file(), "codex fallback wrote {}", cfg.display());
        let text = std::fs::read_to_string(&cfg).unwrap();
        let parsed: toml::Value = text.parse().unwrap();
        assert!(
            parsed["mcp_servers"]["lain"]["command"].as_str() == Some("/usr/bin/lain"),
            "codex entry should have the right command; got: {parsed:?}"
        );
    }

    #[test]
    fn codex_preserves_unrelated_entries() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = codex_fixture();
        let cfg_dir = fake_home.join(".codex");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[mcp_servers.OtherServer]\ncommand = \"/usr/bin/other\"\nargs = [\"serve\"]\n\
             [model]\nname = \"gpt-5\"\n",
        )
        .unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("codex".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_codex(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let parsed: toml::Value = std::fs::read_to_string(cfg_dir.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(parsed["mcp_servers"]["OtherServer"].is_table());
        assert!(parsed["model"]["name"].as_str() == Some("gpt-5"));
        assert!(parsed["mcp_servers"]["lain"].is_table());
    }

    #[test]
    fn codex_malformed_input_returns_failed() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = codex_fixture();
        let cfg_dir = fake_home.join(".codex");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "this is not [valid toml").unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("codex".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_codex(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Failed));
        assert!(
            out.detail.as_deref().unwrap_or("").contains("invalid TOML"),
            "detail should name the parse failure; got: {:?}",
            out.detail
        );
    }

    // ─── M8: cursor adapter ──────────────────────────────────────────────

    fn cursor_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::env::set_var("HOME", fake_home.as_os_str());
        (tmp, fake_home)
    }

    #[test]
    fn cursor_empty_config_writes_lain_entry() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = cursor_fixture();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("cursor".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_cursor(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let cfg = fake_home.join(".cursor/mcp.json");
        assert!(cfg.is_file());
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(parsed["mcpServers"]["lain"]["command"], "/usr/bin/lain");
    }

    #[test]
    fn cursor_preserves_unrelated_entries() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = cursor_fixture();
        let cursor_dir = fake_home.join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(
            cursor_dir.join("mcp.json"),
            r#"{
  "mcpServers": {
    "Other": {"command": "/usr/bin/other", "args": ["serve"]}
  },
  "theme": "dark"
}"#,
        )
        .unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("cursor".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_cursor(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cursor_dir.join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(parsed["mcpServers"]["Other"]["command"], "/usr/bin/other");
        assert_eq!(parsed["theme"], "dark");
        assert_eq!(parsed["mcpServers"]["lain"]["command"], "/usr/bin/lain");
    }

    #[test]
    fn cursor_malformed_input_returns_failed() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = cursor_fixture();
        let cursor_dir = fake_home.join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::fs::write(cursor_dir.join("mcp.json"), "{not valid json").unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("cursor".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_cursor(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Failed));
        assert!(out.detail.as_deref().unwrap_or("").contains("invalid JSON"));
    }

    // ─── M8: vscode adapter ───────────────────────────────────────────────

    fn vscode_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::env::set_var("HOME", fake_home.as_os_str());
        #[cfg(windows)]
        std::env::set_var("APPDATA", fake_home.join("AppData/Roaming"));
        std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
        (tmp, fake_home)
    }

    #[test]
    fn vscode_writes_user_scoped_when_no_project_scoped() {
        let _guard = SERIAL.lock().unwrap();
        let (tmp, fake_home) = vscode_fixture();
        // `dirs::config_dir()` honours `$XDG_CONFIG_HOME` on Linux;
        // setting it explicitly here makes the test independent of
        // whatever other env state previous tests left behind (the
        // SERIAL mutex keeps the env-var mutation sequential, but
        // this is the cleaner assertion anyway).
        std::env::set_var("XDG_CONFIG_HOME", fake_home.join(".config"));
        let opts = SetupOptions {
            workspace: None,
            agent: Some("vscode".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_vscode(
            tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let cfg = vscode_user_config_path().expect("vscode user config path must resolve");
        assert!(
            cfg.is_file(),
            "vscode wrote user-scoped config: {}",
            cfg.display()
        );
    }

    #[test]
    fn vscode_writes_project_scoped_when_present() {
        let _guard = SERIAL.lock().unwrap();
        let (tmp, _fake_home) = vscode_fixture();
        // Project-scoped config already exists.
        std::fs::write(
            tmp.path().join(".vscode/mcp.json"),
            r#"{"servers": {"Other": {"type": "stdio", "command": "/bin/other"}}}"#,
        )
        .unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("vscode".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_vscode(
            tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".vscode/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["servers"]["lain"]["type"], "stdio");
        assert_eq!(parsed["servers"]["Other"]["command"], "/bin/other");
    }

    // ─── M8: continue adapter ─────────────────────────────────────────────

    fn continue_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::env::set_var("HOME", fake_home.as_os_str());
        (tmp, fake_home)
    }

    #[test]
    fn continue_writes_lain_in_model_context_protocol_servers() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = continue_fixture();
        // A legacy JSON setup: config.json and no config.yaml.
        std::fs::create_dir_all(fake_home.join(".continue")).unwrap();
        std::fs::write(fake_home.join(".continue/config.json"), r#"{"models": []}"#).unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("continue".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_continue(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let cfg = fake_home.join(".continue/config.json");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(parsed["models"], json!([]), "other keys are kept");
        let servers = parsed["experimental"]["modelContextProtocolServers"]
            .as_array()
            .expect("modelContextProtocolServers must be an array");
        let lain = servers
            .iter()
            .find(|s| s.get("name").and_then(|v| v.as_str()) == Some("lain"))
            .expect("a 'lain' server entry must exist in the array");
        assert_eq!(lain["transport"]["type"], "stdio");
        assert_eq!(lain["transport"]["command"], "/usr/bin/lain");
        assert_eq!(lain["transport"]["args"], json!(["mcp"]));
    }

    #[test]
    fn continue_yaml_users_get_a_workspace_block_file() {
        let _guard = SERIAL.lock().unwrap();
        let (tmp, fake_home) = continue_fixture();
        std::fs::create_dir_all(fake_home.join(".continue")).unwrap();
        let yaml = "# mine\nname: cfg\n";
        std::fs::write(fake_home.join(".continue/config.yaml"), yaml).unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("continue".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let out = configure_continue(
            &root,
            std::path::Path::new("/opt/my apps/lain"),
            Some(std::path::Path::new("/m: x.onnx")),
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        assert_eq!(
            std::fs::read_to_string(fake_home.join(".continue/config.yaml")).unwrap(),
            yaml,
            "the user's config.yaml is not rewritten"
        );
        let block = std::fs::read_to_string(root.join(".continue/mcpServers/lain.yaml")).unwrap();
        assert!(block.contains("command: \"/opt/my apps/lain\""), "{block}");
        assert!(
            block.contains("LAIN_EMBEDDING_MODEL: \"/m: x.onnx\""),
            "{block}"
        );
        assert!(block.contains("schema: v1"), "{block}");
    }

    #[test]
    fn continue_dedups_existing_lain_entry() {
        let _guard = SERIAL.lock().unwrap();
        let (_tmp, fake_home) = continue_fixture();
        let continue_dir = fake_home.join(".continue");
        std::fs::create_dir_all(&continue_dir).unwrap();
        // Two `lain` entries already in the array; the apply step
        // should reduce to one.
        std::fs::write(
            continue_dir.join("config.json"),
            r#"{
  "experimental": {
    "modelContextProtocolServers": [
      {"name": "Other", "command": "/usr/bin/other", "transport": "stdio"},
      {"name": "lain", "command": "/old/lain", "transport": "stdio"},
      {"name": "lain", "command": "/older/lain", "transport": "stdio"}
    ]
  }
}"#,
        )
        .unwrap();
        let opts = SetupOptions {
            workspace: None,
            agent: Some("continue".into()),
            json: false,
            dry_run: false,
            print_config: false,
            yes: true,
            no_model: true,
            lsp: None,
        };
        let out = configure_continue(
            _tmp.path(),
            std::path::Path::new("/usr/bin/lain"),
            None,
            &opts,
        );
        assert!(matches!(out.state, ConfigurationState::Configured));
        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(continue_dir.join("config.json")).unwrap(),
        )
        .unwrap();
        let servers = parsed["experimental"]["modelContextProtocolServers"]
            .as_array()
            .unwrap();
        let lain_entries: Vec<&serde_json::Value> = servers
            .iter()
            .filter(|s| s.get("name").and_then(|v| v.as_str()) == Some("lain"))
            .collect();
        assert_eq!(
            lain_entries.len(),
            1,
            "duplicate 'lain' server entries must be deduplicated; got: {servers:?}"
        );
        assert_eq!(lain_entries[0]["transport"]["command"], "/usr/bin/lain");
    }
}
