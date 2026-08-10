#!/bin/bash
# End-to-end sandboxed test for LAIN installer
# Runs in a fake HOME, does NOT touch real files

PASS=0
FAIL=0

pass() { echo "  [PASS] $1"; ((PASS++)); }
fail() { echo "  [FAIL] $1"; ((FAIL++)); }
skip() { echo "  [SKIP] $1"; }

FAKE_HOME=$(mktemp -d)
REAL_HOME="$HOME"
FAKE_WORKSPACE="$FAKE_HOME/test-project"
INSTALL_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/install.sh"
LAIN_BINARY="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/lain"

cleanup() {
    rm -rf "$FAKE_HOME"
}
trap cleanup EXIT

echo "==============================================="
echo "  LAIN End-to-End Test Suite (sandboxed)"
echo "==============================================="
echo "Fake HOME: $FAKE_HOME"
echo ""

# ---------- SETUP ----------
echo "--- Setup ---"
export HOME="$FAKE_HOME"
export LAIN_INSTALL_DIR="$HOME/.local/lain"
mkdir -p "$LAIN_INSTALL_DIR"
mkdir -p "$FAKE_WORKSPACE/.git"  # fake git repo
cp "$LAIN_BINARY" "$LAIN_INSTALL_DIR/lain"
chmod +x "$LAIN_INSTALL_DIR/lain"
rm -f "$HOME/.claude/settings.json"  # fresh state
pass "Test environment created"

# ---------- BINARY TESTS ----------
echo ""
echo "--- Binary Tests ---"

"$LAIN_BINARY" --version >/dev/null 2>&1 && pass "lain --version" || fail "lain --version"
"$LAIN_BINARY" --help >/dev/null 2>&1 && pass "lain --help" || fail "lain --help"
"$LAIN_BINARY" init --help >/dev/null 2>&1 && pass "lain init --help" || fail "lain init --help"
"$LAIN_BINARY" query --help >/dev/null 2>&1 && pass "lain query --help" || fail "lain query --help"
"$LAIN_BINARY" hook --help >/dev/null 2>&1 && pass "lain hook --help" || fail "lain hook --help"
"$LAIN_BINARY" ask --help >/dev/null 2>&1 && pass "lain ask --help" || fail "lain ask --help"

# ---------- INSTALL SCRIPT TESTS ----------
echo ""
echo "--- Install Script Tests ---"

# Test 1: Help
bash "$INSTALL_SCRIPT" --help >/dev/null 2>&1 && pass "install.sh --help" || fail "install.sh --help"

# Test 2: Source detection (should not run main)
output=$(bash -c "source '$INSTALL_SCRIPT' 2>/dev/null && echo 'sourced_ok'" 2>&1 || true)
echo "$output" | grep -q "sourced_ok" && pass "install.sh can be sourced without running main" || fail "install.sh source guard"

# Test 3: Unknown option
bash "$INSTALL_SCRIPT" --bogus 2>&1 | grep -q "Unknown option" && pass "install.sh catches unknown options" || fail "install.sh unknown option handling"

# Test 4: Platform detection
platform=$(bash -c "source '$INSTALL_SCRIPT' 2>/dev/null; detect_platform" 2>&1)
[[ -n "$platform" && "$platform" != "unsupported" ]] && pass "detect_platform: $platform" || fail "detect_platform returned: $platform"

# Test 5: check_writeable
bash -c "source '$INSTALL_SCRIPT' 2>/dev/null; check_writeable" 2>/dev/null && pass "check_writeable passes" || fail "check_writeable"

# Test 6: check_in_path (empty PATH)
PATH="" bash -c "source '$INSTALL_SCRIPT' 2>/dev/null; check_in_path" 2>/dev/null && fail "check_in_path should fail with empty PATH" || pass "check_in_path correctly fails when lain not in PATH"

# Test 7: download_onnx_model returns clean path
result=$(bash -c "source '$INSTALL_SCRIPT' 2>/dev/null; download_onnx_model" 2>/dev/null || true)
if [ -z "$result" ]; then
    pass "download_onnx_model returns empty on failure (no network in test)"
else
    fail "download_onnx_model should return empty without network"
fi

