# Quickstart for AI Agents using Lain

If you are an AI agent (Claude, Gemini, etc.) connecting to this MCP server, follow this strategy to understand the codebase efficiently.

## 1. Initialize & Verify
Start by checking the server's health and knowledge freshness.
- Call `get_health`: See which language servers are ready.
- Call `get_master_map`: See if the knowledge base is stale.
- If a language is missing, call `install_language_server(language: "ext")`.

## 2. Global Orientation (The Telescope)
Don't read files yet. Get the macro-view.
- Call `get_layered_map(layer: 0, granularity: "module")`: Identify the root modules.
- Call `find_anchors(limit: 5)`: Identify the most foundational building blocks (stable nodes with high fan-in).
- Call `list_entry_points`: Find where the application logic begins.

## 3. Targeted Exploration
Once you have a target subsystem:
- Call `get_layered_map(layer: 1, granularity: "file")`: See the files inside the modules you identified.
- Call `explore_architecture(max_depth: 2)`: Get a topological summary.

## 4. Deep Reasoning
When you need to perform a task:
- **Semantic Search**: Use `semantic_search(query: "intent")` to find code by meaning, not just names.
- **Impact Analysis**: Use `get_blast_radius(symbol: "Name", include_coupling: true)` to see the ripple effects of a change.
- **Dependency Tracing**: Use `get_call_chain(from: "A", to: "B")` to find the shortest functional path.
- **Detailed Summary**: Use `explain_symbol(symbol: "Name")` for a "God-view" of a single symbol.

## 5. Syncing State
If you make changes to the code or switch git branches:
- Call `sync_state`: This refreshes the graph using Git deltas and updates the "Staleness Report."
