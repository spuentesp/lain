use anyhow::Result;
use std::fs;
use std::io::Write;
use std::process::Command;

use crate::cmds::agents::adapters::AUTO_WORKSPACE;

/// Supported agent names. Anything else is a user error and `run_init` will
/// refuse rather than silently writing nothing.
const SUPPORTED_AGENTS: &[&str] = &["claude", "gemini", "cursor", "windsurf", "cline", "copilot", "kimi", "opencode", "auto"];

// ── Bundled agent resources ────────────────────────────────────────────────
//
// These are the canonical per-agent install artifacts, embedded at compile
// time so downloaded-release binaries don't need filesystem lookups at
// runtime. Edit the source files under hooks/<agent>/ and the change ships
// with the next release; do not duplicate the content as string literals
// elsewhere.
const CLAUDE_AWARENESS_MD: &str = include_str!("../../hooks/claude/lain-awareness.md");
const COPILOT_INSTRUCTIONS_MD: &str = include_str!("../../hooks/copilot/copilot-instructions.md");
const CLAUDE_HOOK_SH: &str = include_str!("../../hooks/claude/lain-hook.sh");
const GEMINI_AWARENESS_MD: &str = include_str!("../../hooks/gemini/GEMINI.md");
const CURSOR_AWARENESS_MD: &str = include_str!("../../hooks/cursor/lain-awareness.md");
const WINDSURF_RULES_MD: &str = include_str!("../../hooks/windsurf/lain-rules.md");
const CLINE_RULES_MD: &str = include_str!("../../hooks/cline/lain-rules.md");
const KIMI_SKILL_MD: &str = include_str!("../../hooks/kimi/skills/lain/SKILL.md");
const KIMI_PLUGIN_WRAPPER_SH: &str = include_str!("kimi_plugin_wrapper.sh");
const OPENCODE_AGENTS_MD: &str = include_str!("../../hooks/opencode/AGENTS.md");

pub fn run_init(
    agent: &str,
    workspace: Option<&std::path::Path>,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
    scope: &str,
) -> Result<()> {
    if !SUPPORTED_AGENTS.contains(&agent) {
        anyhow::bail!(
            "Unknown agent '{}'. Supported: {}",
            agent,
            SUPPORTED_AGENTS.join(", ")
        );
    }

    let workspace = workspace.unwrap_or_else(|| std::path::Path::new("."));
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    // Pre-flight: if the workspace isn't a git repo, refuse with a clear
    // message rather than letting `build_core_memory` crash with a libgit2 error.
    if !workspace.join(".git").exists() {
        anyhow::bail!(
            "No Git repository found at {:?}. Lain requires a .git folder.\n\
             Run `git init` (or open an existing repo) and try again.",
            workspace
        );
    }

    // Auto-detect ONNX model at the default install location if not explicitly provided
    let default_model_path = home_dir.join(".local/lain/models/all-MiniLM-L6-v2.onnx");
    let resolved_model: Option<std::path::PathBuf> = embedding_model
        .map(|p| p.to_path_buf())
        .or_else(|| {
            if default_model_path.exists() {
                Some(default_model_path.clone())
            } else {
                None
            }
        });
    let embedding_model = resolved_model.as_deref();

    let agent_type = if agent == "auto" { detect_agent(&home_dir) } else { agent };

    println!("Initializing LAIN for agent: {}", agent_type);
    println!("  Workspace: {}", workspace.display());
    if let Some(ref model) = embedding_model {
        println!("  Embedding model: {}", model.display());
    } else {
        println!("  Embedding model: none (semantic_search unavailable)");
    }
    println!("  Transport: {}", transport);
    println!("  Port: {}", port);

    match agent_type {
        "claude" => {
            let claude_dir = home_dir.join(".claude");
            let settings_path = claude_dir.join("settings.json");
            let lain_md_path = claude_dir.join("LAIN.md");
            init_claude(embedding_model, transport, port, yes, &claude_dir, &settings_path, &lain_md_path)?;
        }
        "gemini" => {
            let gemini_dir = home_dir.join(".gemini");
            let settings_path = gemini_dir.join("settings.json");
            init_gemini(embedding_model, transport, port, yes, &gemini_dir, &settings_path)?;
        }
        "cursor" => {
            let cursor_dir = home_dir.join(".cursor");
            init_cursor(embedding_model, transport, port, yes, &cursor_dir)?;
        }
        "windsurf" => {
            let windsurf_dir = home_dir.join(".codeium/windsurf");
            init_windsurf(embedding_model, transport, port, yes, &windsurf_dir)?;
        }
        "cline" => {
            let cline_dir = home_dir.join(".cline");
            init_cline(embedding_model, transport, port, yes, &cline_dir)?;
        }
        "kimi" => {
            let kimi_root = home_dir.join(".kimi-code");
            init_kimi(embedding_model, transport, port, yes, &kimi_root)?;
        }
        "opencode" => {
            init_opencode(
                workspace,
                embedding_model,
                transport,
                port,
                yes,
                scope,
            )?;
        }
        "copilot" => {
            init_copilot(
                workspace,
                embedding_model,
                transport,
                port,
                yes,
                scope,
            )?;
        }
        other => {
            anyhow::bail!("Unknown agent '{}'", other);
        }
    }

    install_agent_doc(agent_type, &home_dir)?;

    // Build the code graph now so `lain query` works immediately after init.
    // Without this the README's quickstart (init → query) is broken: the
    // user has to also run `lain --workspace …` separately to populate
    // .lain/graph.bin before queries return anything.
    println!("\nIndexing workspace (one-shot, may take a moment)...");
    match index_workspace_blocking(workspace, embedding_model) {
        Ok(()) => println!("Indexed workspace into .lain/graph.bin."),
        Err(e) => eprintln!(
            "Warning: indexing failed: {}. Run `lain --workspace {}` to retry.",
            e,
            workspace.display()
        ),
    }

    println!("\nLAIN initialization complete!");
    println!("Restart your agent to use LAIN.");

    // Auto-register the project so `lain use <name>` works later without
    // the user having to type the path. Uses register_or_touch: if the
    // path is already registered under any name, we just update
    // last_used instead of creating a duplicate entry.
    match lain::state::Projects::register_or_touch(workspace) {
        Ok(true) => eprintln!("registered project under directory basename"),
        Ok(false) => {} // already registered, just touched
        Err(e) => eprintln!("Note: could not auto-register project: {}", e),
    }
    Ok(())
}

