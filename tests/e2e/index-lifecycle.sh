#!/usr/bin/env bash
# End-to-end acceptance for the index-convergence / pruning / freshness work.
set -u

# Source shared helpers — single source of truth for the MCP
# protocol version, queried from the binary at runtime.
source "$(dirname "$0")/lib.sh"

SP="${TMPDIR:-/tmp}/lain-acceptance"; mkdir -p "$SP"
L="${LAIN_BIN:-$(git rev-parse --show-toplevel)/target/release/lain}"
REPO="$(git rev-parse --show-toplevel)"
W="$SP/acc"; PASS=0; FAIL=0
ok(){ printf "  \033[32mPASS\033[0m %s\n" "$1"; PASS=$((PASS+1)); }
no(){ printf "  \033[31mFAIL\033[0m %s\n     got: %s\n" "$1" "$2"; FAIL=$((FAIL+1)); }
chk(){ # name  expected-substring  actual
  case "$3" in *"$2"*) ok "$1";; *) no "$1" "$(echo "$3"|head -c 160)";; esac; }
nochk(){ # name  forbidden-substring  actual
  case "$3" in *"$2"*) no "$1" "$(echo "$3"|head -c 160)";; *) ok "$1";; esac; }

# index <budget> — run one process purely to index, and let it exit.
index(){ local budget=$1
  { printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'"'"'$LAIN_MCP_PROTOCOL_VERSION'"'"'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
    sleep $((budget+25)); } \
  | LAIN_REINDEX_TIMEOUT=$budget timeout $((budget+45)) $L mcp --workspace "$W" >/dev/null 2>&1
}

# ask <timeout> <reindex_budget> <json-lines-file> -> prints "id<TAB>text" per result
ask(){ local wait=$1 budget=$2 file=$3
  { printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'"'"'$LAIN_MCP_PROTOCOL_VERSION'"'"'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
    sleep "$wait"; cat "$file"; sleep 12; } \
  | LAIN_REINDEX_TIMEOUT=$budget timeout $((wait+40)) $L mcp --workspace "$W" 2>/dev/null \
  | python3 -c '
import sys,json
for line in sys.stdin:
    try: d=json.loads(line)
    except: continue
    i=d.get("id")
    if i and i!=1:
        r=d.get("result"); e=d.get("error")
        t=(r["content"][0]["text"] if r else "ERROR:"+str(e))
        print(f"{i}\t"+t.replace("\n","\\n"))'
}
get(){ grep -P "^$1\t" | cut -f2-; }

echo "=== setup: fresh clone ==="
rm -rf "$W" "$SP/accdata"; git clone --local --no-hardlinks -q "$REPO" "$W"
cd "$W"
mkdir -p src
cat > src/acc_probe.rs <<'EOF'
pub fn acc_keep_me() -> u32 {
    let v = 1;
    v
}
pub fn acc_delete_me() -> u32 {
    let v = 2;
    v
}
pub fn acc_leaf_never_called() -> u32 {
    let v = 3;
    v
}
pub fn acc_has_caller() -> u32 {
    let v = 4;
    v
}
pub fn acc_the_caller() -> u32 {
    let v = acc_has_caller();
    v + 1
}
EOF
cat > src/acc_doomed.rs <<'EOF'
pub fn acc_in_doomed_file() -> u32 {
    let v = 9;
    v
}
EOF
python3 - <<'PY'
s=open('src/lib.rs').read()
add="pub mod acc_probe;\npub mod acc_doomed;\n"
if 'acc_probe' not in s: open('src/lib.rs','w').write(add+s)
PY
git add -A && git -c user.email=t@t -c user.name=t commit -qm "acc: probes"
echo "  base commit $(git rev-parse --short HEAD)"

echo
echo "=== PHASE 1: full index of a fresh clone ==="
cat > "$SP/q1.txt" <<'EOF'
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":10}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_keep_me"}},"id":11}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_call_sites","arguments":{"symbol":"acc_has_caller"}},"id":12}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_call_sites","arguments":{"symbol":"acc_leaf_never_called"}},"id":13}
EOF
index 200
R1=$(ask 15 1 "$SP/q1.txt")
H=$(echo "$R1"|get 10)
chk   "graph indexed and current"         "(current)"            "$H"
nochk "no staging-dir workspace"          "lain-federation"      "$H"
chk   "Calls edges present"               "Calls"                "$H"
chk   "explain_symbol resolves"           "acc_keep_me"          "$(echo "$R1"|get 11)"
nochk "paths are relative (no /home)"     "/home/sebastian"      "$(echo "$R1"|get 11)"
chk   "explain_symbol has Source"         "### Source"           "$(echo "$R1"|get 11)"
chk   "real caller found"                 "acc_the_caller"       "$(echo "$R1"|get 12)"
chk   "leaf reported as leaf"             "is a leaf"            "$(echo "$R1"|get 13)"
nochk "clean file -> no staleness note"   "was modified"         "$(echo "$R1"|get 11)"

