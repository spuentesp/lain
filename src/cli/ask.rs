use anyhow::Result;
use std::io::IsTerminal;

pub fn run_ask(question: Option<&str>) -> Result<()> {
    use std::io::Read;

    // The hook payload arrives either as the argument (hooks/claude's
    // wrapper reads stdin and passes it as the "question") or on stdin.
    // Reading only stdin made the wrapper a no-op: it had already drained
    // stdin, so `lain ask` always saw an empty pipe and exited 0.
    let input = match question.map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) if q.starts_with('{') => q.to_string(),
        Some(q) => {
            // A plain-language question: this command does not answer
            // those, and exiting 0 silently read as "nothing found".
            eprintln!(
                "lain ask handles PreToolUse hook JSON, not questions. To search the \
                 code, run:\n  lain oneshot search_code \"{q}\""
            );
            std::process::exit(2);
        }
        None => {
            if std::io::stdin().is_terminal() {
                eprintln!(
                    "lain ask is a PreToolUse hook handler, not an interactive \
                     CLI. Pipe a JSON request on stdin, e.g.:\n  \
                     echo '{{\"tool_name\":\"...\",\"tool_input\":{{...}}}}' | lain ask"
                );
                return Ok(());
            }
            let mut input = String::new();
            if std::io::stdin().read_to_string(&mut input).is_err() {
                return Ok(());
            }
            input
        }
    };
    // Empty input (e.g. `echo -n | lain ask`) is a no-op, not an error.
    if input.trim().is_empty() {
        return Ok(());
    }

    let json: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lain ask: invalid JSON on stdin: {e}");
            return Ok(());
        }
    };

    let tool_name = json.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let command = json
        .get("tool_input")
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
