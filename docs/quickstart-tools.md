# LAIN-mcp - Tools Quickstart

Quick reference for LAIN MCP tools.

## Initialization

### get_health
Check server health, LSP status, and repository info.
```json
{ "name": "get_health", "arguments": {} }
```

### install_language_server
Install a language server.
```json
{ "name": "install_language_server", "arguments": { "language": "rust" } }
```

## Global Orientation

### find_anchors
Find the most-called, most-stable symbols (architectural pillars).
```json
{ "name": "find_anchors", "arguments": { "limit": 5 } }
```

### list_entry_points
Find `main()`, route handlers, app initialization.
```json
{ "name": "list_entry_points", "arguments": {} }
```

### explore_architecture
High-level tree of modules and files.
```json
{ "name": "explore_architecture", "arguments": { "max_depth": 2 } }
```

### describe_schema
Understand the graph schema (node types, edge types).
```json
{ "name": "describe_schema", "arguments": {} }
```

## Dependency Intelligence

### get_blast_radius
Everything affected by changing a symbol (transitive).
```json
{ "name": "get_blast_radius", "arguments": { "symbol": "my_function" } }
```

### get_call_chain
Shortest path between two functions.
```json
{ "name": "get_call_chain", "arguments": { "from": "caller", "to": "callee" } }
```

### trace_dependency
Everything a symbol depends on (recursive).
```json
{ "name": "trace_dependency", "arguments": { "symbol": "my_function" } }
```

### get_coupling_radar
Files that co-change with this one.
```json
{ "name": "get_coupling_radar", "arguments": { "symbol": "my_file.rs" } }
```

## Search

### semantic_search
Find code by meaning, not just names (uses ONNX embeddings).
```json
{ "name": "semantic_search", "arguments": { "query": "error handling" } }
```

### query_graph
Flexible graph query via ops-array. See `docs/quickstart-query.md`.
```json
{ "name": "query_graph", "arguments": { "spec": { "ops": [...] } } }
```

## Code Health

### find_dead_code
Potentially unreachable code. Filters trait defaults, common names. Optional semantic filtering.
```json
{ "name": "find_dead_code", "arguments": { "like": "optional query" } }
```

### suggest_refactor_targets
High-coupling, low-stability nodes.
```json
{ "name": "suggest_refactor_targets", "arguments": {} }
```

## Analysis

### explain_symbol
Human-readable summary with signature and metrics.
```json
{ "name": "explain_symbol", "arguments": { "symbol": "my_function" } }
```

### get_call_sites
All callers of a function.
```json
{ "name": "get_call_sites", "arguments": { "symbol": "my_function" } }
```

### get_context_depth
Distance from an entry point (abstraction layers).
```json
{ "name": "get_context_depth", "arguments": { "symbol": "my_function" } }
```

## Testing

### find_untested_functions
Functions with no incoming call edges.
```json
{ "name": "find_untested_functions", "arguments": { "limit": 20 } }
```

### get_test_template
Generate test scaffold for a function.
```json
{ "name": "get_test_template", "arguments": { "function": "my_function" } }
```

### get_coverage_summary
Structural coverage estimate for a module.
```json
{ "name": "get_coverage_summary", "arguments": { "module": "src/handlers/" } }
```

## Context

### get_context_for_prompt
LLM-optimized context for a symbol.
```json
{ "name": "get_context_for_prompt", "arguments": { "symbol": "my_function" } }
```

### get_code_snippet
File content around a line.
```json
{ "name": "get_code_snippet", "arguments": { "path": "src/main.rs", "line": 42 } }
```

## Architecture

### navigate_to_anchor
Trace back to architectural anchor.
```json
{ "name": "navigate_to_anchor", "arguments": { "symbol": "my_function" } }
```

### get_layered_map
Architecture slice at specific depth.
```json
{ "name": "get_layered_map", "arguments": { "layer": 1, "granularity": "file" } }
```

### compare_modules
Structural diff between two modules.
```json
{ "name": "compare_modules", "arguments": { "a": "src/auth/", "b": "src/billing/" } }
```

### architectural_observations
Cross-boundary couplings, high-fan-out modules.
```json
{ "name": "architectural_observations", "arguments": { "threshold": 0.5 } }
```

## GitOps

### get_file_diff
Uncommitted changes in a file.
```json
{ "name": "get_file_diff", "arguments": { "path": "src/main.rs" } }
```

### get_commit_history
Recent commits.
```json
{ "name": "get_commit_history", "arguments": { "limit": 10 } }
```

### get_branch_status
Current branch name.
```json
{ "name": "get_branch_status", "arguments": {} }
```

## System

### sync_state
Refresh graph from git HEAD.
```json
{ "name": "sync_state", "arguments": {} }
```

### run_enrichment
Full co-change and anchor recalculation.
```json
{ "name": "run_enrichment", "arguments": {} }
```

### export_graph_json
Dump graph for auditing.
```json
{ "name": "export_graph_json", "arguments": {} }
```

### get_agent_strategy
Strategy guide for AI agents.
```json
{ "name": "get_agent_strategy", "arguments": {} }
```

## Build Integration

### run_build
Build with toolchain error parsing.
```json
{ "name": "run_build", "arguments": { "cwd": "/path/to/project", "release": false } }
```

### run_tests
Run tests with error parsing.
```json
{ "name": "run_tests", "arguments": { "cwd": "/path/to/project", "filter": "" } }
```

### run_clippy
Run cargo clippy.
```json
{ "name": "run_clippy", "arguments": { "cwd": "/path/to/project", "fix": false } }
```
