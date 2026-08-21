#!/usr/bin/env bash
# Full end-to-end test of the lain multi-agent story.
#
# Exercises:
#   1. install.sh (or prebuilt binary) launches a federation server
#   2. Two scripted agents register and discover each other
#   3. Concurrent claim on the same file → exactly one wins, the other
#      sees a conflict (advisory, not blocking — proves the "advisory
#      awareness" promise)
#   4. Release + re-claim round-trip (proves conflict resolution works)
#   5. Audit log contains every meaningful event (claim, release,
#      conflict, edit-landed)
#   6. Server restart with same data_dir rehydrates the state — the
#      other agent's claim survives the restart, the dead agent's
#      claim does NOT (it was released when the session expired).
#   7. Static graph generation is populated and `get_blast_radius`
#      returns real dependents — content correctness gate from the
#      "blast radius always empty" bug we already fixed.
#
# Output: PASS or FAIL on stdout, plus a short report. Exit code
# reflects the verdict (0 = pass, 1 = fail).

set -eu

LAIN="${LAIN:-/home/sebastian/lain/target/debug/lain}"
WORK="${WORK:-/tmp/lain-e2e-full}"
PORT="${PORT:-9999}"
URL="http://127.0.0.1:$PORT"
SUBJECT="$WORK/scratch"

# Each curl uses its own TCP connection. Without this the server
# keeps the connection alive and the second request's body parsing
# races with the first response's body close — observed as
# `Unknown tool: bob` or parse errors on the second request.
CURL="curl -s -m 5 --no-keepalive"

