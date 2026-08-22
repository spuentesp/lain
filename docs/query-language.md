# Lain Query Language — Machine Reference

Query the LAIN knowledge graph via `query_graph` MCP tool.

> **Federation note:** cross-repo queries (`get_cross_repo_blast_radius`, `search_org`) require federation mode (`lain server --config repos.yaml`). See [`docs/FEDERATION.md`](./FEDERATION.md) for the full guide.

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
| `type` | string \| `["Type1", "Type2"]` | Node type — see [Node Types](#node-types). Omit to match every kind |
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
| `edge` | `Calls`, `Contains`, `Uses`, `Implements`, `Imports`, `CoChangedWith`, `Pattern`, `CallsHttp`, `Produces`, `Consumes`, `DeployedTo`, `CrossRepoSameSymbol` |
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
    { "op": "connect", "edge": "CoChangedWith", "direction": "both", "depth": 1 }
  ]
}
```

### Get all untested functions in a module

```json
{
  "ops": [
    { "op": "find", "type": "Function", "path": "src/handlers/" },
    { "op": "connect", "edge": "Calls", "direction": "incoming", "depth": 0 },
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

These are generated from the `EdgeType` enum, so `describe_schema` and
this table cannot drift from what the indexer actually emits.

| Edge | Meaning |
|------|---------|
| `Calls` | Function invocation |
| `Contains` | File/module contains a symbol |
| `Uses` | Code uses a variable or type |
| `Implements` | Class implements an interface |
| `Imports` | Import/use statement |
| `CoChangedWith` | Historical co-change (temporal coupling, not a static dependency) |
| `Pattern` | Semantic boundary indicator (path prefix, topic name) |
| `CallsHttp` | HTTP route → handler |
| `Produces` / `Consumes` | Producer/consumer ↔ message topic |
| `DeployedTo` | IaC resource → cloud resource |
| `CrossRepoSameSymbol` | Federation-only: same symbol in two repos |

## Node Types

`File`, `Namespace`, `Module`, `Package`, `Class`, `Interface`, `Struct`,
`Enum`, `Trait`, `Function`, `Method`, `Property`, `Variable`, `Constant`,
`HttpRoute`, `Topic`, `Resource`, `Schema`

`Function` and `Method` are distinct. In Rust and other impl-heavy
languages most code is a `Method`, so a query filtered to `Function`
alone silently misses it — call `describe_schema` if in doubt, and note
that `find` without a `type` matches every kind.
