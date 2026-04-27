# Lain Query Language — Machine Reference

Query the LAIN knowledge graph via `query_graph` MCP tool.

---

## Tool Call

```json
{
  "name": "query_graph",
  "arguments": {
    "spec": {
      "ops": [...],
      "mode": "auto",
      "named": null
    }
  }
}
```

---

## QuerySpec

```json
{
  "ops": [...],           // Array of GraphOp (required)
  "mode": "auto",         // "query" | "tool" | "auto" (default: auto)
  "named": null           // Prebuilt query name (see Named Queries)
}
```

---

## Operations

### find

Locate nodes in the graph.

```json
{ "op": "find", "type": "Function", "name": "foo", "label": "test" }
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string \| `["Type1", "Type2"]` | Node type: `File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`, `Variable`, `Constant` |
| `name` | string | Exact name (or object with glob/startsWith/endsWith) |
| `id` | string | Node UUID |
| `label` | string \| `["L1"]` \| `{ "not": ["L1"] }` | Node label |
| `path` | string | File path containing the node |

Name selector object:
```json
{ "name": { "exact": "foo" } }
{ "name": { "glob": "foo*" } }
{ "name": { "startsWith": "test_" } }
{ "name": { "endsWith": "_test" } }
```

### connect

Traverse edges from found nodes.

```json
{ "op": "connect", "edge": "Calls", "direction": "outgoing", "depth": 3 }
```

```json
{ "op": "connect", "edge": "Calls", "direction": "incoming", "depth": { "min": 1, "max": 2 }, "target": { "type": "Function" } }
```

| Field | Description |
|-------|-------------|
| `edge` | `Calls`, `Contains`, `Defines`, `Inherits`, `Imports`, `CO_CHANGED_WITH`, `TestedBy`, `AnchoredAt` |
| `direction` | `outgoing` \| `incoming` \| `both` |
| `depth` | Integer or `{ "min": N, "max": M }` |
| `target` | Optional nested `FindOp` — only follow edges to matching nodes |

### filter

Narrow current result set.

```json
{ "op": "filter", "type": "Function", "name": "test_*", "label": "deprecated" }
```

### group

Group results.

```json
{ "op": "group", "by": "type" }
```

`by`: `type` \| `label` \| `name`

### sort

Order results.

```json
{ "op": "sort", "by": "name", "direction": "asc" }
```

`by`: `name` \| `type` \| `label`
`direction`: `asc` \| `desc`

### limit

Paginate.

```json
{ "op": "limit", "count": 10, "offset": 0 }
```

---

## Named Queries

Pass `named` instead of `ops` for prebuilt queries:

```json
{ "named": "get_blast_radius" }
{ "named": "get_call_chain" }
{ "named": "get_callers" }
{ "named": "get_callees" }
{ "named": "get_file_functions" }
{ "named": "get_function_imports" }
{ "named": "get_module_functions" }
{ "named": "get_test_coverage" }
{ "named": "get_deprecated_functions" }
```

---

## Result Format

```json
{
  "nodes": [{ "id": "...", "type": "Function", "name": "foo", "label": null }],
  "edges": [{ "id": "...", "type": "Calls", "from": "N1", "to": "N2" }],
  "paths": [{ "nodes": [...], "edges": [...] }],
  "count": 42,
  "legacy": false,
  "meta": {
    "exec_us": 1234,
    "nodes_visited": 156,
    "plan": "find -> connect(1) -> connect(2)"
  },
  "groups": null
}
```

---

## Examples

### Find all functions named "handle" and their callers

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "handle" },
    { "op": "connect", "edge": "Calls", "direction": "incoming", "depth": 1 },
    { "op": "limit", "count": 20 }
  ]
}
```

### Get blast radius of a symbol

```json
{ "named": "get_blast_radius" }
```

With args via `ops` equivalent:
```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "my_func" },
    { "op": "connect", "edge": "Calls", "direction": "outgoing", "depth": { "min": 1, "max": 2 } },
    { "op": "connect", "edge": "Calls", "direction": "incoming", "depth": { "min": 1, "max": 2 } }
  ]
}
```

### Find files that co-change with a given file

```json
{
  "ops": [
    { "op": "find", "type": "File", "name": "auth.rs" },
    { "op": "connect", "edge": "CO_CHANGED_WITH", "direction": "both", "depth": 1 }
  ]
}
```

### Get all untested functions in a module

```json
{
  "ops": [
    { "op": "find", "type": "Function", "path": "src/handlers/" },
    { "op": "connect", "edge": "TestedBy", "direction": "incoming", "depth": 0 },
    { "op": "filter", "type": "Function" }
  ]
}
```

### Find deprecated public functions

```json
{
  "ops": [
    { "op": "find", "type": "Function", "label": { "not": ["test", "mock"] } },
    { "op": "filter", "name": { "startsWith": "deprecated_" } },
    { "op": "sort", "by": "name" },
    { "op": "limit", "count": 50 }
  ]
}
```

### Get call chain between two functions

```json
{ "named": "get_call_chain" }
```

With explicit ops:
```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "caller" },
    { "op": "connect", "edge": "Calls", "direction": "outgoing", "depth": 10, "target": { "name": "callee" } }
  ]
}
```

### Structural coverage summary for a module

```json
{ "named": "get_test_coverage" }
```

### Explain a symbol

```json
{
  "ops": [
    { "op": "find", "type": "Function", "name": "process_request" },
    { "op": "connect", "edge": "Calls", "direction": "both", "depth": 1 },
    { "op": "group", "by": "type" }
  ]
}
```

---

## Edge Types

| Edge | Meaning |
|------|---------|
| `Calls` | Function invocation |
| `Contains` | File/Module contains child |
| `Defines` | Module/scope defines symbol |
| `Inherits` | Class inheritance |
| `Imports` | Import/use statement |
| `CO_CHANGED_WITH` | Historical co-change |
| `TestedBy` | Test coverage |
| `AnchoredAt` | Anchor relationship |

## Node Types

`File`, `Module`, `Function`, `Method`, `Class`, `Interface`, `Trait`, `Variable`, `Constant`
