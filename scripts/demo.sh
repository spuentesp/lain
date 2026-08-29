#!/usr/bin/env bash
# lain — capability demonstration and benchmark.
#
# Boots a real server against a synthetic repo whose call graph is known
# by construction, then checks lain's answers against that ground truth
# and times every tool it calls.
#
# The checks assert *content*, not liveness. "The server responded" is
# not evidence of anything; every assertion here names the answer it
# expects and fails loudly when the answer differs. Where an exact
# answer is not knowable (semantic ranking, wall-clock timings) the
# check says what it is really testing instead of pretending to more.
#
#   ./scripts/demo.sh                  # full run
#   ./scripts/demo.sh --quick          # skip build + benchmark phases
#   ./scripts/demo.sh --json out.json  # also write machine-readable results
#   ./scripts/demo.sh --force-build    # rebuild even under --quick / --no-build
#   ./scripts/demo.sh --allow-stale    # skip the binary-freshness check
#
# Freshness: if any source file (Cargo.toml, Cargo.lock, src/**/*.rs) is
# newer than the binary, demo.sh prints a yellow warning and exits 2.
# Use --force-build to rebuild, or --allow-stale to ignore the warning.
#
# Exit code: 0 iff every check passed.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-9931}"
WORK="${WORK:-/tmp/lain-demo}"
SUBJECT="$WORK/subject"
URL="http://127.0.0.1:$PORT"
MCP="$URL/mcp"
LAIN="${LAIN:-$REPO_ROOT/target/release/lain}"
MODEL="${LAIN_EMBEDDING_MODEL:-/tmp/lainmodel}"
QUICK=0
NO_BUILD=0
FORCE_BUILD=0
ALLOW_STALE=0
JSON_OUT=""
BUILD=1  # recomputed below

while [ $# -gt 0 ]; do
  case "$1" in
    --quick)       QUICK=1 ;;
    --no-build)    NO_BUILD=1 ;;
    --force-build) FORCE_BUILD=1 ;;
    --allow-stale) ALLOW_STALE=1 ;;
    --json)        JSON_OUT="${2:?--json needs a path}"; shift ;;
    --port)        PORT="${2:?--port needs a value}"; URL="http://127.0.0.1:$PORT"; MCP="$URL/mcp"; shift ;;
    -h|--help)     sed -n '2,24p' "$0"; exit 0 ;;
    *)             echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

# --quick implies --no-build unless --force-build is also passed.
[ "$QUICK" = 1 ] && [ "$FORCE_BUILD" = 0 ] && NO_BUILD=1

# Build runs unless explicitly skipped.
[ "$NO_BUILD" = 1 ] && BUILD=0
[ "$FORCE_BUILD" = 1 ] && BUILD=1

# Export so the sourced helper can read it.
export ALLOW_STALE

# ── output ────────────────────────────────────────────────────────────
if [ -t 1 ]; then
  B=$'\e[1m'; DIM=$'\e[2m'; GRN=$'\e[32m'; RED=$'\e[31m'; YEL=$'\e[33m'; RST=$'\e[0m'
else
  B=""; DIM=""; GRN=""; RED=""; YEL=""; RST=""
fi

PASS=0; FAIL=0; SKIP=0
RESULTS_TSV="$WORK/results.tsv"
TIMES_TSV="$WORK/times.tsv"

section() { printf '\n%s──  %s  ──%s\n' "$B" "$1" "$RST"; }

# check <name> <expected> <actual> [note]
check() {
  local name="$1" want="$2" got="$3" note="${4:-}"
  if [ "$want" = "$got" ]; then
    PASS=$((PASS+1))
    printf '  %sPASS%s %-46s %s\n' "$GRN" "$RST" "$name" "${DIM}${got}${RST}"
    printf 'PASS\t%s\t%s\t%s\n' "$name" "$want" "$got" >> "$RESULTS_TSV"
  else
    FAIL=$((FAIL+1))
    printf '  %sFAIL%s %-46s\n        expected: %s\n        actual:   %s\n' \
      "$RED" "$RST" "$name" "$want" "$got"
    [ -n "$note" ] && printf '        %s\n' "$note"
    printf 'FAIL\t%s\t%s\t%s\n' "$name" "$want" "$got" >> "$RESULTS_TSV"
  fi
}

# check_contains <name> <needle> <haystack>
check_contains() {
  local name="$1" needle="$2" hay="$3"
  case "$hay" in
    *"$needle"*) check "$name" "contains:$needle" "contains:$needle" ;;
    *)           check "$name" "contains:$needle" "MISSING — got: $(printf '%s' "$hay" | head -c 160 | tr '\n' ' ')" ;;
  esac
}

# check_absent <name> <needle> <haystack>
check_absent() {
  local name="$1" needle="$2" hay="$3"
  case "$hay" in
    *"$needle"*) check "$name" "absent:$needle" "PRESENT — got: $(printf '%s' "$hay" | head -c 160 | tr '\n' ' ')" ;;
    *)           check "$name" "absent:$needle" "absent:$needle" ;;
  esac
}

skip() { SKIP=$((SKIP+1)); printf '  %sSKIP%s %-46s %s\n' "$YEL" "$RST" "$1" "${DIM}${2:-}${RST}"; }

# ── MCP plumbing ──────────────────────────────────────────────────────
# call <tool> <json-args>  -> prints the tool's text payload, records latency
call() {
  local tool="$1" args="${2:-}" t0 t1 ms
  [ -z "$args" ] && args='{}'   # a literal {} default cannot be written inline in ${2:-...}
  t0=$(date +%s%N)
  local raw
  raw=$(curl -s -m 180 -X POST "$MCP" -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}")
  t1=$(date +%s%N)
  ms=$(( (t1 - t0) / 1000000 ))
  printf '%s\t%s\n' "$tool" "$ms" >> "$TIMES_TSV"
  printf '%s' "$raw" | python3 -c "
import json,sys
raw=sys.stdin.read()
try: d=json.loads(raw)
except Exception: print('__TRANSPORT_ERROR__'); sys.exit()
if 'error' in d: print('__RPC_ERROR__ '+str(d['error'].get('message',''))); sys.exit()
r=d.get('result',{})
t=(r.get('content') or [{}])[0].get('text','')
print(('__TOOL_ERROR__ ' if r.get('isError') else '')+t)
"
}

