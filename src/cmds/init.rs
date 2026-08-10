use anyhow::Result;
use std::fs;
use std::io::Write;
use std::process::Command;

use crate::cmds::agents::adapters::AUTO_WORKSPACE;

/// Supported agent names. Anything else is a user error and `run_init` will
/// refuse rather than silently writing nothing.
const SUPPORTED_AGENTS: &[&str] = &["claude", "gemini", "cursor", "windsurf", "cline", "kimi", "auto"];

// ── Bundled agent resources ────────────────────────────────────────────────
//
// These are the canonical per-agent install artifacts, embedded at compile
// time so downloaded-release binaries don't need filesystem lookups at
// runtime. Edit the source files under hooks/<agent>/ and the change ships
// with the next release; do not duplicate the content as string literals
// elsewhere.
const CLAUDE_AWARENESS_MD: &str = include_str!("../../hooks/claude/lain-awareness.md");
const CLAUDE_HOOK_SH: &str = include_str!("../../hooks/claude/lain-hook.sh");
const GEMINI_AWARENESS_MD: &str = include_str!("../../hooks/gemini/GEMINI.md");
const CURSOR_AWARENESS_MD: &str = include_str!("../../hooks/cursor/lain-awareness.md");
const WINDSURF_RULES_MD: &str = include_str!("../../hooks/windsurf/lain-rules.md");
const CLINE_RULES_MD: &str = include_str!("../../hooks/cline/lain-rules.md");
const KIMI_SKILL_MD: &str = include_str!("../../hooks/kimi/skills/lain/SKILL.md");
const KIMI_PLUGIN_WRAPPER_SH: &str = include_str!("kimi_plugin_wrapper.sh");

pub fn run_init(
    agent: &str,
    workspace: Option<&std::path::Path>,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
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
        "command": which::which("lain").map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "lain".to_string()),
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
        println!("Updated ~/.claude/settings.json");
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
        "command": which::which("lain").map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "lain".to_string()),
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
        "command": which::which("lain").map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "lain".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_claude_writes_workspace_auto() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        // git repo required by init pre-flight
        Command::new("git").args(["init", "--quiet"]).current_dir(&workspace).status().unwrap();

        let claude_dir = home.join(".claude");
        let settings = claude_dir.join("settings.json");
        let lain_md = claude_dir.join("LAIN.md");
        init_claude(None, "stdio", 0, true, &claude_dir, &settings, &lain_md).unwrap();

        let body = std::fs::read_to_string(&settings).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let args = json.pointer("/mcpServers/lain/args").unwrap().as_array().unwrap();
        let slice: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(
            slice.windows(2).any(|w| w == ["--workspace", "auto"]),
            "expected --workspace auto in args, got: {slice:?}"
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
}
