use anyhow::Result;

pub fn run_ask() -> Result<()> {
    use std::io::Read;

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(0);
    }

    let json: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => std::process::exit(0),
    };

    let tool_name = json.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let command = json.get("tool_input")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if tool_name != "Bash" && tool_name != "bash" {
        std::process::exit(0);
    }

    let is_lain_query = command.contains("blast")
        || command.contains("call chain")
        || command.contains("graph")
        || command.contains("dependencies")
        || command.contains("find_anchors")
        || command.contains("get_blast")
        || command.contains("get_call");

    if !is_lain_query {
        std::process::exit(0);
    }

    let response = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": { "command": command }
        }
    });

    println!("{}", serde_json::to_string(&response).unwrap_or_default());
    Ok(())
}
