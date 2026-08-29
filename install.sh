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
DEFAULT_AGENT="auto"

# Parsed options
OPT_AGENT=""
OPT_EMBEDDING_MODEL=""
OPT_DOWNLOAD_MODEL=""
OPT_YES=""
# Use default-empty so callers that source install.sh with
# OPT_INTERACTIVE already set (e.g. tests driving the helper) don't get
# clobbered by the source-time re-initialization.
OPT_INTERACTIVE="${OPT_INTERACTIVE:-}"

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
      --workspace|--transport|--port)
        warn "$1 is deprecated and ignored — the shipped MCP entry is 'lain mcp' (zero-config stdio, walks up for .git)"
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
      --interactive)
        OPT_INTERACTIVE="yes"
        shift
        ;;
      -h|--help)
        echo "Usage: $0 [OPTIONS]"
        echo ""
        echo "Options:"
        echo "  --agent AGENT           Target agent: auto, claude, gemini, cursor, windsurf, cline, kimi [default: auto]"
        echo "  --embedding-model PATH  Path to ONNX embedding model (dir with model.onnx + tokenizer.json)"
        echo "  --download-model        Download default ONNX model (all-MiniLM-L6-v2.onnx)"
        echo "  -y, --yes               Skip all confirmation prompts"
        echo "      --interactive       Force prompts even when stdin is not a TTY"
        echo "  -h, --help              Show this help message"
        echo ""
        echo "The MCP entry point is 'lain mcp' (zero-config: walks up for .git,"
        echo "stdio transport, no repos.yaml needed)."
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
  apply_noninteractive_defaults
fi

# Auto-enable non-interactive mode when stdin is not a TTY (e.g. when
# install.sh is run via `curl … | bash` from CI or a container init).
# The user can opt back into interactive prompts with --interactive.
apply_noninteractive_defaults() {
  # Only override if the user hasn't explicitly chosen a mode.
  if [ -n "$OPT_INTERACTIVE" ] || [ -n "$OPT_YES" ]; then
    return 0
  fi
  if [ -t 0 ]; then
    return 0   # stdin is a real terminal; keep existing behavior
  fi
  OPT_YES="yes"
  echo "[install.sh] stdin is not a TTY — enabling --yes mode automatically."
  echo "[install.sh] Pass --interactive to answer prompts (e.g. via heredoc)."
  return 0
}

# Decide whether to append the `export PATH=...` line to the user's
# shell RC. Never silently mutates ~/.bashrc / ~/.zshrc when stdin is
# not a TTY — the actual footgun D-L1 calls out.
prompt_path_mutation() {
  # skip if already in PATH
  if check_in_path; then
    echo -e "${GREEN}[OK]${NC} $BIN_NAME is in your PATH"
    return 0
  fi

  local shell_rc=""
  if [ "$(basename "${SHELL:-}")" = "zsh" ] || [ -n "${ZSH_VERSION:-}" ]; then
    shell_rc="$HOME/.zshrc"
  elif [ "$(basename "${SHELL:-}")" = "bash" ] || [ -n "${BASH_VERSION:-}" ]; then
    shell_rc="$HOME/.bashrc"
  fi

  local export_line="export PATH=\"$INSTALL_DIR:\$PATH\""
  local do_add=""

  if [ -n "$OPT_YES" ] && [ -t 0 ]; then
    # TTY + explicit --yes: user confirmed on a real terminal.
    do_add="yes"
  elif [ -n "$OPT_YES" ] && [ ! -t 0 ]; then
    # Non-interactive install — the footgun we're fixing.
    echo "[PATH] stdin is not a TTY; skipping auto-mutation of $shell_rc."
    echo "[PATH] To add lain to your PATH manually, run:"
    echo "    echo '$export_line' >> \"$shell_rc\""
    echo "[PATH] Then reload: source \"$shell_rc\""
    return 0
  elif [ -n "$shell_rc" ]; then
    # Genuinely interactive: ask, defaulting to Y (preserves old UX).
    echo ""
    echo -e "${YELLOW}[PATH]${NC} lain is not in your PATH."
    read -p "Add to $shell_rc automatically? [Y/n] " -n 1 -r path_reply || path_reply="n"
    echo ""
    if [[ $path_reply =~ ^[Yy]$ ]] || [ -z "$path_reply" ]; then
      do_add="yes"
    fi
  fi

  if [ -n "$do_add" ] && [ -n "$shell_rc" ]; then
    if grep -qF "$INSTALL_DIR" "$shell_rc" 2>/dev/null; then
      info "PATH entry already in $shell_rc"
    else
      printf '\n# Added by LAIN installer\n%s\n' "$export_line" >> "$shell_rc"
      info "Added to $shell_rc"
    fi
    info "Run: source $shell_rc  (or open a new terminal)"
    # Also export for the current session so the agent registration
    # step below can invoke the freshly installed binary.
    export PATH="$INSTALL_DIR:$PATH"
  elif [ -n "$shell_rc" ]; then
    # User said "n" at the prompt — print the manual line and move on.
    echo -e "${YELLOW}[ADD TO PATH]${NC} Add to your shell profile:"
    echo "    $export_line"
    echo ""
  fi
  return 0
}

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
  local tokenizer_file="$model_dir/tokenizer.json"
  # Hugging Face is the live source; the old sentence-transformers
  # GitHub-release URL is dead (404). The embedder refuses to load
  # without BOTH model + tokenizer.
  local model_url="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx"
  local tokenizer_url="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json"

  if [ -f "$model_file" ] && [ -f "$tokenizer_file" ]; then
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
    rm -f "$model_file" "$tokenizer_file"
  fi

  mkdir -p "$model_dir"

  info "Downloading ONNX embedding model + tokenizer..." >&2
  info "Sources: $model_url" >&2
  info "         $tokenizer_url" >&2
  info "Destination: $model_dir" >&2

  local dl_ok=0
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$model_file" "$model_url" && \
    curl -fsSL -o "$tokenizer_file" "$tokenizer_url" && dl_ok=1
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$model_file" "$model_url" && \
    wget -q -O "$tokenizer_file" "$tokenizer_url" && dl_ok=1
  else
    error "curl or wget is required to download the model." >&2
  fi

  if [ "$dl_ok" != "1" ]; then
    # Don't leave a half-downloaded model behind.
    rm -f "$model_file" "$tokenizer_file"
    error "Failed to download. You can download both files manually:" >&2
    echo "  $model_url" >&2
    echo "  $tokenizer_url" >&2
    return 1
  fi

  local model_size=$(du -h "$model_file" | cut -f1)
  info "Model downloaded successfully ($model_size)" >&2
  echo "$model_file"
  return 0
}

