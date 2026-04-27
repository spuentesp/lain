#!/bin/bash
set -e

REPO="spuentesp/lain"
VERSION=$(curl -s https://api.github.com/repos/$REPO/releases/latest | grep '"tag_name"' | sed 's/.*"v\?\([^"]*\)".*/\1/')

if [ -z "$VERSION" ]; then
  echo "Could not detect latest version. Using cargo install method."
  VERSION="latest"
fi

echo "Installing LAIN-mcp v${VERSION}..."

detect_platform() {
  case "$(uname -s)" in
    Linux*)  echo "x86_64-unknown-linux-gnu";;
    Darwin*)
      if [ "$(uname -m)" = "arm64" ]; then
        echo "aarch64-apple-darwin"
      else
        echo "x86_64-apple-darwin"
      fi
      ;;
    MINGW*|MSYS*|CYGWIN*) echo "x86_64-pc-windows-msvc.exe";;
    *) echo "unsupported";;
  esac
}

PLATFORM=$(detect_platform)
BINARY="lain-${VERSION}-${PLATFORM}"
TMPDIR=$(mktemp -d)

if [ "$PLATFORM" = "unsupported" ]; then
  echo "Unsupported platform. Please compile from source:"
  echo "  cargo install --git https://github.com/spuentesp/lain.git"
  exit 1
fi

echo "Downloading ${BINARY}..."
curl -L "https://github.com/${REPO}/releases/download/v${VERSION}/${BINARY}" -o "${TMPDIR}/lain"

if [ "$PLATFORM" = "x86_64-pc-windows-msvc.exe" ]; then
  mv "${TMPDIR}/lain" "${TMPDIR}/lain.exe"
else
  chmod +x "${TMPDIR}/lain"
fi

BIN_DIR="${HOME}/.local/bin"
mkdir -p "$BIN_DIR"
mv "${TMPDIR}/lain" "${BIN_DIR}/lain"
rm -rf "$TMPDIR"

echo ""
echo "Installed to ${BIN_DIR}/lain"
echo ""
echo "Add to your MCP config (e.g. ~/.claude/settings.json):"
echo ""
echo '{
  "mcpServers": {
    "lain": {
      "command": "'"${BIN_DIR}/lain"'",
      "args": ["--workspace", ".", "--transport", "stdio"]
    }
  }
}'
echo ""
echo "Or compile from source:"
echo "  cargo install --git https://github.com/spuentesp/lain.git"