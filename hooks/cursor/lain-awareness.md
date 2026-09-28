# LAIN — Query for structure. Use tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

## Quick Examples

```bash
lain query "find Function | limit 10"
lain query "find Class name User | connect Calls direction outgoing depth 1..=2"
lain query "find File name main.rs | connect Contains | connect Calls"
lain query "find Function name test_ | filter label deprecated"
```

Full reference: `docs/query-language.md`

## When NOT to query (use MCP tools)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source code
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers

## Dynamic Dispatch Caveat

An empty `get_blast_radius` means **no static edges found**, not **no impact**. Before mutating any symbol whose blast radius is empty, run **all three**:
1. `get_coupling_radar` on the touched paths (git co-change partners).
2. `find_anchors` on the file to surface nearby high-fan-in hubs.
3. Check `docs/dynamic-boundaries.md` in the repo for documented dispatch points (message buses, DI containers, schema-driven routers).

If the file is not in `dynamic-boundaries.md` and all three queries are empty, proceed with the smoke command. Otherwise treat the result as **evidence insufficient** and require a broader smoke run before the mutation lands on disk.

You can also call `explain_dispatch <symbol>` for a single-shot synthesis with a `verdict` field — `insufficient_evidence` is the explicit "do not assume safe" signal.

Full reference: `docs/dynamic-dispatch.md` in the lain repo.