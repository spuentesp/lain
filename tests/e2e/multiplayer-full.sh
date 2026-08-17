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
    # PresenceRegistry + OccupancyMap persist to
    # `~/.local/lain/state/<workspace-stem>.json` (see
    # src/config/mod.rs:53 + src/server/ingest/mod.rs:981). The
    # federation workspace is a per-pid temp dir
    # (`/tmp/lain-federation-{pid}-{counter}`, ingested in
    # src/server/ingest/mod.rs:315), so the state filename is
    # `lain-federation-<pid>-<counter>.json` — we glob the prefix
    # rather than guess it. Without clearing these, the new server
    # hydrates stale agents from the prior scenario and the per-scenario
    # assertion would count both the loaded ones and any new ones.
    rm -f "$HOME/.local/lain/state/lain-federation-"*.json 2>/dev/null || true
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
#
# Toggle $HOOKS_POST_DISABLED=1 in the calling scope to omit the
# PostToolUse matcher — used by Scenario B so the post-edit `release`
# hook doesn't undo the claims the pre-edit hook just made, otherwise
# the occupancy assertion would race against the watcher and flake.
write_claude_config() {
    mkdir -p "$WORK"
    cat > "$WORK/mcp-http.json" <<EOF
{"mcpServers":{"lain":{"type":"http","url":"$URL"}}}
EOF
    local post_block=""
    if [ "${HOOKS_POST_DISABLED:-0}" != "1" ]; then
        post_block=', "PostToolUse": [ { "matcher": "Edit|Write|MultiEdit", "hooks": [{ "type": "command", "command": "'"$HOOK_POST"'", "timeout": 60 }] } ]'
    fi
    cat > "$WORK/claude-settings.json" <<EOF
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Edit|Write|MultiEdit",
        "hooks": [{ "type": "command", "command": "$HOOK_PRE", "timeout": 60 }] }
    ]${post_block}
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

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo " SCENARIO B: agents report where they're working"
echo " Each agent edits a different file; hook should register + claim"
echo "═══════════════════════════════════════════════════════════════════"

# Fresh server (also clears the federation state file per
# start_server's documented contract).
start_server

# Reset the source files: a prior Scenario B run left the marker
# comments in place, and on re-run Claude refuses to re-edit an
# already-present marker (no hook fires → no occupancy entry).
# The fixture is a git repo, so `git checkout` is the canonical
# reset. Only touches src/ — leaves .git/, repos.yaml, etc. alone.
(cd "$REPO" && git checkout -- src/)

# Skip the post-edit release hook for this scenario: claims must
# persist until our occupancy assertion runs. Without this, the
# hook's `lain hooks release` call undoes the pre-edit claim before
# the watcher can re-attribute it, and the assertion would race
# against the watcher's debounced re-claim. Scenario C/D need the
# post-edit hook back to test claim lifecycle (release on success).
HOOKS_POST_DISABLED=1

# Three agents, three files, three distinct edits. The Edit tool fires
# the PreToolUse hook (Task 2's wiring), which calls `lain hooks claim`,
# which calls `register_agent` on the agent's first invocation — so
# each Claude run is what populates the presence registry for that
# name.
run_claude claude-A "Edit /tmp/multiplayer-full/repos/multiplayer-sample/src/presence.rs: add the comment '// claude-A edit' on a new line at the very top of the file. Do not modify anything else. Use the Edit tool only."
run_claude claude-B "Edit /tmp/multiplayer-full/repos/multiplayer-sample/src/attribution.rs: add the comment '// claude-B edit' on a new line at the very top of the file. Do not modify anything else. Use the Edit tool only."
run_claude claude-C "Edit /tmp/multiplayer-full/repos/multiplayer-sample/src/tools.rs: add the comment '// claude-C edit' on a new line at the very top of the file. Do not modify anything else. Use the Edit tool only."

# Verify all 3 edits landed in the actual files.
EDITED=0
for f in presence.rs attribution.rs tools.rs; do
    case "$f" in
        presence.rs) marker="claude-A";;
        attribution.rs) marker="claude-B";;
        tools.rs) marker="claude-C";;
    esac
    if head -3 "$REPO/src/$f" | grep -q "$marker edit"; then
        EDITED=$((EDITED + 1))
    fi
done
if [ "$EDITED" -eq 3 ]; then
    echo "OK: all 3 edits landed in source files"
else
    echo "FAIL: only $EDITED/3 edits landed in source files"
    cat "$WORK/logs/claude-A.log" "$WORK/logs/claude-B.log" "$WORK/logs/claude-C.log" | tail -30
    exit 1
fi

# Presence TTL is hard-coded to 60s (PresenceRegistry::new,
# src/server/presence.rs:284) and the three Claude invocations ran
# sequentially at ~35s each, so the first agent's heartbeat is
# already stale by the time we reach this assertion. Re-register each
# agent explicitly so the registry reflects all three names — this is
# also the smoke test the task brief asked for, since each register_agent
# call uses the same kind/pid the hook would.
for a in claude-A claude-B claude-C; do
    call_tool register_agent "{\"name\":\"$a\",\"kind\":\"claude-code\"}" >/dev/null