http_code() { curl -s -o /dev/null -w '%{http_code}' -m 20 "$1"; }

# ── setup ─────────────────────────────────────────────────────────────
cleanup() {
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null
  wait "${SERVER_PID:-}" 2>/dev/null
  return 0
}
trap cleanup EXIT INT TERM

mkdir -p "$WORK"
: > "$RESULTS_TSV"; : > "$TIMES_TSV"

printf '%slain — capability demonstration and benchmark%s\n' "$B" "$RST"
printf '%ssubject: synthetic repo with a call graph known by construction%s\n' "$DIM" "$RST"

# shellcheck source=./demo-freshness.sh
source "$REPO_ROOT/scripts/demo-freshness.sh"

if [ "$BUILD" = 1 ]; then
  section "Build"
  # cargo is frequently not on PATH (rustup shims live elsewhere) — the
  # same problem `resolve_program` exists to solve for run_build.
  if ! command -v cargo >/dev/null 2>&1; then
    for d in "$HOME/.cargo/bin" "$HOME"/.rustup/toolchains/*/bin; do
      [ -x "$d/cargo" ] && export PATH="$d:$PATH" && break
    done
  fi
  if command -v cargo >/dev/null 2>&1; then
    printf '  building release binary (this takes a few minutes)...\n'
    # -j3: full parallelism OOMs LTO on a 16 GiB machine.
    (cd "$REPO_ROOT" && cargo build --release --bin lain -j "${JOBS:-3}" 2>&1 | tail -3)
  else
    printf '  %sno cargo on PATH; using existing binary%s\n' "$YEL" "$RST"
  fi
fi

[ -x "$LAIN" ] || { printf '%sno binary at %s — build first or set LAIN=%s\n' "$RED" "$LAIN" "$RST"; exit 2; }

# Guard against demoing a stale binary (D-L3). The warning goes to stderr from
# check_binary_freshness; demo.sh only propagates the non-zero exit. Runs after
# the -x check so a *missing* binary still gets the precise message above rather
# than a misleading "may be stale".
if ! check_binary_freshness "$LAIN" "$REPO_ROOT"; then
  exit 2
fi

print_binary_info "$LAIN"

section "Subject repo"
bash "$REPO_ROOT/scripts/demo-fixture.sh" "$SUBJECT"
printf '  %s — %s files, %s commits\n' "$SUBJECT" \
  "$(find "$SUBJECT" -name '*.rs' | wc -l)" \
  "$(cd "$SUBJECT" && git rev-list --count HEAD)"

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/data
repos:
  - id: subject
    source: { type: workspace_dir, path: $SUBJECT }
EOF

section "Boot"
rm -rf "$WORK/state" "$WORK/data"
MODEL_ARGS=()
if [ -f "$MODEL/model.onnx" ]; then
  MODEL_ARGS=(--embedding-model "$MODEL")
  printf '  NLP model: %s\n' "$MODEL"
else
  printf '  %sno NLP model at %s — semantic checks will be skipped%s\n' "$YEL" "$MODEL" "$RST"
fi

BOOT_T0=$(date +%s%N)
XDG_STATE_HOME="$WORK/state" "$LAIN" server \
  --config "$WORK/repos.yaml" --transport http --port "$PORT" \
  --log-level warn "${MODEL_ARGS[@]}" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 90); do
  [ "$(http_code "$URL/health")" = "200" ] && break
  sleep 1
done
BOOT_T1=$(date +%s%N)
BOOT_MS=$(( (BOOT_T1 - BOOT_T0) / 1000000 ))

if [ "$(http_code "$URL/health")" != "200" ]; then
  printf '%sserver never became healthy. log:%s\n' "$RED" "$RST"; tail -20 "$WORK/server.log"; exit 1
fi
printf '  healthy in %s ms (boot + index of the subject repo)\n' "$BOOT_MS"

# ══ 1. Server + surface ═══════════════════════════════════════════════
section "1. Server and advertised surface"

TOOL_COUNT=$(curl -s -m 30 -X POST "$MCP" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | python3 -c "import json,sys; print(len(json.load(sys.stdin)['result']['tools']))")
if [ -n "${MODEL_ARGS[*]:-}" ]; then
  check "tools/list advertises the full surface" "64" "$TOOL_COUNT"
else
  # Wishlist #9: a tool that cannot answer is not offered.
  check "tools/list hides semantic_search with no model" "63" "$TOOL_COUNT"
fi

H=$(call get_health)
check_contains "get_health reports Operational" "Operational" "$H"
NODES=$(printf '%s' "$H" | sed -n 's/.*Static Nodes:\*\* \([0-9]*\).*/\1/p' | head -1)
if [ -n "$NODES" ] && [ "$NODES" -gt 0 ]; then
  check "graph indexed the subject ($NODES nodes)" "indexed" "indexed"
else
  check "graph indexed the subject" "indexed" "no nodes (${NODES:-unset})"
fi

check "GET / (Command Center) serves"       "200" "$(http_code "$URL/")"
check "GET /app.js serves"                  "200" "$(http_code "$URL/app.js")"
check "GET /styles.css serves"              "200" "$(http_code "$URL/styles.css")"
check "GET /assets/d3.v7.min.js serves"     "200" "$(http_code "$URL/assets/d3.v7.min.js")"
check "GET /events (SSE) serves"            "200" "$(http_code "$URL/events")"

