# LAIN-mcp — Technical Reference

*Deep dive into how Lain works under the hood.*

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      AI Agent (Claude Code)                  │
└─────────────────────────────────────────────────────────────┘
                              │ MCP (JSON-RPC over stdio/HTTP)
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                         LAIN-mcp                            │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │ MCP Handler │  │  Tool Exec   │  │   Background Jobs   │ │
│  │  (rust-mcp) │  │  (inventory) │  │  (sync, enrich)     │ │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘ │
│         │                │                    │            │
│  ┌──────▼────────────────▼────────────────────▼──────────┐ │
│  │                    LainServer                         │ │
│  │                                                      │ │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────────┐ │ │
│  │  │  Graph  │ │   LSP   │ │   NLP   │ │ Git Sensor  │ │ │
│  │  │(petgr.) │ │(bridge) │ │ (ONNX)  │ │   (git2)    │ │ │
│  │  └────┬────┘ └────┬────┘ └────┬────┘ └──────┬──────┘ │ │
│  │       │           │           │             │         │ │
│  │       ▼           ▼           ▼             ▼         │ │
│  │  .lain/graph  LSP servers  ONNX model   Git history    │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

---

## Core Components

### 1. Knowledge Graph (`src/graph.rs`)

The graph is a **petgraph** directed acyclic graph stored at `.lain/graph.bin`.

**Node Types:**
- `File` — Source file
- `Module` — Language module/namespace
- `Function` / `Method` / `Class` — Code symbols
- `Interface` / `Trait` — Type definitions
- `Variable` / `Constant` — Value bindings

**Edge Types:**

| Edge | Meaning | Source |
|------|---------|--------|
| `Calls` | Function invocation | LSP `find_references` (high confidence) or Tree-sitter heuristic (medium) |
| `Contains` | File contains module | Tree-sitter AST |
| `Defines` | Module defines symbol | Tree-sitter AST |
| `Inherits` | Class inheritance | Tree-sitter AST |
| `Imports` | Import/use statement | Tree-sitter AST |
| `CO_CHANGED_WITH` | Historical co-change | Git history analysis |

**Node Identity:** UUID v5 derived from `(NodeType, FilePath, SymbolName)` for deterministic, stable IDs across runs.

### 2. Volatile Overlay (`src/overlay.rs`)

In-memory graph layer for real-time changes before persistence:

- **Overlay nodes** — newly created symbols not yet persisted
- **Dirty edges** — modified relationships not yet written to disk
- **Staleness tracking** — per-node `last_lsp_sync` and `last_git_sync`

When `sync_state` is called, overlay is merged into the persistent graph.

### 3. LSP Bridge (`src/lsp.rs`)

Multi-language server protocol multiplexer supporting:

| Language | Server | Status |
|----------|--------|--------|
| Rust | rust-analyzer | ✅ |
| Go | gopls | ✅ |
| TypeScript/JS | typescript-language-server, volar | ✅ |
| Python | pylsp | ✅ |
| C/C++ | clangd | ✅ |
| C# | omnisharp | ✅ |
| Java | jdtls | ✅ |
| Kotlin | kotlin-language-server | ✅ |
| Ruby | solargraph | ✅ |
| Scala | metals | ✅ |
| Svelte | svelte-language-server | ✅ |

**On-demand reference ingestion:** When `get_blast_radius` or `get_call_chain` is called, Lain uses LSP `find_references` to build real `Calls` edges—never static heuristics alone.

### 4. NLP Embedder (`src/nlp.rs`)

