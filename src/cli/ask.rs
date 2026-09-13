use anyhow::Result;
use std::io::IsTerminal;

pub fn run_ask() -> Result<()> {
    use std::io::Read;

    // Pre-fix this called `stdin.read_to_string` unconditionally.
    // When a user ran `lain ask` from a terminal (no JSON hook piped
    // on stdin), `read_to_string` blocked until the user typed
    // Ctrl+D — the only way out of the wait was EOF, after which the
    // process silently exited with status 0. A TTY interactive
    // `lain ask` invocation was effectively a no-op that hung
    // forever. The MCP wire protocol expects this command to be
    // a PreToolUse hook handler, not a user-facing CLI: stdin
    // either carries the hook JSON or is empty. Distinguish:
    //
    //   - stdin is a TTY → no hook JSON will arrive, exit cleanly
    //     with a hint that `lain ask` is a hook handler, not a CLI
    //   - stdin is a pipe (e.g. a closed pipe or a JSON payload) →
    //     read whatever's there; on EOF, exit 0 like before
    //   - stdin is a JSON object/array → parse and dispatch
    //
    // The "is TTY" check uses `stdin().is_terminal()` from
    // `std::io::IsTerminal`, which is the canonical stdlib check on
    // every Rust target (Unix, Windows, WASI-without-term).
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
    // Empty stdin (e.g. `echo -n | lain ask`) is a no-op, not an
    // error. Pre-fix this silently exited 0; preserve that.
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