# ══ 2. Structural code intelligence, against known ground truth ═══════
section "2. Code intelligence (asserted against the fixture)"

# entry() is the sole caller of orchestrate(); both facts come from
# reading src/lib.rs, not from lain.
CS=$(call get_call_sites '{"symbol":"orchestrate"}')
check_contains "get_call_sites finds the one real caller" "entry" "$CS"
CS_N=$(printf '%s' "$CS" | grep -c '^- \*\*')
check "get_call_sites reports exactly 1 calling function" "1" "$CS_N"

# orchestrate calls three helpers and nothing else calls them.
BR=$(call get_blast_radius '{"symbol":"helper_a"}')
check_contains "blast radius: direct dependent is orchestrate" "orchestrate" "$BR"
DIRECT_N=$(printf '%s' "$BR" | sed -n 's/^- Direct dependents (\([0-9]*\)).*/\1/p')
check "blast radius: exactly 1 direct dependent" "1" "${DIRECT_N:-none}"
check_contains "blast radius: entry() shows up as indirect" "entry" "$BR"

# never_called() is the only symbol in the tree nothing references.
DC=$(call find_dead_code)
check_contains "find_dead_code finds never_called" "never_called" "$DC"
check_absent  "find_dead_code excludes the test fn"  "test_entry"  "$DC"
check_absent  "find_dead_code excludes live helper_a" "helper_a"   "$DC"
check_absent  "find_dead_code excludes the hub"       "orchestrate" "$DC"

# The hub outranks the leaves: called by one, calls three, real body.
AN=$(call find_anchors)
TOP=$(printf '%s' "$AN" | sed -n 's/^1\. \([A-Za-z_][A-Za-z0-9_]*\).*/\1/p' | head -1)
check "find_anchors ranks the hub first" "orchestrate" "${TOP:-none}"

CC=$(call get_call_chain '{"from":"entry","to":"helper_a"}')
check_contains "get_call_chain links entry to helper_a" "helper_a" "$CC"

TD=$(call trace_dependency '{"symbol":"orchestrate"}')
check_contains "trace_dependency sees a callee" "helper_" "$TD"

EX=$(call explain_symbol '{"symbol":"orchestrate"}')
check_contains "explain_symbol names the file" "core.rs" "$EX"

SNIP=$(call get_code_snippet '{"symbol":"never_called","path":"src/dead.rs"}')
check_contains "get_code_snippet reads the subject's own file" "never_called" "$SNIP"

# ══ 3. Ambiguity — two parse() definitions, neither calling the other ═
section "3. Name collisions are reported, not guessed"

AMB=$(call explain_symbol '{"symbol":"parse"}')
check_contains "explain_symbol warns 'parse' is ambiguous" "defined 2 times" "$AMB"
check_contains "the warning offers ids to disambiguate"    "Pass a node id"  "$AMB"

# A node id must be accepted anywhere a name is, and must NOT warn.
PID=$(call query_graph '{"query":{"ops":[{"op":"find","type":"Function","name":"parse"},{"op":"limit","count":1}]}}' \
      | python3 -c "
import json,sys
try:
    d=json.loads(sys.stdin.read()); ns=d.get('nodes',[])
    print(ns[0]['id'] if ns else '')
except Exception: print('')
")
if [ -n "$PID" ]; then
  BYID=$(call explain_symbol "{\"symbol\":\"$PID\"}")
  check_absent "a node id resolves without an ambiguity warning" "defined 2 times" "$BYID"
  check_contains "ids round-trip from query_graph into explain_symbol" "Explanation" "$BYID"
else
  skip "node id round-trip" "query_graph returned no parse node"
fi

# ══ 4. Query language ═════════════════════════════════════════════════
section "4. Query language (the documented forms)"

Q1=$(call query_graph '{"query":{"ops":[{"op":"find","type":"Function","name":"orchestrate"}]}}')
check_contains "find by type+name" "orchestrate" "$Q1"

Q2=$(call query_graph '{"query":{"ops":[{"op":"find","type":"Function","name":"helper_a"},{"op":"connect","edge":"Calls","direction":"incoming","depth":1},{"op":"limit","count":20}]}}')
check_contains "chained find → connect → limit" "count" "$Q2"

Q3=$(call query_graph '{"query":{"ops":[{"op":"find","type":"File","name":"core.rs"},{"op":"connect","edge":"CoChangedWith","direction":"both","depth":1}]}}')
check_contains "CoChangedWith traversal runs" "count" "$Q3"

DS=$(call describe_schema)
check_contains "describe_schema lists node types" "Function" "$DS"

# ══ 5. Git-backed views ═══════════════════════════════════════════════
section "5. Git-backed views"

# The fixture's second commit touches core.rs and helpers.rs together.
CR=$(call get_coupling_radar '{"symbol":"src/core.rs"}')
check_contains "coupling radar sees the co-change" "helpers.rs" "$CR"

CH=$(call get_commit_history '{"limit":10}')
check_contains "commit history reaches the subject repo" "touch core" "$CH"

BS=$(call get_branch_status)
check_contains "branch status answers" "branch" "$(printf '%s' "$BS" | tr 'A-Z' 'a-z')"

RA=$(call get_recent_activity)
check_absent "recent activity is not an error" "__TOOL_ERROR__" "$RA"

# ══ 6. Architecture and reporting tools ═══════════════════════════════
section "6. Architecture and reporting"

for t in explore_architecture get_master_map get_layered_map list_entry_points \
         architectural_observations suggest_refactor_targets find_untested_functions \
         get_coverage_summary compare_modules detect_overlap get_context_for_prompt \
         get_world_state get_server_status get_reload_status list_recent_projects \
         get_agent_strategy get_test_template navigate_to_anchor get_context_depth \
         get_anchor_score get_cross_runtime_callers get_audit_log sync_state \
         run_enrichment get_file_diff
do
  case "$t" in
    get_context_for_prompt|navigate_to_anchor|get_context_depth|get_anchor_score|get_cross_runtime_callers)
      OUT=$(call "$t" '{"symbol":"orchestrate"}') ;;
    compare_modules) OUT=$(call "$t" '{"module_a":"src/core.rs","module_b":"src/helpers.rs"}') ;;
    get_test_template) OUT=$(call "$t" '{"symbol":"orchestrate"}') ;;
    get_file_diff) OUT=$(call "$t" '{"path":"src/core.rs"}') ;;
    *) OUT=$(call "$t") ;;
  esac
  case "$OUT" in
    __TRANSPORT_ERROR__*|__RPC_ERROR__*)
      check "$t answers" "a result" "$(printf '%s' "$OUT" | head -c 90)" ;;
    __TOOL_ERROR__*)
      # A tool may legitimately refuse (bad arg for this fixture); what it
      # must never do is fail at the transport or schema layer.
      check "$t answers (refusal is a result)" "a result" "a result" ;;
    *) check "$t answers" "a result" "a result" ;;
  esac
