#!/bin/bash
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
EXPECTED_BINARIES=(
    "lain-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
    "lain-${VERSION}-aarch64-apple-darwin.tar.gz"
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

declare -A SHA256_HASHES

for binary in "${EXPECTED_BINARIES[@]}"; do
    echo "  Downloading $binary..."
    if curl -fsSL "https://github.com/spuentesp/lain/releases/download/v${VERSION}/${binary}" -o "$TEMP_DIR/$binary"; then
        echo "  Calculating SHA256 for $binary..."
        SHA256=$(shasum -a 256 "$TEMP_DIR/$binary" | cut -d' ' -f1)
        SHA256_HASHES["$binary"]="$SHA256"
        echo "    SHA256: $SHA256"
        pass "Downloaded and verified: $binary"
    else
        fail "Failed to download: $binary"
    fi
done

# Check 4: Extract and verify binaries work
echo ""
echo "Check 4: Extracting and verifying binaries..."

# Check macOS ARM binary
if [ "$(uname -s)" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]; then
    MACOS_BINARY="lain-${VERSION}-aarch64-apple-darwin.tar.gz"
    echo "  Extracting $MACOS_BINARY..."
    cd "$TEMP_DIR"
    tar xzf "$MACOS_BINARY"

    if [ -f "lain" ]; then
        chmod +x lain
        if ./lain --version >/dev/null 2>&1; then
            pass "macOS ARM binary is executable"
        else
            fail "macOS ARM binary is not executable"
        fi
    else
        fail "macOS ARM binary not found in archive"
    fi
    cd - >/dev/null
else
    warn "Skipping binary verification (not on macOS ARM)"
fi

# Check 5: Generate Homebrew formula update instructions
echo ""
echo "Check 5: Generating Homebrew formula update..."
cat <<EOF
==========================================
  Homebrew Formula Update Instructions
==========================================

Update Formula/lain.rb with the following SHA256 hashes:

EOF

for binary in "${EXPECTED_BINARIES[@]}"; do
    SHA256="${SHA256_HASHES[$binary]}"
    case "$binary" in
        *aarch64-apple-darwin*)
            echo "  macOS ARM (aarch64-apple-darwin):"
            echo "    sha256 \"$SHA256\" => :aarch64_darwin"
            ;;
        *x86_64-unknown-linux-gnu*)
            echo "  Linux x86_64 (x86_64-unknown-linux-gnu):"
            echo "    sha256 \"$SHA256\" => :x86_64_linux"
            ;;
        *x86_64-pc-windows-msvc*)
            echo "  Windows x86_64 (x86_64-pc-windows-msvc):"
            echo "    sha256 \"$SHA256\" => :x86_64_windows"
            ;;
    esac
done

echo ""
echo "=========================================="
echo -e "${GREEN}✅ All post-flight checks passed!${NC}"
echo "=========================================="
echo ""
echo "Next steps:"
echo "  1. Update Formula/lain.rb with the SHA256 hashes above"
echo "  2. Test the formula locally:"
echo "     brew install --build-from-source ./Formula/lain.rb"
echo "  3. Commit and push the formula:"
echo "     git add Formula/lain.rb"
echo "     git commit -m \"chore: update formula for v$VERSION\""
echo "     git push origin main"
echo "  4. Test the release:"
echo "     curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash"
echo ""
