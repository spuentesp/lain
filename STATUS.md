# LAIN-mcp - Status Summary

## Compilation & Tests
- `cargo check` passes cleanly — zero errors.
- `cargo build --release` succeeds.
- **390/390 tests passing** (comprehensive unit, integration, and property tests).

## MCP Server Transport
- **Dual transport support**: `stdio` (for Claude Code/MCP clients) and `http` (for web diagnostics)
- `lain --workspace . --transport stdio` — MCP JSON-RPC over stdin/stdout
- `lain --workspace . --transport http --port 9999` — HTTP server with:
  - `GET /` — HTML diagnostic dashboard
  - `GET /health` — Health check endpoint
  - `POST /mcp` — MCP JSON-RPC endpoint

## Implemented Features

### Phase 1-3: Core Infrastructure
- Project scaffolding, Git sensor, LSP multiplexer, Petgraph schema
- Temporal Co-Change analysis, Structural Anchors, Context Depth calculation
- Automated and Incremental enrichment pipelines

### Phase 4-5: NLP & Architectural Reasoning
- ONNX inference for semantic search — any embedding model supported
- 33 MCP tools including `get_call_chain`, `explain_symbol`, `compare_modules`, `query_graph`
- **On-demand Reference Ingestion**: Real `CALLS` edges built via LSP `find_references`
- **Query Language**: Ops-array interface for flexible graph traversal (`find`, `connect`, `filter`, `group`, `sort`, `limit`)

### Phase 6-7: Performance & Bootstrapping
- Parallel indexing via `tokio::task::JoinSet` and `parking_lot` high-performance locks
- **LSP Auto-Installer**: Automatic setup of missing language servers
- Observability via `get_health` dashboard

### Phase 8: Persistence, Incremental Updates & Staleness
- **Persistent Graph**: Knowledge graph stored in `.lain/graph.bin` via Petgraph + Bincode
- **Incremental Indexing**: Git delta detection skips redundant LSP scans
- **Staleness Engine**: Tracks `last_lsp_sync` and `last_git_sync` per node
- **Root Module Discovery**: Identifies architectural entry points

## Optional ONNX Model
- NLP embedding model is **optional** and **model-agnostic**
- Any ONNX model producing fixed-dimension sentence embeddings works (e.g. `all-MiniLM-L6-v2`, `paraphrase-multilingual`)
- Without model: `semantic_search` returns "unavailable" with install instructions
- With model: Set `LAIN_EMBEDDING_MODEL` env var or `--embedding-model` flag
- Embedding dimension auto-detected at load time

## Key Bug Fixes Applied
- **Full Async Refactor**: `ToolExecutor` and all tests async
- **Deterministic Identity**: UUID v5 `(NodeType, Path, Name)` for stable persistence
- **SOLID Modularization**: Handlers in `src/tools/handlers/`
- **Architecture Stability**: Petgraph + Bincode for 100% compilation stability
- **Stdout Protection**: Logs to `stderr` to prevent MCP protocol corruption