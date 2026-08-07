#!/bin/bash
# LAIN installer — requires bash (not sh/dash)
if [ -z "$BASH_VERSION" ]; then
    echo "Error: LAIN installer requires bash. Please run: curl ... | bash" >&2
    exit 1
fi
set -e

REPO="spuentesp/lain"
INSTALL_DIR="${LAIN_INSTALL_DIR:-$HOME/.local/lain}"
BIN_NAME="lain"

# Default configuration
DEFAULT_WORKSPACE="."
DEFAULT_TRANSPORT="stdio"
DEFAULT_PORT="9999"
DEFAULT_AGENT="auto"

# Parsed options
OPT_WORKSPACE=""
OPT_TRANSPORT=""
OPT_PORT=""
OPT_AGENT=""
OPT_EMBEDDING_MODEL=""
OPT_DOWNLOAD_MODEL=""
OPT_YES=""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Parse command-line arguments
parse_args() {
  while [[ $# -gt 0 ]]; do
    case $1 in
      --workspace)
        OPT_WORKSPACE="$2"
        shift 2
        ;;
      --transport)
        OPT_TRANSPORT="$2"
        shift 2
        ;;
      --port)
        OPT_PORT="$2"
        shift 2
        ;;
      --agent)
        OPT_AGENT="$2"
        shift 2
        ;;
      --embedding-model)
        OPT_EMBEDDING_MODEL="$2"
        shift 2
        ;;
      --download-model)
        OPT_DOWNLOAD_MODEL="yes"
        shift
        ;;
      -y|--yes)
        OPT_YES="yes"
        shift
        ;;
      -h|--help)
        echo "Usage: $0 [OPTIONS]"
        echo ""
        echo "Options:"
        echo "  --workspace PATH         Workspace path for LAIN [default: .]"
        echo "  --transport MODE        MCP transport: stdio, http, both [default: stdio]"
        echo "  --port PORT             HTTP port for MCP server [default: 9999]"
        echo "  --agent AGENT           Target agent: auto, claude, gemini, cursor, windsurf, cline, kimi [default: auto]"
        echo "  --embedding-model PATH  Path to ONNX embedding model"
        echo "  --download-model        Download default ONNX model (all-MiniLM-L6-v2.onnx)"
        echo "  -y, --yes               Skip all confirmation prompts"
        echo "  -h, --help              Show this help message"
        echo ""
        echo "Environment Variables:"
        echo "  LAIN_INSTALL_DIR        Installation directory [default: ~/.local/lain]"
        exit 0
        ;;
      *)
        error "Unknown option: $1"
        echo "Use -h or --help for usage information"
        exit 1
        ;;
    esac
  done
}

# Only parse args when executed or piped (not sourced)
# BASH_SOURCE[0] is empty when piped via `curl | bash`
if [[ -z "${BASH_SOURCE[0]}" ]] || [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  parse_args "$@"
fi

detect_platform() {
  local os arch platform
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
    Linux*)
      if [ "$arch" = "aarch64" ] || [ "$arch" = "arm64" ]; then
        platform="aarch64-unknown-linux-gnu"
      else
        platform="x86_64-unknown-linux-gnu"
      fi
      ;;
    Darwin*)
      if [ "$arch" = "arm64" ]; then
        platform="aarch64-apple-darwin"
      else
        echo "unsupported"
        return 1
      fi
      ;;
    MINGW*|MSYS*|CYGWIN*)
      if [ "$arch" = "x86_64" ] || [ "$arch" = "AMD64" ]; then
        platform="x86_64-pc-windows-msvc"
      else
        platform="x86_64-pc-windows-msvc"  # fallback to 64-bit
      fi
      ;;
    *)
      echo "unsupported"
      return 1
      ;;
  esac
  echo "$platform"
}

