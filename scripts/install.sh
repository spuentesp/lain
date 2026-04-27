#!/bin/bash
# Lain installation script — sets up user config directory with defaults
#
# This creates ~/.lain/ with:
#   • tuning.toml      — default tuning config (can be overridden per-workspace)
#   • toolchains/     — user toolchain overrides (merge with built-ins)
#   • models/         — ONNX embedding model (optional, for semantic search)
#
# Usage:
#   ./scripts/install.sh          # Interactive (with prompts)
#   ./scripts/install.sh --fast  # Non-interactive, skip model download
#   ./scripts/install.sh --help  # Show help

set -e

LAIN_DIR="${LAIN_DIR:-$HOME/.lain}"
MODELS_DIR="$LAIN_DIR/models"
TOOLCHAINS_DIR="$LAIN_DIR/toolchains"
TUNING_FILE="$LAIN_DIR/tuning.toml"
SKIP_MODEL=false

# ── Parse args ─────────────────────────────────────────────────────────────────

print_help() {
    cat <<EOF
Lain installation script

Usage: ./scripts/install.sh [options]

Options:
  --fast      Skip ONNX model download (run semantic search in stub mode)
  --dir PATH  Set Lain config directory (default: ~/.lain)
  --help      Show this help

Environment:
  LAIN_DIR           Override config directory
  LAIN_MODELS_DIR    Override models directory

Examples:
  ./scripts/install.sh              # Full install with model download
  ./scripts/install.sh --fast       # Config only, no model
  LAIN_DIR=/etc/lain ./scripts/install.sh --fast
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --fast)      SKIP_MODEL=true; shift ;;
        --dir)       LAIN_DIR="$2"; MODELS_DIR="$LAIN_DIR/models"; TOOLCHAINS_DIR="$LAIN_DIR/toolchains"; shift 2 ;;
        --help|-h)   print_help; exit 0 ;;
        *)           echo "Unknown option: $1"; print_help; exit 1 ;;
    esac
done

# ── Helpers ───────────────────────────────────────────────────────────────────

info()  { echo "  [info]  $*"; }
ok()    { echo "  [ok]    $*"; }
warn()  { echo "  [warn]  $*" >&2; }

create_tuning_config() {
    info "Creating tuning.toml..."
    mkdir -p "$LAIN_DIR"
    cat > "$TUNING_FILE" <<'EOF'
# Lain tuning configuration
# This file is loaded from ~/.lain/tuning.toml (or <workspace>/.lain/tuning.toml)
# Values here override Lain's built-in defaults.
# All fields are optional — omit a field to keep the default.

[semantic]
# Semantic search: minimum cosine similarity threshold
# Range: [0.0, 1.0]
threshold = 0.1

# Hybrid ranking: weight for anchor_score
# hybrid_score = cosine_similarity + anchor_weight * anchor_score
# Range: [0.0, 1.0]
anchor_weight = 0.3

[ingestion]
# Ceiling on cross-boundary coupling edges (0 = disable pattern edges)
max_pattern_edges = 200

# LSP pool size (number of parallel language servers)
lsp_pool_size = 4

# Files per batch when scanning the workspace
files_per_batch = 50

# Maximum files to scan in one pass (0 = unlimited)
max_files_per_scan = 5000

# Commit window for co-change analysis (number of commits to analyze)
cochange_commit_window = 100

# Minimum pair count to retain a co-change relationship
cochange_min_pair_count = 2

# Skip mega-commits with more than this many files
cochange_max_commit_files = 100

[execution]
# Default command timeout (seconds)
default_command_timeout_secs = 60

# Default test timeout (seconds)
default_test_timeout_secs = 300

# LSP symbol poll timeout (seconds)
lsp_symbol_poll_timeout_secs = 2

# LSP symbol poll interval (milliseconds)
lsp_symbol_poll_interval_ms = 50
EOF
    ok "Created $TUNING_FILE"
}

create_toolchain_overrides() {
    info "Creating toolchains/ directory..."
    mkdir -p "$TOOLCHAINS_DIR"
    cat > "$TOOLCHAINS_DIR/README.md" <<'EOF'
# User toolchain overrides

Files in this directory override Lain's built-in toolchain profiles.
To override a toolchain, copy the built-in config and modify it:

    cp toolchains/rust.toml ~/.lain/toolchains/

See the built-in configs at `toolchains/` in the Lain source for the full format.
EOF
    ok "Created $TOOLCHAINS_DIR/"
}

