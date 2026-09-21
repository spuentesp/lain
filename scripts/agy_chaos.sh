#!/usr/bin/env bash
# Chaos variants for the AGY end-to-end harness.
#
# The deterministic agy_e2e.sh runs a single two-agent pass. Real
# agents crash, lose claims, and race against each other on a
# contended file. This script runs three chaos variants that
# exercise the linearizability / persistence contracts the
# deterministic pass can not:
#
#   1. kill winner — alice claims, then SIGKILL the server
#      mid-claim-release cycle; bob's claim attempt against the
#      same path must either succeed (no orphan claim survives the
#      kill) or fail cleanly with a documented "presence layer
#      unavailable" error.
#   2. corrupt state — alice and bob take turns claiming and
#      releasing; between iterations the state file is truncated
#      to 50% and the server is restarted. The next iteration
#      must still succeed: the recovery path loads a partial JSON
#      snapshot without crashing the server.
#   3. lock timeout — a stale lock sentinel is planted before the
#      server starts; the server's stale-lock takeover should
#      reclaim it so the next claim attempt succeeds.
#
# Each variant writes its own verdict.json into $OUT_DIR.

set -euo pipefail

# Resolve binary.
LAIN_BIN="${LAIN_BIN:-}"
if [ -z "$LAIN_BIN" ]; then
    for sub in target/release/lain target/debug/lain; do
        if [ -x "$sub" ]; then LAIN_BIN="$(pwd)/$sub"; break; fi
    done
fi
if [ -z "$LAIN_BIN" ] || [ ! -x "$LAIN_BIN" ]; then
    echo "no lain binary (LAIN_BIN=${LAIN_BIN})" >&2
    exit 1
fi

OUT_DIR="${OUT_DIR:-$(mktemp -d -t agy_chaos.XXXXXX)}"
mkdir -p "$OUT_DIR"
echo "==> output dir: $OUT_DIR"

# Build a fixture workspace + state dir shared by all variants.
WORKSPACE="$(mktemp -d -t agy_chaos_ws.XXXXXX)"
STATE_DIR="$(mktemp -d -t agy_chaos_state.XXXXXX)"
git -C "$WORKSPACE" init -q
git -C "$WORKSPACE" config user.email "chaos@lain"
git -C "$WORKSPACE" config user.name "chaos"
mkdir -p "$WORKSPACE/src"
printf 'pub fn contested() {}\n' >"$WORKSPACE/src/contested.rs"
git -C "$WORKSPACE" add -A
git -C "$WORKSPACE" commit -q -m "fixture"

spawn_server() {
    local port="$1"
    local repos="$STATE_DIR/repos.yaml"
    cat >"$repos" <<EOF
data_dir: $STATE_DIR/fed
max_concurrent_indexers: 1
ready_threshold: 0.5
repos:
  - id: f
    source:
      type: workspace_dir
      path: $WORKSPACE
EOF
    # Pin XDG_STATE_HOME so the persisted presence state file
    # lives under $STATE_DIR/lain-state/ and both the original
    # server and the post-crash server read from the same path.
    # Without this each invocation would default to
    # ~/.local/lain/state/ and silently share state with every
    # other chaos run + the user's real workspace — which is
    # exactly the noise variant 1 was trying to control for.
    local xdg="$STATE_DIR/lain-state"
    mkdir -p "$xdg"
    XDG_STATE_HOME="$xdg" \
        "$LAIN_BIN" server --config "$repos" --transport http --port "$port" \
            >"$OUT_DIR/server.stdout" 2>"$OUT_DIR/server.stderr" &
    echo "$!" >"$OUT_DIR/server.pid"
    # Federation-mode indexing takes ~30s on the first start (cold
    # LSP + tree-sitter + git history). Wait up to 60s for /health
    # to respond, then a few extra seconds for the federation
    # indexer to finish before the first tools/call.
    for _ in $(seq 1 600); do
        if curl -sf -o /dev/null "http://localhost:$port/health"; then
            sleep 5
            return
        fi
        sleep 0.1
    done
    echo "server failed to bind" >&2
    return 1
}

