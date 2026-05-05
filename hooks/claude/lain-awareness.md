# LAIN — Query for structure. Use tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

## Quick Examples

```bash
lain query "find Function name handle | connect Calls direction incoming depth 1..=2"
lain query "find Function name save | connect Calls direction outgoing depth 1..=3"
lain query "find File name db.rs | connect CO_CHANGED_WITH"
lain query "find Function | limit 20"
```

Full reference: `docs/query-language.md`

## When NOT to query (use MCP tools)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source of any symbol
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
- `get_file_diff`, `get_commit_history` — git operations
- `get_health` — server health check

## Workflow

1. **Before editing** → query for blast radius
2. **Understanding flow** → query for call chain
3. **Need source** → `get_code_snippet`
4. **Meaning search** → `semantic_search`