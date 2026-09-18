# LAIN — Query for structure. Use tools only when query can't answer.

Rule: before any structural edit, query LAIN first.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

## Query for Structure. Use MCP Tools for Everything Else.

| Need | Command |
|------|---------|
| Who calls X? | `lain query "find Function name X \| connect Calls direction incoming"` |
| What does X call? | `lain query "find Function name X \| connect Calls direction outgoing depth 1..=2"` |
| Blast radius | `lain query "find Function name X \| connect Calls direction outgoing depth 1..=3"` |
| Co-change risk | `lain query "find File name X \| connect CO_CHANGED_WITH"` |
| Find by meaning | `semantic_search` tool |
| Read code | `get_code_snippet` tool |
| Find dead code | `find_dead_code` tool |

## Dynamic Dispatch Caveat

An empty `get_blast_radius` means **no static edges found**, not **no impact**. Before mutating any symbol whose blast radius is empty, run **all three**:
1. `get_coupling_radar` on the touched paths (git co-change partners).
2. `find_anchors` on the file to surface nearby high-fan-in hubs.
3. Check `docs/dynamic-boundaries.md` in the repo for documented dispatch points (message buses, DI containers, schema-driven routers).

If the file is not in `dynamic-boundaries.md` and all three queries are empty, proceed with the smoke command. Otherwise treat the result as **evidence insufficient** and require a broader smoke run before the mutation lands on disk.

You can also call `explain_dispatch <symbol>` for a single-shot synthesis with a `verdict` field — `insufficient_evidence` is the explicit "do not assume safe" signal.

Full reference: `docs/dynamic-dispatch.md` in the lain repo.