download_model() {
    local model_name="${1:-sentence-transformers/all-MiniLM-L6-v2}"
    info "Downloading ONNX embedding model..."
    mkdir -p "$MODELS_DIR"

    # Try huggingface-cli first
    if command -v huggingface-cli &>/dev/null; then
        info "Using huggingface-cli..."
        huggingface-cli download "$model_name" \
            --include "*.onnx" \
            --include "tokenizer.json" \
            --local-dir "$MODELS_DIR" \
            --local-dir-use-symlinks False \
            2>&1 | while read line; do info "  $line"; done
        if [[ -f "$MODELS_DIR/model.onnx" ]]; then
            ok "Model ready at $MODELS_DIR"
            return 0
        fi
        warn "huggingface-cli failed, trying Python..."
    fi

    # Python fallback
    if command -v python3 &>/dev/null; then
        info "Trying Python/huggingface_hub..."
        python3 <<'PYEOF'
from huggingface_hub import hf_hub_download, snapshot_download
import os

try:
    p = hf_hub_download(repo_id='sentence-transformers/all-MiniLM-L6-v2',
                        filename='onnx/model.onnx',
                        local_dir=os.environ.get('MODELS_DIR', '/tmp'),
                        local_dir_use_symlinks=False)
    print("  ok: onnx/model.onnx")
except Exception as e:
    print(f"  skip: onnx/model.onnx ({e})")

try:
    p = hf_hub_download(repo_id='sentence-transformers/all-MiniLM-L6-v2',
                        filename='tokenizer.json',
                        local_dir=os.environ.get('MODELS_DIR', '/tmp'),
                        local_dir_use_symlinks=False)
    print("  ok: tokenizer.json")
except Exception as e:
    print(f"  skip: tokenizer.json ({e})")
PYEOF
        if [[ -f "$MODELS_DIR/model.onnx" ]]; then
            ok "Model ready at $MODELS_DIR"
            return 0
        fi
        warn "Python download incomplete, trying curl fallback..."
    fi

    # Direct curl fallback
    local HF_BASE="https://huggingface.co/$model_name/resolve/main"
    info "Using curl fallback..."

    echo -n "  downloading model.onnx... "
    if curl -sL "$HF_BASE/onnx/model.onnx" -o "$MODELS_DIR/model.onnx"; then
        echo "ok"
    else
        echo "failed"
    fi

    echo -n "  downloading tokenizer.json... "
    if curl -sL "$HF_BASE/tokenizer.json" -o "$MODELS_DIR/tokenizer.json"; then
        echo "ok"
    else
        echo "failed"
    fi

    if [[ -f "$MODELS_DIR/model.onnx" && -f "$MODELS_DIR/tokenizer.json" ]]; then
        ok "Model ready at $MODELS_DIR"
    else
        warn "Model incomplete — semantic search will run in stub mode"
    fi
}

# ── Main ───────────────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════"
echo "  Lain installation"
echo "═══════════════════════════════════════════"
echo ""
echo "  Config directory: $LAIN_DIR"
echo ""

# Detect existing install
if [[ -d "$LAIN_DIR" ]]; then
    info "Config directory already exists: $LAIN_DIR"
else
    info "Creating config directory: $LAIN_DIR"
    mkdir -p "$LAIN_DIR"
fi

# Tuning config
if [[ -f "$TUNING_FILE" ]]; then
    info "tuning.toml already exists — skipping (delete to re-install)"
else
    create_tuning_config
fi

# Toolchain overrides
if [[ -d "$TOOLCHAINS_DIR" ]]; then
    info "toolchains/ already exists — skipping"
else
    create_toolchain_overrides
fi

# ONNX model
if $SKIP_MODEL; then
    info "Skipping model download (--fast)"
elif [[ -f "$MODELS_DIR/model.onnx" && -f "$MODELS_DIR/tokenizer.json" ]]; then
    info "Model files already exist — skipping"
else
    echo ""
    echo "Optional: ONNX embedding model"
    echo ""
    echo "  This enables semantic search ('Find code by meaning')."
    echo "  Without it, Lain runs in stub mode — all other features work."
    echo ""
    read -p "Download model now? [Y/n]: " choice
    choice="${choice:-Y}"
    if [[ "$choice" =~ ^[Yy]$ ]]; then
        download_model
    else
        info "Skipping model download"
    fi
fi

echo ""
echo "═══════════════════════════════════════════"
echo "  Installation complete"
echo "═══════════════════════════════════════════"
echo ""
echo "  Config:   $LAIN_DIR/tuning.toml"
echo "  Tools:    $TOOLCHAINS_DIR/"
echo "  Models:   $MODELS_DIR/"
echo ""
echo "  To run Lain:"
echo "    lain --workspace /path/to/project"
echo ""
echo "  To enable semantic search, add to your shell profile:"
echo "    export LAIN_EMBEDDING_MODEL=\"\$HOME/.lain/models/model.onnx\""
echo ""
