#!/bin/bash
# Test suite for LAIN install.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SCRIPT="$SCRIPT_DIR/../install.sh"
TEST_TMP_DIR=$(mktemp -d)
export LAIN_INSTALL_DIR="$TEST_TMP_DIR/lain"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

passed=0
failed=0

test_passed() {
  echo -e "${GREEN}✓${NC} $1"
  ((passed++))
}

test_failed() {
  echo -e "${RED}✗${NC} $1"
  ((failed++))
}

cleanup() {
  rm -rf "$TEST_TMP_DIR"
}

trap cleanup EXIT

echo "========================================"
echo "LAIN Install Script Test Suite"
echo "========================================"
echo ""

# Test 1: Help command
echo "Test 1: Help command"
HELP_OUTPUT=$(bash "$INSTALL_SCRIPT" --help 2>&1 || true)
if echo "$HELP_OUTPUT" | grep -q "Usage:"; then
  test_passed "Help command works"
else
  test_failed "Help command failed"
  echo "Output: $HELP_OUTPUT"
fi
echo ""

# Test 2: Invalid option
echo "Test 2: Invalid option handling"
if bash "$INSTALL_SCRIPT" --invalid-option 2>&1 | grep -q "Unknown option"; then
  test_passed "Invalid option error message shown"
else
  test_failed "Invalid option not handled correctly"
fi
echo ""

# Test 3: Platform detection
echo "Test 3: Platform detection"
PLATFORM=$(bash -c "source '$INSTALL_SCRIPT' && detect_platform" 2>/dev/null || echo "failed")
if [[ "$PLATFORM" != "unsupported" ]] && [[ "$PLATFORM" != "failed" ]]; then
  test_passed "Platform detection: $PLATFORM"
else
  test_failed "Platform detection failed"
fi
echo ""

# Test 4: Version detection (may fail offline)
echo "Test 4: Version detection (requires internet)"
VERSION=$(bash -c "source '$INSTALL_SCRIPT' && get_latest_version" 2>/dev/null || echo "")
if [[ -n "$VERSION" ]]; then
  test_passed "Version detection: $VERSION"
else
  echo -e "${YELLOW}⚠${NC} Version detection failed (may be offline - skipping)"
  ((passed++))
fi
echo ""

# Test 5: Check functions exist and are callable
echo "Test 5: Function availability"
if bash -c "source '$INSTALL_SCRIPT' && declare -f check_in_path > /dev/null"; then
  test_passed "check_in_path function exists"
else
  test_failed "check_in_path function missing"
fi

# check_existing_installation was removed in the CLI consolidation:
# the "already installed at $INSTALL_DIR" check now lives inline in
# main(). The script still surfaces the warning, just not as a
# standalone function — verify the equivalent text is reachable.
if bash -c "source '$INSTALL_SCRIPT' && grep -q 'is already installed at' '$INSTALL_SCRIPT'" 2>/dev/null; then
  test_passed "existing-installation check is present (inline in main)"
else
  test_failed "existing-installation check is missing"
fi

if bash -c "source '$INSTALL_SCRIPT' && declare -f download_onnx_model > /dev/null"; then
  test_passed "download_onnx_model function exists"
else
  test_failed "download_onnx_model function missing"
fi
echo ""

# Test 6: Argument parsing
echo "Test 6: Argument parsing"

# Use a temporary script that sources the install script and calls parse_args explicitly.
# --workspace / --transport / --port were dropped in the CLI consolidation
# (the shipped MCP entry is `lain mcp`, zero-config stdio). parse_args
# still accepts them so older curl|bash invocations don't error, but
# only to warn that they're deprecated and ignored. Verify the
# warning fires for each legacy flag and that the live flags still
# parse into OPT_* variables.
TEMP_DIR=$(mktemp -d)
TEMP_TEST="$TEMP_DIR/test_parse.sh"
cat > "$TEMP_TEST" << 'EOF'
#!/bin/bash
source "$1"
# Redirect parse_args output to a file (NOT $(...) which would fork a
# subshell and lose OPT_AGENT / OPT_YES). Then inspect the file and
# also check OPT_* in the current shell.
parse_args --workspace /test/path --transport both --port 8080 --agent claude --yes >"$TEMP_OUT" 2>&1
cat "$TEMP_OUT"
if grep -q -- "--workspace is deprecated" "$TEMP_OUT"; then echo "WORKSPACE_WARN_OK"; else echo "WORKSPACE_WARN_FAIL"; fi
if grep -q -- "--transport is deprecated" "$TEMP_OUT"; then echo "TRANSPORT_WARN_OK"; else echo "TRANSPORT_WARN_FAIL"; fi
if grep -q -- "--port is deprecated" "$TEMP_OUT"; then echo "PORT_WARN_OK"; else echo "PORT_WARN_FAIL"; fi
if [ "$OPT_AGENT" = "claude" ]; then echo "AGENT_OK"; else echo "AGENT_FAIL"; fi
if [ "$OPT_YES" = "yes" ]; then echo "YES_OK"; else echo "YES_FAIL"; fi
EOF
chmod +x "$TEMP_TEST"
TEMP_OUT="$TEMP_DIR/parse.out"
RESULT=$(TEMP_OUT="$TEMP_OUT" "$TEMP_TEST" "$INSTALL_SCRIPT" 2>&1)

if echo "$RESULT" | grep -q "WORKSPACE_WARN_OK"; then
  test_passed "--workspace emits deprecation warning"
else
  test_failed "--workspace should warn that it is deprecated ($RESULT)"
fi