get_latest_version() {
  local version
  version=$(curl -s https://api.github.com/repos/$REPO/releases/latest | sed -nE 's/.*"tag_name": *"v?([^"]+)",?.*/\1/p')
  if [ -z "$version" ] || [ "$version" = "latest" ]; then
    error "Cannot determine latest version. Check your internet connection or GitHub API rate limits."
    echo ""
    echo "You can install a specific version from:"
    echo "  https://github.com/$REPO/releases"
    exit 1
  fi
  echo "$version"
}

download_onnx_model() {
  local model_dir="$HOME/.local/lain/models"
  local model_file="$model_dir/all-MiniLM-L6-v2.onnx"
  local model_url="https://github.com/sentence-transformers/sentence-transformers/releases/download/v2.2.0/all-MiniLM-L6-v2.onnx"

  if [ -f "$model_file" ]; then
    if [ -n "$OPT_YES" ]; then
      info "Model already exists at $model_file" >&2
      echo "$model_file"
      return 0
    fi
    echo "" >&2
    echo -e "${YELLOW}Model already exists at:${NC} $model_file" >&2
    read -p "Redownload? [y/N] " -n 1 -r reply || reply="n"
    echo "" >&2
    if [[ ! $reply =~ ^[Yy]$ ]]; then
      info "Using existing model." >&2
      echo "$model_file"
      return 0
    fi
    rm -f "$model_file"
  fi

  mkdir -p "$model_dir"

  info "Downloading ONNX embedding model..." >&2
  info "Source: $model_url" >&2
  info "Destination: $model_file" >&2

  if command -v curl >/dev/null 2>&1; then
    if ! curl -fsSL -o "$model_file" "$model_url"; then
      error "Failed to download model. You can download it manually:" >&2
      echo "  $model_url" >&2
      return 1
    fi
  elif command -v wget >/dev/null 2>&1; then
    if ! wget -q -O "$model_file" "$model_url"; then
      error "Failed to download model. You can download it manually:" >&2
      echo "  $model_url" >&2
      return 1
    fi
  else
    error "curl or wget is required to download the model." >&2
    echo "Please download manually: $model_url" >&2
    return 1
  fi

  local model_size=$(du -h "$model_file" | cut -f1)
  info "Model downloaded successfully ($model_size)" >&2
  echo "$model_file"
  return 0
}

check_in_path() {
  if command -v "$BIN_NAME" >/dev/null 2>&1; then
    local existing_path
    existing_path=$(which "$BIN_NAME" 2>/dev/null || true)
    if [ -n "$existing_path" ]; then
      return 0
    fi
  fi
  return 1
}

check_existing_installation() {
  if command -v "$BIN_NAME" >/dev/null 2>&1; then
    local existing_path
    existing_path=$(which "$BIN_NAME" 2>/dev/null || true)
    if [ -n "$existing_path" ]; then
      warn "Found existing $BIN_NAME at $existing_path"
      return 0
    fi
  fi
  return 1
}

check_writeable() {
  if [ -d "$INSTALL_DIR" ]; then
    if [ -w "$INSTALL_DIR" ]; then
      return 0
    fi
  else
    # Check if we can create the directory
    if mkdir -p "$INSTALL_DIR" 2>/dev/null; then
      rmdir "$INSTALL_DIR" 2>/dev/null || true
      return 0
    fi
  fi
  return 1
}

install() {
  local version="$1"
  local platform="$2"
  local tmpdir
  tmpdir=$(mktemp -d)

  info "Installing LAIN v${version} for $platform..."

  local download_url="https://github.com/$REPO/releases/download/v${version}/lain-${version}-${platform}.tar.gz"

  info "Downloading from $download_url..."

  if command -v curl >/dev/null 2>&1; then
    if ! curl -fsSL "$download_url" -o "${tmpdir}/lain.tar.gz"; then
      error "Failed to download. Check your internet connection."
      rm -rf "$tmpdir"
      exit 1
    fi
  elif command -v wget >/dev/null 2>&1; then
    if ! wget -q "$download_url" -O "${tmpdir}/lain.tar.gz"; then
      error "Failed to download. Check your internet connection."
      rm -rf "$tmpdir"
      exit 1
    fi
  else
    error "curl or wget is required to download LAIN."
    rm -rf "$tmpdir"
    exit 1
  fi

  info "Extracting..."
  tar xzf "${tmpdir}/lain.tar.gz" -C "$tmpdir" || {
    error "Failed to extract. The release may be malformed."
    rm -rf "$tmpdir"
    exit 1
  }

  mkdir -p "$INSTALL_DIR"

  if [ -f "${tmpdir}/lain" ]; then
    mv "${tmpdir}/lain" "${INSTALL_DIR}/${BIN_NAME}"
  elif [ -f "${tmpdir}/lain.exe" ]; then
    mv "${tmpdir}/lain.exe" "${INSTALL_DIR}/${BIN_NAME}.exe"
  else
    error "Binary not found in archive."
    ls -la "$tmpdir"
    rm -rf "$tmpdir"
    exit 1
  fi

  chmod +x "${INSTALL_DIR}/${BIN_NAME}" || chmod +x "${INSTALL_DIR}/${BIN_NAME}.exe"
  rm -rf "$tmpdir"

  info "Installed to ${INSTALL_DIR}/${BIN_NAME}"
}

verify_installation() {
  local bin_path="${INSTALL_DIR}/${BIN_NAME}"

  if [ ! -f "$bin_path" ] && [ ! -f "${bin_path}.exe" ]; then
    error "Binary not found at $bin_path"
    return 1
  fi

  # Try to run it
  if "${bin_path}" --version >/dev/null 2>&1; then
    local installed_version
    installed_version=$("${bin_path}" --version 2>&1 | head -1)
    info "Successfully installed: $installed_version"
    return 0
  elif "${bin_path}.exe" --version >/dev/null 2>&1; then
    local installed_version
    installed_version=$("${bin_path}.exe" --version 2>&1 | head -1)
    info "Successfully installed: $installed_version"
    return 0
  else
    warn "Binary installed but --version check failed."
    return 1
  fi
}

main() {
  echo ""
  echo "LAIN Installer"
  echo "=============="
  echo ""

  local platform
  platform=$(detect_platform)

  if [ "$platform" = "unsupported" ]; then
    error "Unsupported platform: $(uname -s)"
    echo ""
    echo "Please compile from source:"
    echo "  cargo install --git https://github.com/spuentesp/lain.git"
    echo ""
    exit 1
  fi

  local version
  version=$(get_latest_version)

  # Check if already installed
  if check_existing_installation; then
    echo ""
    echo -e "${YELLOW}Warning:${NC} $BIN_NAME is already in your PATH."
    read -p "Reinstall anyway? [y/N] " -n 1 -r reply || reply="n"
    echo ""
    if [[ ! $reply =~ ^[Yy]$ ]]; then
      info "Keeping existing installation."
      exit 0
    fi
  fi

  # Check if install dir is writeable
  if ! check_writeable; then
    error "Cannot write to $INSTALL_DIR"
    echo ""
    echo "Options:"
    echo "  1. Set LAIN_INSTALL_DIR to a writable location:"
    echo "       export LAIN_INSTALL_DIR=~/.local/lain"
    echo "  2. Create the directory and try again:"
    echo "       mkdir -p ~/.local/lain"
    echo ""
    exit 1
  fi

  # Interactive configuration (unless --yes is set)
  # Ask BEFORE installing so user can cancel if they don't like the settings
  if [ -z "$OPT_YES" ]; then
    echo ""
    echo "========================================"
    echo -e "${BLUE}Configuration${NC}"
    echo "========================================"
    echo ""

    # Ask for workspace
    if [ -z "$OPT_WORKSPACE" ]; then
      read -p "Workspace path [default: .]: " workspace_input || workspace_input=""
      OPT_WORKSPACE="${workspace_input:-.}"
    fi

    # Ask for transport mode
    if [ -z "$OPT_TRANSPORT" ]; then
      echo ""
      echo "MCP Transport Mode:"
      echo "  1) stdio   - Standard MCP (recommended for Claude Code)"
      echo "  2) http    - HTTP only (for debugging)"
      echo "  3) both    - Both stdio + HTTP (recommended for development)"
      read -p "Choose transport mode [default: stdio]: " transport_input || transport_input=""
      
      case "$transport_input" in
        1|"") OPT_TRANSPORT="stdio" ;;
        2)   OPT_TRANSPORT="http" ;;
        3)   OPT_TRANSPORT="both" ;;
        stdio|http|both) OPT_TRANSPORT="$transport_input" ;;
        *)   warn "Invalid choice, using stdio"; OPT_TRANSPORT="stdio" ;;
      esac
    fi

    # Ask for port if http/both
    if [ "$OPT_TRANSPORT" = "http" ] || [ "$OPT_TRANSPORT" = "both" ]; then
      if [ -z "$OPT_PORT" ]; then
        read -p "HTTP port [default: 9999]: " port_input || port_input=""
        OPT_PORT="${port_input:-9999}"
      fi
    fi

    # Ask for agent
    if [ -z "$OPT_AGENT" ]; then
      echo ""
      echo "Target Agent:"
      echo "  1) auto     - Auto-detect (recommended)"
      echo "  2) claude   - Claude Code"
      echo "  3) cursor   - Cursor AI"
      echo "  4) windsurf - Windsurf Cascade"
      echo "  5) cline    - Cline / Roo Code"
      echo "  6) gemini   - Gemini CLI"
      echo "  7) kimi     - Kimi Code"
      read -p "Choose agent [default: auto]: " agent_input || agent_input=""

      case "$agent_input" in
        1|"") OPT_AGENT="auto" ;;
        2)     OPT_AGENT="claude" ;;
        3)     OPT_AGENT="cursor" ;;
        4)     OPT_AGENT="windsurf" ;;
        5)     OPT_AGENT="cline" ;;
        6)     OPT_AGENT="gemini" ;;
        7)     OPT_AGENT="kimi" ;;
        auto|claude|gemini|cursor|windsurf|cline|kimi) OPT_AGENT="$agent_input" ;;
        *)     warn "Invalid choice, using auto"; OPT_AGENT="auto" ;;
      esac
    fi

    echo ""
    echo "========================================"
    echo "Configuration Summary:"
    echo "  Workspace:     ${OPT_WORKSPACE:-.}"
    echo "  Transport:     ${OPT_TRANSPORT:-stdio}"
    if [ "$OPT_TRANSPORT" = "http" ] || [ "$OPT_TRANSPORT" = "both" ]; then
      echo "  Port:          ${OPT_PORT:-9999}"
    else
      echo "  Port:          N/A (stdio only)"
    fi
    echo "  Agent:         ${OPT_AGENT:-auto}"
    echo "========================================"
    echo ""
    read -p "Continue with installation? [Y/n] " -n 1 -r confirm_reply || confirm_reply="y"
    echo ""
    if [[ ! $confirm_reply =~ ^[Yy]$ ]] && [ -n "$confirm_reply" ]; then
      warn "Installation cancelled. You can re-run with --yes to skip prompts."
      exit 1
    fi
  fi

  # Now install the binary
  echo ""
  echo "========================================"
  echo -e "${BLUE}Installing LAIN v${version} for ${platform}...${NC}"
  echo "========================================"
  
  install "$version" "$platform"

  echo ""
  echo "Post-install:"
  echo ""

  # Check PATH and add if missing
  if check_in_path; then
    echo -e "${GREEN}[OK]${NC} $BIN_NAME is in your PATH"
  else
    local shell_rc=""
    if [ "$(basename "${SHELL:-}")" = "zsh" ] || [ -n "${ZSH_VERSION:-}" ]; then
      shell_rc="$HOME/.zshrc"
    elif [ "$(basename "${SHELL:-}")" = "bash" ] || [ -n "${BASH_VERSION:-}" ]; then
      shell_rc="$HOME/.bashrc"
    fi

    local export_line='export PATH="$HOME/.local/lain:$PATH"'
    local do_add=""

    if [ -n "$OPT_YES" ]; then
      do_add="yes"
    elif [ -n "$shell_rc" ]; then
      echo ""
      echo -e "${YELLOW}[PATH]${NC} lain is not in your PATH."
      read -p "Add to $shell_rc automatically? [Y/n] " -n 1 -r path_reply || path_reply="y"
      echo ""
      if [[ $path_reply =~ ^[Yy]$ ]] || [ -z "$path_reply" ]; then
        do_add="yes"
      fi
    fi

    if [ -n "$do_add" ] && [ -n "$shell_rc" ]; then
      if grep -qF '.local/lain' "$shell_rc" 2>/dev/null; then
        info "PATH entry already in $shell_rc"
      else
        printf '\n# Added by LAIN installer\n%s\n' "$export_line" >> "$shell_rc"
        info "Added to $shell_rc"
      fi
      info "Run: source $shell_rc  (or open a new terminal)"
      # Also export for the current session so lain init can run
      export PATH="$HOME/.local/lain:$PATH"
    else
      echo -e "${YELLOW}[ADD TO PATH]${NC} Add to your shell profile:"
      echo "    $export_line"
      echo ""
    fi
  fi

  # Offer to download ONNX model
  local model_path=""
  if [ -n "$OPT_EMBEDDING_MODEL" ]; then
    model_path="$OPT_EMBEDDING_MODEL"
    info "Using provided model: $model_path"
  elif [ -n "$OPT_DOWNLOAD_MODEL" ]; then
    model_path=$(download_onnx_model)
  elif [ -z "$OPT_YES" ]; then
    echo ""
    echo -e "${BLUE}[OPTIONAL]${NC} Download ONNX embedding model for semantic search?"
    echo "  Model: all-MiniLM-L6-v2.onnx (~120MB)"
    echo "  Required for: semantic_search tool"
    read -p "Download model now? [y/N] " -n 1 -r reply || reply="n"
    echo ""
    if [[ $reply =~ ^[Yy]$ ]]; then
      model_path=$(download_onnx_model)
    fi
  fi

  # Build init command
  local init_cmd="${INSTALL_DIR}/${BIN_NAME} init --agent ${OPT_AGENT:-$DEFAULT_AGENT}"
  if [ -n "$OPT_WORKSPACE" ]; then
    init_cmd="$init_cmd --workspace $OPT_WORKSPACE"
  fi
  if [ -n "$OPT_TRANSPORT" ]; then
    init_cmd="$init_cmd --transport $OPT_TRANSPORT"
  fi
  if [ -n "$OPT_PORT" ]; then
    init_cmd="$init_cmd --port $OPT_PORT"
  fi
  if [ -n "$model_path" ]; then
    init_cmd="$init_cmd --embedding-model $model_path"
  fi
  if [ -n "$OPT_YES" ]; then
    init_cmd="$init_cmd --yes"
  fi

  echo ""
  echo "Configuring LAIN for agent..."
  echo -e "${BLUE}Running:${NC} $init_cmd"
  echo ""

  # Run init command
  if $init_cmd; then
    info "LAIN initialized successfully!"
  else
    warn "LAIN initialization failed. You can run manually:"
    echo "  $init_cmd"
  fi

  # Quick verify
  echo ""
  echo "Verifying installation..."
  if verify_installation; then
    echo ""
    info "Installation complete!"
    echo ""
    echo "Next steps:"
    echo "  1. Open a new terminal (or: source ~/.zshrc)"
    echo "  2. Restart your agent (Claude Code, Cursor, etc.)"
    echo "  3. Try: lain query \"find Function | limit 5\""
    echo ""
    echo "Documentation: https://github.com/spuentesp/lain"
  else
    echo ""
    warn "Installation completed but verification failed."
    echo "Try running: ${INSTALL_DIR}/${BIN_NAME} --version"
  fi
}

# Only run main when executed or piped (not sourced)
# BASH_SOURCE[0] is empty when piped via `curl | bash`
if [[ -z "${BASH_SOURCE[0]}" ]] || [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi