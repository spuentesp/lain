#!/usr/bin/env bash
# End-to-end verification of the Lain multiplayer co-edit awareness.
#
# Sets up a real lain server against a 5-file fixture repo, wires 3
# Claude Code instances with the pre-edit hook + distinct LAIN_AGENT_NAME
# values, and exercises four scenarios:
#   A. agents ask about the code via the MCP tools
#   B. agents report where they're working via the hook
#   C. no clobbering — two agents edit different symbols in the same file
#   D. conflict detection — two agents claim the same symbol

set -euo pipefail

ROOT="${ROOT:-/home/sebastian/lain/.worktrees/consolidation}"
LAIN="$ROOT/target/release/lain"
URL="${URL:-http://localhost:9999/mcp}"
HOOK_PRE="$ROOT/hooks/claude-code/pre-edit.sh"
HOOK_POST="$ROOT/hooks/claude-code/post-edit.sh"
CLAUDE="${CLAUDE:-/home/sebastian/.local/bin/claude}"
WORK="${WORK:-/tmp/multiplayer-full}"
REPO="$WORK/repos/multiplayer-sample"
TIMEOUT="${TIMEOUT:-120}"

mkdir -p "$WORK"
trap 'cleanup' EXIT

stop_server() {
    # Two patterns on purpose: a server left over from an earlier run may
    # have been started with a relative --config, in which case the $WORK
    # pattern misses it, the new server cannot bind the port, and the
    # readiness probe below happily talks to the stale process instead.
    pkill -f "lain server.*$WORK" 2>/dev/null || true
    pkill -f "lain server.*--port 9999" 2>/dev/null || true
}

cleanup() {
    stop_server
    rm -rf "$WORK/.lain" "$WORK/hooks-state" 2>/dev/null || true
}

start_server() {
    stop_server
    sleep 2
    rm -rf "$WORK/.lain"
    mkdir -p "$WORK/.lain/federation"
    (cd "$WORK" && "$LAIN" server --config "$WORK/repos.yaml" --workspace multiplayer --transport http --port 9999 > "$WORK/server.log" 2>&1 &)
    for i in $(seq 1 60); do
        if curl -s -m 1 -X POST "$URL" -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","method":"tools/list","id":1}' >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "FAIL: server failed to start within 60s"
    return 1
}

mcp() {
    local body="$1"
    curl -s -m "$TIMEOUT" -X POST "$URL" -H 'Content-Type: application/json' -d "$body"
}

call_tool() {
    local name="$1" args="$2" id="${3:-$RANDOM}"
    mcp "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args},\"id\":$id}"
}

# Per-run Claude config. Written by write_claude_config, consumed by run_claude.
#
#   mcp-http.json          — points the agents at the *running* HTTP server.
#                            The user's global config registers `lain` as a
#                            stdio server, which spawns a private server per
#                            instance; agents would never appear in the
#                            registry this script asserts against.
#   claude-settings.json   — the multiplayer pre/post-edit hooks only. Loaded
#                            with --setting-sources project,local so the
#                            user's ~/.claude/settings.json stays out of the
#                            run: its catch-all `lain ask` PreToolUse hook
#                            errors on ToolSearch, which stops Claude from
#                            ever loading the mcp__lain__* schemas.
write_claude_config() {
    mkdir -p "$WORK"
    cat > "$WORK/mcp-http.json" <<EOF
{"mcpServers":{"lain":{"type":"http","url":"$URL"}}}
EOF
    cat > "$WORK/claude-settings.json" <<EOF
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Edit|Write|MultiEdit",
        "hooks": [{ "type": "command", "command": "$HOOK_PRE", "timeout": 60 }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit",
        "hooks": [{ "type": "command", "command": "$HOOK_POST", "timeout": 60 }] }
    ]
  }
}
EOF
}

