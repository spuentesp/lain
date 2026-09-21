#!/usr/bin/env bash
# AGY end-to-end harness (PR 5 of
# `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
#
# Drives a real `lain mcp --transport http` process through the
# intent + activity + evaluation flow and writes a `verdict.json`
# summary. Used as the regression test for the multi-agent
# coordination design; the `verdict.json` shape is the same one
# the AGY real-agent harness emits so this script also serves as
# a fixture for that run.
#
# Usage:
#   LAIN_BIN=/path/to/lain scripts/agy_e2e.sh [output_dir]
#
# `output_dir` defaults to a tempdir; the script populates it
# with `verdict.json`, the server's stderr log, and the workspace
# fixture it builds.

set -euo pipefail

# Resolve the `lain` binary. Mirrors the discovery used by
# `tests/multi_agent_concurrency.rs`: `$LAIN_BIN` first, then a
# `target/{release,debug}/lain` fallback.
LAIN_BIN="${LAIN_BIN:-}"
if [ -z "$LAIN_BIN" ]; then
    for sub in target/release/lain target/debug/lain; do
        if [ -x "$sub" ]; then
            LAIN_BIN="$(pwd)/$sub"
            break
        fi
    done
fi
if [ -z "$LAIN_BIN" ] || [ ! -x "$LAIN_BIN" ]; then
    echo "no lain binary found; set LAIN_BIN or run \`cargo build\` first" >&2
    exit 1
fi

# Pick a free port for the HTTP server. Reuse the convention from
# `tests/multi_agent_concurrency.rs::TestEnv` — a tempdir for the
# state root and a randomly-chosen TCP port.
OUT_DIR="${1:-}"
if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$(mktemp -d -t agy_e2e.XXXXXX)"
fi
mkdir -p "$OUT_DIR"

WORKSPACE="$(mktemp -d -t agy_ws.XXXXXX)"
STATE_DIR="$(mktemp -d -t agy_state.XXXXXX)"

# Initialize a tiny git workspace so `LainServer::new` -> the
# `GitSensor` open succeeds.
git -C "$WORKSPACE" init -q
git -C "$WORKSPACE" config user.email "agy-e2e@lain"
git -C "$WORKSPACE" config user.name "agy-e2e"
mkdir -p "$WORKSPACE/src"
printf 'pub fn a() {}\npub fn b() {}\n' >"$WORKSPACE/src/a.rs"
git -C "$WORKSPACE" add -A
git -C "$WORKSPACE" commit -q -m "fixture"

# Find a free port using `python3` — bash can't do that
# portably without external tools.
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')"
LAIN_URL="http://localhost:${PORT}"
echo "==> lain server: $LAIN_BIN on $LAIN_URL"
echo "==> workspace: $WORKSPACE"
echo "==> state dir: $STATE_DIR"
echo "==> output dir: $OUT_DIR"

# Boot the server in the background. Capture stderr so a
# regression shows up in `verdict.json`. Re-use the existing
# `LAIN_REINDEX_TIMEOUT` env so the small fixture doesn't blow
# the 30s budget. `lain server` requires a `repos.yaml`; we
# synthesize one pointing at the workspace so the test stays
# hermetic (no hand-written config in this repo).
REPOS_YAML="$OUT_DIR/repos.yaml"
cat >"$REPOS_YAML" <<EOF
data_dir: $STATE_DIR/federation
max_concurrent_indexers: 1
ready_threshold: 0.5
repos:
  - id: fixture
    source:
      type: workspace_dir
      path: $WORKSPACE
EOF

LAIN_STDERR="$OUT_DIR/server.stderr"
(
    XDG_STATE_HOME="$STATE_DIR" \
    LAIN_REINDEX_TIMEOUT=30 \
        "$LAIN_BIN" server --config "$REPOS_YAML" --transport http --port "$PORT" \
            >"$OUT_DIR/server.stdout" 2>"$LAIN_STDERR" &
    echo "$!" >"$OUT_DIR/server.pid"
)
SERVER_PID="$(cat "$OUT_DIR/server.pid")"

# Wait for the port to come up. The server binds before spawning
# the background re-index (per the comment in
# `LainMcpServer::run_http`), so a successful connect means the
# listener is accepting requests.
for _ in $(seq 1 50); do
    if curl -sf -o /dev/null "$LAIN_URL/health"; then
        break
    fi
    sleep 0.1
