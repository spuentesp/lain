#!/usr/bin/env bash
# Real-case latency + coordination benchmark.
#
# Unlike tests/coordination_benchmark.rs (in-process handler calls, no
# network), this drives a REAL `lain server` over HTTP, indexing a REAL
# repo (by default the lain repo itself), with N concurrent scripted
# agents doing realistic loops: search_org → get_blast_radius →
# claim_files → heartbeat → release_files, with random think time.
# Non-deterministic by design — it measures the system as agents
# actually experience it.
#
# Usage:
#   tests/e2e/real-bench.sh                 # 6 agents x 20 cycles
#   AGENTS=10 CYCLES=40 tests/e2e/real-bench.sh
#   SUBJECT=/path/to/other/repo tests/e2e/real-bench.sh
#
# Output: per-op p50/p90/p99 table on stdout and in $WORK/report.md.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LAIN="${LAIN:-$REPO_ROOT/target/release/lain}"
SUBJECT="${SUBJECT:-$REPO_ROOT}"
WORK="${WORK:-/tmp/lain-real-bench}"
PORT="${PORT:-9998}"
URL="http://127.0.0.1:$PORT"
AGENTS="${AGENTS:-6}"
CYCLES="${CYCLES:-20}"

# Real symbols + files from the subject repo (defaults match lain itself).
SYMS=(run_claim_files serve_sse get_blast_radius PresenceEvent LainServer
      OccupancyMap replay_after emit_presence_event run_register_agent
      EventsLog)
FILES=(src/server/presence.rs src/server/sse.rs src/server/ingest/mod.rs
       src/server/events_log.rs src/server/mcp/handler.rs)

if [ ! -x "$LAIN" ]; then
    echo "release binary missing, building (cargo build --release)..."
    (cd "$REPO_ROOT" && cargo build --release) || exit 1
fi

mkdir -p "$WORK/raw"
cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/data
repos:
  - id: subject
    source: { type: workspace_dir, path: $SUBJECT }
EOF

# Kill any previous server on this port/workdir, start fresh.
pkill -f "lain server.*--port $PORT" 2>/dev/null || true
sleep 1
(cd "$WORK" && "$LAIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" > "$WORK/server.log" 2>&1 &)
echo "server starting (first run indexes $(du -sh --exclude=target --exclude=.git "$SUBJECT" 2>/dev/null | cut -f1) of source)..."
for i in $(seq 1 300); do
    if curl -s -m 1 "$URL/health" >/dev/null 2>&1; then break; fi
    sleep 1
done
curl -s -m 2 "$URL/health" | python3 -c 'import sys,json; d=json.load(sys.stdin); print("server ready:", d.get("status"), "| repos:", [r["id"]+":"+r["health"] for r in d.get("federation",{}).get("repos",[])])' || { echo "FAIL: server did not start"; tail -5 "$WORK/server.log"; exit 1; }

call_tool() {  # name args outfile → prints wall seconds
    local name="$1" args="$2" out="$3"
    curl -s -m 15 -o "$out" -w '%{time_total}' -X POST "$URL/mcp" \
        -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":$RANDOM,\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args}}"
}

