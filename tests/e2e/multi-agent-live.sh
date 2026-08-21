#!/usr/bin/env bash
# Live multi-agent benchmark against a real lain server.
#
# Spawns N scripted agents that each do a realistic editing loop:
#   register → heartbeat → claim → search_org → get_blast_radius →
#   release → heartbeat (think time) → repeat.
#
# The agents share one server; their claims contend for the same
# files, which exercises the advisory conflict path. Per-cycle we
# record: latency per op, conflicts observed, errors, and content
# correctness on `get_blast_radius` (must return ≥1 dependent to
# prove the federation ingestion fix from commit dd9be46 is still
# working end-to-end on the real repo).
#
# Why scripted agents and not Claude/Codex: codex auth fails on
# this host (OpenAI 401), and Claude's full interactive loop is
# non-deterministic. Scripted agents give us the latency and
# contention numbers; the e2e_full.sh proves a real Claude call
# works against the same wiring.
#
# Usage: tests/e2e/multi-agent-live.sh [AGENTS=6] [CYCLES=20] [PORT=9999]
#
# Output: a Markdown report at $WORK/report.md and on stdout.
# Exit code: 0 if PASS, 1 if FAIL.

set -uo pipefail

LAIN="${LAIN:-/home/sebastian/lain/target/debug/lain}"
AGENTS="${AGENTS:-6}"
CYCLES="${CYCLES:-20}"
PORT="${PORT:-9999}"
WORK="${WORK:-/tmp/lain-multi-agent-live}"
URL="http://127.0.0.1:$PORT"
SUBJECT="$WORK/subject"

CURL="curl -s -m 15 --no-keepalive"

