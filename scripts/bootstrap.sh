#!/bin/bash
# Lain startup bootstrap — checks dependencies and optionally downloads models

set -e

APP_NAME="lain"
MODELS_DIR="${LAIN_MODELS_DIR:-$HOME/.lain/models}"
DEFAULT_MODEL="sentence-transformers/all-MiniLM-L6-v2"

check_model_files() {
    [[ -f "$MODELS_DIR/model.onnx" && -f "$MODELS_DIR/tokenizer.json" ]]
}

download_model() {
    local model_name="$1"
    echo "📦 Downloading embedding model: $model_name"
    echo "   This may take a few minutes on first run..."
    echo ""

    mkdir -p "$MODELS_DIR"

    # Try huggingface-cli first (if available)
    if command -v huggingface-cli &>/dev/null; then
        echo "   Trying huggingface-cli..."
        huggingface-cli download "$model_name" \
            --include "*.onnx" \
            --include "tokenizer.json" \
            --local-dir "$MODELS_DIR" \
            --local-dir-use-symlinks False \
            2>&1
        if [[ -f "$MODELS_DIR/model.onnx" ]]; then
            echo "   ✅ via huggingface-cli"
            return 0
        fi
    fi

    # Python fallback
    if command -v python3 &>/dev/null; then
        echo "   Trying Python/huggingface_hub..."
        python3 -c "
from huggingface_hub import hf_hub_download, snapshot_download
import os

try:
    p = hf_hub_download(repo_id='$model_name', filename='onnx/model.onnx', local_dir='$MODELS_DIR', local_dir_use_symlinks=False)
    print('   ✅ onnx/model.onnx')
except: pass

try:
    p = hf_hub_download(repo_id='$model_name', filename='tokenizer.json', local_dir='$MODELS_DIR', local_dir_use_symlinks=False)
    print('   ✅ tokenizer.json')
except: pass
" 2>&1

        if [[ -f "$MODELS_DIR/model.onnx" ]]; then
            echo "   ✅ via Python"
            return 0
        fi
    fi

    # Direct curl fallback
    echo "   Trying direct download..."
    HF_BASE="https://huggingface.co/$model_name/resolve/main"

    echo -n "   downloading model.onnx... "
    if curl -L "$HF_BASE/onnx/model.onnx" -o "$MODELS_DIR/model.onnx" --progress-bar 2>/dev/null; then
        echo "✅"
    else
        echo "❌ (continuing)"
    fi

    echo -n "   downloading tokenizer.json... "
    if curl -L "$HF_BASE/tokenizer.json" -o "$MODELS_DIR/tokenizer.json" --progress-bar 2>/dev/null; then
        echo "✅"
    else
        echo "❌ (continuing)"
    fi

    echo -n "   downloading config.json... "
    curl -sL "$HF_BASE/config.json" -o "$MODELS_DIR/config.json" 2>/dev/null && echo "✅" || echo "skip"

    if [[ -f "$MODELS_DIR/model.onnx" && -f "$MODELS_DIR/tokenizer.json" ]]; then
        echo ""
        echo "✅ Model ready at $MODELS_DIR"
        return 0
    else
        echo ""
        echo "⚠️  Model incomplete — will run in stub mode"
        return 1
    fi
}

use_custom_model() {
    echo ""
    echo "Provide model (leave blank for default):"
    echo "  • HuggingFace ID (e.g. sentence-transformers/all-MiniLM-L6-v2)"
    echo "  • Local .onnx file path"
    echo "  • URL to download from"
    echo ""
    read -p "Model: " model_input
    model_input="${model_input:-$DEFAULT_MODEL}"

    if [[ -f "$model_input" ]]; then
        echo "✅ Using local model: $model_input"
        export LAIN_EMBEDDING_MODEL="$model_input"
    elif [[ "$model_input" =~ ^https?:// ]]; then
        echo "📥 Downloading..."
        mkdir -p "$MODELS_DIR"
        echo -n "   "
        if curl -L "$model_input" -o "$MODELS_DIR/model.onnx" --progress-bar 2>/dev/null; then
            echo "✅ Downloaded"
            export LAIN_EMBEDDING_MODEL="$MODELS_DIR/model.onnx"
        fi
    else
        download_model "$model_input"
    fi
}

# ── Bootstrap ────────────────────────────────────────────────────────────────

echo "═══════════════════════════════════════════"
echo "  $APP_NAME bootstrap"
echo "═══════════════════════════════════════════"
echo ""

if check_model_files; then
    echo "✅ NLP model files found at $MODELS_DIR"
else
    echo "⚠️  NLP model not found at $MODELS_DIR"
    echo ""
    echo "  [1] Download default ($DEFAULT_MODEL)"
    echo "  [2] Custom model (path/ID/URL)"
    echo "  [3] Skip — run in stub mode"
    echo ""
    read -p "Select [1]: " choice
    case "${choice:-1}" in
        1) download_model "$DEFAULT_MODEL" ;;
        2) use_custom_model ;;
        *) echo "⏭️  Skipping NLP" ;;
    esac
fi

echo ""
echo "🚀 Starting $APP_NAME..."
exec ./target/debug/lain "$@"