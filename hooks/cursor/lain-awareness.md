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