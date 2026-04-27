# LAIN-mcp Agent Strategy

Use LAIN-mcp when you need to understand a codebase's architecture, find code relationships, or assess impact of changes.

## Quick Start
1. Call `get_health` to verify server is ready
2. Call `get_master_map` to see overall staleness
3. If stale, call `sync_state` to refresh

## Core Workflow

### 1. Orientation (The Telescope)
Never read files blindly. Get the macro-view first:
- `get_layered_map(layer: 0, granularity: "module")` → identify root modules
- `find_anchors(limit: 5)` → find most foundational symbols
- `get_entry_points` → find where app logic begins

### 2. Targeted Exploration
Once you have a target:
- `get_layered_map(layer: 1, granularity: "file")` → see files in module
- `explore_architecture(max_depth: 2)` → topological summary
- `get_outgoing_edges(symbol: "Name")` → symbol's relationships

### 3. Deep Reasoning
- **Impact Analysis**: `get_blast_radius(symbol: "X", include_coupling: true)` → ripple effects
- **Path Tracing**: `get_call_chain(from: "A", to: "B")` → shortest execution path
- **Semantic Search**: `semantic_search(query: "intent")` → find by meaning, not name
- **Symbol Explanation**: `explain_symbol(symbol: "Name")` → full context summary

### 4. Safety Check
Before any refactor:
- `get_blast_radius` to understand scope
- `get_coupling_report` to see co-change patterns
- `compare_modules(a: "ModuleA", b: "ModuleB")` for structural diff

### 5. Stay Fresh
- After git operations: call `sync_state`
- After major edits: call `sync_state`
- Check staleness with `get_master_map`

## Tool Categories

### Architecture
`get_layered_map`, `get_master_map`, `get_entry_points`, `find_anchors`, `explore_architecture`

### Search & Navigation
`search_symbols`, `semantic_search`, `get_node_info`, `get_outgoing_edges`, `get_incoming_edges`

### Impact Analysis
`get_blast_radius`, `get_call_chain`, `compare_modules`, `get_coupling_report`

### Context & Explainability
`explain_symbol`, `get_context_window`, `find_similar_symbols`

### Health & Operations
`get_health`, `list_jobs`, `get_job_status`, `sync_state`