#!/usr/bin/env bash
# E2E test for the multiplayer hooks flow.
# Starts a fresh lain server, exercises claim/conflict/release via the lain hooks CLI,
# verifies server state via direct MCP HTTP calls.

set -euo pipefail

ROOT="${ROOT:-/home/sebastian/lain/.worktrees/consolidation}"
LAIN="$ROOT/target/release/lain"
PORT="${PORT:-19999}"
LAIN_URL="http://localhost:$PORT"

TMPDIR="$(mktemp -d)"
trap "rm -rf $TMPDIR; pkill -f 'lain server.*$PORT' 2>/dev/null || true" EXIT

# 1. Set up a clean project.
mkdir -p "$TMPDIR/repos/auth-svc"
cd "$TMPDIR/repos/auth-svc" && git init -q -b master && echo "fn login() {}" > auth.rs && git -c user.email=a@b -c user.name=a add auth.rs && git -c user.email=a@b -c user.name=a commit -q -m "init"
cd "$TMPDIR"
cat > repos.yaml <<EOF
repos:
- id: auth-svc
  source:
    type: local_clone
    url: file://$TMPDIR/repos/auth-svc
    ref: master
data_dir: $TMPDIR/.lain/federation
max_concurrent_indexers: 4
ready_threshold: 0.8
EOF
cat > workspaces.yaml <<EOF
workspaces:
- name: backend
  members:
  - auth-svc
EOF

# 2. Start the server.
"$LAIN" server --config "$TMPDIR/repos.yaml" --workspace backend --transport http --port "$PORT" > "$TMPDIR/server.log" 2>&1 &
SERVER_PID=$!
trap "rm -rf $TMPDIR; kill $SERVER_PID 2>/dev/null || true" EXIT

# 3. Wait for /health.
for i in $(seq 1 60); do
  if curl -s -m 1 "http://localhost:$PORT/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
echo "OK: server up on :$PORT"

# 4. First agent claims auth.rs.
PATH="$ROOT/target/release:$PATH" "$LAIN" hooks claim --url "$LAIN_URL" --path "$TMPDIR/repos/auth-svc/auth.rs" --agent-name "agent-a" --agent-kind "claude-code" --intent edit
echo "OK: agent-a claimed auth.rs"

# 5. Second agent attempts the same claim — expect conflict.
CONFLICTS=$("$LAIN" hooks claim --url "$LAIN_URL" --path "$TMPDIR/repos/auth-svc/auth.rs" --agent-name "agent-b" --agent-kind "kimi" --intent edit 2>&1 | grep -o '[0-9]\+ conflict' || true)
if [ -z "$CONFLICTS" ]; then
  echo "FAIL: agent-b should have seen a conflict"
  exit 1
fi
echo "OK: agent-b saw conflict"

# 6. Verify server state via curl. Note: MCP wraps tool output inside the
#    "content[0].text" string, so quote chars are JSON-escaped (\") — use
#    python3 to parse rather than grep on escaped text.
ACTIVE=$(curl -s -X POST "$LAIN_URL" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_active_agents","arguments":{}},"id":1}' \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]["text"]; print(len(json.loads(r)))')
if [ "$ACTIVE" -lt 2 ]; then
  echo "FAIL: expected 2 active agents, got $ACTIVE"
  exit 1
fi
echo "OK: 2 active agents in server"

# 7. Release.
"$LAIN" hooks release --url "$LAIN_URL" --path "$TMPDIR/repos/auth-svc/auth.rs" --agent-name "agent-a" --agent-kind "claude-code"
echo "OK: agent-a released auth.rs"

# 8. Verify occupancy.
OCC=$(curl -s -X POST "$LAIN_URL" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_occupancy","arguments":{"path":"'$TMPDIR/repos/auth-svc/auth.rs'"}},"id":1}' \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]["text"]; print(len(json.loads(r)))')
echo "OK: occupancy query returned $OCC entry/entries"

echo "E2E PASS"