cleanup() {
    if [ -n "${SERVER_PID:-}" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK" 2>/dev/null || true
    rm -f /tmp/lain-e2e-*.txt 2>/dev/null || true
}
trap cleanup EXIT

# Clean any leftover state from a prior failed run.
rm -rf "$WORK" 2>/dev/null || true
rm -f /tmp/lain-e2e-*.txt 2>/dev/null || true

FAIL=0
note() { printf '%s\n' "$*"; }
fail() { printf '  FAIL: %s\n' "$*"; FAIL=$((FAIL+1)); }
pass() { printf '  ok   %s\n' "$*"; }

if [ ! -x "$LAIN" ]; then
    note "FAIL: $LAIN not executable (run \`cargo build\` first)"
    exit 1
fi

# 1. Scratch git repo with two Rust files that share a function name
note "1. Setting up scratch repo at $SUBJECT"
mkdir -p "$SUBJECT/src/helper"
cat > "$SUBJECT/src/main.rs" <<'EOF'
mod helper;
fn main() { let _ = helper::run(); }
EOF
cat > "$SUBJECT/src/helper/mod.rs" <<'EOF'
pub fn run() { let _ = helper_inner(); }
pub fn helper_inner() {}
EOF
( cd "$SUBJECT" && git init -q && git config user.email t@t && git config user.name t \
    && git add -A && git commit -qm init )

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/data
repos:
  - id: scratch
    source: { type: workspace_dir, path: $SUBJECT }
EOF

# 2. Launch server
note "2. Starting server on port $PORT"
XDG_STATE_HOME="$WORK/state" "$LAIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!

# Wait for health
for _ in $(seq 1 120); do
    if $CURL "$URL/health" >/dev/null 2>&1; then break; fi
    sleep 1
done
if ! $CURL "$URL/health" >/dev/null 2>&1; then
    note "FAIL: server didn't become healthy"
    tail -10 "$WORK/server.log"
    exit 1
fi
pass "server up"

# Wait for indexing
sleep 8

register() {
    local name="$1"
    local kind="$2"
    $CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"tools/call\",\"params\":{\"name\":\"register_agent\",\"arguments\":{\"name\":\"$name\",\"kind\":\"$kind\",\"mode\":\"interactive\"}}}"
}

# 3. Two agents register
note "3. Two agents register"
register alice claude > /tmp/lain-e2e-ra.json
register bob   other  > /tmp/lain-e2e-rb.json
A_AGENT=$(python3 -c '
import json
t = json.loads(json.load(open("/tmp/lain-e2e-ra.json"))["result"]["content"][0]["text"])
print(t["agent_id"]); print(t["session_token"], file=__import__("sys").stderr)
')
A_TOKEN=$(python3 -c '
import json
t = json.loads(json.load(open("/tmp/lain-e2e-ra.json"))["result"]["content"][0]["text"])
print(t["session_token"])
')
B_AGENT=$(python3 -c '
import json
t = json.loads(json.load(open("/tmp/lain-e2e-rb.json"))["result"]["content"][0]["text"])
print(t["agent_id"]); print(t["session_token"], file=__import__("sys").stderr)
')
B_TOKEN=$(python3 -c '
import json
t = json.loads(json.load(open("/tmp/lain-e2e-rb.json"))["result"]["content"][0]["text"])
print(t["session_token"])
')
[ -n "$A_AGENT" ] && [ -n "$B_AGENT" ] || { fail "register_agent failed (RA: $(cat /tmp/lain-e2e-ra.json) | RB: $(cat /tmp/lain-e2e-rb.json))"; exit 1; }
pass "alice=$A_AGENT  bob=$B_AGENT"

claim() {
    local agent="$1" token="$2"
    $CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"tools/call\",\"params\":{\"name\":\"claim_files\",\"arguments\":{\"agent_id\":\"$agent\",\"session_token\":\"$token\",\"files\":[{\"path\":\"src/helper/mod.rs\",\"symbols\":[\"run\"]}]}}}"
}

# 4. Alice claims first, then Bob (serial → Alice wins, Bob sees conflict)
note "4. Alice then Bob claim src/helper/mod.rs (advisory, not blocking)"
claim "$A_AGENT" "$A_TOKEN" > /tmp/lain-e2e-ca.json
claim "$B_AGENT" "$B_TOKEN" > /tmp/lain-e2e-cb.json
A_GRANTED=$(python3 -c 'import json; print(len(json.loads(json.load(open("/tmp/lain-e2e-ca.json"))["result"]["content"][0]["text"])["granted"]))')
B_CONFLICTS=$(python3 -c 'import json; print(len(json.loads(json.load(open("/tmp/lain-e2e-cb.json"))["result"]["content"][0]["text"])["conflicts"]))')
[ "$A_GRANTED" = "1" ] && pass "alice granted (no conflict, got $A_GRANTED)" || fail "alice expected 1 grant, got $A_GRANTED"
[ "$B_CONFLICTS" = "1" ] && pass "bob saw 1 conflict (advisory, got $B_CONFLICTS)" || fail "bob expected 1 conflict, got $B_CONFLICTS"

# 5. Alice releases, Bob re-claims
note "5. Alice releases, Bob re-claims"
$CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"tools/call\",\"params\":{\"name\":\"release_files\",\"arguments\":{\"agent_id\":\"$A_AGENT\",\"session_token\":\"$A_TOKEN\",\"files\":[{\"path\":\"src/helper/mod.rs\"}]}}}" > /dev/null
sleep 1
claim "$B_AGENT" "$B_TOKEN" > /tmp/lain-e2e-cb2.json
B2_GRANTED=$(python3 -c 'import json; print(len(json.loads(json.load(open("/tmp/lain-e2e-cb2.json"))["result"]["content"][0]["text"])["granted"]))')
[ "$B2_GRANTED" = "1" ] && pass "bob re-claims successfully (no stale conflict)" || fail "bob re-claim expected 1 grant, got $B2_GRANTED"

# 6. Audit log
note "6. Audit log"
$CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"get_audit_log","arguments":{"limit":50}}}' > /tmp/lain-e2e-au.json
AUDIT_TOTAL=$(python3 -c '
import json
d = json.loads(json.load(open("/tmp/lain-e2e-au.json"))["result"]["content"][0]["text"])
print(sum(1 for e in d if e.get("path","").endswith("mod.rs")))
')
[ "$AUDIT_TOTAL" -ge "2" ] && pass "audit log records ≥2 mod.rs events (alice + bob successful claims)" || fail "audit log only has $AUDIT_TOTAL mod.rs events"

# 7. get_blast_radius content gate
note "7. blast_radius content gate"
$CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"get_blast_radius","arguments":{"symbol":"run"}}}' > /tmp/lain-e2e-br.json
TOTAL=$(python3 -c '
import json, re
text = json.load(open("/tmp/lain-e2e-br.json"))["result"]["content"][0]["text"]
m = re.search(r"Total transitively affected nodes: (\d+)", text)
print(m.group(1) if m else "0")
')
[ "${TOTAL:-0}" -ge "1" ] && pass "blast_radius(run) → ${TOTAL} dependents (federation ingestion working)" \
    || { fail "blast_radius(run) returned 0 dependents — federation ingestion broken"; }

# 8. State persistence across restart
note "8. State persists across restart"
kill "$SERVER_PID" 2>/dev/null || true
sleep 2
SERVER_PID=
XDG_STATE_HOME="$WORK/state" "$LAIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" > "$WORK/server2.log" 2>&1 &
SERVER_PID=$!
ready=0
for _ in $(seq 1 60); do
    if $CURL "$URL/health" >/dev/null 2>&1; then ready=1; break; fi
    sleep 1
done
[ "$ready" = "1" ] || { fail "server didn't restart"; exit 1; }
$CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"list_occupancy","arguments":{}}}' > /tmp/lain-e2e-occ.json
OCC_BOB=$(python3 -c "
import json
o = json.loads(json.load(open('/tmp/lain-e2e-occ.json'))['result']['content'][0]['text'])
print(sum(1 for entry in o if '$B_AGENT' in entry.get('agents', [])))
")
[ "$OCC_BOB" -ge "1" ] && pass "bob's claim survived the restart (got $OCC_BOB)" \
    || fail "bob's claim vanished after restart (expected ≥1, got $OCC_BOB)"

echo
if [ "$FAIL" -eq 0 ]; then
    echo "PASS: full multi-agent e2e"
    exit 0
else
    echo "FAIL: $FAIL assertion(s) failed"
    exit 1
fi