# Test 8: parse_args - yes flag
bash -c "source '$INSTALL_SCRIPT' 2>/dev/null; parse_args --yes; [ \"\$OPT_YES\" = 'yes' ] && echo ok" | grep -q ok && pass "parse_args --yes" || fail "parse_args --yes"

# ---------- INIT TESTS (sandboxed) ----------
echo ""
echo "--- Init Tests ---"

# Ensure clean state
rm -rf "$HOME/.claude" "$HOME/.cursor" "$HOME/.windsurf" "$HOME/.cline"

# Test: init claude
echo "Testing: lain init --agent claude --workspace $FAKE_WORKSPACE --yes"
"$LAIN_BINARY" init --agent claude --workspace "$FAKE_WORKSPACE" --yes 2>&1 | grep -q "initialization complete" && pass "lain init --agent claude" || fail "lain init --agent claude"

# Verify settings.json was created
[ -f "$HOME/.claude/settings.json" ] && pass "~/.claude/settings.json created" || fail "~/.claude/settings.json not created"

# Verify LAIN.md was created
[ -f "$HOME/.claude/LAIN.md" ] && pass "~/.claude/LAIN.md created" || fail "~/.claude/LAIN.md not created"

# Verify settings.json has lain mcp entry.
# Note: when `lain` is on the host PATH the binary resolves the absolute path
# via `which::which("lain")` (see src/cmds/init.rs), so we accept either form.
python3 -c "
import json
with open('$HOME/.claude/settings.json') as f:
    d = json.load(f)
assert 'mcpServers' in d, 'mcpServers missing'
assert 'lain' in d['mcpServers'], 'lain entry missing'
cmd = d['mcpServers']['lain']['command']
assert cmd == 'lain' or cmd.endswith('/lain'), f'unexpected command: {cmd!r}'
args = d['mcpServers']['lain'].get('args', [])
assert '--workspace' in args and 'auto' in args, f'unexpected args: {args!r}'
print('OK')
" 2>/dev/null && pass "settings.json has correct lain MCP entry" || fail "settings.json MCP entry wrong"

# Test: init claude again (should skip)
"$LAIN_BINARY" init --agent claude --workspace "$FAKE_WORKSPACE" --yes 2>&1 | grep -q "already configured" && pass "lain init skips when already configured" || fail "lain init re-config check"

# Test: init cursor
"$LAIN_BINARY" init --agent cursor --yes 2>&1 | grep -q "initialization complete" && pass "lain init --agent cursor" || fail "lain init --agent cursor"
[ -f "$HOME/.cursor/LAIN.md" ] && pass "~/.cursor/LAIN.md created" || fail "~/.cursor/LAIN.md not created"

# Test: init windsurf
"$LAIN_BINARY" init --agent windsurf --yes 2>&1 | grep -q "initialization complete" && pass "lain init --agent windsurf" || fail "lain init --agent windsurf"
[ -f "$HOME/.windsurf/lain-rules.md" ] && pass "~/.windsurf/lain-rules.md created" || fail "~/.windsurf/lain-rules.md not created"

# Test: init cline
"$LAIN_BINARY" init --agent cline --yes 2>&1 | grep -q "initialization complete" && pass "lain init --agent cline" || fail "lain init --agent cline"
[ -f "$HOME/.cline/lain-rules.md" ] && pass "~/.cline/lain-rules.md created" || fail "~/.cline/lain-rules.md not created"

# ---------- AWARENESS DOC CONTENT TESTS ----------
echo ""
echo "--- Awareness Doc Content Tests ---"

# Claude doc
grep -q "Query for structure" "$HOME/.claude/LAIN.md" && pass "Claude doc: query-first title" || fail "Claude doc: missing query-first title"
grep -q "lain query" "$HOME/.claude/LAIN.md" && pass "Claude doc: mentions lain query" || fail "Claude doc: missing lain query"
grep -q "find Function name handle" "$HOME/.claude/LAIN.md" && pass "Claude doc: has query example" || fail "Claude doc: missing query example"
grep -q "semantic_search" "$HOME/.claude/LAIN.md" && pass "Claude doc: mentions semantic_search" || fail "Claude doc: missing semantic_search"
grep -q "get_code_snippet" "$HOME/.claude/LAIN.md" && pass "Claude doc: mentions get_code_snippet" || fail "Claude doc: missing get_code_snippet"
# Should NOT mention blast_radius (has query equivalent)
grep -q "get_blast_radius\|get_call_chain\|get_coupling_radar\|find_anchors" "$HOME/.claude/LAIN.md" && fail "Claude doc: should NOT mention query-equivalent tools" || pass "Claude doc: correctly excludes query-equivalent tools"

