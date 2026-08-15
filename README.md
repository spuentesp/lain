# LAIN-mcp

LAIN builds a map of how all the code in your project connects — what calls what, what depends on what, which files tend to change together. Then it lets your AI coding assistant ask questions about that map. So instead of the AI just looking at one file and guessing, it can ask "if I change this function, what else breaks?" and get a real answer. It plugs into any AI agent that supports MCP and runs in the background while you work.

<img width="1511" height="767" alt="Screenshot 2026-04-29 at 9 18 15 PM" src="https://github.com/user-attachments/assets/3bfbfe83-6813-416a-8dfc-c1c17959a00d" />


## TL,DR:

```bash
# One-line install (interactive - will ask you to configure and add to PATH)
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash

# After install: reload your shell (or open a new terminal)
source ~/.zshrc   # or ~/.bashrc

# Or non-interactive (skips prompts, auto-adds to PATH)
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --workspace . --transport both --yes
```
## What is Lain?

Lain is a persistent code-intelligence MCP server. It builds a queryable knowledge graph of your codebase — symbols and their relationships extracted via LSP and tree-sitter, augmented with git co-change history and optional semantic embeddings — and exposes that graph through MCP tools. The value over LSP-only or RAG-based approaches is cross-file structural reasoning: blast radius for proposed changes, transitive dependency traces, anchor identification, co-change correlation, and contextual build failure decoration so agents can reason about callers rather than just the failing line. Written in Rust, persists across sessions, stays fresh during editing via a file watcher that updates a volatile overlay layered on top of the static graph.

---

## Installation

### Quick Install (recommended - interactive)

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash
```

The installer downloads the LAIN binary to `~/.local/lain`. After that,
configure your agent's MCP config to launch `lain server --config
./repos.yaml --transport stdio` — see "Wire your agent" in the Quick
Start below.

**Non-interactive install (with options):**

```bash
# Install with specific workspace and download ONNX model for semantic search
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --workspace . --transport both --download-model --yes

# Install for specific agent
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --agent cursor --yes

# See all options
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --help
```

**Install options:**

| Option | Description | Default |
|--------|-------------|---------|
| `--download-model` | Download default ONNX model (all-MiniLM-L6-v2.onnx, ~120MB) | - |
| `-y, --yes` | Skip all confirmation prompts | - |

(Workspace path, transport, and port are no longer installer flags —
they are `lain server` flags. See the Quick Start below.)

**After installation:**

```bash
# Reload your shell (the installer adds to ~/.zshrc or ~/.bashrc automatically)
source ~/.zshrc   # or ~/.bashrc, then open a new terminal

# Verify installation
lain --version

# Query the graph
lain query "find Function | limit 5"
```

### Homebrew

```bash
brew tap spuentesp/lain https://github.com/spuentesp/lain
brew install lain

# Run the server for your project
lain server --config ./repos.yaml
```

### Pre-built Binary

Download the latest release for your platform from [GitHub releases](https://github.com/spuentesp/lain/releases), then:

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

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash
```

### 2. Configure your project

A project is one paired `repos.yaml` + `workspaces.yaml`. Pick or create a directory:

```bash
mkdir -p ~/projects/biller
cd ~/projects/biller
lain repos add auth-svc https://github.com/acme/auth-svc.git
lain repos add billing-svc https://github.com/acme/billing-svc.git
lain workspaces create biller-core --members auth-svc,billing-svc
```

### 3. Run the server

```bash
lain server --config ./repos.yaml --transport http --port 9999
# Open http://localhost:9999 in your browser for the Command Center.
```

### 4. Wire your agent