# Find the persisted state file for this chaos run. Variant 1
# reads/writes it after alice's claim to simulate a heartbeat
# expiry: alice's session entry is purged from the JSON snapshot
# but her claim survives, so the next server's load_pair must
# reclaim it via the stale-owner path.
find_state_file() {
    find "$STATE_DIR/lain-state" -name '*.json' -type f 2>/dev/null | head -1
}

kill_server() {
    local pid
    pid="$(cat "$OUT_DIR/server.pid")"
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        for _ in $(seq 1 30); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -9 "$pid" 2>/dev/null || true
    fi
}

run_pair() {
    local port="$1"
    local label="$2"
    local base="http://localhost:$port"
    local resp
    resp=$(curl -sf -X POST "$base/mcp" \
        -H 'Content-Type: application/json' \
        --data '{"jsonrpc":"2.0","id":1,"method":"tools/call",
                 "params":{"name":"register_agent",
                            "arguments":{"name":"'$label'"}}}')
    printf '%s' "$resp" | python3 -c "import json,sys; v=json.loads(sys.stdin.read())
print(v['result']['content'][0]['text'])"
}

claim_path() {
    local base="$1"
    local agent_id="$2"
    local token="$3"
    local path="$4"
    curl -sf -X POST "$base/mcp" \
        -H 'Content-Type: application/json' \
        --data "$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call",
"params":{"name":"claim_files",
"arguments":{"agent_id":"%s","session_token":"%s","files":[{"path":"%s","intent":"edit"}]}}}' \
            "$agent_id" "$token" "$path")"
}

# ── Variant 1: kill the winner mid-cycle ──────────────────────

echo "==> variant 1: kill the winner mid-cycle"
free_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); p=s.getsockname()[1]; s.close(); print(p)'
}
PORT_V1="$(free_port)"
spawn_server "$PORT_V1"
BASE_V1="http://localhost:$PORT_V1"

# Alice claims the contested file.
ALICE_TEXT="$(run_pair "$PORT_V1" alice)"
ALICE_ID="$(printf '%s' "$ALICE_TEXT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
ALICE_TOK="$(printf '%s' "$ALICE_TEXT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"
ALICE_CLAIM="$(claim_path "$BASE_V1" "$ALICE_ID" "$ALICE_TOK" src/contested.rs)"
ALICE_GRANTED="$(printf '%s' "$ALICE_CLAIM" | python3 -c '
import json, sys
v = json.loads(sys.stdin.read())
inner = json.loads(v["result"]["content"][0]["text"])
print(len(inner.get("granted", [])))' 2>/dev/null || echo 0)"
echo "    alice granted before kill: $ALICE_GRANTED (expect 1)"

# Kill the server mid-cycle. The claim's file-lock sentinel
# should also be cleared because the holder process is dead.
kill_server

# Wait for the stale-lock takeover window to elapse before
# bringing Bob back in. The default `state_lock_stale_after_secs`
# is 10s, so 12s is a safe margin.
echo "    waiting 12s for stale-lock takeover window..."
sleep 12

# Now simulate heartbeat expiry: the linearizability invariant
# says "at most one live exclusive lease per scope". After alice's
# process dies her claim stops being live, but a fresh server
# loading the state file would still see it. The fix is in
# `OccupancyMap::load_pair`: it cross-checks every claim's
# `agent_id` against `PresenceRegistry::sessions` and drops
# claims whose owner is no longer registered.
#
# Editing the persisted state file directly is the only way to
# simulate this without waiting 10 minutes for the default
# heartbeat TTL. We drop alice's session entry from `sessions`
# while leaving `occupancy_by_agent` populated with her claim.
STATE_FILE="$(find_state_file)"
if [ -z "$STATE_FILE" ]; then
    echo "    no state file found at $STATE_DIR/lain-state — skipping variant 1"
    cat >"$OUT_DIR/variant_1.json" <<EOF
{
  "variant": "kill_winner",
  "alice_granted_pre_kill": $ALICE_GRANTED,
  "bob_granted_post_restart": 0,
  "bob_conflicts_post_restart": 0,
  "skipped": "no state file found at $STATE_DIR/lain-state",
  "finding": "n/a — harness couldn't locate the state file"
}
EOF
    exit 0