# Cursor doc
grep -q "lain query" "$HOME/.cursor/LAIN.md" && pass "Cursor doc: mentions lain query" || fail "Cursor doc: missing lain query"
grep -q "get_code_snippet" "$HOME/.cursor/LAIN.md" && pass "Cursor doc: mentions get_code_snippet" || fail "Cursor doc: missing tools"

# Windsurf doc
grep -q "lain query" "$HOME/.windsurf/lain-rules.md" && pass "Windsurf doc: mentions lain query" || fail "Windsurf doc: missing lain query"
grep -q "semantic_search" "$HOME/.windsurf/lain-rules.md" && pass "Windsurf doc: mentions semantic_search" || fail "Windsurf doc: missing tools"

# Cline doc
grep -q "lain query" "$HOME/.cline/lain-rules.md" && pass "Cline doc: mentions lain query" || fail "Cline doc: missing lain query"

# ---------- HOOK TESTS ----------
echo ""
echo "--- Hook Tests ---"

# Install hook
"$LAIN_BINARY" hook --agent claude 2>&1 | grep -q "installation complete" && pass "lain hook install" || fail "lain hook install"
[ -f "$HOME/.claude/hooks/lain-ask.sh" ] && pass "Hook script created" || fail "Hook script not created"
[ -x "$HOME/.claude/hooks/lain-ask.sh" ] && pass "Hook script is executable" || fail "Hook script not executable"

# Verify settings.json has hook entry
python3 -c "
import json
with open('$HOME/.claude/settings.json') as f:
    d = json.load(f)
hooks = d.get('hooks', {}).get('PreToolUse', [])
has_lain = any('lain' in str(h) for h in hooks)
assert has_lain, 'No lain hook found'
print('OK')
" 2>/dev/null && pass "settings.json has lain PreToolUse hook" || fail "settings.json missing hook"

# Uninstall hook
"$LAIN_BINARY" hook --uninstall 2>&1 | grep -q "uninstalled" && pass "lain hook uninstall" || fail "lain hook uninstall"

# ---------- ASK COMMAND TESTS ----------
echo ""
echo "--- Ask Command Tests ---"

# Pass-through (non-LAIN command)
output=$(echo '{"tool_name":"Bash","tool_input":{"command":"ls -la"}}' | "$LAIN_BINARY" ask 2>&1 || true)
[ -z "$output" ] && pass "ask: silent passthrough for non-LAIN cmd" || fail "ask: should be silent for non-LAIN cmd"

# Interception (LAIN command)
output=$(echo '{"tool_name":"Bash","tool_input":{"command":"lain query blast radius"}}' | "$LAIN_BINARY" ask 2>&1)
echo "$output" | grep -q "hookSpecificOutput" && pass "ask: intercepts LAIN commands" || fail "ask: failed to intercept LAIN command"

# ---------- QUERY TEST ----------
echo ""
echo "--- Query Test ---"
output=$("$LAIN_BINARY" query "find Function | limit 1" --workspace "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" 2>&1)
# QueryResult skips `nodes` when empty (serde skip_serializing_if), so check
# for `count` which is always present and reflects the number of matched nodes.
echo "$output" | grep -q '"count"' && pass "lain query returns JSON with count" || fail "lain query output: $output"

# ---------- VERIFY NO POLLUTION ----------
echo ""
echo "--- Pollution Check ---"
[ -f "$REAL_HOME/.claude/settings.json" ] && skip "Real ~/.claude/settings.json exists (pre-existing)" || pass "No pollution to real ~/.claude/settings.json"
[ -d "$REAL_HOME/.local/lain" ] && skip "Real ~/.local/lain exists (pre-existing)" || pass "No pollution to real ~/.local/lain"

# ---------- RESULTS ----------
echo ""
echo "==============================================="
echo "  Results: $PASS passed, $FAIL failed"
echo "==============================================="

if [ "$FAIL" -gt 0 ]; then
    echo "SOME TESTS FAILED"
    exit 1
else
    echo "ALL TESTS PASSED"
    exit 0
fi
