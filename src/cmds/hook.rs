use anyhow::Result;

pub fn run_hook_install(agent: Option<String>, uninstall: bool) -> Result<()> {
    use std::fs;

    let agent = agent.unwrap_or_else(|| "auto".to_string());
    let home_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let claude_dir = home_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    let settings_path = claude_dir.join("settings.json");

    if uninstall {
        return uninstall_hook(&agent, &settings_path);
    }

    println!("Installing LAIN hook for agent: {}", agent);
    fs::create_dir_all(&hooks_dir)?;

    let hook_script = include_str!("../../hooks/claude/lain-hook.sh");
    let hook_path = hooks_dir.join("lain-ask.sh");
    fs::write(&hook_path, hook_script)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&hook_path)?.permissions();
        let mut new_perms = perms.clone();
        new_perms.set_mode(0o755);
        fs::set_permissions(&hook_path, new_perms)?;
    }

    println!("Created ~/.claude/hooks/lain-ask.sh");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !settings.get("hooks").and_then(|v| v.as_object()).is_some() {
        if let Some(obj) = settings.as_object_mut() {
            obj.insert("hooks".to_string(), serde_json::json!({}));
        }
    }

    let hooks = settings.get_mut("hooks")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| anyhow::anyhow!("settings.json has corrupt hooks field"))?;

    if !hooks.contains_key("PreToolUse") {
        hooks.insert("PreToolUse".to_string(), serde_json::json!([]));
    }

    let pre_tool_use = hooks.get_mut("PreToolUse")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("settings.json PreToolUse is not an array"))?;

    pre_tool_use.retain(|v| {
        !v.as_object()
            .and_then(|o| o.get("hook"))
            .and_then(|h| h.as_str())
            .map(|s| s.contains("lain"))
            .unwrap_or(false)
    });

    pre_tool_use.push(serde_json::json!({ "hook": "lain ask" }));

    let settings_json = serde_json::to_string_pretty(&settings)?;
    let tmp_path = settings_path.with_extension("json.tmp");
    fs::write(&tmp_path, &settings_json)?;
    fs::rename(&tmp_path, &settings_path)?;

    println!("Updated ~/.claude/settings.json with PreToolUse hook");
    println!("\nHook installation complete!");
    println!("Restart your agent to use LAIN hook.");

    Ok(())
}

fn uninstall_hook(agent: &str, settings_path: &std::path::Path) -> Result<()> {
    use std::fs;
    println!("Uninstalling LAIN hook for agent: {}", agent);

    if !settings_path.exists() {
        println!("Hook uninstalled.");
        return Ok(());
    }

    let content = fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&content)
        .unwrap_or_else(|_| serde_json::json!({}));

    if let Some(obj) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        if let Some(arr) = obj.get_mut("PreToolUse").and_then(|v| v.as_array_mut()) {
            arr.retain(|v| {
                !v.as_object()
                    .and_then(|o| o.get("hook"))
                    .and_then(|h| h.as_str())
                    .map(|s| s.contains("lain"))
                    .unwrap_or(false)
            });
        }
    }

    let settings_json = serde_json::to_string_pretty(&settings)?;
    fs::write(settings_path, settings_json)?;
    println!("Hook uninstalled.");

    Ok(())
}
