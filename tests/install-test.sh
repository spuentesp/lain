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

if bash -c "source '$INSTALL_SCRIPT' && declare -f check_existing_installation > /dev/null"; then
  test_passed "check_existing_installation function exists"
else
  test_failed "check_existing_installation function missing"
fi

if bash -c "source '$INSTALL_SCRIPT' && declare -f download_onnx_model > /dev/null"; then
  test_passed "download_onnx_model function exists"
else
  test_failed "download_onnx_model function missing"
fi
echo ""

# Test 6: Argument parsing
echo "Test 6: Argument parsing"

# Use a temporary script that sources the install script and calls parse_args explicitly
TEMP_DIR=$(mktemp -d)
TEMP_TEST="$TEMP_DIR/test_parse.sh"
cat > "$TEMP_TEST" << 'EOF'
#!/bin/bash
source "$1"
parse_args --workspace /test/path --transport both --port 8080 --agent claude --yes
if [ "$OPT_WORKSPACE" = "/test/path" ]; then echo "WORKSPACE_OK"; else echo "WORKSPACE_FAIL"; fi
if [ "$OPT_TRANSPORT" = "both" ]; then echo "TRANSPORT_OK"; else echo "TRANSPORT_FAIL"; fi
if [ "$OPT_PORT" = "8080" ]; then echo "PORT_OK"; else echo "PORT_FAIL"; fi
if [ "$OPT_AGENT" = "claude" ]; then echo "AGENT_OK"; else echo "AGENT_FAIL"; fi
if [ "$OPT_YES" = "yes" ]; then echo "YES_OK"; else echo "YES_FAIL"; fi
EOF
chmod +x "$TEMP_TEST"
RESULT=$("$TEMP_TEST" "$INSTALL_SCRIPT" 2>&1)

if echo "$RESULT" | grep -q "WORKSPACE_OK"; then
  test_passed "Workspace argument parsed correctly"
else
  test_failed "Workspace argument not parsed ($RESULT)"
fi

if echo "$RESULT" | grep -q "TRANSPORT_OK"; then
  test_passed "Transport argument parsed correctly"
else
  test_failed "Transport argument not parsed ($RESULT)"
fi

if echo "$RESULT" | grep -q "PORT_OK"; then
  test_passed "Port argument parsed correctly"
else
  test_failed "Port argument not parsed ($RESULT)"
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