Local ONNX-based semantic search using [ORT (ONNX Runtime)](https://onnxruntime.ai/):

- **Model-agnostic** — any ONNX model producing fixed-dimension embeddings works
- **Default model:** `all-MiniLM-L6-v2` (384 dimensions)
- **Tokenization:** Hugging Face `tokenizers` crate

When `semantic_search(query)` is called:
1. Tokenize query
2. Run ONNX inference
3. Cosine similarity against all stored embeddings
4. Return top-k results by semantic similarity

### 5. Git Sensor (`src/git.rs`)

Analyzes git history for co-change patterns:

1. **Walk commits** — extract file-change sets per commit
2. **Build co-change matrix** — how often files change together
3. **Compute coupling scores** — Jaccard similarity between file sets
4. **Attach `CO_CHANGED_WITH` edges** to graph nodes

The `get_coupling_radar` tool uses this to find files that "live together" across commits.

### 6. Background Jobs (`src/server/jobs.rs`)

Async job system for long-running tasks:

| Job | Trigger | Frequency |
|-----|---------|-----------|
| **Incremental sync** | Git push / file save | On-change |
| **Full enrichment** | `run_enrichment` | Manual |
| **Sliding Window** | Periodic | Every 30s |
| **Background Sync** | Periodic | Every 60s |
| **Lazy NLP** | Post-sync | On-demand |

---

## Build System

### Compilation Requirements

- **Rust:** 1.75+ ( edition 2021 )
- **C compiler:** Required for some dependencies (git2, tree-sitter)
- **Git:** Required at runtime for co-change analysis
- **ONNX Runtime:** Bundled via `ort` crate

### Build Commands

```bash
# Development build (faster compilation)
cargo build

# Release build (optimized, ~2-3x faster)
cargo build --release

# Check without building
cargo check

# Run tests
cargo test

# Lint
cargo clippy
```

### Release Profile

The `Cargo.toml` configures aggressive optimization:

```toml
[profile.release]
opt-level = 3      # Maximum optimization
lto = true         # Link-time optimization
codegen-units = 1  # Single codegen unit for better optimization
```

### Output Binary

After build, the binary is at:
- Dev: `./target/debug/lain`
- Release: `./target/release/lain`

---

## MCP Protocol Implementation

### Transport Modes

**stdio (default):**
```
Claude Code <--stdin/stdout--> Lain MCP handler
```
Uses `rust-mcp-sdk` with JSON-RPC over process I/O.

**HTTP (diagnostics):**
```
HTTP POST /mcp  --> MCP handler --> JSON-RPC response
GET /           --> HTML diagnostic dashboard
GET /health     --> Health check JSON
```

**Both:**
```
stdio + HTTP server on --port (default 9999)
```

### Tool Dispatch

Tools are registered via `inventory` crate:

```rust
// src/tools.rs
inventory::collect!(ToolDefinition, TOOLS);

// src/tools/handlers/*.rs - each handler implements ToolHandler
```

The `ToolExecutor` dispatches based on tool name, routing to the appropriate handler in `src/tools/handlers/`.

---

## Data Flow

### Initial Indexing (First Run)

```
1. server/mod.rs::new() initializes components
2. build_core_memory() starts ingestion pipeline
3. scan_file_batch() (scan.rs) --> Map phase
4. Resolve phase (ingestion.rs) --> Link edges
5. calculate_anchor_scores() --> Enrich
6. persist() --> write .lain/graph.bin
```

### Incremental Sync (on file change)

```
1. file_watcher detects change
2. sync_volatile_overlay() (ingestion.rs)
3. process_change() (ingestion.rs)
4. update overlay graph (in-memory)
```

### Query Flow (e.g., `get_blast_radius`)

```
1. MCP request arrives at handler.rs
2. ToolExecutor::execute("get_blast_radius", args)
3. Handler calls GraphDatabase methods
4. If Calls edges are stale --> LSP find_references to refresh
5. Traverse graph transitively
6. Return result to MCP client
```

### Query Language (`query_graph`)

Lain exposes a JSON-based ops-array query interface for flexible graph traversal:

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "foo" },
    { "op": "connect", "edge": "Calls", "depth": { "min": 1, "max": 3 } },
    { "op": "filter", "label": "test" },
    { "op": "limit", "count": 10 }
  ],
  "mode": "auto"
}
```

**Available ops:**
| Op | Description |
|----|-------------|
| `find` | Locate nodes by type, name, label, path, or id |
| `connect` | Traverse edges with direction and depth |
| `filter` | Narrow results by type, name, or label |
| `group` | Group results by type, label, or name |
| `sort` | Order results by field and direction |
| `limit` | Paginate with count and offset |

**Selectors (find, filter):**
- `type`: `Function`, `Method`, `Class`, `File`, `Module`, etc.
- `name`: exact, `glob` (`foo*`), `startsWith`, `endsWith`
- `label`: exact, `Or`, `Not`
- `path`: file path string
- `id`: node UUID

**Connect:**
- `edge`: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`
- `direction`: `outgoing`, `incoming`, `both`
- `depth`: `1` or `{ "min": 1, "max": 3 }`
- `target`: optional nested `FindOp` for multi-hop queries

