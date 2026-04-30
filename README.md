# LAIN-mcp

LAIN builds a map of how all the code in your project connects — what calls what, what depends on what, which files tend to change together. Then it lets your AI coding assistant ask questions about that map. So instead of the AI just looking at one file and guessing, it can ask "if I change this function, what else breaks?" and get a real answer. It plugs into any AI agent that supports MCP and runs in the background while you work.

---

## What is Lain?

Lain is a persistent code-intelligence MCP server. It builds a queryable knowledge graph of your codebase — symbols and their relationships extracted via LSP and tree-sitter, augmented with git co-change history and optional semantic embeddings — and exposes that graph through MCP tools. The value over LSP-only or RAG-based approaches is cross-file structural reasoning: blast radius for proposed changes, transitive dependency traces, anchor identification, co-change correlation, and contextual build failure decoration so agents can reason about callers rather than just the failing line. Written in Rust, persists across sessions, stays fresh during editing via a file watcher that updates a volatile overlay layered on top of the static graph.

---

## Installation

### Pre-built Binary (fastest)

Download the latest release for your platform from GitHub releases, then:

```bash
# Make executable
chmod +x lain

# Run directly
./lain --workspace /path/to/your/project --transport stdio
```

### Build from Source

```bash
# Clone the repo
git clone https://github.com/spuentesp/lain.git
cd lain

# Build (requires Rust 1.75+)
cargo build --release

# Binary will be at ./target/release/lain
```

---

## Quick Start

### 1. Build Lain

```bash
cargo build --release
```

### 2. Configure Claude Code

Add to your `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "lain": {
      "command": "/path/to/lain/target/release/lain",
      "args": ["--workspace", "/path/to/your/project", "--transport", "stdio"]
    }
  }
}
```

### 3. Run

```bash
# Standard mode (for Claude Code)
./lain --workspace /path/to/project --transport stdio

# With HTTP diagnostics (web UI at http://localhost:9999)
./lain --workspace /path/to/project --transport both --port 9999
```

### 4. Verify

```bash
# Check health and LSP status
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":1}'
```

---

## Key Features

### Query Language (`query_graph`)
JSON-based ops array for flexible graph traversals:
```json
{
  "ops": [
    { "op": "find", "type": "Function" },
    { "op": "connect", "edge": "Calls", "depth": { "min": 1, "max": 3 } },
    { "op": "filter", "label": "test" },
    { "op": "semantic_filter", "like": "error handling", "threshold": 0.35 },
    { "op": "limit", "count": 10 }
  ]
}
```
Available ops: `find`, `connect`, `filter`, `semantic_filter`, `group`, `sort`, `limit`

### Dependency Intelligence
- **`get_call_chain`** — Shortest path between two functions
- **`get_blast_radius`** — Everything affected by a change
- **`trace_dependency`** — What a symbol depends on
- **`get_coupling_radar`** — Files that change together

### Architectural Analysis
- **`find_anchors`** — Most-called, most-stable symbols (architectural pillars)
- **`list_entry_points`** — Find `main()`, route handlers, app initialization
- **`get_context_depth`** — How far from an entry point (abstraction layers)
- **`explore_architecture`** — High-level tree of modules and files

### Search
- **`semantic_search`** — Find code by meaning, not just names (uses local ONNX embeddings)

### Code Health
- **`find_dead_code`** — Potentially unreachable code (filters trait defaults, common names)
- **`suggest_refactor_targets`** — High-coupling, low-stability nodes

### Build Integration
Lain enriches build failures with architectural context:
- **`run_build`** — Build with Rust/Go/JS/Python toolchain error parsing
- **`run_tests`** — Tests with error enrichment
- **`run_clippy`** — cargo clippy with context

---

## Requirements

| Requirement | Details |
|-------------|---------|
| Rust | 1.75 or newer |
| Git | Required for co-change analysis |
| ONNX Model | Optional — for semantic search |

### Optional: Semantic Search

For `semantic_search` to work, you need an ONNX embedding model. The easiest way to set this up is using the provided install script:

```bash
./scripts/install.sh
```

Alternatively, you can set it up manually:

```bash
# Create model directory
mkdir -p .lain/models

# Download all-MiniLM-L6-v2 (or any compatible model)
# Model produces 384-dim embeddings
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx -o .lain/models/model.onnx
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json -o .lain/models/tokenizer.json
```

Set the model path:
```bash
export LAIN_EMBEDDING_MODEL=$PWD/.lain/models/model.onnx
# or
./lain --embedding-model ./.lain/models/model.onnx ...
```

Without the model, `semantic_search` returns "unavailable" but all other features work.

---

## MCP Transport Modes

| Mode | Command | Use Case |
|------|---------|----------|
| `stdio` | `--transport stdio` | Claude Code, MCP clients |
| `http` | `--transport http --port 9999` | Web diagnostics dashboard |
| `both` | `--transport both --port 9999` | Both stdio + diagnostics |

---

## Troubleshooting

**LSP servers not ready?**
```bash
# Install missing language servers
curl -X POST http://localhost:9999/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"install_language_server","arguments":{"language":"rust"}},"id":2}'
```

**Graph stale?**
```bash
# Sync to current git HEAD
curl -X POST http://localhost:9999/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"sync_state","arguments":{}},"id":3}'
```

**View all available tools:**
```bash
curl -s -X POST http://localhost:9999/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_agent_strategy","arguments":{}},"id":4}'
```

---

## A/B Testing Results

A simple A/B test was run on the `asciinema_fix_pty_bug` (a small fork i made from https://github.com/asciinema/asciinema.git ) across **5 passes, 4 times** using a script. Median numbers are reported.

| Metric | with_lain | without_lain |
|--------|-----------|--------------|
| Pass rate | 5/5 (100%) | 5/5 (100%) |
| Median duration | 39.3s | 54.1s |
| Median tokens in | 35,488 | 41,731 |

**Key observations:**

- Both conditions passed 100% — the bug fix worked in both conditions, with variation per run.
- `with_lain` used fewer input tokens (~35k vs ~42k median), a difference of ~7k tokens per run.

**About the bug:** The failing test (`pty::tests::spawn_extra_env` on macOS) stems from `handle_child()` setting env vars via `env::set_var()` before `execvp()`. The shell's interpretation of `echo -n $VAR` varies across platforms — sometimes `-n` is treated as a literal argument. The fix: use `printf "%s" "$ASCIINEMA_TEST_FOO"` instead, portable across all Unix-like systems.

> This was a test I did for A/B comparison — not a rigorous evaluation.

---

## License

MIT — Copyright (c) 2026 spuentesp