/// Build the code graph synchronously. We can't call `block_on` directly
/// because `main()` is already inside a `#[tokio::main]` runtime. Spawn a
/// fresh OS thread that owns its own single-threaded tokio runtime.
fn index_workspace_blocking(
    workspace: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
) -> Result<()> {
    let memory_path = workspace.join(".lain/graph.bin");
    std::fs::create_dir_all(workspace.join(".lain"))?;

    let workspace_owned = workspace.to_path_buf();
    let memory_path_owned = memory_path.clone();
    let model_owned = embedding_model.map(|p| p.to_path_buf());

    let handle = std::thread::Builder::new()
        .name("lain-init-indexer".into())
        .spawn(move || -> Result<()> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let server = lain::server::LainServer::new(
                    &workspace_owned,
                    &memory_path_owned,
                    model_owned.as_deref(),
                )?;
                let mut bg = server.clone_for_background();
                bg.build_core_memory().await?;
                Ok::<(), anyhow::Error>(())
            })
        })?;
    handle.join().map_err(|_| anyhow::anyhow!("indexing thread panicked"))??;
    Ok(())
}

fn detect_agent(home_dir: &std::path::Path) -> &'static str {
    if home_dir.join(".kimi-code").exists() { return "kimi"; }
    if home_dir.join(".claude").exists() { return "claude"; }
    if home_dir.join(".gemini").exists() { return "gemini"; }
    if home_dir.join(".cursor").exists() { return "cursor"; }
    if home_dir.join(".windsurf").exists() { return "windsurf"; }
    "claude"
}

/// Register the Lain MCP server with Claude Code by shelling out to
/// `claude mcp add`. Claude Code (v2.1+) reads MCP server configuration
/// from `~/.claude.json`, NOT from `~/.claude/settings.json`. The old
/// init wrote the `mcpServers` block into `settings.json` and Claude
/// silently ignored it — the server was registered but never
/// connected, and `claude mcp list` did not list it. Verified via
/// `claude mcp list` and a live behavior test that reached
/// `get_health` only after switching to `claude mcp add`.
///
/// `claude_bin` is the path to the `claude` binary; tests inject a
/// stub to verify the exact arguments without touching real state.
fn register_claude_mcp(
    claude_bin: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
) -> Result<()> {
    let mut args: Vec<String> = vec![
        "mcp".to_string(),
        "add".to_string(),
        "--scope".to_string(),
        "user".to_string(),
        "lain".to_string(),
        "--".to_string(),
        "lain".to_string(),
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        transport.to_string(),
    ];
    if let Some(model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }
    if transport != "stdio" {
        args.push("--port".to_string());
        args.push(port.to_string());
    }

    let status = std::process::Command::new(claude_bin)
        .args(&args)
        .status()
        .map_err(|e| anyhow::anyhow!(
            "failed to spawn `claude mcp add` ({claude_bin:?}): {e}. \
             Is the Claude Code CLI installed and on PATH?"
        ))?;
    if !status.success() {
        anyhow::bail!(
            "`claude mcp add` exited with {status:?}. Run `claude mcp add` \
             manually to see the error."
        );
    }
    Ok(())
}

