#!/bin/bash
set -e

REPO="spuentesp/lain"
VERSION=$(curl -s https://api.github.com/repos/$REPO/releases/latest | grep '"tag_name"' | sed 's/.*"v\?\([^"]*\)".*/\1/')

if [ -z "$VERSION" ]; then
  echo "Could not detect latest version. Please compile from source:"
  echo "  cargo install --git https://github.com/$REPO.git"
  exit 1
fi

echo "Installing LAIN-mcp v${VERSION}..."

detect_platform() {
  case "$(uname -s)" in
    Linux*)
      if [ "$(uname -m)" = "aarch64" ]; then
        echo "aarch64-unknown-linux-gnu"
      else
        echo "x86_64-unknown-linux-gnu"
      fi
      ;;
    Darwin*)
      if [ "$(uname -m)" = "arm64" ]; then
        echo "aarch64-apple-darwin"
      else
        echo "unsupported"  # no x86_64-apple-darwin asset is published
      fi
      ;;
    MINGW*|MSYS*|CYGWIN*) echo "x86_64-pc-windows-msvc";;
    *) echo "unsupported";;
  esac
}

PLATFORM=$(detect_platform)

if [ "$PLATFORM" = "unsupported" ]; then
  echo "Unsupported platform. Please compile from source:"
  echo "  cargo install --git https://github.com/$REPO.git"
  exit 1
fi

ARCHIVE="lain-${VERSION}-${PLATFORM}.tar.gz"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading ${ARCHIVE}..."
# -f: fail on HTTP errors instead of saving the 404 page as the binary
# (the bug that installed an HTML error page as ~/.local/bin/lain).
if ! curl -fsSL "https://github.com/${REPO}/releases/download/v${VERSION}/${ARCHIVE}" -o "${TMPDIR}/lain.tar.gz"; then
  echo "Download failed (no asset for ${PLATFORM}?). Compile from source:"
  echo "  cargo install --git https://github.com/$REPO.git"
  exit 1
fi

tar xzf "${TMPDIR}/lain.tar.gz" -C "$TMPDIR"

BIN_DIR="${HOME}/.local/bin"
mkdir -p "$BIN_DIR"
if [ -f "${TMPDIR}/lain.exe" ]; then
  mv "${TMPDIR}/lain.exe" "${BIN_DIR}/lain.exe"
else
  mv "${TMPDIR}/lain" "${BIN_DIR}/lain"
  chmod +x "${BIN_DIR}/lain"
fi

echo ""
echo "Installed to ${BIN_DIR}/lain"
echo ""
echo "Add to your MCP config (e.g. ~/.claude.json or agent settings):"
echo ""
echo '{
  "mcpServers": {
    "lain": {
      "command": "'"${BIN_DIR}/lain"'",
      "args": ["mcp"]
    }
  }
}'
echo ""
echo "Or compile from source:"
echo "  cargo install --git https://github.com/$REPO.git"