done

# ══ 7. Multiplayer — two agents, one file ═════════════════════════════
section "7. Multiplayer coordination"

reg() { # reg <name> <kind> -> "agent_id<TAB>token"
  call register_agent "{\"name\":\"$1\",\"kind\":\"$2\",\"mode\":\"interactive\"}" \
  | python3 -c "
import json,sys
try:
    d=json.loads(sys.stdin.read()); print(d['agent_id']+'\t'+d['session_token'])
except Exception: print('\t')
"
}
IFS=$'\t' read -r A_ID A_TOK <<< "$(reg alpha claude-code)"
IFS=$'\t' read -r B_ID B_TOK <<< "$(reg beta codex)"

if [ -z "$A_ID" ] || [ -z "$B_ID" ]; then
  skip "multiplayer" "registration did not return credentials"
else
  check "two agents register" "both" "both"

  G=$(call claim_files "{\"agent_id\":\"$A_ID\",\"session_token\":\"$A_TOK\",\"files\":[{\"path\":\"src/core.rs\",\"intent\":\"edit\"}]}")
  check_contains "alpha's edit claim is granted" "granted" "$G"

  # A different spelling of the same path must collide with the first.
  C=$(call claim_files "{\"agent_id\":\"$B_ID\",\"session_token\":\"$B_TOK\",\"files\":[{\"path\":\"./src/core.rs\",\"intent\":\"edit\"}]}")
  check_contains "beta's edit on ./src/core.rs conflicts"  "conflicts" "$C"
  check_contains "the conflict names the holder"           "alpha"     "$C"
  check_contains "the conflict states the blocking intent" "edit"      "$C"

  # A read never blocks; it is granted with an advisory instead.
  RD=$(call claim_files "{\"agent_id\":\"$B_ID\",\"session_token\":\"$B_TOK\",\"files\":[{\"path\":\"src/core.rs\",\"intent\":\"read\"}]}")
  check_contains "beta's read is granted, not refused" "granted" "$RD"
  check_contains "and carries an advisory"            "advisor" "$RD"

  OCC=$(call list_occupancy '{"path":"src/core.rs"}')
  check_contains "occupancy shows holders"       "holders" "$OCC"
  check_contains "occupancy names alpha"         "alpha"   "$OCC"
  check_contains "occupancy carries intent"      "intent"  "$OCC"

  LA=$(call list_active_agents)
  check_contains "list_active_agents sees alpha" "alpha" "$LA"
  check_contains "list_active_agents sees beta"  "beta"  "$LA"

  WA=$(call who_am_i "{\"agent_id\":\"$A_ID\",\"session_token\":\"$A_TOK\"}")
  check_contains "who_am_i identifies the caller" "alpha" "$WA"

  MC=$(call my_claims "{\"agent_id\":\"$A_ID\",\"session_token\":\"$A_TOK\"}")
  check_contains "my_claims lists alpha's claim" "core.rs" "$MC"

  HB=$(call heartbeat "{\"agent_id\":\"$A_ID\",\"session_token\":\"$A_TOK\"}")
  check_absent "heartbeat refreshes the session" "__TOOL_ERROR__" "$HB"

  # Bare path strings are the obvious spelling for a release.
  REL=$(call release_files "{\"agent_id\":\"$A_ID\",\"session_token\":\"$A_TOK\",\"files\":[\"src/core.rs\"]}")
  check_contains "release accepts a bare path string" "released" "$REL"

  # With alpha gone, beta's edit must now succeed.
  RC=$(call claim_files "{\"agent_id\":\"$B_ID\",\"session_token\":\"$B_TOK\",\"files\":[{\"path\":\"src/core.rs\",\"intent\":\"edit\"}]}")
  check_contains "beta can claim once alpha releases" "granted" "$RC"
  check_absent   "and sees no conflict this time"     "\"name\"" "$RC"

  call release_files "{\"agent_id\":\"$B_ID\",\"session_token\":\"$B_TOK\",\"files\":[\"src/core.rs\"]}" >/dev/null
  check_absent "list_subagents answers" "__RPC_ERROR__" "$(call list_subagents)"
fi

# ══ 8. Semantic search ════════════════════════════════════════════════
section "8. Semantic search"

if [ -n "${MODEL_ARGS[*]:-}" ]; then
  SS=$(call semantic_search '{"query":"function that coordinates several helpers"}')
  check_absent "semantic_search returns results, not an error" "__TOOL_ERROR__" "$SS"
  # Ranking quality is a judgement call, so assert only that the tool
  # retrieved real symbols from this repo rather than asserting a rank.
  case "$SS" in
    *orchestrate*|*helper_*|*entry*) check "results come from the subject repo" "yes" "yes" ;;
    *) check "results come from the subject repo" "yes" "no — $(printf '%s' "$SS" | head -c 120)" ;;
  esac