fn init_claude(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
    claude_dir: &std::path::Path,
    settings_path: &std::path::Path,
    lain_md_path: &std::path::Path,
) -> Result<()> {
    use std::fs;

    if !claude_dir.exists() {
        fs::create_dir_all(claude_dir)?;
    }

    let mut settings = if settings_path.exists() {
        let content = fs::read_to_string(settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // MCP server registration is delegated to `claude mcp add` (see
    // `register_claude_mcp`). Do NOT write a `mcpServers` block into
    // settings.json — Claude Code 2.1+ reads MCP config from
    // `~/.claude.json` and silently ignores the settings.json entry.

    if let Some(claude_bin) = which::which("claude").ok() {
        match register_claude_mcp(&claude_bin, embedding_model, transport, port) {
            Ok(()) => println!("Registered Lain MCP server with Claude Code (claude mcp add)."),
            Err(e) => eprintln!(
                "Warning: failed to register MCP server: {e}. \
                 Run `claude mcp add --scope user lain -- lain --workspace auto --transport stdio` \
                 manually."
            ),
        }
    } else {
        eprintln!(
            "Warning: `claude` not on PATH; skipping MCP registration. \
             Run `claude mcp add --scope user lain -- lain --workspace auto --transport stdio` \
             once Claude Code is installed."
        );
    }

    // Install Claude Code PreToolUse hook (separate from MCP). The
    // hook reads stdin and delegates to `lain ask`, which is the
    // CLI-side bridge for tools that don't go through MCP.
    install_claude_hook(claude_dir, settings_path, &mut settings)?;

    // Write awareness markdown from the bundled source.
    write_awareness_doc(lain_md_path, CLAUDE_AWARENESS_MD)?;

    Ok(())
}

/// Install the Claude Code `PreToolUse` hook. Copies the bundled
/// `lain-hook.sh` script to `~/.claude/hooks/`, makes it executable,
/// and registers it under `hooks.PreToolUse` in settings.json.
/// Idempotent: skips both the script copy and the settings.json
/// registration if either already references the hook.
fn install_claude_hook(
    claude_dir: &std::path::Path,
    settings_path: &std::path::Path,
    settings: &mut serde_json::Value,
) -> Result<()> {
    use std::fs;

    let hook_path = claude_dir.join("hooks/lain-hook.sh");
    let hook_path_str = hook_path.to_string_lossy().to_string();

    // 1. Copy the hook script if missing.
    if let Some(parent) = hook_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    if hook_path.exists() {
        println!("Claude hook script already exists - skipped.");
    } else {
        fs::write(&hook_path, CLAUDE_HOOK_SH)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }
        println!("Installed {}", hook_path.display());
    }

    // 2. Register under hooks.PreToolUse if missing.
    let already_registered = settings
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command").and_then(|c| c.as_str()) == Some(hook_path_str.as_str())
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if already_registered {
        println!("Claude PreToolUse hook already registered - skipped.");
        return Ok(());
    }

    if !settings.get("hooks").map(|h| h.is_object()).unwrap_or(false) {
        settings["hooks"] = serde_json::json!({});
    }
    if !settings["hooks"].get("PreToolUse").map(|p| p.is_array()).unwrap_or(false) {
        settings["hooks"]["PreToolUse"] = serde_json::json!([]);
    }
    let hook_entry = serde_json::json!({
        "matcher": "",
        "hooks": [
            { "type": "command", "command": hook_path_str }
        ]
    });
    settings["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(hook_entry);

    let settings_json = serde_json::to_string_pretty(&*settings)?;
    let tmp_path = settings_path.with_extension("json.tmp");
    fs::write(&tmp_path, settings_json)?;
    fs::rename(&tmp_path, settings_path)?;
    println!("Registered Claude PreToolUse hook in {}", settings_path.display());

    Ok(())
}

fn init_gemini(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
    gemini_dir: &std::path::Path,
    settings_path: &std::path::Path,
) -> Result<()> {
    if !gemini_dir.exists() {
        fs::create_dir_all(gemini_dir)?;
    }

    let mut settings = if settings_path.exists() {
        let content = fs::read_to_string(settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !settings.get("mcpServers").and_then(|v| v.as_object()).is_some() {
        settings.as_object_mut().unwrap().insert("mcpServers".to_string(), serde_json::json!({}));
    }

    let mcp_servers = settings.get_mut("mcpServers").unwrap().as_object_mut().unwrap();

    let mut args = vec![
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        transport.to_string(),
    ];

    if let Some(ref model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }

    if transport != "stdio" {
        args.push("--port".to_string());
        args.push(port.to_string());
    }

    let lain_entry = serde_json::json!({
        // IMPORTANT: `command` must be a PATH-resolvable name, not an
        // absolute path. Claude Code's MCP loader silently ignores stdio
        // entries whose `command` is absolute, so the server is
        // registered in settings.json but never connected (verified:
        // `claude mcp list` and a live behavior test reported "no Lain
        // MCP server connected" until the path was replaced with the
        // bare name). The Kimi adapter applies the same rule via a
        // `./bin/lain` wrapper for the same reason. Resolving the
        // binary via `which` would defeat the purpose, so we hardcode
        // the name and trust the user to keep `lain` on PATH.
        "command": "lain",
        "args": args
    });

    let do_write = if mcp_servers.get("lain").is_some() {
        if yes {
            println!("MCP server already configured - skipped.");
            false
        } else {
            print!("LAIN MCP server already configured. Overwrite? [y/N] ");
            std::io::stdout().flush()?;
            let mut reply = String::new();
            std::io::stdin().read_line(&mut reply)?;
            let overwrite = reply.trim().starts_with('y') || reply.trim().starts_with('Y');
            if !overwrite { println!("Skipped."); }
            overwrite
        }
    } else {
        true
    };

    if do_write {
        mcp_servers.insert("lain".to_string(), lain_entry);
        let settings_json = serde_json::to_string_pretty(&settings)?;
        let tmp_path = settings_path.with_extension("json.tmp");
        fs::write(&tmp_path, &settings_json)?;
        fs::rename(&tmp_path, settings_path)?;
        println!("Updated ~/.gemini/settings.json");
    }

    Ok(())
}

/// Write an MCP server entry into a JSON settings file at `dir/settings_file`.
/// `agent_label` is shown in the interactive overwrite prompt and the
/// "Updated <path>" log. `extra_fields` is merged into the lain server entry
/// (e.g. Cline's `"disabled": false`).
fn write_mcp_server_entry(
    dir: &std::path::Path,
    settings_file: &str,
    agent_label: &str,
    extra_fields: Option<serde_json::Value>,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    yes: bool,
) -> Result<()> {
    use std::fs;
    if !dir.exists() { fs::create_dir_all(dir)?; }
    let settings_path = dir.join(settings_file);

    let mut settings: serde_json::Value = if settings_path.exists() {
        serde_json::from_str(&fs::read_to_string(&settings_path)?).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !settings.get("mcpServers").and_then(|v| v.as_object()).is_some() {
        settings.as_object_mut().unwrap().insert("mcpServers".to_string(), serde_json::json!({}));
    }
    let mcp_servers = settings.get_mut("mcpServers").unwrap().as_object_mut().unwrap();

    if mcp_servers.get("lain").is_some() {
        if !yes {
            print!("LAIN MCP server already configured for {}. Overwrite? [y/N] ", agent_label);
            std::io::stdout().flush()?;
            let mut reply = String::new();
            std::io::stdin().read_line(&mut reply)?;
            if !(reply.trim().starts_with('y') || reply.trim().starts_with('Y')) {
                println!("Skipped.");
                return Ok(());
            }
        } else {
            println!("MCP server already configured - skipped.");
            return Ok(());
        }
    }

    let mut args = vec![
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        transport.to_string(),
    ];
    if let Some(model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }

    let mut entry = serde_json::json!({
        // IMPORTANT: `command` must be a PATH-resolvable name, not an
        // absolute path. Claude Code's MCP loader silently ignores stdio
        // entries whose `command` is absolute, so the server is
        // registered in settings.json but never connected (verified:
        // `claude mcp list` and a live behavior test reported "no Lain
        // MCP server connected" until the path was replaced with the
        // bare name). The Kimi adapter applies the same rule via a
        // `./bin/lain` wrapper for the same reason. Resolving the
        // binary via `which` would defeat the purpose, so we hardcode
        // the name and trust the user to keep `lain` on PATH.
        "command": "lain",
        "args": args
    });
    if let Some(extra) = extra_fields {
        if let (Some(entry_obj), Some(extra_obj)) = (entry.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                entry_obj.insert(k.clone(), v.clone());
            }
        }
    }
    mcp_servers.insert("lain".to_string(), entry);

    let json = serde_json::to_string_pretty(&settings)?;
    let tmp = settings_path.with_extension("json.tmp");
    fs::write(&tmp, &json)?;
    fs::rename(&tmp, &settings_path)?;
    println!("Updated ~/{}/{}", dir.file_name().and_then(|s| s.to_str()).unwrap_or(""), settings_file);
    Ok(())
}

/// Cursor MCP config: `~/.cursor/mcp.json`. Schema documented at
/// https://docs.cursor.com/context/model-context-protocol#configuration
fn init_cursor(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    _port: u16,
    yes: bool,
    cursor_dir: &std::path::Path,
) -> Result<()> {
    write_mcp_server_entry(
        cursor_dir, "mcp.json", "Cursor", None,
        embedding_model, transport, yes,
    )
}

/// Windsurf MCP config: `~/.codeium/windsurf/mcp_config.json`. Schema
/// documented at https://docs.codeium.com/windsurf/mcp
fn init_windsurf(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    _port: u16,
    yes: bool,
    windsurf_dir: &std::path::Path,
) -> Result<()> {
    write_mcp_server_entry(
        windsurf_dir, "mcp_config.json", "Windsurf", None,
        embedding_model, transport, yes,
    )
}

/// Cline MCP config: `~/.cline/mcp_settings.json`. Schema documented at
/// https://docs.cline.bot/mcp/configuring-mcp-servers
fn init_cline(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    _port: u16,
    yes: bool,
    cline_dir: &std::path::Path,
) -> Result<()> {
    write_mcp_server_entry(
        cline_dir, "mcp_settings.json", "Cline",
        Some(serde_json::json!({ "disabled": false })),
        embedding_model, transport, yes,
    )
}

fn write_awareness_doc(path: &std::path::Path, content: &str) -> Result<()> {
    if path.exists() {
        println!("Awareness doc already exists - skipped.");
        return Ok(());
    }
    fs::write(path, content)?;
    println!("Created {}", path.display());
    Ok(())
}

fn install_agent_doc(agent_type: &str, home_dir: &std::path::Path) -> Result<()> {
    match agent_type {
        // gemini-cli reads GEMINI.md (the canonical filename per
        // gemini-cli docs). LAIN.md is silently ignored, so we must
        // use the correct filename.
        "gemini" => write_agent_doc(home_dir, ".gemini", "GEMINI.md", GEMINI_AWARENESS_MD)?,
        "cursor" => write_agent_doc(home_dir, ".cursor", "LAIN.md", CURSOR_AWARENESS_MD)?,
        "windsurf" => write_agent_doc(home_dir, ".windsurf", "lain-rules.md", WINDSURF_RULES_MD)?,
        "cline" => write_agent_doc(home_dir, ".cline", "lain-rules.md", CLINE_RULES_MD)?,
        // claude is handled inside init_claude via write_awareness_doc
        // + install_claude_hook, so it doesn't appear here.
        // kimi is handled inside init_kimi (skill markdown lives in
        // the managed plugin directory, not ~/.kimi-code/).
        _ => {}
    }
    Ok(())
}

fn write_agent_doc(home_dir: &std::path::Path, dir_name: &str, file_name: &str, content: &str) -> Result<()> {
    use std::fs;
    let dir = home_dir.join(dir_name);
    let path = dir.join(file_name);
    if !dir.exists() { fs::create_dir_all(&dir)?; }
    if path.exists() {
        println!("{} doc already exists - skipped.", dir_name);
    } else {
        fs::write(&path, content)?;
        println!("Created ~/{}/{}", dir_name, file_name);
    }
    Ok(())
}

/// Install lain as a kimi-code plugin. Kimi uses a `managed/` plugin
/// directory with a `kimi.plugin.json` manifest and a `skills/<name>/`
/// subdirectory for skill markdown. The `command` field must be a
/// PATH-resolvable name (or start with `./`); an absolute path is
/// rejected by the plugin manager.
fn init_kimi(
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    _yes: bool,
    kimi_root: &std::path::Path,
) -> Result<()> {
    let plugin_root = kimi_root.join("plugins/managed/lain");
    fs::create_dir_all(plugin_root.join("skills/lain"))?;

    // Kimi's plugin security model only allows stdio MCP commands that are
    // either on PATH or a `./` path inside the plugin root, and `cwd` must
    // also be `./` and inside the plugin root. An absolute command path is
    // silently ignored. Build a wrapper script at `bin/lain` that resolves
    // `--workspace auto` from the parent agent's cwd (Kimi pins this
    // subprocess's cwd to the plugin root, so the wrapper has to peek at
    // /proc/$PPID/cwd) and then execs the real `lain` from PATH.
    let bin_dir = plugin_root.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let wrapper_path = bin_dir.join("lain");
    fs::write(&wrapper_path, KIMI_PLUGIN_WRAPPER_SH)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Build args conditionally: --embedding-model is optional (semantic
    // search is unavailable without it), and --port is only meaningful
    // for non-stdio transports. The wrapper replaces `--workspace <sentinel>`
    // with the resolved repo before exec'ing the real `lain`.
    let mut args: Vec<String> = vec![
        "--workspace".to_string(),
        AUTO_WORKSPACE.to_string(),
    ];
    if let Some(model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }
    args.push("--transport".to_string());
    args.push(transport.to_string());
    if transport != "stdio" {
        args.push("--port".to_string());
        args.push(port.to_string());
    }

    // Build the mcpServers entry with the plugin-root-relative command/cwd.
    // Args can be absolute paths (workspace, model) because they are not
    // subject to the plugin-root restriction.
    let plugin_json = serde_json::json!({
        "name": "lain",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Structural code intelligence for AI coding agents with semantic search, blast radius, and architectural analysis.",
        "author": { "name": "spuentesp", "homepage": "https://github.com/spuentesp/lain" },
        "homepage": "https://github.com/spuentesp/lain",
        "license": "MIT",
        "keywords": ["code-intelligence", "mcp", "semantic-search", "architecture", "rust"],
        "mcpServers": {
            "lain": {
                "command": "./bin/lain",
                "args": args,
                "cwd": "./",
            },
        },
        "interface": {
            "displayName": "LAIN Code Intelligence",
            "shortDescription": "Structural code intelligence: semantic search, blast radius, dependency traces, architectural analysis",
            "developerName": "spuentesp",
        },
    });
    let plugin_path = plugin_root.join("kimi.plugin.json");
    fs::write(&plugin_path, serde_json::to_string_pretty(&plugin_json)?)?;

    // Write the bundled skill markdown (compiled into the binary via
    // include_str!) so the plugin ships with skill content even when
    // installed from a downloaded release tarball.
    let skill_md_dst = plugin_root.join("skills/lain/SKILL.md");
    fs::write(&skill_md_dst, KIMI_SKILL_MD)?;

    // Register the plugin in installed.json so the plugin manager
    // picks it up on the next session start.
    let installed_path = kimi_root.join("plugins/installed.json");
    let mut registry: serde_json::Value = if installed_path.exists() {
        serde_json::from_str(&fs::read_to_string(&installed_path)?)
            .unwrap_or_else(|_| serde_json::json!({"version": 1, "plugins": []}))
    } else {
        serde_json::json!({"version": 1, "plugins": []})
    };
    if !registry.get("plugins").map(|p| p.is_array()).unwrap_or(false) {
        registry["plugins"] = serde_json::json!([]);
    }
    let plugins_arr = registry["plugins"].as_array_mut().unwrap();
    // Remove any existing lain entry so re-running init replaces it.
    plugins_arr.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some("lain"));
    plugins_arr.push(serde_json::json!({
        "id": "lain",
        "root": plugin_root.to_string_lossy().to_string(),
        "source": "local-path",
        "enabled": true,
        "originalSource": plugin_root.to_string_lossy().to_string(),
    }));
    fs::create_dir_all(installed_path.parent().unwrap())?;
    fs::write(&installed_path, serde_json::to_string_pretty(&registry)?)?;

    println!("Installed kimi-code plugin: {}", plugin_root.display());
    println!("Restart kimi-code (or open a new window) to load the plugin.");
    Ok(())
}

/// Install Lain for OpenCode. Writes `opencode.json` (MCP config) and,
/// when `scope == "project"`, `AGENTS.md` (awareness doc) in the
/// workspace root. When `scope == "user"`, writes the global
/// `~/.config/opencode/opencode.json` and skips `AGENTS.md` (a
/// per-project convention, inappropriate to write globally).
///
/// `AGENTS.md` is **never** clobbered by default: if a file already
/// exists at the target path and `yes` is false we print a message
/// and skip the write. Pass `--yes` to force-overwrite the awareness
/// doc (the bundled doc replaces whatever is there). This mirrors the
/// spec at `docs/superpowers/specs/2026-08-10-opencode-agent-design.md`
/// and the skip-on-exists pattern used by `write_awareness_doc` for
/// the Claude awareness doc.
fn init_opencode(
    workspace: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
    _transport: &str,
    _port: u16,
    yes: bool,
    scope: &str,
) -> Result<()> {
    if scope != "project" && scope != "user" {
        anyhow::bail!(
            "init_opencode: --scope must be 'project' or 'user', got '{}'",
            scope
        );
    }
    use crate::cmds::agents::adapters::opencode::build_opencode_lain_entry;

    let target_path: std::path::PathBuf = if scope == "project" {
        workspace.join("opencode.json")
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        home.join(".config/opencode/opencode.json")
    };

    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: serde_json::Value = if target_path.exists() {
        let raw = std::fs::read_to_string(&target_path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    {
        let schema = doc.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("opencode.json root is not a JSON object"))?;
        let mcp = schema.entry("mcp".to_string()).or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("opencode.json `mcp` is not an object"))?;
        mcp_obj.insert("lain".to_string(), build_opencode_lain_entry(embedding_model));
    }
    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&target_path, serialized)?;
    println!("Wrote OpenCode MCP config to {}", target_path.display());

    if scope == "project" {
        let agents_path = workspace.join("AGENTS.md");
        if agents_path.exists() && !yes {
            println!(
                "{} already exists - skipped (use --yes to overwrite).",
                agents_path.display()
            );
        } else {
            std::fs::write(&agents_path, OPENCODE_AGENTS_MD)?;
            println!("Wrote OpenCode awareness doc to {}", agents_path.display());
        }
    }

    Ok(())
}

/// Install Lain for GitHub Copilot in VS Code. Writes `.vscode/mcp.json`
/// (MCP config) and, when `scope == "project"`, `.github/copilot-instructions.md`
/// (awareness doc) in the workspace root. When `scope == "user"`, writes
/// the global `~/.copilot/mcp-config.json` and skips the awareness doc
/// (a per-repo convention, inappropriate to write globally).
fn init_copilot(
    workspace: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
    _transport: &str,
    _port: u16,
    yes: bool,
    scope: &str,
) -> Result<()> {
    if scope != "project" && scope != "user" {
        anyhow::bail!(
            "init_copilot: --scope must be 'project' or 'user', got '{}'",
            scope
        );
    }
    use crate::cmds::agents::adapters::copilot::build_copilot_lain_entry;

    let target_path: std::path::PathBuf = if scope == "project" {
        workspace.join(".vscode/mcp.json")
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        home.join(".copilot/mcp-config.json")
    };
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: serde_json::Value = if target_path.exists() {
        let raw = std::fs::read_to_string(&target_path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    {
        let root = doc.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp.json root is not a JSON object"))?;
        let section = root.entry("servers".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let section_obj = section.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("`servers` is not an object"))?;
        section_obj.insert("lain".to_string(), build_copilot_lain_entry(embedding_model));
    }
    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&target_path, serialized)?;
    println!("Wrote GitHub Copilot/VS Code MCP config to {}", target_path.display());

    if scope == "project" {
        let instructions_dir = workspace.join(".github");
        std::fs::create_dir_all(&instructions_dir)?;
        let instructions_path = instructions_dir.join("copilot-instructions.md");
        if instructions_path.exists() && !yes {
            println!(
                "Copilot instructions file already exists at {} - skipped.",
                instructions_path.display()
            );
        } else {
            std::fs::write(&instructions_path, COPILOT_INSTRUCTIONS_MD)?;
            println!("Wrote GitHub Copilot awareness doc to {}", instructions_path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Drop`-based HOME restore. Captures the previous value on `set`,
    /// then restores it on scope exit — including the panic path. Without
    /// this, an assertion panic between `set_var("HOME", tmp)` and the
    /// explicit `set_var("HOME", prev)` would leak the tempdir HOME into
    /// every subsequent test that runs in this process (every test that
    /// uses the same `HOME_LOCK`, plus any test whose `opencode.json`
    /// resolution walks `HOME`).
    struct HomeGuard(Option<String>);
    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var("HOME").ok();
            std::env::set_var("HOME", path);
            Self(prev)
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    const OPENCODE_AGENTS_MD: &str = include_str!("../../hooks/opencode/AGENTS.md");

    /// Regression: Claude Code silently ignores stdio MCP entries whose
    /// `command` is an absolute path. The MCP server is registered but
    /// never connected (verified via `claude mcp list` and a live
    /// behavior test reporting "no Lain MCP server connected"). The
    /// init must write a bare PATH-resolvable name.
    #[test]
    /// `register_claude_mcp` must invoke the Claude CLI with the right
    /// subcommand and arguments, using a bare PATH-resolvable name for
    /// the binary (not an absolute path, which Claude Code silently
    /// ignores for stdio MCP servers).
    #[test]
    fn register_claude_mcp_invokes_claude_with_workspace_auto_and_bare_lain() {
        let tmp = tempfile::tempdir().unwrap();
        // Stub `claude` binary that records its argv to a file and exits 0.
        let stub = tmp.path().join("claude");
        let args_file = tmp.path().join("args.txt");
        let script = format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
            args_file.display()
        );
        std::fs::write(&stub, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        register_claude_mcp(&stub, None, "stdio", 0).unwrap();

        let recorded: Vec<String> = std::fs::read_to_string(&args_file)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();

        // Verify the subcommand shape: `claude mcp add --scope user lain -- lain <args>`
        assert_eq!(recorded.first().map(String::as_str), Some("mcp"), "first arg must be `mcp`");
        assert_eq!(recorded.get(1).map(String::as_str), Some("add"), "second arg must be `add`");
        assert_eq!(recorded.get(2).map(String::as_str), Some("--scope"));
        assert_eq!(recorded.get(3).map(String::as_str), Some("user"));
        assert_eq!(recorded.get(4).map(String::as_str), Some("lain"));
        let sep_idx = recorded.iter().position(|a| a == "--").expect("`--` separator present");
        // The token right after `--` is the actual binary name passed to
        // the MCP server. It must be a bare name on PATH, not an
        // absolute path.
        let binary = &recorded[sep_idx + 1];
        assert_eq!(binary, "lain", "binary after `--` must be the bare name `lain`");
        assert!(!binary.starts_with('/'), "binary must not be an absolute path");

        // Verify --workspace auto is forwarded.
        let ws_idx = recorded.iter().position(|a| a == "--workspace").expect("--workspace present");
        assert_eq!(recorded[ws_idx + 1], "auto", "workspace must be `auto`");
        // Verify --transport stdio.
        let tr_idx = recorded.iter().position(|a| a == "--transport").expect("--transport present");
        assert_eq!(recorded[tr_idx + 1], "stdio");
    }

    /// `init_claude` no longer writes MCP into settings.json. Claude
    /// Code 2.1+ reads MCP config from `~/.claude.json` (via `claude
    /// mcp add`), and silently ignores the settings.json `mcpServers`
    /// block. This test stubs `claude` on PATH so the init does not
    /// mutate real config, and verifies the non-MCP outputs: the hook
    /// is installed and the awareness doc is written. The MCP
    /// invocation itself is covered by
    /// `register_claude_mcp_invokes_claude_with_workspace_auto_and_bare_lain`.
    #[test]
    fn init_claude_writes_hook_and_awareness_without_settings_mcp_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&workspace).status().unwrap();

        // Prepend a tempdir containing a no-op `claude` stub to PATH
        // so `init_claude` does not call the real `claude mcp add`
        // (which would mutate the user's `~/.claude.json`).
        let stub_dir = tempfile::tempdir().unwrap();
        let stub = stub_dir.path().join("claude");
        std::fs::write(&stub, "#!/usr/bin/env bash\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!(
            "{}:{}",
            stub_dir.path().display(),
            original_path
        );
        // SAFETY: setting PATH in a test is racy with parallel tests, but
        // no other test in the `lain` bin test binary depends on the
        // identity of `claude` on PATH.
        std::env::set_var("PATH", &new_path);

        let result = (|| -> Result<(), anyhow::Error> {
            let claude_dir = home.join(".claude");
            let settings = claude_dir.join("settings.json");
            let lain_md = claude_dir.join("LAIN.md");
            init_claude(None, "stdio", 0, true, &claude_dir, &settings, &lain_md)?;

            // settings.json must NOT have mcpServers (Claude Code reads
            // MCP from ~/.claude.json, not settings.json).
            let body = std::fs::read_to_string(&settings).unwrap();
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(
                json.pointer("/mcpServers/lain").is_none(),
                "settings.json must not contain mcpServers.lain; Claude Code \
                 ignores it and uses ~/.claude.json instead. body: {body}"
            );

            // The hook script and the awareness doc must be installed.
            let hook = home.join(".claude/hooks/lain-hook.sh");
            assert!(hook.exists(), "PreToolUse hook script not installed at {hook:?}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = hook.metadata().unwrap().permissions().mode();
                assert!(mode & 0o111 != 0, "hook script must be executable (mode={mode:o})");
            }
            assert!(lain_md.exists(), "awareness doc not installed at {lain_md:?}");
            Ok(())
        })();

        // Restore PATH regardless of outcome.
        std::env::set_var("PATH", &original_path);
        result.unwrap();
    }

    /// Regression pin for the bundled OpenCode `AGENTS.md`. The agent only
    /// reaches for the right tool if the doc actually contains the trigger
    /// phrases and tool table. Asserts the structural shape so a future edit
    /// can't silently strip the guidance without a test failure.
    #[test]
    fn opencode_agents_md_contains_key_guidance() {
        let doc = OPENCODE_AGENTS_MD;
        assert!(
            doc.contains("When to use lain"),
            "AGENTS.md must have a 'When to use lain' section"
        );
        let required_tools = [
            "get_health",
            "find_anchors",
            "get_blast_radius",
            "trace_dependency",
            "semantic_search",
            "explain_symbol",
            "get_code_snippet",
            "find_dead_code",
            "get_coupling_radar",
        ];
        let missing: Vec<&str> = required_tools
            .iter()
            .filter(|name| !doc.contains(**name))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "AGENTS.md is missing tools: {missing:?}"
        );
        assert!(doc.contains("Workflows"), "missing 'Workflows' section");
        assert!(doc.contains("Caveats"), "missing 'Caveats' section");
    }

    /// Regression pin for the bundled Copilot `copilot-instructions.md`.
    /// Same intent as `claude_awareness_doc_contains_key_guidance` and
    /// `opencode_agents_md_contains_key_guidance`: the agent only
    /// reaches for the right tool if the doc actually contains the
    /// trigger phrases and tool table. A future edit that strips the
    /// guidance fails this test.
    #[test]
    fn copilot_instructions_md_contains_key_guidance() {
        let doc = COPILOT_INSTRUCTIONS_MD;
        assert!(
            doc.contains("When to use lain"),
            "copilot-instructions.md must have a 'When to use lain' section"
        );
        let required_tools = [
            "get_health",
            "find_anchors",
            "get_blast_radius",
            "trace_dependency",
            "semantic_search",
            "explain_symbol",
            "get_code_snippet",
            "find_dead_code",
            "get_coupling_radar",
        ];
        let missing: Vec<&str> = required_tools
            .iter()
            .filter(|name| !doc.contains(**name))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "copilot-instructions.md is missing tools: {missing:?}"
        );
        assert!(doc.contains("Workflows"), "missing 'Workflows' section");
        assert!(doc.contains("Caveats"), "missing 'Caveats' section");
    }

    /// Regression pin for the Claude awareness doc. The agent only reaches
    /// for the right tool if the doc actually contains the trigger phrases
    /// and tool table. Asserts the structural shape so a future edit can't
    /// silently strip the guidance without a test failure.
    #[test]
    fn claude_awareness_doc_contains_key_guidance() {
        let doc = CLAUDE_AWARENESS_MD;

        // Trigger / when-to-use section.
        assert!(
            doc.contains("When to use lain"),
            "Claude awareness doc must have a 'When to use lain' section"
        );

        // Full tool table — every tool an agent should know about must be
        // named explicitly. Adding a tool means adding a row here.
        let required_tools = [
            "get_health",
            "find_anchors",
            "get_blast_radius",
            "trace_dependency",
            "semantic_search",
            "explain_symbol",
            "get_code_snippet",
            "find_dead_code",
            "get_coupling_radar",
        ];
        let missing: Vec<&str> = required_tools
            .iter()
            .filter(|name| !doc.contains(*name))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "Claude awareness doc is missing tools: {missing:?}"
        );

        // Canonical workflows.
        assert!(doc.contains("Workflows"), "missing 'Workflows' section");
        assert!(doc.contains("I'm new here"), "missing 'new here' workflow");
        assert!(
            doc.contains("refactor"),
            "missing refactor / blast-radius workflow"
        );

        // Caveats must warn about cold-call latency and workspace scope.
        assert!(doc.contains("Caveats"), "missing 'Caveats' section");
        assert!(doc.contains("latency"), "missing cold-call latency note");
        assert!(doc.contains("workspace"), "missing workspace scope note");

        // No hardcoded /home/.../langostino path — workspace is auto-resolved.
        assert!(
            !doc.contains("/home/sebastian/orca/workspaces/lain/langostino"),
            "awareness doc must not hardcode the absolute workspace path"
        );
    }

    #[test]
    fn init_kimi_writes_workspace_auto() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        // git repo not strictly required by init_kimi, but matches the
        // pattern used by the claude test and reflects how a real user
        // would invoke `lain init kimi` (init pre-flight elsewhere
        // requires a git repo).
        Command::new("git").args(["init", "--quiet"]).current_dir(&workspace).status().unwrap();

        let kimi_root = home.join(".kimi-code");
        init_kimi(None, "stdio", 0, true, &kimi_root).unwrap();

        let plugin_path = kimi_root.join("plugins/managed/lain/kimi.plugin.json");
        let body = std::fs::read_to_string(&plugin_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let args = json.pointer("/mcpServers/lain/args").unwrap().as_array().unwrap();
        let slice: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(
            slice.windows(2).any(|w| w == ["--workspace", "auto"]),
            "expected --workspace auto in args, got: {slice:?}"
        );
    }

    /// Same `include_str!` source the install paths use, exposed at test
    /// time so the wrapper-resolution tests exercise the exact bytes that
    /// land on disk in `~/.kimi-code/plugins/managed/lain/bin/lain`.
    const KIMI_PLUGIN_WRAPPER_SCRIPT: &str = include_str!("kimi_plugin_wrapper.sh");

    #[test]
    fn kimi_wrapper_resolves_workspace_from_parent_cwd() {
        // The wrapper script reads /proc/$PPID/cwd and walks up to the
        // enclosing git repo, then execs the real `lain` (PATH). To make
        // $PPID point at a process whose cwd is the temp repo we spawn
        // `bash -c "cd <repo> && <wrapper> ...; sleep 60"` — the trailing
        // `sleep` keeps bash from exec'ing into the wrapper (which would
        // make its parent the test binary instead of bash).
        if which::which("lain").is_err() {
            eprintln!("skipping: `lain` is not on PATH");
            return;
        }
        if which::which("git").is_err() {
            eprintln!("skipping: `git` is not on PATH");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");

        // Materialise the wrapper exactly as `init_kimi` would.
        let wrapper_path = tmp.path().join("wrapper.sh");
        std::fs::write(&wrapper_path, KIMI_PLUGIN_WRAPPER_SCRIPT).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &wrapper_path,
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "cd {} && {} --workspace auto --transport stdio; sleep 60",
                repo.display(),
                wrapper_path.display(),
            ))
            .env("LAIN_PORT", "19997")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wrapper");

        let mut stderr = String::new();
        let start = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(20);
        let mut pipe = BufReader::new(child.stderr.take().unwrap()).lines();
        loop {
            match pipe.next() {
                Some(Ok(line)) => {
                    stderr.push_str(&line);
                    stderr.push('\n');
                    if stderr.contains("Serving repo") {
                        break;
                    }
                }
                Some(Err(_)) | None => break,
            }
            if start.elapsed() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timeout waiting for Serving repo (stderr so far: {stderr})");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            stderr.contains("Serving repo"),
            "stderr should advertise the resolved workspace; got: {stderr}"
        );
        assert!(
            stderr.contains(repo.to_str().unwrap()),
            "stderr should include the resolved repo path; got: {stderr}"
        );
    }

    #[test]
    fn kimi_wrapper_errors_outside_repo() {
        // Run the wrapper in a tempdir that is NOT a git repo. The wrapper
        // should refuse to fall back to a random directory and emit a
        // message that points the user at `--workspace <path>`.
        if which::which("git").is_err() {
            eprintln!("skipping: `git` is not on PATH");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let nonrepo = tmp.path().join("norepo");
        std::fs::create_dir_all(&nonrepo).unwrap();

        let wrapper_path = tmp.path().join("wrapper.sh");
        std::fs::write(&wrapper_path, KIMI_PLUGIN_WRAPPER_SCRIPT).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &wrapper_path,
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "cd {} && {} --workspace auto --transport stdio; sleep 60",
                nonrepo.display(),
                wrapper_path.display(),
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wrapper");

        let mut stderr = String::new();
        let start = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(5);
        let mut pipe = BufReader::new(child.stderr.take().unwrap()).lines();
        while let Some(Ok(line)) = pipe.next() {
            stderr.push_str(&line);
            stderr.push('\n');
            if stderr.contains("--workspace auto") {
                break;
            }
            if start.elapsed() > deadline {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            stderr.contains("--workspace auto"),
            "stderr should explain the missing git repo; got: {stderr}"
        );
    }

    fn temp_git_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        (tmp, ws)
    }

    #[test]
    fn init_opencode_writes_verified_mcp_config() {
        let (_tmp, ws) = temp_git_workspace();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(ws.join("opencode.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
        assert_eq!(lain["type"], "local");
        let cmd = lain["command"].as_array().expect("command is JSON array");
        let cmd: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(cmd.first().map(String::as_str), Some("lain"));
        assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
        assert_eq!(lain["enabled"], true);
        assert_eq!(lain["timeout"], 30000);
    }

    #[test]
    fn init_opencode_includes_embedding_model_when_provided() {
        let (_tmp, ws) = temp_git_workspace();
        let model = std::path::Path::new("/models/all-MiniLM-L6-v2.onnx");
        init_opencode(&ws, Some(model), "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("opencode.json")).unwrap()).unwrap();
        let cmd: Vec<String> = doc.pointer("/mcp/lain/command").unwrap().as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let idx = cmd.iter().position(|s| s == "--embedding-model").expect("--embedding-model present");
        assert_eq!(cmd[idx + 1], "/models/all-MiniLM-L6-v2.onnx");
    }

    #[test]
    fn init_opencode_writes_agents_md_in_project_root() {
        let (_tmp, ws) = temp_git_workspace();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let agents = ws.join("AGENTS.md");
        assert!(agents.exists(), "AGENTS.md must be written to project root");
        let body = std::fs::read_to_string(&agents).unwrap();
        assert!(body.contains("When to use lain"));
        assert!(body.contains("find_anchors"));
    }

    /// Regression: `init_opencode` used to unconditionally clobber an
    /// existing `AGENTS.md`, losing any project-specific guidance the
    /// user had written there. With `--yes` not passed, an existing
    /// awareness doc must be left alone.
    #[test]
    fn init_opencode_does_not_overwrite_existing_agents_md_without_yes() {
        let (_tmp, ws) = temp_git_workspace();
        let agents = ws.join("AGENTS.md");
        let custom = "# Project-specific AGENTS.md\n\nUser-curated guidance.\n";
        std::fs::write(&agents, custom).unwrap();

        init_opencode(&ws, None, "stdio", 0, /* yes = */ false, "project").unwrap();

        let body = std::fs::read_to_string(&agents).unwrap();
        assert_eq!(
            body, custom,
            "AGENTS.md must be preserved when yes=false; the existing file \
             was overwritten despite the spec saying to skip when not --yes"
        );
    }

    /// Mirror pin for the --yes branch: passing `yes=true` does
    /// overwrite an existing `AGENTS.md` with the bundled awareness
    /// doc, even after the skip-on-exists gate was added.
    #[test]
    fn init_opencode_yes_overwrites_existing_agents_md() {
        let (_tmp, ws) = temp_git_workspace();
        let agents = ws.join("AGENTS.md");
        std::fs::write(&agents, "# project doc\n").unwrap();

        init_opencode(&ws, None, "stdio", 0, /* yes = */ true, "project").unwrap();

        let body = std::fs::read_to_string(&agents).unwrap();
        assert_ne!(body, "# project doc\n", "yes=true must replace the existing doc");
        assert!(body.contains("When to use lain"), "bundled awareness doc content expected");
    }

    #[test]
    fn init_opencode_scope_user_writes_global_config() {
        // Serialize against other tests that mutate HOME. Cargo runs tests
        // in parallel, and a concurrent HOME change here would derail
        // `claude_round_trip_under_temp_home` (in cmds::agents::tests) and
        // `opencode_adapter_*` (in cmds::agents::adapters::opencode::tests).
        // Note: as of Task 3, both `HOME_LOCK` aliases resolve to the same
        // mutex (`crate::cmds::agents::tests::HOME_LOCK`), so we only lock
        // once — `std::sync::Mutex` is not reentrant.
        let _home_lock_guard = crate::cmds::agents::tests::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        // `HomeGuard` restores the prior HOME on scope exit (including
        // the panic path); binding it to a local keeps the restoration
        // tied to this test's frame.
        let _home_guard = HomeGuard::set(tmp.path());
        init_opencode(&ws, None, "stdio", 0, true, "user").unwrap();

        let global = tmp.path().join(".config/opencode/opencode.json");
        assert!(global.exists(), "user-scope must write ~/.config/opencode/opencode.json");
        assert!(!ws.join("opencode.json").exists(), "user-scope must NOT write project config");
        assert!(!ws.join("AGENTS.md").exists(), "user-scope must NOT write AGENTS.md");
    }

    #[test]
    fn init_opencode_merges_with_existing_opencode_json() {
        let (_tmp, ws) = temp_git_workspace();
        std::fs::write(
            ws.join("opencode.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "mcp": {
                    "other-server": { "type": "local", "command": ["x"], "enabled": true }
                },
                "$schema": "https://opencode.ai/config.json"
            }))
            .unwrap(),
        )
        .unwrap();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("opencode.json")).unwrap()).unwrap();
        assert!(doc.pointer("/mcp/other-server").is_some(), "other-server preserved");
        assert!(doc.pointer("/mcp/lain").is_some(), "lain added");
        assert_eq!(doc["$schema"], "https://opencode.ai/config.json", "other top-level keys preserved");
    }

    /// Local `temp_git_workspace_copilot` helper. Mirrors
    /// `temp_git_workspace` (the opencode helper), but named for
    /// grouping with the `init_copilot` tests. Each helper is kept
    /// separately so a future change to one suite's repo bootstrap
    /// doesn't accidentally affect the other.
    fn temp_git_workspace_copilot() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        (tmp, ws)
    }

    #[test]
    fn init_copilot_writes_verified_mcp_config() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/servers/lain").expect("servers.lain present");
        assert_eq!(lain["command"], "lain");
        let args = lain["args"].as_array().expect("args is JSON array");
        let cmd: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
    }

    #[test]
    fn init_copilot_includes_embedding_model_when_provided() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let model = std::path::Path::new("/models/all-MiniLM-L6-v2.onnx");
        init_copilot(&ws, Some(model), "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap()).unwrap();
        let cmd: Vec<String> = doc.pointer("/servers/lain/args").unwrap().as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let idx = cmd.iter().position(|s| s == "--embedding-model").expect("--embedding-model present");
        assert_eq!(cmd[idx + 1], "/models/all-MiniLM-L6-v2.onnx");
    }

    #[test]
    fn init_copilot_writes_copilot_instructions_md_in_project_root() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let instructions = ws.join(".github/copilot-instructions.md");
        assert!(instructions.exists(), ".github/copilot-instructions.md must be written to project root");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(body.contains("When to use lain"));
        assert!(body.contains("find_anchors"));
    }

    #[test]
    fn init_copilot_scope_user_writes_global_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        let _home_guard = HomeGuard::set(tmp.path());
        init_copilot(&ws, None, "stdio", 0, true, "user").unwrap();
        drop(_home_guard); // restore HOME before assertions, in case a later assert panics

        let global = tmp.path().join(".copilot/mcp-config.json");
        assert!(global.exists(), "user-scope must write ~/.copilot/mcp-config.json");
        assert!(!ws.join(".vscode/mcp.json").exists(), "user-scope must NOT write project .vscode/mcp.json");
        assert!(!ws.join(".github/copilot-instructions.md").exists(), "user-scope must NOT write project awareness doc");
    }

    #[test]
    fn init_copilot_merges_with_existing_mcp_json() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        std::fs::create_dir_all(ws.join(".vscode")).unwrap();
        std::fs::write(
            ws.join(".vscode/mcp.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "servers": {
                    "other-server": { "command": "x", "args": ["y"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap()).unwrap();
        assert!(doc.pointer("/servers/other-server").is_some(), "other-server preserved");
        assert!(doc.pointer("/servers/lain").is_some(), "lain added");
    }

    #[test]
    fn init_copilot_does_not_overwrite_existing_instructions_md_without_yes() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let instructions = ws.join(".github/copilot-instructions.md");
        std::fs::create_dir_all(instructions.parent().unwrap()).unwrap();
        std::fs::write(&instructions, "# my custom instructions\n").unwrap();
        init_copilot(&ws, None, "stdio", 0, false, "project").unwrap();
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert_eq!(body, "# my custom instructions\n", "existing awareness doc must be preserved when yes=false");
    }

    #[test]
    fn init_copilot_yes_overwrites_existing_instructions_md() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let instructions = ws.join(".github/copilot-instructions.md");
        std::fs::create_dir_all(instructions.parent().unwrap()).unwrap();
        std::fs::write(&instructions, "# my custom instructions\n").unwrap();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(body.contains("When to use lain"), "yes=true must replace with the bundled doc");
    }
}
