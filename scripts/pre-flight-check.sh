#!/bin/bash
# Pre-flight checks before creating a release
# This script MUST pass before creating a tag

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    exit 1
}

pass() {
    echo -e "${GREEN}[PASS]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

echo "=========================================="
echo "  LAIN Pre-Flight Release Checks"
echo "=========================================="
echo ""

# Check 1: All tests pass
echo "Check 1: Running all tests..."
if cargo test 2>&1 | grep -q "test result: ok"; then
    pass "All tests pass"
else
    fail "Tests failed"
fi

# Check 2: No compilation warnings
echo "Check 2: Checking for compilation warnings..."
if cargo build --release 2>&1 | grep -i warning > /dev/null 2>&1; then
    fail "Compilation warnings found. Run: cargo build --release 2>&1 | grep -i warning"
else
    pass "No compilation warnings"
fi

# Check 3: Version consistency
echo "Check 3: Checking version consistency..."
VERSION=$(grep '^version = "' Cargo.toml | head -1 | sed 's/version = "\([^"]*\)"/\1/')
if [ -z "$VERSION" ]; then
    fail "Could not extract version from Cargo.toml"
fi

# Check Cargo.toml
if grep -q "version = \"$VERSION\"" Cargo.toml; then
    pass "Cargo.toml version: $VERSION"
else
    fail "Cargo.toml version mismatch"
fi

# Check README.md
if grep -q "v$VERSION" README.md || ! grep -q "v[0-9]" README.md; then
    pass "README.md version: $VERSION"
else
    fail "README.md version mismatch"
fi

# Check server.json
if grep -q "\"version\": \"$VERSION\"" server.json; then
    pass "server.json version: $VERSION"
else
    fail "server.json version mismatch"
fi

# Check 4: Binary builds locally
echo "Check 4: Building binary locally..."
if cargo build --release 2>&1 | grep -q "Finished \`release\`"; then
    pass "Binary builds successfully"
else
    fail "Binary build failed"
fi

# Check 5: Binary is executable
echo "Check 5: Checking binary is executable..."
if [ -f "target/release/lain" ]; then
    if target/release/lain --version >/dev/null 2>&1; then
        pass "Binary is executable"
    else
        fail "Binary exists but is not executable"
    fi
else
    fail "Binary not found at target/release/lain"
fi

# Check 6: Git status is clean
echo "Check 6: Checking git status..."
if [ -n "$(git status --porcelain)" ]; then
    fail "Git working directory is not clean. Commit or stash changes."
else
    pass "Git working directory is clean"
fi

# Check 7: No uncommitted changes
echo "Check 7: Checking for uncommitted changes..."
if [ -z "$(git diff --name-only)" ] && [ -z "$(git diff --cached --name-only)" ]; then
    pass "No uncommitted changes"
else
    fail "There are uncommitted changes"
fi

# Check 8: No existing tag for this version
echo "Check 8: Checking for existing tag..."
if git tag -l "v$VERSION" | grep -q "v$VERSION"; then
    fail "Tag v$VERSION already exists. Delete it first: git tag -d v$VERSION && git push origin :refs/tags/v$VERSION"
else
    pass "No existing tag for v$VERSION"
fi

# Check 9: Release workflow exists and is valid
echo "Check 9: Checking release workflow..."
if [ -f ".github/workflows/release.yml" ]; then
    if yamllint .github/workflows/release.yml >/dev/null 2>&1 || grep -q "name:" .github/workflows/release.yml; then
        pass "Release workflow exists and is valid"
    else
        fail "Release workflow is invalid"
    fi
else
    fail "Release workflow not found"
fi

# Check 10: Awareness docs exist
echo "Check 10: Checking awareness docs..."
AWARENESS_DOCS=(
    "hooks/claude/lain-awareness.md"
    "hooks/cursor/lain-awareness.md"
    "hooks/windsurf/lain-rules.md"
    "hooks/cline/lain-rules.md"
)

ALL_DOCS_EXIST=true
for doc in "${AWARENESS_DOCS[@]}"; do
    if [ ! -f "$doc" ]; then
        warn "Missing awareness doc: $doc"
        ALL_DOCS_EXIST=false
    fi
done

if [ "$ALL_DOCS_EXIST" = true ]; then
    pass "All awareness docs exist"
else
    fail "Some awareness docs are missing"
fi

# Check 11: Install script exists and is bash-compatible
echo "Check 11: Checking install script..."
if [ -f "install.sh" ]; then
    if bash -n install.sh >/dev/null 2>&1; then
        pass "Install script exists and is bash-compatible"
    else
        fail "Install script has syntax errors"
    fi
else
    fail "Install script not found"
fi

# Check 12: Formula exists and is valid
echo "Check 12: Checking Homebrew formula..."
if [ -f "Formula/lain.rb" ]; then
    if grep -q "class Lain" Formula/lain.rb; then
        pass "Homebrew formula exists and is valid"
    else
        fail "Homebrew formula is invalid"
    fi
else
    fail "Homebrew formula not found"
fi

echo ""
echo "=========================================="
echo -e "${GREEN}✅ All pre-flight checks passed!${NC}"
echo "=========================================="
echo ""
echo "You can now create the release:"
echo "  git tag -a v$VERSION -m \"Release v$VERSION\""
echo "  git push origin v$VERSION"
echo ""
echo "After the CI completes, verify:"
echo "  gh release view v$VERSION --json assets"
echo "  # Download and verify all 3 binaries"
echo ""
echo "Then update the Homebrew formula with SHA256 hashes."
