# LAIN-mcp - Agent Quickstart

If you are an AI agent (Claude, Gemini, etc.) connecting to this MCP server, follow this strategy to understand the codebase efficiently.

## 1. Initialize & Verify
Start by checking the server's health and knowledge freshness.
- Call `get_health`: See which language servers are ready and repository info.
- If a language is missing, call `install_language_server(language: "ext")`.

## 2. Global Orientation (The Telescope)
Don't read files yet. Get the macro-view.
- Call `find_anchors(limit: 5)`: Identify the most foundational building blocks.
- Call `list_entry_points`: Find where the application logic begins.
- Call `explore_architecture(max_depth: 2)`: Get a topological summary.
- Call `describe_schema`: Understand the graph schema.

## 3. Targeted Exploration
Once you have a target subsystem:
- Call `get_layered_map(layer: 1, granularity: "file")`: See files inside modules.
- Use `query_graph` for prebuilt or custom queries.

## 4. Deep Reasoning
When you need to perform a task:

**For query language:** See `docs/quickstart-query.md`
- Prebuilt queries: `get_blast_radius`, `get_call_chain`, etc.
- Custom ops: `find`, `connect`, `filter`, `semantic_filter`, `group`, `sort`, `limit`

**For individual tools:** See `docs/quickstart-tools.md`
- `semantic_search` — Find code by meaning
- `get_blast_radius` — See ripple effects
- `get_call_chain` — Shortest functional path
- `find_dead_code` — Potentially unreachable code
- `explain_symbol` — Symbol summary with metrics
- And 30+ other tools

## 5. Repo Identity
To identify which repository you're working in:
- Call `get_health` — includes `RepoIdentity` parsed from git remote

## 6. Syncing State
If you make changes to the code or switch git branches:
- Call `sync_state`: Refresh the graph using Git deltas.
