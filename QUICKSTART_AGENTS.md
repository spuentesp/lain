# LAIN-mcp - Agent Quickstart

If you are an AI agent (Claude, Gemini, etc.) connecting to this MCP server, follow this strategy to understand the codebase efficiently.

## 1. Initialize & Verify
Start by checking the server's health and knowledge freshness.
- Call `get_health`: See which language servers are ready.
- Call `get_master_map`: See if the knowledge base is stale.
- If a language is missing, call `install_language_server(language: "ext")`.

## 2. Global Orientation (The Telescope)
Don't read files yet. Get the macro-view.
- Call `find_anchors(limit: 5)`: Identify the most foundational building blocks (stable nodes with high fan-in).
- Call `list_entry_points`: Find where the application logic begins.
- Call `explore_architecture(max_depth: 2)`: Get a topological summary.
- Call `describe_schema`: Understand the graph schema (node types, edge types).

## 3. Targeted Exploration
Once you have a target subsystem:
- Call `get_layered_map(layer: 1, granularity: "file")`: See the files inside the modules you identified.
- Call `query_graph` with the `named` field for prebuilt queries (see `docs/query-language.md`).

## 4. Deep Reasoning
When you need to perform a task:
- **Semantic Search**: Use `semantic_search(query: "intent")` to find code by meaning, not just names.
- **Impact Analysis**: Use `get_blast_radius` (or `query_graph` with `named: "get_blast_radius"`) to see ripple effects.
- **Dependency Tracing**: Use `get_call_chain` to find the shortest functional path.
- **Detailed Summary**: Use `explain_symbol` for a "God-view" of a single symbol.
- **Custom Queries**: Use `query_graph` with ops-array for flexible graph traversal (see `docs/query-language.md`).

## 5. Syncing State
If you make changes to the code or switch git branches:
- Call `sync_state`: Refresh the graph using Git deltas.
