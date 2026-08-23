# LAIN-mcp - Tools Quickstart

Quick reference for LAIN MCP tools.

> Haven't configured Lain for your project yet? See the Quick Start
> in `README.md` — `lain repos add`, `lain workspaces create`, and
> `lain server --config ./repos.yaml`.

## Project Management (CLI)

A project is a directory containing `repos.yaml` (and optionally
`workspaces.yaml`). Manage it directly with the CLI:

```bash
lain repos add <name> <url>             # register a repo in repos.yaml
lain repos list                          # show registered repos
lain repos remove <name>                 # unregister a repo
lain workspaces create <name> --members a,b,c   # declare a named workspace
lain workspaces list                     # show all workspaces
lain workspaces show <name>              # print one workspace
lain workspaces use <name>                # activate a workspace (writes ~/.config/lain/active_workspace)
lain workspaces current                  # print the active workspace
lain workspaces forget <name>            # remove a workspace
```

A workspace is loaded by `lain server` when started with `--workspace auto`
(or `--workspace <name>` to pin it). Both `repos.yaml` and
`workspaces.yaml` are hot-reloaded while the server runs — see
[`docs/hot-reload.md`](hot-reload.md). The full config schema is in
[`docs/REPOS_YAML.md`](REPOS_YAML.md); the operating model for
federation mode is in [`docs/FEDERATION.md`](FEDERATION.md).

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

The report separates **direct dependents** (they call or use the symbol
themselves) from **indirect** ones (they only reach it through those
callers, each tagged `[depth N]`):

```
Blast radius for 'canonical_claim_path':
- Direct dependents (3):
  - claim_in_memory (Method) in src/server/presence.rs
  - release (Method) in src/server/presence.rs
  - list_all (Method) in src/server/presence.rs
- Indirect dependents (431), reaching it only through the callers above; deepest chain 7 levels:
  - run_list_occupancy (Function) in src/server/mcp/presence_tools.rs [depth 2]
  - explain_symbol (Function) in src/server/tools/handlers/metrics.rs [depth 2]
  …18 more names…
  ... and 411 more indirect, by depth:
  - depth 2: 18
  - depth 3: 96
- Total transitively affected nodes: 434
```

Read the total as reach, not as work. Transitive closure through a
central dispatcher is large and still correct — a three-caller helper
legitimately reports hundreds. The three lines you act on are the direct
ones; that is why they are listed first and separately.

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
Find code by meaning, not just names. Uses local ONNX embeddings with hybrid scoring (cosine similarity + stemmed token-overlap) and shows body excerpts in the response.

Best results with the **BGE** model family (`bge-small-en-v1.5` recommended); set `query_prefix` in `.lain/tuning.toml` for asymmetric retrieval — it is applied to queries only, never to the indexed corpus, and covers every tool that embeds a query (`semantic_search`, `find_dead_code --like`, and `semantic_filter` in `query_graph`).

```json
{ "name": "semantic_search", "arguments": { "query": "error handling", "limit": 5 } }
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
Human-readable summary with signature, body excerpt, anchor score, depth, co-change partners, and a **Call Graph** section listing callers and callees. The most useful tool for "what is this symbol?" — answers the three questions in one call.
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
Trace back to architectural anchor. If no more-foundational anchor is reachable from the input symbol, returns the corpus's overall top anchor as a fallback so the user always gets a useful pointer.
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

---

## Multiplayer

Coordination tools, for when more than one agent shares the repo. Full
contract in [`multiplayer.md`](multiplayer.md).

### register_agent
Once at startup. Returns `agent_id`, `session_token`, `expires_at_unix`.
```json
{ "name": "register_agent", "arguments": { "name": "claude", "kind": "claude-code" } }
```
No heartbeat loop is needed — any authenticated call refreshes the
session.

### claim_files
Before editing. Returns `granted`, `conflicts` (refused — someone else
holds it) and `advisories` (granted, but someone is editing it anyway).
```json
{ "name": "claim_files", "arguments": {
    "agent_id": "…", "session_token": "…",
    "files": [{ "path": "src/auth.rs", "symbols": ["login"], "intent": "edit" }] } }
```
Paths are canonicalized, so `src/auth.rs`, `./src/auth.rs` and the
absolute form are one claim.

### release_files / my_claims / list_occupancy / who_am_i
```json
{ "name": "list_occupancy", "arguments": {} }
```

---

## Reading the answers

A few tools are easy to over-trust. What they actually mean:

| Tool | Reports | Does **not** mean |
|------|---------|-------------------|
| `find_dead_code` | No incoming `Calls` edges **and** no textual reference anywhere in the workspace | "Safe to delete." It excludes tests, unindexed files, and any symbol whose name appears elsewhere in the tree — and reports each exclusion count. The textual sweep exists because `Calls` edges depend on how far indexing got: on a partial index the call graph alone reported 9 dead symbols of which 7 were called from another file. A name reached only through a macro-built identifier is still invisible to both checks. |
| `get_blast_radius` | Transitive `Calls`/`Uses` dependents, split into direct and indirect | "Everything in these files", and the total is not a work estimate. It deliberately does not follow `Contains`; a file is not a dependent of its own symbols. Reach through a central dispatcher is genuinely large — act on the **direct** list. |
| `get_call_sites` | The exact lines where the symbol is called, grouped by calling function | "All callers." A call inside a macro argument may not be indexed. Lines come from scanning the caller's body, so a call written differently than the symbol's name (via an alias or a trait object) is not located. |
| `find_anchors` | Orchestration hubs — called by many, calling many, with a real body. Each row names the node's type and path | "Most important." A leaf that calls nothing scores 0 by design. Results are deduped by name keeping the best-scoring definition, so read the path to see which one it means. |
| `explain_symbol` / `get_anchor_score` / `get_call_sites` / `get_blast_radius` by **name** | One node that has that name, chosen deterministically (by path), with a `⚠` line naming the other definitions and their ids when the name is not unique | "The only node with that name." The warning tells you the answer is about one of several; pass a node **id** (from `query_graph`, or from the warning itself) to pick a different one. Ids round-trip between all these tools. |
| `get_coupling_radar` | Files that change together in git history | A static dependency. It is temporal correlation. |
| `semantic_search` | Nearest neighbours by embedding | Exact matches. Use `query_graph` or `get_call_sites` for those. Needs `--embedding-model`; without it the tool says `NLP Model: Not loaded` rather than returning a wrong answer. |
| `get_cross_repo_blast_radius` | Callers across the federation (**incoming** `Calls`) | What the symbol depends on — that is `trace_dependency`. `depth` is a string range (`"1..3"`), not a number. |

When the graph is behind HEAD, `get_health` says so and "not found"
answers point at it. Treat a `Degraded ⚠` server's silence as "not in
this graph", never as "does not exist".

A "not found" against a graph with **0 nodes** says so explicitly rather
than blaming uncommitted code — usually the workspace has not finished
indexing; check `get_health`.

In federation mode, per-repo tools bind to the repo the call resolves
to: pass `repo_id`, or a `symbol` that resolves to exactly one repo.
With a single repo there is nothing to choose and it binds there
automatically. Relative paths resolve against that repo's checkout, so
`src/lib.rs` means *that* repo's `src/lib.rs`.
