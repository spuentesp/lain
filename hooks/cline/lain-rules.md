# LAIN — Query for structure. Use tools only when query can't answer.

Rule: query for structure. Use MCP tools only when query can't answer.

## Query Syntax

```
lain query "find TYPE [name PATTERN] | connect EDGE [DIRECTION] depth N | limit N"
```

Types: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`
Edges: `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`

Full reference: `docs/query-language.md`

## Most Queries Use This Pattern

```bash
lain query "find Function name X | connect Calls direction incoming depth 1..=2"   # callers
lain query "find Function name X | connect Calls direction outgoing depth 1..=3"    # callees
lain query "find File name X | connect CO_CHANGED_WITH"                             # co-change
lain query "find Function | limit 20"                                                # overview
```

## MCP Tools (non-query operations only)

- `semantic_search` — meaning-based code search
- `get_code_snippet` — read source code
- `find_dead_code` — unused definitions
- `get_cross_runtime_callers` — cross-language callers
- `get_file_diff`, `get_commit_history` — git operations