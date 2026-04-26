# Lain Roadmap

## Phase 1: Core Infrastructure ✅

- [x] Project scaffolding (Cargo.toml, directory structure)
- [x] Error types and error handling
- [x] Git sensor (git2) for file walking and change detection
- [x] LSP multiplexer (lsp-bridge) for multi-language support
- [x] Graph schema (nodes: File, Module, Class, Function, etc.; edges: CONTAINS, IMPORTS, CALLS, etc.)
- [x] KùzuDB integration for persistent graph storage
- [x] Volatile overlay (petgraph) for uncommitted changes
- [x] MCP server protocol stub (`run_mcp_server` blocks on `pending()` — rmcp tool handler wiring is Phase 1.5)
- [x] Basic tools implemented in `ToolExecutor`: `explore_architecture`, `trace_dependency`, `semantic_search`, `get_blast_radius`, `find_dead_code`
- [x] LSP multiplexer rewritten to match lsp-bridge v0.2 async API (`register_server` → `start_server` → `get_document_symbols`); `DocumentSymbol` children recursively flattened
- [x] `VolatileOverlay` made cheaply cloneable via `Arc<RwLock<DiGraph<...>>>`
- [x] Full async server: `build_core_memory`, `sync_volatile_overlay`, `process_file`, `process_change` are all `async fn`; entry point is `#[tokio::main]`
- [x] `cargo check` passes clean (zero errors)

---

## Phase 2: Advanced Reasoning Engine

### 2.1 Temporal Co-Change (Coupling Radar) ✅

- [x] Add `CO_CHANGED_WITH` edge type to schema
- [x] Implement `GitSensor::get_commit_history(count: usize)` to walk recent commits
- [x] Implement `GitSensor::analyze_co_changes(commits, threshold: usize)` to find file pairs
- [x] Add method to create/update CO_CHANGED_WITH edges in graph
- [x] Add `get_coupling_radar(symbol: String)` tool for MCP

---

### 2.2 Structural Anchors (Entry Point Map) ✅

- [x] Add `anchor_score`, `fan_in`, `fan_out` attributes to graph nodes
- [x] Implement `GraphDatabase::calculate_anchor_scores()` to compute degree metrics
- [x] Implement `GraphDatabase::find_anchors(limit: usize)` to get top building blocks
- [x] Add `find_anchors()` tool for MCP
- [x] Add `get_anchor_score(symbol: String)` tool for MCP

---

### 2.3 Distance to Main (Context Depth) ✅

- [x] Add `depth_from_main` attribute on graph nodes
- [x] Implement `GraphDatabase::find_entry_points()` to locate main/App functions
- [x] Implement BFS traversal to calculate hop distances
- [x] Implement `GraphDatabase::calculate_depths()` to refresh all node depths
- [x] Add `get_context_depth(symbol: String)` tool for MCP

---

## Phase 3: Graph Enrichment Pipeline

### 3.1 Automated Enrichment Runner ✅

- [x] Create `run_enrichment` tool that runs after initial graph build
- [x] Run co-change analysis on recent commits
- [x] Calculate anchor scores for all nodes
- [x] Compute depth-from-main for all symbols
- [x] Batch update Kùzu with new metrics

### 3.2 Incremental Updates ✅

- [x] Update co-change pairs on new commits (`sync_state` tool + `get_new_commits_since`)
- [x] Recalculate affected anchor scores on graph changes (on every sync)
- [x] Refresh depths for modified subgraphs (on every sync)
- [x] Track processed commits persistently across server restarts

---

## Phase 4: Semantic Vocabulary & NLP Depth ✅

- [x] Load `all-MiniLM-L6-v2.onnx` via `ort::Session` in `NlpEmbedder::new()`, replacing the hash placeholder
- [x] Load HuggingFace `tokenizer.json` for BERT-compatible tokenization
- [x] Mean pooling on last hidden state for sentence-level embeddings
- [x] Graceful fallback to hash embedder if ONNX load fails
- [x] Validate embedding pipeline end-to-end: ingest symbol → embed via ONNX session → store as node property → query by cosine similarity
- [x] Build a code-aware vocabulary layer: enrich node embeddings with docstrings, type signatures, and surrounding context (not just symbol names)
- [x] Implement hybrid search: combine embedding similarity with graph structure (prefer semantically similar nodes that are also architecturally central)
- [x] Add `semantic_search` result ranking: return anchor score alongside similarity so agents can prioritize foundational matches