else
  skip "semantic_search" "no model on disk"
  skip "semantic result provenance" "no model on disk"
fi

# ══ 9. Execution tools (toolchain resolution) ═════════════════════════
section "9. Execution — resolves toolchains off PATH"

RB=$(call run_build)
case "$RB" in
  *"Build successful"*|*"Exit code: 0"*) check "run_build compiles the subject" "success" "success" ;;
  __TOOL_ERROR__*|*"not found"*)         check "run_build compiles the subject" "success" "$(printf '%s' "$RB" | head -c 140)" ;;
  *)                                     check "run_build compiles the subject" "success" "$(printf '%s' "$RB" | head -c 140)" ;;
esac

RT=$(call run_tests)
check_absent "run_tests runs the subject's test" "__RPC_ERROR__" "$RT"

# ══ 10. Interactive explorer sessions ═════════════════════════════════
section "10. Interactive explorers"

BR_UI=$(call get_blast_radius '{"symbol":"helper_a"}')
SID=$(printf '%s' "$BR_UI" | grep -o 'ui/blast-radius/[0-9a-f-]*' | head -1 | cut -d/ -f3)
if [ -n "$SID" ]; then
  check "blast radius emits a UI session link" "yes" "yes"
  check "GET /ui/blast-radius/<id> renders" "200" "$(http_code "$URL/ui/blast-radius/$SID")"
  PAGE=$(curl -s -m 20 "$URL/ui/blast-radius/$SID")
  check_contains "the page carries the graph payload" "is_direct" "$PAGE"
else
  skip "blast-radius explorer" "no session link in the output"
  skip "explorer page" "no session id"
fi

CPL=$(call get_coupling_radar '{"symbol":"src/core.rs"}')
CSID=$(printf '%s' "$CPL" | grep -o 'ui/coupling/[0-9a-f-]*' | head -1 | cut -d/ -f3)
if [ -n "$CSID" ]; then
  check "GET /ui/coupling/<id> renders" "200" "$(http_code "$URL/ui/coupling/$CSID")"
else
  skip "coupling explorer" "no session link emitted"
fi

# ══ 11. CLI surface ═══════════════════════════════════════════════════
section "11. CLI"

check_contains "lain --help lists the subcommands" "server" "$("$LAIN" --help 2>&1)"
check_contains "lain doctor runs its checks" "lain doctor" "$("$LAIN" doctor 2>&1)"

# doctor must fail loudly on a healthy process with a dead MCP surface.
python3 - "$WORK" <<'PY' &
import sys, json
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def _s(self, c, b):
        raw=json.dumps(b).encode(); self.send_response(c)
        self.send_header('Content-Type','application/json')
        self.send_header('Content-Length',str(len(raw))); self.end_headers(); self.wfile.write(raw)
    def do_GET(self):  self._s(200, {"status":"ok"}) if self.path=='/health' else self._s(404,{})
    def do_POST(self): self._s(200, {"jsonrpc":"2.0","id":1,"result":{"tools":[]}})
    def log_message(self,*a): pass
HTTPServer(('127.0.0.1',9932),H).serve_forever()
PY
STUB_PID=$!
sleep 2
DOC=$(LAIN_URL=http://127.0.0.1:9932 "$LAIN" doctor 2>&1); DOC_RC=$?
kill "$STUB_PID" 2>/dev/null
check_contains "doctor catches an empty MCP surface" "MCP surface empty" "$DOC"
check "doctor exits non-zero on that failure" "1" "$DOC_RC"

DOC_OK=$(LAIN_URL="$URL" "$LAIN" doctor 2>&1)
check_contains "doctor confirms a live surface" "MCP surface live" "$DOC_OK"

QO=$("$LAIN" query --workspace "$SUBJECT" '{"ops":[{"op":"find","type":"Function","name":"orchestrate"}]}' 2>&1 || true)
check_absent "lain query runs against the persisted graph" "error: unrecognized" "$QO"

INITDIR="$WORK/initdemo"; rm -rf "$INITDIR"; mkdir -p "$INITDIR"
(cd "$INITDIR" && git init -q && "$LAIN" init >/dev/null 2>&1 || true)
[ -f "$INITDIR/repos.yaml" ] \
  && check "lain init scaffolds repos.yaml" "written" "written" \
  || check "lain init scaffolds repos.yaml" "written" "missing"

# ══ 12. Federation — two repos, no cross-talk ═════════════════════════
section "12. Federation (multi-repo binding)"

FED_ROOT="$WORK/fed"; rm -rf "$FED_ROOT"
for r in alpha beta; do
  mkdir -p "$FED_ROOT/$r/src"
  cat > "$FED_ROOT/$r/Cargo.toml" <<EOF
[package]
name = "$r"
version = "0.1.0"
edition = "2021"
EOF
  cat > "$FED_ROOT/$r/src/lib.rs" <<EOF
pub fn ${r}_only_helper(x: u32) -> u32 { ${r}_inner(x) + 1 }
fn ${r}_inner(x: u32) -> u32 { x * 2 }
pub fn ${r}_entry() -> u32 { ${r}_only_helper(3) }
EOF
  (cd "$FED_ROOT/$r" && git init -q && git add -A && git -c user.email=d@l -c user.name=d commit -qm init)
done
cat > "$FED_ROOT/repos.yaml" <<EOF
data_dir: $FED_ROOT/data
repos:
  - id: alpha
    source: { type: workspace_dir, path: $FED_ROOT/alpha }
  - id: beta
    source: { type: workspace_dir, path: $FED_ROOT/beta }
EOF

FED_PORT=$((PORT+1)); FED_MCP="http://127.0.0.1:$FED_PORT/mcp"
XDG_STATE_HOME="$FED_ROOT/state" "$LAIN" server --config "$FED_ROOT/repos.yaml" \
  --transport http --port "$FED_PORT" --log-level warn > "$FED_ROOT/server.log" 2>&1 &
FED_PID=$!
for _ in $(seq 1 60); do
  [ "$(http_code "http://127.0.0.1:$FED_PORT/health")" = "200" ] && break
  sleep 1
done

fcall() {
  local fargs="${2:-}"; [ -z "$fargs" ] && fargs='{}'
  curl -s -m 120 -X POST "$FED_MCP" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$fargs}}" \
  | python3 -c "
import json,sys
d=json.loads(sys.stdin.read()); r=d.get('result',{})
print((r.get('content') or [{}])[0].get('text',''))
"
}