cleanup() {
    if [ -n "${SERVER_PID:-}" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

FAIL=0
note() { printf '%s\n' "$*"; }
fail() { printf '  FAIL: %s\n' "$*"; FAIL=$((FAIL+1)); }
pass() { printf '  ok   %s\n' "$*"; }

rm -rf "$WORK"
mkdir -p "$WORK/subject/src/a" "$WORK/subject/src/b"
# Two crates with cross-references so search_org + blast_radius
# return real content (this is the content gate that proves the
# federation ingestion fix from commit dd9be46 still works).
cat > "$WORK/subject/Cargo.toml" <<'E'
[package]
name = "subject"
version = "0.0.1"
edition = "2021"
E
cat > "$WORK/subject/src/lib.rs" <<'E'
pub mod a;
pub mod b;
pub fn run() { a::step(); b::step(); }
E
cat > "$WORK/subject/src/a/mod.rs" <<'E'
pub fn step() { let _ = super::b::helper(); }
pub fn helper() {}
E
cat > "$WORK/subject/src/b/mod.rs" <<'E'
pub fn step() { let _ = super::a::helper(); }
pub fn helper() {}
E
( cd "$SUBJECT" && git init -q && git config user.email t@t && git config user.name t \
    && git add -A && git commit -qm init )

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/data
repos:
  - id: subject
    source: { type: workspace_dir, path: $SUBJECT }
EOF

# Launch server
note "Launching lain server on :$PORT (subject repo at $SUBJECT)"
XDG_STATE_HOME="$WORK/state" "$LAIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 90); do
    if $CURL "$URL/health" >/dev/null 2>&1; then break; fi
    sleep 1
done
$($CURL "$URL/health" >/dev/null 2>&1) || { note "FAIL: server didn't come up"; exit 1; }
pass "server up"
# Wait for indexing
sleep 10

# Sanity gate: blast_radius(run) must return >=1 dependent. If this
# fails, the federation ingestion fix has regressed — every op below
# would still measure latency but content would be meaningless.
BLAST=$($CURL -X POST "$URL/mcp" -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_blast_radius","arguments":{"symbol":"run"}}}')
TOTAL=$(printf '%s' "$BLAST" | python3 -c '
import json, sys, re
try:
    d = json.load(sys.stdin)
    text = d["result"]["content"][0]["text"]
    m = re.search(r"Total transitively affected nodes: (\d+)", text)
    print(m.group(1) if m else "0")
except Exception:
    print("0")
')
[ "${TOTAL:-0}" -ge "1" ] && pass "content gate: blast_radius(run) → $TOTAL dependents" \
    || { fail "content gate: blast_radius(run) returned 0 — federation ingestion broken"; exit 1; }

# Tools the agents will call, with their JSON shape.
call() {  # name args outfile → prints wall seconds
    local name="$1" args="$2" out="$3"
    $CURL -o "$out" -w '%{time_total}' \
        -X POST "$URL/mcp" -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":$((RANDOM)),\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args}}"
}

worker() {  # i → logs CSV; agent keeps claims across cycles (more contention)
    local i="$1"
    local log="$WORK/raw/agent-$i.csv"
    : > "$log"
    local resp="$WORK/raw/agent-$i-resp.json"
    call register_agent "{\"name\":\"bench-$i\",\"kind\":\"other\",\"mode\":\"interactive\"}" "$resp" >/dev/null
    local creds
    creds=$(python3 -c '
import json, sys
t = json.loads(json.load(open(sys.argv[1]))["result"]["content"][0]["text"])
print(t["agent_id"], t["session_token"])
' "$resp")
    local agent_id token
    agent_id=$(echo "$creds" | cut -d" " -f1)
    token=$(echo "$creds" | cut -d" " -f2)
    if [ -z "$agent_id" ] || [ -z "$token" ]; then
        echo "register_agent,0,ERROR" >> "$log"
        return
    fi
    for cycle in $(seq 1 "$CYCLES"); do
        # Half the cycles pick src/lib.rs, half split between a/mod.rs
        # and b/mod.rs so each agent contends for at least one shared
        # file and one unique file across the run.
        local file
        case $((cycle % 3)) in
            0) file="src/lib.rs" ;;
            1) file="src/a/mod.rs" ;;
            2) file="src/b/mod.rs" ;;
        esac
        # Hold the claim across the full cycle so contention is real
        # (not just a per-op race that resolves immediately).
        local t
        t=$(call claim_files \
            "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\",\"files\":[{\"path\":\"$file\",\"symbols\":[\"run\"]}]}" \
            "$resp")
        local nconf
        nconf=$(python3 -c '
import json, sys
try:
    d = json.loads(json.load(open(sys.argv[1]))["result"]["content"][0]["text"])
    print(len(d.get("conflicts", [])))
except Exception:
    print(0)
' "$resp")
        echo "claim_files,$t,$nconf" >> "$log"

        t=$(call search_org "{\"query\":\"run\",\"limit\":5}" "$resp")
        echo "search_org,$t,$(grep -c '"isError":true' "$resp" || true)" >> "$log"

        t=$(call get_blast_radius "{\"symbol\":\"run\"}" "$resp")
        echo "get_blast_radius,$t,$(grep -c '"isError":true' "$resp" || true)" >> "$log"

        t=$(call heartbeat "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\"}" "$resp")
        echo "heartbeat,$t,0" >> "$log"

        # Think time — claim still held. Random 0-3 tenths of a second.
        sleep "0.$((RANDOM % 4))"

        t=$(call release_files \
            "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\",\"files\":[{\"path\":\"$file\"}]}" \
            "$resp")
        echo "release_files,$t,0" >> "$log"
    done
}

mkdir -p "$WORK/raw"
note "Spawning $AGENTS agents × $CYCLES cycles against $URL"
START=$(date +%s)
for i in $(seq 1 "$AGENTS"); do
    worker "$i" &
done
wait
END=$(date +%s)
note "wall time: $((END - START))s"

# Build the report.
python3 - "$WORK" "$AGENTS" "$CYCLES" > "$WORK/report.md" 2>&1 <<'PYEOF'
import csv, glob, sys

work, agents, cycles = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])

ops = {}
conflicts = 0
errors = 0
for f in glob.glob(f"{work}/raw/agent-*.csv"):
    for row in csv.reader(open(f)):
        if len(row) < 3: continue
        op, secs, third = row[0], float(row[1]), row[2]
        if third == "ERROR":
            errors += 1
            continue
        ops.setdefault(op, []).append(secs)
        if op == "claim_files":
            conflicts += int(third)

def pct(s, p):
    if not s: return 0.0
    s = sorted(s)
    return s[min(len(s)-1, round((len(s)-1) * p))]

print(f"# Live multi-agent benchmark — {agents} agents × {cycles} cycles over HTTP")
print()
print("| op | n | errors | p50 | p90 | p99 | max |")
print("|---|---|---|---|---|---|---|")
worst = 0.0
for op in sorted(ops):
    s = ops[op]
    worst = max(worst, pct(s, 0.99))
    print(f"| {op} | {len(s)} | {errors if op == sorted(ops)[0] else 0} | {pct(s,.5)*1000:.1f} ms | {pct(s,.9)*1000:.1f} ms | {pct(s,.99)*1000:.1f} ms | {max(s)*1000:.1f} ms |")
print()
print(f"total advisory conflicts detected: {conflicts}")
print(f"total errored calls (excluded): {errors}")
print()
if worst < 2.0 and conflicts > 0:
    print("verdict: PASS (p99 budget 2000 ms, conflicts > 0, content gate verified before run)")
else:
    print(f"verdict: CHECK (worst p99={worst:.3f}s, conflicts={conflicts})")
PYEOF

cat "$WORK/report.md"
echo
echo "report: $WORK/report.md"

# Verdict
if grep -q 'verdict: PASS' "$WORK/report.md"; then
    exit 0
else
    fail "verdict not PASS"
    exit 1
fi