---

## Directory Structure

```
lain/
├── src/
│   ├── bin/                    # Binary entry point
│   ├── lib.rs                  # Library root
│   ├── server/                 # Server orchestration (modular)
│   │   ├── mod.rs              # LainServer definition
│   │   ├── ingestion.rs        # Main ingestion pipeline
│   │   ├── scan.rs             # Parallel file scanning
│   │   └── jobs.rs             # Background job lifecycle
│   ├── graph.rs                # Petgraph knowledge graph
│   ├── overlay.rs              # Volatile in-memory overlay
│   ├── lsp.rs                  # LSP bridge
│   ├── nlp.rs                  # ONNX embedding
│   ├── git.rs                  # Git sensor
│   ├── treesitter.rs           # Static analysis
│   ├── toolchains.rs           # Language toolchains
│   ├── mcp/                    # MCP protocol layer
│   │   ├── mod.rs
│   │   ├── handler.rs          # MCP request handler
│   │   └── front_end_monitor.html  # Diagnostics UI
│   ├── tools/                  # Tool definitions
│   │   ├── mod.rs              # Tool executor + registry
│   │   └── handlers/           # Handler modules
│   │       ├── architecture.rs
│   │       ├── context.rs
│   │       ├── decoration.rs
│   │       ├── enrichment.rs
│   │       ├── execution.rs
│   │       ├── filesystem.rs
│   │       ├── gitops.rs
│   │       ├── impact.rs
│   │       ├── metrics.rs
│   │       ├── navigation.rs
│   │       ├── query.rs
│   │       ├── search.rs
│   │       ├── testing.rs
│   │       ├── cross_runtime.rs
│   │       └── registry_impl.rs
│   ├── query/                  # Graph query engine
│   │   ├── mod.rs
│   │   ├── spec.rs             # Query ops types
│   │   ├── executor.rs
│   │   └── schema.rs
│   └── ui/                     # Interactive diagnostic UIs
│       ├── blast-radius.html
│       ├── call-chain.html
│       └── coupling.html
├── tests/                      # Integration tests
├── toolchains/                 # Toolchain definitions (Rust/Go/JS/Python)
├── .lain/                      # Runtime data directory
│   └── graph.bin               # Persistent graph
├── Cargo.toml
├── README.md                   # Basic user-facing docs
└── TECHNICAL.md                # This file
```

---

## Edge Confidence

Not all graph edges are equally reliable:

| Edge Type | Source | Confidence | Notes |
|-----------|--------|------------|-------|
| `Calls` (LSP) | `find_references` | **High** | Language-aware, precise |
| `Calls` (heuristic) | Tree-sitter patterns | **Medium** | May have false positives |
| `Contains` | Tree-sitter AST | **High** | Structural, unambiguous |
| `Defines` | Tree-sitter AST | **High** | Based on AST node type |
| `Inherits` | Tree-sitter superclass | **High** | Language grammar |
| `Imports` | Tree-sitter import | **High** | Import statements |
| `CO_CHANGED_WITH` | Git history | **Historical** | Reflects past patterns |

Use `get_health` to see which LSP servers are ready (affects `Calls` edge quality).

---

## All MCP Tools

### Dependency Intelligence
- `get_call_chain(from, to)` — Shortest execution path between two symbols
- `get_blast_radius(symbol)` — All functions affected by changing this (transitive)
- `trace_dependency(symbol)` — Everything a symbol depends on (recursive)
- `get_coupling_radar(symbol)` — Files that co-change with this one

### Architectural Understanding
- `find_anchors(limit)` — Most-called, most-stable symbols (pillars)
- `navigate_to_anchor(symbol)` — Trace back to architectural anchor
- `list_entry_points` — Find `main()`, route handlers, app init
- `get_context_depth(symbol)` — Distance from entry point
- `explore_architecture(depth)` — High-level module tree
- `get_layered_map(layer)` — Architecture slice at specific depth
- `compare_modules(a, b)` — Structural diff between modules
- `architectural_observations` — Cross-boundary couplings, high-fan-out modules