Add the following to your agent's MCP config (the URL is documented for your specific agent):

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["server", "--config", "./repos.yaml", "--transport", "stdio"]
    }
  }
}
```

That's it. The next time your agent starts, it sees the federation, the workspace, and the full MCP tool surface.

---

## Federation mode

For org-wide structural questions — "who else uses this function?", "what depends on this service?" — run `lain server --config repos.yaml` to index N repos and answer cross-repo queries. Federation mode exposes six MCP tools (`list_repos`, `get_repo_info`, `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`, and `get_cross_repo_blast_radius_for_repo`) that answer questions spanning repos. See [`docs/FEDERATION.md`](docs/FEDERATION.md) for the full guide and [`docs/REPOS_YAML.md`](docs/REPOS_YAML.md) for the config schema.

---

## Key Features

- **Federation mode** — index N repos and answer org-wide structural questions across them

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
- **`semantic_search`** — Find code by meaning, not just names. Uses local ONNX embeddings with hybrid scoring (cosine similarity + stemmed token-overlap) and shows body excerpts in the response. BGE-small-en-v1.5 is the recommended model (better than MiniLM for technical corpora); use a query prefix to enable BGE-style asymmetric retrieval.

### Code Health
- **`find_dead_code`** — Potentially unreachable code (filters trait defaults, common names)
- **`suggest_refactor_targets`** — High-coupling, low-stability nodes

### Build Integration
Lain enriches build failures with architectural context:
- **`run_build`** — Build with Rust/Go/JS/Python toolchain error parsing
- **`run_tests`** — Tests with error enrichment
- **`run_clippy`** — cargo clippy with context

### Project Management

A project is a directory containing `repos.yaml` (and optionally
`workspaces.yaml`). Manage it directly with the CLI:

- **`lain repos add <name> <url>`** — register a repo in `repos.yaml`
- **`lain repos list`** — show registered repos
- **`lain repos remove <name>`** — unregister a repo
- **`lain workspaces create <name> --members a,b,c`** — declare a named workspace
- **`lain workspaces list`** — show all workspaces
- **`lain workspaces use <name>`** — activate a workspace (writes `~/.config/lain/active_workspace`)
- **`lain workspaces current`** — print the active workspace
- **`lain workspaces forget <name>`** — remove a workspace

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

# Option A: bge-small-en-v1.5 (recommended — better MTEB scores, 384d, ~120MB)
curl -L https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/onnx/model.onnx \
  -o .lain/models/model.onnx
curl -L https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/tokenizer.json \
  -o .lain/models/tokenizer.json

# Option B: all-MiniLM-L6-v2 (smaller, 384d, ~80MB)
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx \
  -o .lain/models/model.onnx
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json \
  -o .lain/models/tokenizer.json
```

Set the model path:
```bash
export LAIN_EMBEDDING_MODEL=$PWD/.lain/models/model.onnx
# or
./lain --embedding-model ./.lain/models/model.onnx ...
```

For BGE-style asymmetric retrieval (better for short queries), set the
query prefix in `.lain/tuning.toml`:

```toml
query_prefix = "Represent this sentence for searching relevant passages: "
```

Tune the CPU thread usage (default auto-detects, min(cores, 4)):

```toml
[ingestion]
nlp_max_threads = 0  # 0 = auto, or set to a number
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

## Recent Improvements

## v0.5.0
- **Consolidated CLI surface** — `lain init`, `lain agents`, `lain hook`, `lain projects`, and the top-level `lain use` are gone. The kept subcommands are `server`, `workspaces`, `repos`, `query`, `ask`. Federation / repo / per-repo concerns are reached via `server` + `repos`. Workspaces keep their own subcommand tree.
- **Project model** — a project is a directory containing `repos.yaml` + `workspaces.yaml`. Add repos with `lain repos add`, group them with `lain workspaces create`, run `lain server --config ./repos.yaml`.
- **Federation server** — `lain server --config ./repos.yaml --transport http --port 9999` is the headline command. The HTTP transport also serves the Command Center dashboard.
- **Bug fixes**: file-watcher tolerates inaccessible subdirs; LSP timeouts; expanded Claude `LAIN.md`.

## v0.4.x (historical)
- **Hybrid semantic scoring**: `semantic_search` combines cosine similarity with **stemmed token-overlap** (query "running" matches symbols named `index`, `indexed`, `indexes`, etc.)
- **Body excerpts in responses**: both `semantic_search` and `explain_symbol` show the actual code, not just metadata.
- **Call Graph section**: `explain_symbol` shows callers and callees alongside the source excerpt.
- **Anchor percentile normalization**: anchor scores are bounded to [0, 100] via min-max within the candidate set.
- **Batched inference API**: `NlpEmbedder::embed_batch()` for larger models / GPU.
- **Configurable ONNX thread count**: `.lain/tuning.toml` has `nlp_max_threads`.
- **Cross-encoder reranker** (opt-in): `cross-encoder/ms-marco-MiniLM-L6-v2`.
- **Volatile embedding persistence**: cold-query embeddings are written back to `graph.bin`.

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
