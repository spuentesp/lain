# LAIN — Codebase Intelligence

LAIN is a code-intelligence MCP server at tool prefix `mcp__lain__*`.

## When to use lain

Use lain when the question is about **structure**:
- Where should I start reading this codebase?
- What does X depend on? Who calls X?
- If I change X, what else breaks?
- Where do we do X? (semantic search)
- What is this function/class doing?
- Is there unused code?
- What files change together?

Skip lain for simple file reads or single-line searches.

## The most useful tools

| Tool | When to use it |
|------|----------------|
| `mcp__lain__get_health` | First call in any session |
| `mcp__lain__find_anchors` | "Where should I start reading?" — most-called, most-stable symbols |
| `mcp__lain__list_entry_points` | Find `main()`s |
| `mcp__lain__explore_architecture` | High-level tree |
| `mcp__lain__get_blast_radius` | "If I change X, what breaks?" |
| `mcp__lain__trace_dependency` | "What does X depend on?" |
| `mcp__lain__semantic_search` | Find code by meaning (with body excerpts) |
| `mcp__lain__explain_symbol` | "What is this symbol?" (source + callers + callees) |
| `mcp__lain__find_dead_code` | Unused definitions |
| `mcp__lain__get_coupling_radar` | "What files change with X?" |

## Workflows

**"I'm new here, where do I start?"**
1. `mcp__lain__get_health`
2. `mcp__lain__find_anchors` limit=5
3. `mcp__lain__explain_symbol` <top anchor>
4. `mcp__lain__get_blast_radius` <top anchor>

**"Where do we do X?" (semantic)**
1. `mcp__lain__semantic_search` <natural query> limit=5
2. `mcp__lain__explain_symbol` <top result>

## Caveats

- **Cold-query latency**: first call is 5-10s. Don't panic.
- **Workspace scope**: bound to `/home/sebastian/orca/workspaces/lain/langostino`.
- **`get_call_chain` may hang** in v0.4.0 (known bug). Use `trace_dependency` and `get_blast_radius` instead.

## Don't

- Don't use `semantic_search` with literal symbol names — use natural language.
- Don't ask lain to analyze a different repo.