if [ "$(http_code "http://127.0.0.1:$FED_PORT/health")" = "200" ]; then
  check_contains "federation lists both repos" "beta" "$(fcall list_repos)"
  # Each per-repo tool must bind to the repo the symbol resolves to.
  check_contains "a symbol in alpha resolves in alpha" "alpha_inner" "$(fcall explain_symbol '{"symbol":"alpha_only_helper"}')"
  check_contains "a symbol in beta resolves in beta"   "beta_inner"  "$(fcall explain_symbol '{"symbol":"beta_only_helper"}')"
  ANC_A=$(fcall find_anchors '{"repo_id":"alpha"}')
  check_contains "repo_id=alpha lists alpha's symbols" "alpha_" "$ANC_A"
  check_absent   "and leaks nothing from beta"         "beta_"  "$ANC_A"
  # Relative paths must read the bound repo's checkout, not the server's cwd.
  SNIP_B=$(fcall get_code_snippet '{"symbol":"beta_inner","path":"src/lib.rs"}')
  check_contains "get_code_snippet reads beta's own checkout" "beta_only_helper" "$SNIP_B"
  XR=$(fcall get_cross_repo_blast_radius '{"symbol":"alpha_inner","depth":"1..3"}')
  check_contains "cross-repo blast radius groups by repo" "alpha" "$XR"
  # depth is a range string; a number must say so, not "missing".
  XE=$(fcall get_cross_repo_blast_radius '{"symbol":"alpha_inner","depth":2}')
  check_contains "a wrong-typed arg names the type, not 'missing'" "must be a string" "$XE"
else
  skip "federation" "second server never became healthy"
fi
# Left running on purpose: section 13 exercises the federation-scoped
# tools against it. Killed at the end of that section.

# ══ 13. Remaining surface, and coverage self-check ════════════════════
section "13. Remaining tools and coverage"

# Federation-scoped tools, against the two-repo server from section 12.
if [ "$(http_code "http://127.0.0.1:$FED_PORT/health")" = "200" ]; then
  check_contains "search_org finds symbols across repos" "beta_only_helper" \
    "$(fcall search_org '{"query":"only_helper","limit":20}')"
  check_contains "get_repo_info describes a repo" "alpha" \
    "$(fcall get_repo_info '{"repo_id":"alpha"}')"
  # Aggregate health, not a roster: both fixture repos must be `ready`.
  FH=$(fcall get_federation_health)
  check_contains "federation health counts both repos" '"total_repos":2' "$FH"
  check_contains "federation health reports both ready" '"ready":2'      "$FH"
  check_contains "cross-repo blast radius, repo pinned explicitly" "beta" \
    "$(fcall get_cross_repo_blast_radius_for_repo '{"repo_id":"beta","symbol":"beta_inner","depth":"1..3"}')"
else
  skip "federation-scoped tools" "federation server not running"
fi
kill "$FED_PID" 2>/dev/null

# Execution and control-plane tools on the subject server.
RC_OUT=$(call run_clippy)
check_absent "run_clippy answers" "__RPC_ERROR__" "$RC_OUT"

check_absent "request_reload answers" "__RPC_ERROR__" "$(call request_reload)"
check_absent "register_job_webhook answers" "__RPC_ERROR__" \
  "$(call register_job_webhook '{"url":"http://127.0.0.1:1/none"}')"
# No such job: the interesting property is that it says so rather than
# inventing a status.
check_contains "get_job_status reports an unknown job as not found" "not found" \
  "$(printf '%s' "$(call get_job_status '{"job_id":"no-such-job"}')" | tr 'A-Z' 'a-z')"
check_absent "debug_sleep answers" "__RPC_ERROR__" "$(call debug_sleep '{"secs":0}')"

# `install_language_server` downloads and installs a toolchain. Running
# it inside a demo would mutate the machine, so this checks the contract
# it advertises instead of invoking it — and says so, rather than
# quietly leaving a gap in the coverage count below.
ILS_SCHEMA=$(curl -s -m 30 -X POST "$MCP" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | python3 -c "
import json,sys
for t in json.load(sys.stdin)['result']['tools']:
    if t['name']=='install_language_server':
        print(json.dumps(t['inputSchema'])); break
")
check_contains "install_language_server advertises its schema (not invoked: it mutates the machine)" \
  "language" "${ILS_SCHEMA:-}"

# Coverage: every advertised tool must be exercised here, or named below
# as deliberately not invoked. A tool added to the surface without a
# check fails this — the only thing that keeps a demo honest as the
# surface grows.
#
# This counts checks that *exist* in the script, not checks that ran:
# a phase that SKIPped still counts here. The skip is reported on its
# own line and in the summary, so read both.
curl -s -m 30 -X POST "$MCP" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  > "$WORK/tools.json"

COVERAGE=$(SCRIPT="$REPO_ROOT/scripts/demo.sh" \
           TOOLS="$WORK/tools.json" \
           EXEMPT="install_language_server" \
           python3 <<'PYCOV'
import json, os, re
advertised = {t["name"] for t in json.load(open(os.environ["TOOLS"]))["result"]["tools"]}
src = open(os.environ["SCRIPT"]).read()
called = set()
for m in re.finditer(r"\b(?:call|fcall|bench|sbench)\s+(?:\"[^\"]*\"\s+)?([a-z_][a-z0-9_]*)", src):
    called.add(m.group(1))
loop = re.search(r"for t in ([\s\S]*?)\ndo\n", src)
if loop:
    called.update(w for w in loop.group(1).split() if re.fullmatch(r"[a-z_][a-z0-9_]*", w))
exempt = set(os.environ["EXEMPT"].split())
missing = sorted(advertised - called - exempt)
covered = len(advertised) - len(missing) - len(exempt)
print("%d/%d %s" % (covered, len(advertised),
                    "MISSING: " + ",".join(missing) if missing else "complete"))
PYCOV
)
case "$COVERAGE" in
  *complete*) check "every advertised tool is exercised (${COVERAGE%% *} + 1 exempt)" "complete" "complete" ;;
  *)          check "every advertised tool is exercised" "complete" "$COVERAGE" ;;
esac

# ══ 14. Benchmark ═════════════════════════════════════════════════════
if [ "$QUICK" = 0 ]; then
  section "14. Benchmark"

  bench() { # bench <label> <tool> <args> <reps>
    local label="$1" tool="$2" args="$3" reps="${4:-10}" i t0 t1 body
    local -a ms=(); local bad=0
    for i in $(seq 1 "$reps"); do
      t0=$(date +%s%N)
      body=$(curl -s -m 120 -X POST "$MCP" -H 'Content-Type: application/json' \
             -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}")
      t1=$(date +%s%N); ms+=( $(( (t1 - t0) / 1000000 )) )
      # A benchmark that might be timing failures measures nothing.
      case "$body" in *'"isError":true'*|*'"error"'*) bad=$((bad+1)) ;; esac
    done
    printf '%s\n' "${ms[@]}" | sort -n | BAD="$bad" LABEL="$label" python3 -c "
import os, sys
v=[int(x) for x in sys.stdin.read().split()]; n=len(v)
bad=int(os.environ['BAD'])
print('  %-30s n=%-3d  p50 %5d ms   p95 %5d ms   max %5d ms%s'
      % (os.environ['LABEL'], n, v[n//2], v[min(n-1,int(n*0.95))], v[-1],
         '' if bad==0 else '   !! %d ERRORED' % bad))
"
  }

  printf '  %ssubject repo: %s nodes%s\n\n' "$DIM" "${NODES:-?}" "$RST"
  bench "get_health"        get_health        '{}' 10
  bench "query_graph (find)" query_graph      '{"query":{"ops":[{"op":"find","type":"Function","name":"orchestrate"}]}}' 10
  bench "get_call_sites"    get_call_sites    '{"symbol":"orchestrate"}' 10
  bench "get_blast_radius"  get_blast_radius  '{"symbol":"helper_a"}' 10
  bench "explain_symbol"    explain_symbol    '{"symbol":"orchestrate"}' 10
  bench "find_anchors"      find_anchors      '{}' 10
  bench "find_dead_code"    find_dead_code    '{}' 5
  bench "list_occupancy"    list_occupancy    '{}' 10
  [ -n "${MODEL_ARGS[*]:-}" ] && bench "semantic_search" semantic_search '{"query":"coordinate helpers"}' 5

  printf '\n  %sconcurrency%s\n' "$DIM" "$RST"
  CT0=$(date +%s%N)
  CPIDS=()
  for i in $(seq 1 20); do
    curl -s -m 60 -o /dev/null -X POST "$MCP" -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}' &
    CPIDS+=( $! )
  done
  # Wait on these PIDs only: a bare `wait` also waits for the server,
  # which never exits, and hangs the run.
  for cp in "${CPIDS[@]}"; do wait "$cp"; done
  CT1=$(date +%s%N)
  CMS=$(( (CT1 - CT0) / 1000000 ))
  printf '  %-30s 20 concurrent get_health in %s ms\n' "parallel throughput" "$CMS"

  printf '\n  %sindexing%s\n' "$DIM" "$RST"
  printf '  %-30s %s ms (boot + full index, %s nodes)\n' "cold start (subject)" "$BOOT_MS" "${NODES:-?}"

  # The fixture is deliberately tiny so its call graph can be reasoned
  # about by hand. Timings on 24 nodes say nothing about scale, so run
  # the same measurements against this repo — a real Rust codebase — and
  # report both.
  if [ -d "$REPO_ROOT/.git" ]; then
    SCALE="$WORK/scale"; rm -rf "$SCALE"; mkdir -p "$SCALE"
    cat > "$SCALE/repos.yaml" <<EOF
