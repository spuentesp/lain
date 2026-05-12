use anyhow::Result;
use std::fs;
use std::io::Write;

pub fn run_init(
    agent: &str,
    workspace: Option<&std::path::Path>,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
) -> Result<()> {

    let workspace = workspace.unwrap_or_else(|| std::path::Path::new("."));
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

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
            init_claude(&workspace, embedding_model, transport, port, yes, &claude_dir, &settings_path, &lain_md_path)?;
        }
        "gemini" => {
            let gemini_dir = home_dir.join(".gemini");
            let settings_path = gemini_dir.join("settings.json");
            init_gemini(&workspace, embedding_model, transport, port, yes, &gemini_dir, &settings_path)?;
        }
        other => {
            eprintln!("Warning: agent '{}' MCP config not supported yet. Supported: claude, gemini, cursor, windsurf, cline", other);
        }
    }

    install_agent_doc(agent_type, &home_dir)?;

    println!("\nLAIN initialization complete!");
    println!("Restart your agent to use LAIN.");
    Ok(())
}

fn detect_agent(home_dir: &std::path::Path) -> &'static str {
    if home_dir.join(".claude").exists() { return "claude"; }
    if home_dir.join(".gemini").exists() { return "gemini"; }
    if home_dir.join(".cursor").exists() { return "cursor"; }
    if home_dir.join(".windsurf").exists() { return "windsurf"; }
    "claude"
}

fn init_claude(
    workspace: &std::path::Path,
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
        workspace.to_string_lossy().to_string(),
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

    write_awareness_doc(lain_md_path)?;

    Ok(())
}

fn init_gemini(
    workspace: &std::path::Path,
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
        workspace.to_string_lossy().to_string(),
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

fn write_awareness_doc(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        println!("Awareness doc already exists - skipped.");
        return Ok(());
    }
    let doc = r#"# LAIN — Query for structure. Use tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

## Quick Examples

```bash
lain query "find Function name handle | connect Calls direction incoming depth 1..=2"
lain query "find Function name save | connect Calls direction outgoing depth 1..=3"
lain query "find File name db.rs | connect CO_CHANGED_WITH"
lain query "find Function | limit 20"
```

Full reference: `docs/query-language.md`

## When NOT to query (use MCP tools)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source of any symbol
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
- `get_file_diff`, `get_commit_history` — git operations
- `get_health` — server health check

## Workflow

1. **Before editing** → query for blast radius
2. **Understanding flow** → query for call chain
3. **Need source** → `get_code_snippet`
4. **Meaning search** → `semantic_search`
"#;
    fs::write(path, doc)?;
    println!("Created ~/.claude/LAIN.md");
    Ok(())
}

fn install_agent_doc(agent_type: &str, home_dir: &std::path::Path) -> Result<()> {
    match agent_type {
        "gemini" => write_agent_doc(home_dir, ".gemini", "LAIN.md", GEMINI_DOC)?,
        "cursor" => write_agent_doc(home_dir, ".cursor", "LAIN.md", CURSOR_DOC)?,
        "windsurf" => write_agent_doc(home_dir, ".windsurf", "lain-rules.md", WINDSURF_DOC)?,
        "cline" => write_agent_doc(home_dir, ".cline", "lain-rules.md", CLINE_DOC)?,
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

const GEMINI_DOC: &str = r#"# LAIN — Query for structure. Use tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

## Quick Examples

```bash
lain query "find Function | limit 10"
lain query "find Class name User | connect Calls direction outgoing depth 1..=2"
lain query "find File name main.rs | connect Contains | connect Calls"
```

## When NOT to query (use MCP tools)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source code
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
"#;

const CURSOR_DOC: &str = r#"# LAIN — Query for structure. Use tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

## Quick Examples

```bash
lain query "find Function | limit 10"
lain query "find Class name User | connect Calls direction outgoing depth 1..=2"
lain query "find File name main.rs | connect Contains | connect Calls"
```

## When NOT to query (use MCP tools)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source code
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
"#;

const WINDSURF_DOC: &str = r#"# LAIN — Query for structure. Use tools only when query can't answer.

Rule: before any structural edit, query LAIN first.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

| Need | Command |
|------|---------|
| Who calls X? | `lain query "find Function name X \| connect Calls direction incoming"` |
| What does X call? | `lain query "find Function name X \| connect Calls direction outgoing depth 1..=2"` |
| Blast radius | `lain query "find Function name X \| connect Calls direction outgoing depth 1..=3"` |
| Co-change risk | `lain query "find File name X \| connect CO_CHANGED_WITH"` |
| Find by meaning | `semantic_search` tool |
| Read code | `get_code_snippet` tool |
| Find dead code | `find_dead_code` tool |
"#;

const CLINE_DOC: &str = r#"# LAIN — Query for structure. Use tools only when query can't answer.

Rule: query for structure. Use MCP tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

```bash
lain query "find Function name X | connect Calls direction incoming depth 1..=2"   # callers
lain query "find Function name X | connect Calls direction outgoing depth 1..=3"    # callees
lain query "find File name X | connect CO_CHANGED_WITH"                             # co-change
lain query "find Function | limit 20"                                                # overview
```

## MCP Tools (non-query operations only)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source code
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
- `get_file_diff`, `get_commit_history` — git operations
"#;