---

## Phase 5: Enhanced Tools ✅

### 5.1 Coupling-Aware Blast Radius ✅
- [x] Enhance `get_blast_radius` to also return files that co-change with affected files
- [x] Add `include_coupling: bool` parameter
- [x] **New**: On-demand LSP reference ingestion for precise structural blast radius.

### 5.2 Anchor Navigation ✅
- [x] Add `navigate_to_anchor(symbol: String)` — trace back to foundational node from any leaf
- [x] Add `list_primitives(feature: String)` — covered by `navigate_to_anchor` and `find_anchors`

### 5.3 Context-Aware Summaries ✅
- [x] Add depth filtering to `explore_architecture`
- [x] Generate summaries at different abstraction levels (shallow = high-level routing, deep = implementation detail)

### 5.4 Extended Tool API ✅
- [x] `get_call_chain(from, to)` — shortest call path between two symbols (now uses real `CALLS` edges)
- [x] `compare_modules(a, b)` — structural diff between two modules (shared dependencies, diverging patterns)
- [x] `suggest_refactor_targets()` — flag high-coupling, low-anchor nodes as refactor candidates
- [x] `explain_symbol(symbol)` — combine depth, anchor score, co-change partners, and semantic embedding into a single agent-readable summary
- [x] `list_entry_points()` — all `main()`/`App`/route handler symbols across the workspace

---

## Phase 6: Production Polish ✅

### 6.1 Performance ✅
- [x] Parallelized initial graph build (`build_core_memory`) using `tokio::task::JoinSet`
- [x] Replaced standard locks with `parking_lot` for better concurrency and no poisoning
- [x] Capped Git co-change analysis to prevent O(N^2) explosions on large commits
- [x] Batch graph updates (implemented `insert_nodes_batch` and `insert_edges_batch`)
- [x] Cache git history analysis results between enrichment runs (done via persistent graph edges)
- [x] Background enrichment jobs (using `tokio::spawn` in `run_enrichment` and `sync_state`)
- [x] Initial graph build time: logged timing and throughput for indexing

### 6.2 Error Handling ✅
- [x] Graceful LSP fallback when language server not found in `$PATH` (uses `which` crate)
- [x] Handle corrupt/missing memory — Native KùzuDB backend with WAL for durability.
- [x] Error normalization across all tools via `LainError`

### 6.3 Observability ✅
- [x] Structured logging with `tracing`
- [x] Metrics: graph node/edge count, query latency, enrichment timing
- [x] Health check tool for MCP clients (`get_health`)

---

## Phase 7: Environment Bootstrapping ✅

### 7.1 LSP Auto-Installer ✅
- [x] Expanded `LANGUAGE_MAP` with installation commands for major languages.
- [x] Implemented `install_server` using `tokio::process::Command`.
- [x] Added `install_language_server` MCP tool for on-demand environment setup.
- [x] Enhanced `get_health` to report language availability status.
- [x] Integrated `which` crate for reliable binary discovery.

---

## Phase 8: Persistence & Incremental Updates ✅

### 8.1 Persistent Graph Backend ✅
- [x] Implemented disk persistence for the core knowledge graph (Native KùzuDB backend).
- [x] Automated state loading upon server initialization.
- [x] Background auto-save after indexing, enrichment, and sync jobs.
- [x] Verified data integrity across server restarts.

### 8.2 Incremental Indexing ✅
- [x] Refactored `build_core_memory` to skip redundant scans.
- [x] Uses Git delta detection to only process changed files since the last recorded commit.
- [x] Hierarchical symbol ingestion: automatically creates `CONTAINS` edges for nested code structure.
- [x] Intelligent polling for LSP readiness: replaced arbitrary sleeps with a data-driven backoff loop.
- [x] Root Module Discovery: automatically identifies architectural entry points based on directory structure.
- [x] Deterministic node IDs (UUID v5) ensuring stable updates across sessions.

### 8.3 Master Map & Staleness Engine ✅
- [x] Injected `last_lsp_sync` and `last_git_sync` timestamps into every node.
- [x] Tracked original `commit_hash` for every architectural unit.
- [x] Implemented `get_master_map` tool: a "Staleness Report" for AI agents.
- [x] Dynamic health indicators (Fresh, Stale, Outdated) based on sync history.
- [x] Metadata-driven confidence: agents can now gauge knowledge reliability before acting.