# Extract the inner JSON payload from the MCP envelope.
inner() { python3 -c '
import sys, json
try:
    d = json.load(open(sys.argv[1]))
    print(d["result"]["content"][0]["text"])
except Exception:
    print("{}")
' "$1"; }

worker() {
    local i=$1
    local log="$WORK/raw/agent-$i.csv"
    : > "$log"
    local resp="$WORK/raw/agent-$i-resp.json"
    call_tool register_agent "{\"name\":\"bench-$i\",\"kind\":\"kimi\"}" "$resp" >/dev/null
    local creds
    creds=$(inner "$resp" | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d.get("agent_id",""), d.get("session_token",""))')
    local agent_id token
    agent_id=$(echo "$creds" | cut -d" " -f1)
    token=$(echo "$creds" | cut -d" " -f2)
    if [ -z "$agent_id" ] || [ -z "$token" ]; then
        echo "register_agent,0,ERROR" >> "$log"
        return
    fi
    for cycle in $(seq 1 "$CYCLES"); do
        local sym=${SYMS[$((RANDOM % ${#SYMS[@]}))]}
        local file=${FILES[$((RANDOM % ${#FILES[@]}))]}
        # Derive the symbol from the chosen file half the time so
        # claim_files does real symbol-hash work on a matching pair.
        if [ $((RANDOM % 2)) -eq 0 ]; then sym=${SYMS[$((cycle % ${#SYMS[@]}))]}; fi

        local t
        # Claim FIRST and hold it across the whole cycle (queries +
        # think time) before releasing — this is what a real agent does
        # (claim → work → release), and it's what makes advisory
        # conflicts actually happen between concurrent agents.
        t=$(call_tool claim_files "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\",\"files\":[{\"path\":\"$file\",\"symbols\":[\"$sym\"]}]}" "$resp")
        local nconf
        nconf=$(inner "$resp" | python3 -c 'import sys,json; print(len(json.load(sys.stdin).get("conflicts",[])))' 2>/dev/null || echo 0)
        echo "claim_files,$t,$nconf" >> "$log"

        t=$(call_tool search_org "{\"query\":\"$sym\",\"limit\":5}" "$resp")
        echo "search_org,$t,$(grep -c '"isError":true' "$resp" || true)" >> "$log"

        t=$(call_tool get_blast_radius "{\"symbol\":\"$sym\"}" "$resp")
        echo "get_blast_radius,$t,$(grep -c '"isError":true' "$resp" || true)" >> "$log"

        t=$(call_tool heartbeat "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\"}" "$resp")
        echo "heartbeat,$t,0" >> "$log"

        # Think time happens WHILE HOLDING the claim — the contention
        # window is the whole cycle, not the ~1ms of the claim call.
        sleep "0.$((RANDOM % 4))"

        t=$(call_tool release_files "{\"agent_id\":\"$agent_id\",\"session_token\":\"$token\",\"files\":[{\"path\":\"$file\"}]}" "$resp")
        echo "release_files,$t,0" >> "$log"
    done
}

echo "spawning $AGENTS agents x $CYCLES cycles against $URL ..."
START=$(date +%s)
for i in $(seq 1 "$AGENTS"); do worker "$i" & done
wait
END=$(date +%s)
echo "wall time: $((END - START))s"

python3 - "$WORK" "$AGENTS" "$CYCLES" "$(cd "$SUBJECT" && git rev-parse --short HEAD)" > "$WORK/report.md" <<'PYEOF'
import csv, glob, sys, datetime

work, agents, cycles, sha = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
ops = {}
conflicts = 0
errors = 0
for f in glob.glob(f"{work}/raw/agent-*.csv"):
    for row in csv.reader(open(f)):
        if len(row) < 3:
            continue
        op, secs, third = row[0], float(row[1]), row[2]
        if third == "ERROR" or (third not in ("0",) and op in ("search_org", "get_blast_radius")):
            errors += 1
            continue
        ops.setdefault(op, []).append(secs)
        if op == "claim_files":
            conflicts += int(third)

def pct(s, p):
    if not s: return 0.0
    s = sorted(s)
    return s[min(len(s) - 1, round((len(s) - 1) * p))]

print(f"# Real-case benchmark — {datetime.date.today()}")
print(f"subject repo: lain @ {sha} · {agents} agents x {cycles} cycles · HTTP transport\n")
print("| op | n | errors | p50 | p90 | p99 | max |")
print("|---|---|---|---|---|---|---|")
worst = 0.0
for op in sorted(ops):
    s = ops[op]
    worst = max(worst, pct(s, 0.99))
    print(f"| {op} | {len(s)} | {errors if op == sorted(ops)[0] else 0} | {pct(s,.5)*1000:.1f} ms | {pct(s,.9)*1000:.1f} ms | {pct(s,.99)*1000:.1f} ms | {max(s)*1000:.1f} ms |")
print(f"\ntotal advisory conflicts detected: {conflicts}")
print(f"total errored calls (excluded): {errors}")
print(f"\nverdict: {'PASS' if worst < 2.0 and conflicts > 0 else 'CHECK'} (p99 budget 2000 ms, conflicts must be > 0)")
PYEOF

cat "$WORK/report.md"
echo
echo "full report: $WORK/report.md"

# Machine-readable verdict for CI: PASS exits 0, CHECK exits 1 so the
# nightly actually goes red when the budget is blown or no conflicts
# were exercised (which would mean the scenario didn't test anything).
if grep -q 'verdict: PASS' "$WORK/report.md"; then
    exit 0
else
    echo "verdict not PASS — see report above" >&2
    exit 1
fi