echo
echo "=== PHASE 2: incremental — add, remove, delete a file (all committed) ==="
cd "$W"
cat > src/acc_probe.rs <<'EOF'
pub fn acc_keep_me() -> u32 {
    let v = 1;
    v
}
pub fn acc_newly_added() -> u32 {
    let v = 7;
    v
}
pub fn acc_leaf_never_called() -> u32 {
    let v = 3;
    v
}
pub fn acc_has_caller() -> u32 {
    let v = 4;
    v
}
pub fn acc_the_caller() -> u32 {
    let v = acc_has_caller();
    v + 1
}
EOF
git rm -q src/acc_doomed.rs
python3 - <<'PY'
s=open('src/lib.rs').read().replace("pub mod acc_doomed;\n","")
open('src/lib.rs','w').write(s)
PY
git add -A && git -c user.email=t@t -c user.name=t commit -qm "acc: add one, remove one, delete a file"
echo "  head commit $(git rev-parse --short HEAD)"
cat > "$SP/q2.txt" <<'EOF'
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":20}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_newly_added"}},"id":21}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_delete_me"}},"id":22}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_in_doomed_file"}},"id":23}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_keep_me"}},"id":24}
EOF
index 200
R2=$(ask 15 1 "$SP/q2.txt")
chk   "still current after incremental"   "(current)"          "$(echo "$R2"|get 20)"
chk   "NEW symbol picked up (revwalk)"    "acc_newly_added"    "$(echo "$R2"|get 21)"
chk   "REMOVED symbol is gone (replace)"  "not found"          "$(echo "$R2"|get 22)"
chk   "DELETED file's symbol gone"        "not found"          "$(echo "$R2"|get 23)"
chk   "untouched symbol survives"         "acc_keep_me"        "$(echo "$R2"|get 24)"

echo
echo "=== PHASE 3: uncommitted edit -> freshness note ==="
sleep 2; echo "pub fn acc_uncommitted() -> u32 { 8 }" >> "$W/src/acc_probe.rs"
cat > "$SP/q3.txt" <<'EOF'
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_keep_me"}},"id":30}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_call_sites","arguments":{"symbol":"acc_leaf_never_called"}},"id":31}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_no_such_symbol_anywhere"}},"id":32}
EOF
R3=$(ask 15 1 "$SP/q3.txt")
chk "edited file -> staleness note"       "was modified"                   "$(echo "$R3"|get 30)"
chk "note names the file"                 "src/acc_probe.rs"               "$(echo "$R3"|get 30)"
chk "stale leaf no longer claims 'leaf'"  "would not appear"               "$(echo "$R3"|get 31)"
chk "not-found explains the limitation"   "committed"                      "$(echo "$R3"|get 32)"

echo
echo "=== PHASE 4: restart persistence + running from a foreign cwd ==="
git -C "$W" checkout -q -- src/acc_probe.rs   # drop the uncommitted edit
BEFORE=$(stat -c %Y "$W/.lain/graph.bin")
cat > "$SP/q4.txt" <<'EOF'
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":40}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"explain_symbol","arguments":{"symbol":"acc_keep_me"}},"id":41}
EOF
cd /tmp   # deliberately NOT the workspace
R4=$(ask 15 1 "$SP/q4.txt")
chk   "survives restart, still current"   "(current)"     "$(echo "$R4"|get 40)"
chk   "restart kept the symbols"          "acc_keep_me"   "$(echo "$R4"|get 41)"
chk   "Source excerpt from foreign cwd"   "### Source"    "$(echo "$R4"|get 41)"
AFTER=$(stat -c %Y "$W/.lain/graph.bin")
if [ "$BEFORE" = "$AFTER" ]; then ok "no needless re-index when current"; else no "no needless re-index when current" "mtime moved"; fi

echo
echo "=== PHASE 5: federation — projection retracts too ==="
cat > "$SP/acc.yaml" <<EOF
data_dir: $SP/accdata
repos:
  - id: accrepo
    source: { type: workspace_dir, path: $W }
EOF
fedask(){ local wait=$1 file=$2
  { printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
    sleep "$wait"; cat "$file"; sleep 12; } \
  | timeout $((wait+40)) $L server --config "$SP/acc.yaml" --transport stdio 2>/dev/null \
  | python3 -c '
import sys,json
for line in sys.stdin:
    try: d=json.loads(line)
    except: continue
    i=d.get("id")
    if i and i!=1:
        r=d.get("result"); e=d.get("error")
        t=(r["content"][0]["text"] if r else "ERROR:"+str(e))
        print(f"{i}\t"+t.replace("\n","\\n"))'
}
cat > "$SP/q5.txt" <<'EOF'
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"search_org","arguments":{"query":"acc_keep_me","limit":3}},"id":50}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"search_org","arguments":{"query":"acc_delete_me","limit":3}},"id":51}
{"jsonrpc":"2.0","method":"tools/call","params":{"name":"search_org","arguments":{"query":"acc_in_doomed_file","limit":3}},"id":52}
EOF
R5=$(fedask 130 "$SP/q5.txt")
chk   "federation finds a live symbol"        "acc_keep_me"        "$(echo "$R5"|get 50)"
nochk "federation drops a removed symbol"     "acc_delete_me"      "$(echo "$R5"|get 51)"
nochk "federation drops a deleted file's sym" "acc_in_doomed_file" "$(echo "$R5"|get 52)"

echo
echo "════════════════════════════════════════"
printf "  PASSED: %d   FAILED: %d\n" "$PASS" "$FAIL"
echo "════════════════════════════════════════"
[ "$FAIL" -eq 0 ]