run_claude() {
    local agent_name="$1"
    local prompt="$2"
    local outfile="$WORK/logs/${agent_name}.log"
    mkdir -p "$WORK/logs"
    write_claude_config
    # The prompt goes in on stdin: --mcp-config is variadic and swallows a
    # positional prompt as a second config path.
    (cd "$REPO" && echo "$prompt" | LAIN_AGENT_NAME="$agent_name" LAIN_URL="$URL" \
        timeout 180 "$CLAUDE" --print --dangerously-skip-permissions \
        --strict-mcp-config --mcp-config "$WORK/mcp-http.json" \
        --settings "$WORK/claude-settings.json" --setting-sources project,local \
        > "$outfile" 2>&1) || true
}

hook_fires() {
    local agent_name="$1"
    local path="$2"
    local cache="$HOME/.config/lain/hooks/${agent_name}.session"
    [ -f "$cache" ] && grep -q "session_token" "$cache" && return 0
    return 1
}

cleanup_claude_settings() {
    if [ -f "$WORK/claude-settings.backup.json" ]; then
        cp "$WORK/claude-settings.backup.json" "$HOME/.claude/settings.json"
        rm -f "$WORK/claude-settings.backup.json"
    fi
}

# NOTE: cleanup_claude_settings is deliberately NOT wired to EXIT. The
# multiplayer hook config in ~/.claude/settings.json (and its backup) has to
# survive between scenarios and between runs; restoring + deleting the backup
# on every exit would clobber it mid-suite. The final restoration step calls
# this explicitly.

echo "═══════════════════════════════════════════════════════════════════"
echo " SCENARIO A: agents ask about the code"
echo " Each Claude instance runs the same MCP-tool prompt"
echo "═══════════════════════════════════════════════════════════════════"

# Start a fresh server for this scenario (clears state from prior runs).
start_server

# Three agents each register, then call the same three MCP tools.
# The register_agent call is what puts them in the presence registry —
# nothing else in this scenario touches presence (the pre-edit hook only
# fires on Edit/Write, which is scenario B).
PROMPT_A="Call the mcp__lain__register_agent tool with name=claude-A, then use the mcp__lain__list_repos, mcp__lain__get_federation_health, and mcp__lain__list_occupancy tools to inspect the workspace. Report what you find in 3 lines."
run_claude claude-A "${PROMPT_A}"
run_claude claude-B "${PROMPT_A//claude-A/claude-B}"
run_claude claude-C "${PROMPT_A//claude-A/claude-C}"

# Verify all 3 are registered. content[0].text is a JSON *string* holding the
# array, so it needs a second parse before counting.
RESULT=$(call_tool list_active_agents '{}')
COUNT=$(echo "$RESULT" | python3 -c "import json,sys; print(len(json.loads(json.loads(sys.stdin.read())['result']['content'][0]['text'])))" 2>/dev/null || echo 0)
NAMES=$(echo "$RESULT" | python3 -c "import json,sys; print(', '.join(sorted(a['name'] for a in json.loads(json.loads(sys.stdin.read())['result']['content'][0]['text']))))" 2>/dev/null || echo "?")
if [ "$COUNT" -eq 3 ]; then
    echo "OK: 3 agents registered ($NAMES)"
else
    echo "FAIL: expected 3 agents, got $COUNT ($NAMES)"
    echo "  list_active_agents: $RESULT"
    for a in claude-A claude-B claude-C; do
        echo "  --- tail $WORK/logs/$a.log"
        tail -n 8 "$WORK/logs/$a.log" 2>/dev/null | sed 's/^/  /' || echo "  (no log)"
    done
    exit 1
fi

# Each agent must see the same workspace state — sanity-check via list_occupancy.
REPOS=$(call_tool list_repos '{}' | python3 -c "import json,sys; r=json.loads(json.loads(sys.stdin.read())['result']['content'][0]['text']); print(len(r))" 2>/dev/null || echo 0)
if [ "$REPOS" -ge 1 ]; then
    echo "OK: $REPOS repo(s) visible"
else
    echo "FAIL: no repos visible"
    exit 1
fi