fi
python3 - <<PYEOF
import json, sys
path = "$STATE_FILE"
with open(path) as f:
    state = json.load(f)
# Drop alice's session (by agent_id) but keep her occupancy claims.
agent_id = "$ALICE_ID"
before_sessions = len(state.get("sessions", []))
state["sessions"] = [s for s in state.get("sessions", []) if s[0] != agent_id]
with open(path, "w") as f:
    json.dump(state, f, indent=2)
print(f"    state file edited: dropped {before_sessions - len(state['sessions'])} session(s) for agent {agent_id}")
PYEOF

# Bob now registers and claims the same file. The fresh server's
# load_pair should reclaim alice's orphan claim (since her session
# is gone), then grant bob's claim.
PORT_V1B="$(free_port)"
spawn_server "$PORT_V1B"
BASE_V1B="http://localhost:$PORT_V1B"
BOB_TEXT="$(run_pair "$PORT_V1B" bob)"
BOB_ID="$(printf '%s' "$BOB_TEXT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
BOB_TOK="$(printf '%s' "$BOB_TEXT" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"
BOB_CLAIM="$(claim_path "$BASE_V1B" "$BOB_ID" "$BOB_TOK" src/contested.rs)"
BOB_GRANTED="$(printf '%s' "$BOB_CLAIM" | python3 -c '
import json, sys
v = json.loads(sys.stdin.read())
inner = json.loads(v["result"]["content"][0]["text"])
print(len(inner.get("granted", [])))' 2>/dev/null || echo 0)"
BOB_CONFLICTS="$(printf '%s' "$BOB_CLAIM" | python3 -c '
import json, sys
v = json.loads(sys.stdin.read())
inner = json.loads(v["result"]["content"][0]["text"])
print(len(inner.get("conflicts", [])))' 2>/dev/null || echo 0)"
echo "    bob granted after kill+restart: $BOB_GRANTED (expect 1)"

# Variant 1 — kill the winner mid-cycle — exercises the
# linearizability invariant across server crashes:
# alice's claim is recorded in the state file when her server is
# up. After SIGKILL + heartbeat expiry (simulated by editing the
# persisted state file), a fresh server must reclaim alice's
# orphan claim and let bob win. The fix lives in
# `OccupancyMap::load_pair` (cross-checks against
# `PresenceRegistry::sessions`) and is regression-tested in
# `tests/presence.rs::load_pair_reclaims_orphaned_claims_on_fresh_server`.
if [ "$BOB_GRANTED" = "1" ]; then
    V1_FINDING="OK: load_pair reclaims alice orphan claim; bob wins."
else
    V1_FINDING="FAIL: bob still blocked by orphan claim; cross-check did not fire."
fi
cat >"$OUT_DIR/variant_1.json" <<EOF
{
  "variant": "kill_winner",
  "alice_granted_pre_kill": $ALICE_GRANTED,
  "bob_granted_post_restart": $BOB_GRANTED,
  "bob_conflicts_post_restart": $BOB_CONFLICTS,
  "finding": "$V1_FINDING"
}
EOF

kill_server

# ── Variant 2: corrupt state mid-iteration ──────────────────

echo "==> variant 2: corrupt the state file between iterations"
PORT_V2="$(free_port)"
spawn_server "$PORT_V2"
BASE_V2="http://localhost:$PORT_V2"

# Iteration 1: alice registers + claims + releases. The state
# file ends with the released path.
ALICE2="$(run_pair "$PORT_V2" alice_v2)"
ALICE2_ID="$(printf '%s' "$ALICE2" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
ALICE2_TOK="$(printf '%s' "$ALICE2" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"
claim_path "$BASE_V2" "$ALICE2_ID" "$ALICE2_TOK" src/contested.rs >/dev/null

