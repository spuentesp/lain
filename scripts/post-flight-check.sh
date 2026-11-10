#!/usr/bin/env bash
# Post-flight checks after CI completes
# This script MUST pass before updating Homebrew formula

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

# The Formula fill writes into the repo, so resolve paths from the
# script's own location rather than the caller's working directory.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FILL_SCRIPT="$REPO_ROOT/scripts/fill-formula-sha256.py"

echo "=========================================="
echo "  LAIN Post-Flight Release Checks"
echo "=========================================="
echo ""

# Get version from arguments
VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    fail "Usage: $0 <version> (e.g., $0 0.2.0)"
fi

# Check 1: Release exists
echo "Check 1: Checking if release exists..."
if gh release view "v$VERSION" >/dev/null 2>&1; then
    pass "Release v$VERSION exists"
else
    fail "Release v$VERSION does not exist. Wait for CI to complete."
fi

# Check 2: All expected binaries are present
echo "Check 2: Checking expected binaries..."
# Only the targets release.yml actually builds. The macOS ARM64 target
# was dropped in v0.14.0 (an unresolvable ONNX Runtime linker error)
# and has not returned, so expecting it here would fail every release.
EXPECTED_BINARIES=(
    "lain-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
    "lain-${VERSION}-x86_64-pc-windows-msvc.tar.gz"
)

RELEASE_ASSETS=$(gh release view "v$VERSION" --json assets --jq '.assets[] | .name')

ALL_BINARIES_PRESENT=true
for binary in "${EXPECTED_BINARIES[@]}"; do
    if echo "$RELEASE_ASSETS" | grep -q "$binary"; then
        pass "Binary found: $binary"
    else
        fail "Binary missing: $binary"
        ALL_BINARIES_PRESENT=false
    fi
done

if [ "$ALL_BINARIES_PRESENT" = false ]; then
    fail "Not all expected binaries are present in the release"
fi

# Check 3: Download and verify each binary
echo ""
echo "Check 3: Downloading and verifying binaries..."
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

SHA256_FILE=$(mktemp)
trap "rm -rf $TEMP_DIR $SHA256_FILE" EXIT

for binary in "${EXPECTED_BINARIES[@]}"; do
    echo "  Downloading $binary..."
    if curl -fsSL "https://github.com/spuentesp/lain/releases/download/v${VERSION}/${binary}" -o "$TEMP_DIR/$binary"; then
        echo "  Calculating SHA256 for $binary..."
        SHA256=$(shasum -a 256 "$TEMP_DIR/$binary" | cut -d' ' -f1)
        echo "$binary|$SHA256" >> "$SHA256_FILE"
        echo "    SHA256: $SHA256"
        pass "Downloaded and verified: $binary"
    else
        fail "Failed to download: $binary"
    fi
done

# Check 4: Extract and verify binaries work
echo ""
echo "Check 4: Extracting and verifying binaries..."

# The release publishes Linux and Windows binaries only. Extract and
# run the one this machine can execute, if any.
VERIFY_BINARY=""
case "$(uname -s)" in
    Linux*)  VERIFY_BINARY="lain-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" ;;
    Darwin*) VERIFY_BINARY="" ;;  # no macOS build since v0.14.0
    MINGW*|MSYS*|CYGWIN*) VERIFY_BINARY="lain-${VERSION}-x86_64-pc-windows-msvc.tar.gz" ;;
esac

if [ -n "$VERIFY_BINARY" ]; then
    echo "  Extracting $VERIFY_BINARY..."
    ( cd "$TEMP_DIR" && tar xzf "$VERIFY_BINARY" )

    if [ -f "$TEMP_DIR/lain" ]; then
        chmod +x "$TEMP_DIR/lain"
        if "$TEMP_DIR/lain" --version >/dev/null 2>&1; then
            pass "$VERIFY_BINARY is executable"
        else
            fail "$VERIFY_BINARY is not executable"
        fi
    else
        fail "lain binary not found in $VERIFY_BINARY"
    fi
else
    warn "Skipping binary verification (no native build for this platform)"
fi

# Check 5: Fill and verify the Homebrew formula checksums
echo ""
echo "Check 5: Filling Formula/lain.rb checksums..."
if python3 "$FILL_SCRIPT" --tag "v$VERSION"; then
    pass "Formula/lain.rb sha256 values filled from the published SHA256SUMS"
else
    fail "Could not fill Formula/lain.rb sha256 values"
fi

if python3 "$FILL_SCRIPT" --tag "v$VERSION" --check; then
    pass "Formula/lain.rb sha256 values match the published artifacts"
else
    fail "Formula/lain.rb sha256 values do not match the published artifacts"
fi

echo ""
echo "=========================================="
echo -e "${GREEN}✅ All post-flight checks passed!${NC}"
echo "=========================================="
echo ""
echo "Next steps:"
echo "  1. Bump the health-badge default to the new release:"
echo "     .github/actions/lain-health-badge/action.yml  (lain-version: 'v$VERSION')"
echo "  2. Test the formula locally:"
echo "     brew install --build-from-source ./Formula/lain.rb"
echo "  3. Commit and push the formula:"
echo "     git add Formula/lain.rb .github/actions/lain-health-badge/action.yml"
echo "     git commit -m \"chore: post-release v$VERSION — formula checksums and badge default\""
echo "     git push origin main"
echo "  4. Test the release:"
echo "     curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash"
echo ""