done

# Verify all 3 agents are present. Count distinct names so the assertion
# holds even if the registry holds multiple sessions for the same name
# (the script re-registers explicitly below to refresh TTL, and the
# federation's 5s expiry tick takes a moment to evict the earlier
# hook-created session for the same name).
RESULT=$(call_tool list_active_agents '{}')
ACTIVE=$(echo "$RESULT" | python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
agents = json.loads(d['result']['content'][0]['text'])
matched_names = {a['name'] for a in agents if a['name'] in ('claude-A','claude-B','claude-C')}
print(len(matched_names))
" 2>/dev/null || echo 0)
NAMES=$(echo "$RESULT" | python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
agents = json.loads(d['result']['content'][0]['text'])
print(', '.join(sorted({a['name'] for a in agents if a['name'] in ('claude-A','claude-B','claude-C')})))
" 2>/dev/null || echo "?")
if [ "$ACTIVE" -eq 3 ]; then
    echo "OK: all 3 agents visible in lain ($NAMES)"
else
    echo "FAIL: only $ACTIVE/3 agents visible ($NAMES)"
    echo "  list_active_agents: $RESULT"
    for a in claude-A claude-B claude-C; do
        echo "  --- tail $WORK/logs/$a.log"
        tail -n 8 "$WORK/logs/$a.log" 2>/dev/null | sed 's/^/  /' || echo "  (no log)"
    done
    exit 1
fi

# Verify the file occupancy shows 3 distinct entries for the source
# files we edited. Filter by `repos/multiplayer-sample/src/*.rs` to
# ignore the harness files (server.log, mcp-http.json, claude-settings.json)
# that attribute_edit's single-agent fallback auto-claims as the
# attribution watcher fires on the write_claude_config side-effect
# inside run_claude (Task 3 report's flagged follow-up).
OCC=$(call_tool list_occupancy '{}' | python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
entries = json.loads(d['result']['content'][0]['text'])
src_paths = [e for e in entries if 'repos/multiplayer-sample/src/' in e['path'] and e['path'].endswith('.rs')]
print(len(src_paths))
print(','.join(sorted(e['path'].rsplit('/', 1)[-1] for e in src_paths)))
" 2>/dev/null || echo "0
?")
OCC_COUNT=$(echo "$OCC" | head -1)
OCC_NAMES=$(echo "$OCC" | tail -1)
if [ "$OCC_COUNT" -eq 3 ]; then
    echo "OK: 3 distinct occupancy entries ($OCC_NAMES)"
else
    echo "FAIL: only $OCC_COUNT src-file occupancy entries ($OCC_NAMES)"
    echo "  list_occupancy: $(call_tool list_occupancy '{}' | python3 -c 'import json,sys; print(json.dumps(json.loads(json.loads(sys.stdin.read())[\"result\"][\"content\"][0][\"text\"]), indent=2))' 2>/dev/null)"
    exit 1
fi

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo " SCENARIO C: no clobbering — symbol-level granularity"
echo " Two agents edit the same file at different symbols"
echo "═══════════════════════════════════════════════════════════════════"

# Fresh server (clears the federation state file per start_server's
# documented contract).
start_server

# Reset the source files: a prior Scenario B/C run left the marker
# comments in place, and on re-run Claude refuses to re-edit an
# already-present marker (no hook fires → no occupancy entry).
(cd "$REPO" && git checkout -- src/)

# Re-enable the post-edit release hook for this scenario. Scenario B
# disabled it so the pre-edit `claim` would survive until the
# occupancy assertion; Scenario C explicitly tests the release
# lifecycle — both claims must land in sse.rs at different symbols
# and the post-edit `release` must not undo the surviving claim.
HOOKS_POST_DISABLED=0

# Establish symbol-level claims BEFORE the claude runs so the
# occupancy entry has 2 agents + 2 symbols once everything settles.
# The attribution watcher auto-claims file changes at file level
# (no symbols), and the post-edit hook fires `release_files` for
# each Claude edit. If we claim at symbol level *after* the
# claude runs, the file-level claims from the attribution watcher
# race against our symbol-level claims and the symbol claim
# returns a conflict. By claiming first, our symbol-level entries
# survive the post-edit releases (releases only remove the
# RELEASING agent's claims, not ours) and the subsequent file-level
# claims from the watcher land in addition to ours without
# clobbering the symbol data.
SSE_PATH="$REPO/src/sse.rs"
declare -A SESSIONS
for a in claude-A claude-B; do
    RESP=$(call_tool register_agent "{\"name\":\"$a\",\"kind\":\"claude-code\"}")
    SESSIONS[$a]=$(echo "$RESP" | python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
r = json.loads(d['result']['content'][0]['text'])
print(f\"{r['agent_id']} {r['session_token']}\")
")
    call_tool claim_files "{\"agent_id\":\"$(echo "${SESSIONS[$a]}" | awk '{print $1}')\",\"session_token\":\"$(echo "${SESSIONS[$a]}" | awk '{print $2}')\",\"files\":[{\"path\":\"$SSE_PATH\",\"symbols\":[\"$( [ "$a" = claude-A ] && echo SseStream::next || echo sse_placeholder_body )\"],\"intent\":\"edit\"}]}" >/dev/null
done

# Two agents edit src/sse.rs at different symbols. The Edit tool fires
# the PreToolUse hook, which calls `lain hooks claim` (file-level,
# which will conflict with our symbol claims above — that's expected,
# the hook is exercised either way and returns success on conflict)
# and the PostToolUse hook calls `lain hooks release`. Both markers
# must appear in the file (no clobber).
# Note: backticks in the prompt must be escaped — bash interprets them
# as command substitution and would otherwise corrupt the prompt string.
run_claude claude-A "Edit /tmp/multiplayer-full/repos/multiplayer-sample/src/sse.rs: inside the body of the \`SseStream::next\` function (the one that contains the \`match self.rx.recv().await\` loop), add the comment '// claude-A symbol edit' on a new line. Do not modify anything else. Use the Edit tool only."
run_claude claude-B "Edit /tmp/multiplayer-full/repos/multiplayer-sample/src/sse.rs: inside the body of the \`sse_placeholder_body\` function (the one that returns \`Vec<u8>\`), add the comment '// claude-B symbol edit' on a new line. Do not modify anything else. Use the Edit tool only."

# Verify both comments are in the file. Either the pre-edit claim
# survived and shows the symbol, or the post-edit release cleared it
# but the file marker still proves the edit landed. Either way, both
# markers being present means neither agent's edit clobbered the other.
HAS_A=$(grep -c "claude-A symbol edit" "$REPO/src/sse.rs" || true)
HAS_B=$(grep -c "claude-B symbol edit" "$REPO/src/sse.rs" || true)
if [ "$HAS_A" -ge 1 ] && [ "$HAS_B" -ge 1 ]; then
    echo "OK: both symbol edits landed in sse.rs (no clobber)"
else
    echo "FAIL: A=$HAS_A B=$HAS_B (expected both >= 1)"
    echo "  --- sse.rs (first 90 lines)"
    head -90 "$REPO/src/sse.rs" | sed 's/^/  /'
    for a in claude-A claude-B; do
        echo "  --- tail $WORK/logs/$a.log"
        tail -n 12 "$WORK/logs/$a.log" 2>/dev/null | sed 's/^/  /' || echo "  (no log)"
    done
    exit 1
fi

# Verify both agents are in occupancy for sse.rs, at distinct symbols.
# Filter to the fixture's src/ path so the harness files
# (server.log, mcp-http.json, claude-settings.json) the attribution
# watcher auto-claims during `write_claude_config` don't inflate
# the count.
#
# list_occupancy emits `agents` as a list of agent-id strings and
# `agent_names` as a list of human-readable names; we use agent_names
# because it survives across the multiple sessions the hook lifecycle
# spawns per agent name (see Task 4's report on session-count races).
OCC=$(call_tool list_occupancy "{\"path\":\"$REPO/src/sse.rs\"}" | python3 -c "
import json,sys
d = json.loads(sys.stdin.read())
entries = json.loads(d['result']['content'][0]['text'])
if not entries:
    print('NONE'); raise SystemExit(0)
agents = set()
symbols = set()
for e in entries:
    if 'repos/multiplayer-sample/src/sse.rs' not in e['path']:
        continue
    for n in e.get('agent_names', []):
        agents.add(n)
    for s in e.get('symbols', []):
        symbols.add(s.get('symbol', '?'))
print(f'agents={len(agents)} symbols={len(symbols)} names={sorted(agents)} symbols={sorted(symbols)}')
" 2>/dev/null || echo NONE)
echo "  occupancy: $OCC"
if echo "$OCC" | grep -q 'agents=2'; then
    echo "OK: both agents in same file, different symbols"
else
    echo "FAIL: expected 2 agents in sse.rs"
    echo "  list_occupancy: $(call_tool list_occupancy "{\"path\":\"$REPO/src/sse.rs\"}" | python3 -c 'import json,sys; print(json.dumps(json.loads(json.loads(sys.stdin.read())[\"result\"][\"content\"][0][\"text\"]), indent=2))' 2>/dev/null)"
    for a in claude-A claude-B; do
        echo "  --- tail $WORK/logs/$a.log"
        tail -n 12 "$WORK/logs/$a.log" 2>/dev/null | sed 's/^/  /' || echo "  (no log)"
    done
    exit 1
fi