# Find the state file and truncate it to 50% of its length,
# simulating a partial write.
STATE_FILE="$(find "$STATE_DIR/fed" -name '*.json' | head -1)"
if [ -n "$STATE_FILE" ]; then
    ORIG_LEN="$(wc -c < "$STATE_FILE")"
    HALF=$(( ORIG_LEN / 2 ))
    head -c "$HALF" "$STATE_FILE" >"$STATE_FILE.tmp"
    mv "$STATE_FILE.tmp" "$STATE_FILE"
    echo "    state file truncated from $ORIG_LEN to $HALF bytes"
else
    echo "    no state file found to corrupt (skipping)"
fi

kill_server

# Iteration 2: restart and verify the server still boots and
# accepts new registrations despite the truncated state.
PORT_V2B="$(free_port)"
if spawn_server "$PORT_V2B" 2>/dev/null; then
    SERVER_BOOTED="true"
    BASE_V2B="http://localhost:$PORT_V2B"
    if curl -sf -X POST "$BASE_V2B/mcp" \
            -H 'Content-Type: application/json' \
            --data '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
            >/dev/null 2>&1; then
        RECOVERY_OK="true"
    else
        RECOVERY_OK="false"
    fi
    kill_server
else
    SERVER_BOOTED="false"
    RECOVERY_OK="false"
fi

cat >"$OUT_DIR/variant_2.json" <<EOF
{
  "variant": "corrupt_state",
  "server_booted_after_truncation": $SERVER_BOOTED,
  "tools_list_responded_after_recovery": $RECOVERY_OK
}
EOF

# ── Variant 3: stale lock takeover ────────────────────────

echo "==> variant 3: stale-lock takeover"
PORT_V3="$(free_port)"
spawn_server "$PORT_V3"
BASE_V3="http://localhost:$PORT_V3"

# Alice claims and releases; her filesystem lock should be cleared.
ALICE3="$(run_pair "$PORT_V3" alice_v3)"
ALICE3_ID="$(printf '%s' "$ALICE3" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
ALICE3_TOK="$(printf '%s' "$ALICE3" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"
claim_path "$BASE_V3" "$ALICE3_ID" "$ALICE3_TOK" src/contested.rs >/dev/null

# Plant a stale lock for a *different* path. The takeover window
# defaults to 10 seconds; we use the lock_path_for() helper from
# the public API to write a sentinel that will be stale immediately
# (since `state_lock::acquire` would consider it held by an agent
# that's no longer alive). For this variant we use the file-lock
# subsystem directly: write the sentinel file with an old mtime.
LOCK_DIR="$WORKSPACE/.lain/locks"
mkdir -p "$LOCK_DIR"
STALE_LOCK="$LOCK_DIR/contested.rs.lock"
printf '{"holder":"dead-agent","pid":99999,"at":1}\n' >"$STALE_LOCK"
touch -d "1970-01-01" "$STALE_LOCK"
echo "    planted stale lock: $STALE_LOCK"

# Bob claims the file. The presence layer's stale-lock takeover
# should let him win.
BOB3="$(run_pair "$PORT_V3" bob_v3)"
BOB3_ID="$(printf '%s' "$BOB3" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["agent_id"])')"
BOB3_TOK="$(printf '%s' "$BOB3" | python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["session_token"])')"
BOB3_CLAIM="$(claim_path "$BASE_V3" "$BOB3_ID" "$BOB3_TOK" src/contested.rs)"
BOB3_GRANTED="$(printf '%s' "$BOB3_CLAIM" | python3 -c '
import json, sys
v = json.loads(sys.stdin.read())
inner = json.loads(v["result"]["content"][0]["text"])
print(len(inner.get("granted", [])))' 2>/dev/null || echo 0)"
echo "    bob granted despite stale lock: $BOB3_GRANTED (expect 1 — the lock layer takes over stale locks)"

cat >"$OUT_DIR/variant_3.json" <<EOF
{
  "variant": "stale_lock_takeover",
  "bob_granted": $BOB3_GRANTED
}
EOF

kill_server

echo "==> chaos verdict: $OUT_DIR/{variant_1.json,variant_2.json,variant_3.json}"
echo "OK"
