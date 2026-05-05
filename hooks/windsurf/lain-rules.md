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