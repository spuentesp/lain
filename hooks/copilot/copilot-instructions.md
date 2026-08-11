# LAIN — Codebase Intelligence

You have access to the Lain MCP server (tool prefix `mcp__lain__`). Lain indexes
the current workspace and exposes tools for structural queries, semantic
search, and code navigation.

## When to use lain

Reach for lain when the question is about **structure**, not single lines:
- "Where should I start reading this codebase?"
- "What does X depend on?" / "Who calls X?"
- "If I change X, what else breaks?"
- "Where do we do X?" (semantic search by meaning)
- "What is this function/class doing?"
- "Is there unused code?"
- "What files change together?"
- "Which test covers X?" / "What's untested?"

Skip lain for simple file reads, single-line `grep`, or trivial edits.

## The most useful tools

| Tool | When to use it |
|------|----------------|
| `get_health` | First call in any session. Returns the resolved workspace, node counts, and status. |
| `find_anchors` | "Where should I start reading?" — most-called, most-stable symbols. |
| `list_entry_points` | Find `main()`s and entry points. |
| `explore_architecture` | High-level module/file tree. |
| `get_blast_radius` | "If I change X, what breaks?" — transitive callers. |
| `trace_dependency` | "What does X depend on?" — callees + imports. |
| `get_call_chain` | Specific call path between two symbols. |
| `semantic_search` | Find code by meaning (e.g. "rate limiting", "retry logic"). Body excerpts included. |
| `explain_symbol` | "What is this symbol?" — source + callers + callees + anchor. |
| `get_code_snippet` | Read the exact source of a symbol by id. |
| `find_dead_code` | Unused definitions (filters false positives). |
| `get_coupling_radar` | "What files change with X?" (co-change history). |
| `get_cross_runtime_callers` | Cross-language callers (e.g. Python → Rust FFI). |
| `find_test_file` / `find_untested_functions` / `get_coverage_summary` | Test discovery and coverage. |
| `get_file_diff` / `get_commit_history` | Git operations on the workspace. |

## Workflows

**"I'm new here, where do I start?"**
1. `get_health` — confirm the workspace resolved correctly.
2. `find_anchors limit=5` — top entry points.
3. `explain_symbol <top anchor>` — understand it.
4. `get_blast_radius <top anchor>` — see what depends on it.

**"I'm about to refactor X"**
1. `get_blast_radius <X>` — who will break.
2. `get_coupling_radar <X>` — what else usually changes with it.
3. `find_test_file <X>` (or `find_untested_functions`) — what's already covered.
4. Make the change, then re-run `get_blast_radius` to verify no surprises.

**"Where do we do X?" (semantic)**
1. `semantic_search query="<natural language>" limit=5` — get candidates with body excerpts.
2. `explain_symbol <top result>` — read it in context.

**"What calls X?" / "What does X call?"**
- Callers (incoming): `get_blast_radius <X>`
- Callees (outgoing): `trace_dependency <X>`

**"Read this symbol"**
- `explain_symbol <X>` for full context, or `get_code_snippet <X>` for raw source.

## Caveats

- **First-call latency**: the very first tool call after a fresh server start can take 5–10s (model warmup). Don't panic.
- **Workspace scope**: bound to the git repository you opened this session in (auto-discovered from the working directory). Lain cannot analyze a different repo.
- **Semantic search needs a query model**: if `get_health` reports no embedding model, `semantic_search` will not work — fall back to `explain_symbol` / `find_anchors` and the query language.

## Don't

- Don't use `semantic_search` with literal symbol names; use natural language describing the concept.
- Don't call lain tools against a path outside the workspace.
- Don't repeatedly call `get_health`; once per session is enough.
