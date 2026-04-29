# LAIN-mcp - Query Language Quickstart

The `query_graph` tool exposes a JSON-based ops array for flexible graph traversals.

## Basic Concept

Chain ops together to build complex queries:
1. `find` — locate nodes
2. `connect` — traverse edges
3. `filter` — narrow results
4. `semantic_filter` — filter by meaning
5. `group` — group results
6. `sort` — order results
7. `limit` — paginate

## Your First Query

Find all functions named "handle":

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "handle" }
  ]
}
```

## Chain Operations

Find functions named "handle" and their callers:

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "handle" },
    { "op": "connect", "edge": "Calls", "direction": "incoming", "depth": 1 },
    { "op": "limit", "count": 20 }
  ]
}
```

## Semantic Filtering

Find code semantically similar to "error handling":

```json
{
  "ops": [
    { "op": "find", "type": "Function" },
    { "op": "semantic_filter", "like": "error handling", "threshold": 0.35 },
    { "op": "limit", "count": 20 }
  ]
}
```

## Named Queries

Prebuilt queries via `named` field:

| Name | Description |
|------|-------------|
| `get_blast_radius` | Everything affected by a change |
| `get_call_chain` | Shortest path between functions |
| `get_callers` | Who calls this function |
| `get_callees` | What this function calls |
| `get_file_functions` | Functions in a file |
| `get_function_imports` | What a function imports |
| `get_module_functions` | Functions in a module |
| `get_test_coverage` | Tests for a function |
| `get_deprecated_functions` | Deprecated functions |

```json
{ "named": "get_blast_radius" }
```

## Available Ops

### find
```json
{ "op": "find", "type": "Function", "name": "foo", "label": "test" }
```

### connect
```json
{ "op": "connect", "edge": "Calls", "direction": "outgoing", "depth": 3 }
```

### filter
```json
{ "op": "filter", "type": "Function", "label": "deprecated" }
```

### semantic_filter
```json
{ "op": "semantic_filter", "like": "error handling", "threshold": 0.35 }
```

### group
```json
{ "op": "group", "by": "type" }
```

### sort
```json
{ "op": "sort", "by": "name", "direction": "asc" }
```

### limit
```json
{ "op": "limit", "count": 10, "offset": 0 }
```

See `docs/query-language.md` for full reference.