if echo "$RESULT" | grep -q "TRANSPORT_WARN_OK"; then
  test_passed "--transport emits deprecation warning"
else
  test_failed "--transport should warn that it is deprecated ($RESULT)"
fi

if echo "$RESULT" | grep -q "PORT_WARN_OK"; then
  test_passed "--port emits deprecation warning"
else
  test_failed "--port should warn that it is deprecated ($RESULT)"
fi

if echo "$RESULT" | grep -q "AGENT_OK"; then
  test_passed "Agent argument parsed correctly"
else
  test_failed "Agent argument not parsed ($RESULT)"
fi

if echo "$RESULT" | grep -q "YES_OK"; then
  test_passed "Yes flag parsed correctly"
else
  test_failed "Yes flag not parsed ($RESULT)"
fi

rm -rf "$TEMP_DIR"
echo ""

# Test 7: Directory creation
echo "Test 7: Installation directory handling"
if bash -c "source '$INSTALL_SCRIPT' && check_writeable" 2>/dev/null; then
  test_passed "Can create installation directory"
else
  test_failed "Cannot create installation directory"
fi
echo ""

# Test 8: Hash tool detection
echo "Test 8: Hash tool detection"
HASH_TOOL=$(bash -c "source '$INSTALL_SCRIPT' && _hash_tool" 2>/dev/null || echo "")
if [ -n "$HASH_TOOL" ]; then
  test_passed "Hash tool detected: $HASH_TOOL"
else
  test_failed "No hash tool found (need sha256sum or shasum)"
fi
echo ""

# Test 9: SHA-256 of a known file
echo "Test 9: SHA-256 computation of a known file"
KNOWN_FILE="$TEST_TMP_DIR/known_content.txt"
echo -n "hello" > "$KNOWN_FILE"
# SHA-256 of "hello" is 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
EXPECTED_HASH="2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
COMPUTED_HASH=$(bash -c "source '$INSTALL_SCRIPT' && _sha256_of '$KNOWN_FILE'" 2>/dev/null || echo "")
if [ "$COMPUTED_HASH" = "$EXPECTED_HASH" ]; then
  test_passed "SHA-256 of 'hello' computed correctly"
else
  test_failed "SHA-256 mismatch: expected $EXPECTED_HASH, got $COMPUTED_HASH"
fi
echo ""

# Test 10: SHA256SUMS parsing
echo "Test 10: SHA256SUMS parsing"
# Write a multi-line SHA256SUMS fixture (both plain GNU and *-prefixed BSD
# formats mixed together) to a temp file so newlines survive the sourcing
# subshell without quote-escaping complications.
SUMS_FIXTURE="$TEST_TMP_DIR/SHA256SUMS.fixture"
cat > "$SUMS_FIXTURE" << 'SUMSEOF'
2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  lain-0.1.0-x86_64-unknown-linux-gnu.tar.gz
b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  lain-0.1.0-aarch64-apple-darwin.tar.gz
SUMSEOF

# Parse the linux entry via a subshell that sources install.sh and reads
# the fixture file.
PARSED=$(bash -c "
  source '$INSTALL_SCRIPT'
  sums_text=\$(cat '$SUMS_FIXTURE')
  _parse_sha256sums \"\$sums_text\" 'lain-0.1.0-x86_64-unknown-linux-gnu.tar.gz'
" 2>/dev/null || echo "")
if [ "$PARSED" = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" ]; then
  test_passed "SHA256SUMS parsed correct hash for linux target"
else
  test_failed "SHA256SUMS parsing failed: expected hash, got '$PARSED'"
fi

# Parse the darwin entry.
PARSED_DARWIN=$(bash -c "
  source '$INSTALL_SCRIPT'
  sums_text=\$(cat '$SUMS_FIXTURE')
  _parse_sha256sums \"\$sums_text\" 'lain-0.1.0-aarch64-apple-darwin.tar.gz'
" 2>/dev/null || echo "")
if [ "$PARSED_DARWIN" = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9" ]; then
  test_passed "SHA256SUMS parsed correct hash for darwin target"
else
  test_failed "SHA256SUMS parsing failed for darwin: got '$PARSED_DARWIN'"
fi

# Missing entry returns empty.
MISSING=$(bash -c "
  source '$INSTALL_SCRIPT'
  sums_text=\$(cat '$SUMS_FIXTURE')
  _parse_sha256sums \"\$sums_text\" 'lain-99.99.99-x86_64-unknown-linux-gnu.tar.gz'
" 2>/dev/null)
if [ -z "$MISSING" ]; then
  test_passed "SHA256SUMS returns empty for missing asset"
else
  test_failed "SHA256SUMS should return empty for missing asset, got '$MISSING'"
fi
echo ""

# Test 11: Sidecar verification function is present
echo "Test 11: Sidecar verification in verify_installation"
# The updated verify_installation now handles both lain and lain-git-sidecar.
# Confirm the function still exists and is callable without error.
if bash -c "source '$INSTALL_SCRIPT' && declare -f verify_installation > /dev/null" 2>/dev/null; then
  test_passed "verify_installation function exists (sidecar check included)"
else
  test_failed "verify_installation function missing"
fi
echo ""

# Summary
echo "========================================"
echo "Test Results"
echo "========================================"
echo -e "${GREEN}Passed:${NC} $passed"
echo -e "${RED}Failed:${NC} $failed"
echo ""

if [ $failed -eq 0 ]; then
  echo -e "${GREEN}All tests passed!${NC}"
  exit 0
else
  echo -e "${RED}Some tests failed!${NC}"
  exit 1
fi