### Search
- `semantic_search(query)` — Intent-based via ONNX embeddings
- `query_graph(spec)` — Flexible graph query via ops-array (see below)

### Analysis
- `explain_symbol(symbol)` — Human-readable summary with signature/metrics
- `suggest_refactor_targets` — High-coupling, low-stability nodes
- `find_dead_code` — Zero-incoming-call nodes
- `get_call_sites(symbol)` — All callers
- `find_untested_functions(limit)` — No incoming call edges
- `get_test_template(function)` — Generate test scaffold
- `get_coverage_summary(module)` — Structural coverage estimate

### Query Language (`query_graph`)

The query engine is separate from named tools — it accepts ops-array JSON for flexible graph traversal:

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "foo" },
    { "op": "connect", "edge": "Calls", "depth": { "min": 1, "max": 3 } },
    { "op": "filter", "label": "test" },
    { "op": "limit", "count": 10 }
  ],
  "mode": "auto"
}
```

**Named prebuilt queries** (via `named` field):
- `get_blast_radius`, `get_call_chain`, `get_file_functions`
- `get_function_imports`, `get_callers`, `get_callees`
- `get_module_functions`, `get_test_coverage`, `get_deprecated_functions`

**Ops:**
| Op | Description |
|----|-------------|
| `find` | Locate nodes by type, name, label, path, or id |
| `connect` | Traverse edges with direction and depth |
| `filter` | Narrow results by type, name, or label |
| `group` | Group results by type, label, or name |
| `sort` | Order results by field and direction |
| `limit` | Paginate with count and offset |

**Selectors (find, filter):**
- `type`: `Function`, `Method`, `Class`, `File`, `Module`, etc.
- `name`: exact, `glob` (`foo*`), `startsWith`, `endsWith`
- `label`: exact, `Or`, `Not`
- `path`: file path string
- `id`: node UUID

**Connect:**
- `edge`: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`
- `direction`: `outgoing`, `incoming`, `both`
- `depth`: `1` or `{ "min": 1, "max": 3 }`
- `target`: optional nested `FindOp` for multi-hop queries

### Analysis
- `explain_symbol(symbol)` — Human-readable summary with signature/metrics
- `suggest_refactor_targets` — High-coupling, low-stability nodes
- `find_dead_code` — Zero-incoming-call nodes
- `get_call_sites(symbol)` — All callers

### Testing
- `find_untested_functions(limit)` — No incoming call edges
- `get_test_template(function)` — Generate test scaffold
- `get_coverage_summary(module)` — Structural coverage estimate

### GitOps
- `get_file_diff(path)` — Uncommitted changes
- `get_commit_history(limit)` — Recent commits
- `get_branch_status` — Current branch

### System
- `get_health` — LSP status, staleness
- `sync_state` — Refresh graph from git HEAD
- `run_enrichment` — Full co-change + anchor recalc
- `install_language_server(lang)` — Install LSP
- `export_graph_json` — Dump graph for auditing
- `get_agent_strategy` — Strategy guide for AI agents

### Context
- `get_context_for_prompt(symbol)` — LLM-optimized context
- `get_code_snippet(path, line)` — File content around line

### Build Integration
- `run_build(cwd, release)` — Build with toolchain error parsing
- `run_tests(cwd, filter)` — Tests with error parsing
- `run_clippy(cwd, fix)` — Clippy with context

---

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LAIN_GRAPH_DIR` | `.lain` | Graph storage directory |
| `LAIN_EMBEDDING_MODEL` | (none) | Path to ONNX embedding model |
| `LAIN_HTTP_PORT` | `9999` | HTTP diagnostics port |
| `RUST_LOG` | `info` | Tracing log level |

### CLI Flags

```
--workspace <path>       Project root (required)
--transport <mode>        stdio | http | both (default: stdio)
--port <port>             HTTP port (default: 9999)
--embedding-model <path>  ONNX model path
--no-index                Skip initial indexing
```

---

## Persistence

**Graph file:** `.lain/graph.bin`

Format: Bincode-serialized petgraph `Graph<String, EdgeWeight, Directed>`

To inspect:
```bash
# Export to JSON
curl -X POST http://localhost:9999/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"export_graph_json","arguments":{}},"id":99}'
```

---

## License

MIT — Copyright (c) 2026 spuentesp