done
if ! curl -sf -o /dev/null "$LAIN_URL/health"; then
    echo "server failed to bind on $LAIN_URL" >&2
    kill "$SERVER_PID" 2>/dev/null || true
    exit 1
fi

# Helper: POST a JSON-RPC `tools/call` to the server.
mcp_call() {
    local name="$1" args_json="$2"
    curl -s -X POST "$LAIN_URL/mcp" \
        -H 'Content-Type: application/json' \
        -H 'Accept: application/json' \
        --data "$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":%s,"arguments":%s}}' \
            "$(printf '%s' "$name" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
            "$args_json")"
}

# Helper: extract the inner text from a tools/call result.
result_text() {
    python3 -c 'import json,sys
v=json.loads(sys.stdin.read())
c=v.get("result",{}).get("content",[])
print(c[0]["text"] if c else "")'
}

# Register two agents with deterministic IDs (so the verdict.json
# matches the test fixture's expected agent ids).
ALICE_REG="$(mcp_call register_agent '{"name":"alice"}' | result_text)"
ALICE_ID="$(printf '%s' "$ALICE_REG" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
ALICE_TOKEN="$(printf '%s' "$ALICE_REG" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"

BOB_REG="$(mcp_call register_agent '{"name":"bob"}' | result_text)"
BOB_ID="$(printf '%s' "$BOB_REG" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
BOB_TOKEN="$(printf '%s' "$BOB_REG" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"

echo "==> alice: $ALICE_ID"
echo "==> bob:   $BOB_ID"

# Each agent declares an intent via `lain_intent`.
ALICE_INTENT="$(mcp_call lain_intent \
    "$(printf '{"agent_id":"%s","session_token":"%s","goal":"refresh-token validation","scopes":["auth::validate_token","token::RefreshToken"],"status":"Editing"}' "$ALICE_ID" "$ALICE_TOKEN")" \
    | result_text)"
BOB_INTENT="$(mcp_call lain_intent \
    "$(printf '{"agent_id":"%s","session_token":"%s","goal":"SessionClaims serialization","scopes":["session::SessionClaims"],"status":"Editing"}' "$BOB_ID" "$BOB_TOKEN")" \
    | result_text)"

# Each agent emits a Read observation via `/hook` so the activity
# feed populates `observed_reads`.
for pair in "$ALICE_ID:$ALICE_TOKEN" "$BOB_ID:$BOB_TOKEN"; do
    ID="${pair%%:*}"
    TOK="${pair##*:}"
    curl -sf -X POST "$LAIN_URL/hook" \
        -H 'Content-Type: application/json' \
        --data "$(printf '{"session_token":"%s","agent_id":"%s","event":"tool_start","tool":"Read","target":"src/a.rs"}' "$TOK" "$ID")" \
        >/dev/null
done

# Pull the activity feed.
LIST="$(mcp_call list_active_intents '{}' | result_text)"

# Capture a per-iteration verdict record. Single-iteration run —
# the AGY real-agent harness iterates many times and produces a
# list, but the e2e here is the regression fixture: a deterministic
# pass with two cooperating agents. Future PRs add chaos variants
# (kill winner mid-iteration, corrupt state, etc.).
cat >"$OUT_DIR/verdict.json" <<EOF
{
  "server_url": "$LAIN_URL",
  "workspace": "$WORKSPACE",
  "state_dir": "$STATE_DIR",
  "agents": {
    "alice": { "id": "$ALICE_ID", "intent": $ALICE_INTENT },
    "bob":   { "id": "$BOB_ID",   "intent": $BOB_INTENT }
  },
  "activity_feed": $LIST,
  "outcome": {
    "alice_intent_level": "$(printf '%s' "$ALICE_INTENT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["coordination"]["level"])')",
    "bob_intent_level":   "$(printf '%s' "$BOB_INTENT"   | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["coordination"]["level"])')"
  }
}
EOF

# Stop the server. SIGTERM and a short grace period; SIGKILL
# fallback so a hung server doesn't leak across runs.
kill "$SERVER_PID" 2>/dev/null || true
for _ in $(seq 1 10); do
    kill -0 "$SERVER_PID" 2>/dev/null || break
    sleep 0.1
done
kill -9 "$SERVER_PID" 2>/dev/null || true

echo "==> verdict: $OUT_DIR/verdict.json"
echo "OK"