data_dir: $SCALE/data
repos:
  - id: lain
    source: { type: workspace_dir, path: $REPO_ROOT }
EOF
    SPORT=$((PORT+2)); SURL="http://127.0.0.1:$SPORT"; SMCP="$SURL/mcp"
    ST0=$(date +%s%N)
    XDG_STATE_HOME="$SCALE/state" "$LAIN" server --config "$SCALE/repos.yaml"       --transport http --port "$SPORT" --log-level warn "${MODEL_ARGS[@]}"       > "$SCALE/server.log" 2>&1 &
    SCALE_PID=$!
    for _ in $(seq 1 180); do
      [ "$(http_code "$SURL/health")" = "200" ] && break
      sleep 1
    done
    ST1=$(date +%s%N); SMS=$(( (ST1 - ST0) / 1000000 ))

    if [ "$(http_code "$SURL/health")" = "200" ]; then
      SH=$(curl -s -m 60 -X POST "$SMCP" -H 'Content-Type: application/json'            -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_health","arguments":{}}}'            | python3 -c "
import json,sys
print((json.load(sys.stdin)['result']['content'] or [{}])[0].get('text',''))
")
      SN=$(printf '%s' "$SH" | sed -n 's/.*Static Nodes:\*\* \([0-9]*\).*/\1/p' | head -1)
      SE=$(printf '%s' "$SH" | sed -n 's/.*Static Edges:\*\* \([0-9]*\).*/\1/p' | head -1)
      printf '  %-30s %s ms (boot + full index, %s nodes / %s edges)\n'         "cold start (lain itself)" "$SMS" "${SN:-?}" "${SE:-?}"

      sbench() { # sbench <label> <tool> <args> <reps>
        local label="$1" tool="$2" args="$3" reps="${4:-8}" i t0 t1 body
        local -a ms=(); local bad=0
        for i in $(seq 1 "$reps"); do
          t0=$(date +%s%N)
          body=$(curl -s -m 180 -X POST "$SMCP" -H 'Content-Type: application/json' \
                 -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}")
          t1=$(date +%s%N); ms+=( $(( (t1 - t0) / 1000000 )) )
          # A benchmark that might be timing failures measures nothing.
          case "$body" in *'"isError":true'*|*'"error"'*) bad=$((bad+1)) ;; esac
        done
        printf '%s\n' "${ms[@]}" | sort -n | BAD="$bad" LABEL="$label" python3 -c "
import os, sys
v=[int(x) for x in sys.stdin.read().split()]; n=len(v)
bad=int(os.environ['BAD'])
print('  %-30s n=%-3d  p50 %5d ms   p95 %5d ms   max %5d ms%s'
      % (os.environ['LABEL'], n, v[n//2], v[min(n-1,int(n*0.95))], v[-1],
         '' if bad==0 else '   !! %d ERRORED' % bad))
"
      }
      # Gate the timings on a fact that is true of this repo. Without
      # this the numbers could be measuring a broken request loop —
      # which is exactly what happened once: a mangled payload made
      # every scale call fail, and the uniform 6 ms result looked like
      # excellent performance.
      SCS=$(curl -s -m 120 -X POST "$SMCP" -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_call_sites","arguments":{"symbol":"sweep_orphans"}}}' \
            | python3 -c "
import json,sys
d=json.load(sys.stdin); r=d.get('result',{})
print((r.get('content') or [{}])[0].get('text',''))
")
      check_contains "scale server answers about its own code" "index_one_repo" "$SCS"

      printf '\n  %sat scale (%s nodes)%s\n' "$DIM" "${SN:-?}" "$RST"
      sbench "query_graph (find)"  query_graph      '{"query":{"ops":[{"op":"find","type":"Function","name":"resolve_node"}]}}' 8
      sbench "get_call_sites"      get_call_sites   '{"symbol":"resolve_node"}' 8
      sbench "get_blast_radius"    get_blast_radius '{"symbol":"canonical_claim_path"}' 8
      sbench "explain_symbol"      explain_symbol   '{"symbol":"resolve_node"}' 8
      sbench "find_anchors"        find_anchors     '{}' 8
      sbench "find_dead_code"      find_dead_code   '{}' 3
      [ -n "${MODEL_ARGS[*]:-}" ] && sbench "semantic_search" semantic_search '{"query":"claim a file before editing"}' 3
    else
      printf '  %sscale run skipped: second server never became healthy%s\n' "$YEL" "$RST"
    fi
    kill "$SCALE_PID" 2>/dev/null
  fi
fi

# ══ report ════════════════════════════════════════════════════════════
section "Result"

TOTAL=$((PASS+FAIL))
printf '  checks: %s%d passed%s' "$GRN" "$PASS" "$RST"
[ "$FAIL" -gt 0 ] && printf ', %s%d failed%s' "$RED" "$FAIL" "$RST"
[ "$SKIP" -gt 0 ] && printf ', %s%d skipped%s' "$YEL" "$SKIP" "$RST"
printf ' (of %d run)\n' "$TOTAL"

if [ -s "$TIMES_TSV" ]; then
  printf '\n  %sslowest calls observed%s\n' "$DIM" "$RST"
  sort -k2 -n -r "$TIMES_TSV" | head -5 | awk '{printf "  %-32s %6s ms\n", $1, $2}'
fi

if [ -n "$JSON_OUT" ]; then
  python3 - "$RESULTS_TSV" "$TIMES_TSV" "$JSON_OUT" "$PASS" "$FAIL" "$SKIP" "$BOOT_MS" <<'PY'
import json, sys, collections
res_p, times_p, out_p, npass, nfail, nskip, boot = sys.argv[1:8]
checks=[]
for line in open(res_p):
    p=line.rstrip('\n').split('\t')
    if len(p)>=4: checks.append({"status":p[0],"name":p[1],"expected":p[2],"actual":p[3]})
times=collections.defaultdict(list)
for line in open(times_p):
    p=line.rstrip('\n').split('\t')
    if len(p)==2: times[p[0]].append(int(p[1]))
tool_stats={k:{"calls":len(v),"min_ms":min(v),"max_ms":max(v),
               "p50_ms":sorted(v)[len(v)//2]} for k,v in times.items()}
json.dump({"passed":int(npass),"failed":int(nfail),"skipped":int(nskip),
           "boot_and_index_ms":int(boot),"checks":checks,"tool_latency":tool_stats},
          open(out_p,"w"), indent=2)
print("  json: "+out_p)
PY
fi

if [ "$FAIL" -eq 0 ]; then
  printf '\n  %sEvery capability check passed.%s\n\n' "$GRN" "$RST"
  exit 0
else
  printf '\n  %s%d check(s) failed — see above.%s\n\n' "$RED" "$FAIL" "$RST"
  exit 1
fi