check_in_path() {
  # True when the directory we installed into is on PATH — checking
  # `command -v lain` instead would credit someone else's install.
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return 0 ;;
    *) return 1 ;;
  esac
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

  # Check if already installed — in OUR install dir, not anywhere on
  # PATH (a package-manager lain elsewhere shouldn't block us). With
  # --yes we reinstall without prompting; otherwise ask, defaulting to
  # "keep" when stdin isn't a TTY (curl | bash).
  if [ -f "${INSTALL_DIR}/${BIN_NAME}" ] || [ -f "${INSTALL_DIR}/${BIN_NAME}.exe" ]; then
    echo ""
    echo -e "${YELLOW}Warning:${NC} $BIN_NAME is already installed at ${INSTALL_DIR}."
    local reply="n"
    if [ -n "$OPT_YES" ]; then
      reply="y"
    else
      read -p "Reinstall anyway? [y/N] " -n 1 -r reply || reply="n"
      echo ""
    fi
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
    echo "  Agent:         ${OPT_AGENT:-auto}"
    echo "  MCP entry:     lain mcp  (zero-config: walks up for .git; no"
    echo "                 repos.yaml, transport, or port needed)"
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

  # Check PATH and add if missing. Logic lives in prompt_path_mutation
  # so non-TTY installs don't silently mutate ~/.bashrc / ~/.zshrc.
  prompt_path_mutation

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

  # Register the MCP server with the chosen agent. The shipped entry
  # point is `lain mcp` — zero-config: it walks up for `.git`, needs
  # no repos.yaml, and runs on stdio. (`lain init` was removed in the
  # CLI consolidation; registration is done here directly.)
  local lain_bin="${INSTALL_DIR}/${BIN_NAME}"
  local lain_args="mcp"
  if [ -n "$model_path" ]; then
    lain_args="mcp --embedding-model $model_path"
  fi
  local mcp_json
  if [ -n "$model_path" ]; then
    mcp_json=$(printf '{"mcpServers":{"lain":{"command":"%s","args":["mcp","--embedding-model","%s"]}}}' "$lain_bin" "$model_path")
  else
    mcp_json=$(printf '{"mcpServers":{"lain":{"command":"%s","args":["mcp"]}}}' "$lain_bin")
  fi

  echo ""
  echo "Configuring LAIN for agent..."
  echo ""

  local agent="${OPT_AGENT:-$DEFAULT_AGENT}"
  # Auto-detect: prefer a Claude Code CLI when present.
  if [ "$agent" = "auto" ]; then
    if command -v claude >/dev/null 2>&1; then
      agent="claude"
    fi
  fi

  case "$agent" in
    claude)
      if command -v claude >/dev/null 2>&1; then
        # shellcheck disable=SC2086
        if claude mcp add --scope user lain -- "$lain_bin" $lain_args; then
          info "Registered 'lain' MCP server with Claude Code (user scope)"
        else
          warn "claude mcp add failed; add this manually to your MCP config:"
          echo "  $mcp_json"
        fi
      else
        warn "claude CLI not found; add this to your agent's MCP config:"
        echo "  $mcp_json"
      fi
      ;;
    *)
      echo "Add lain to your agent's MCP config:"
      echo "  $mcp_json"
      echo ""
      echo "Per-agent setup guides: https://github.com/spuentesp/lain/tree/main/hooks"
      ;;
  esac

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