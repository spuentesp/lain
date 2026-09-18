# Dynamic Dispatch Boundaries

> Per-repo registry of code regions where the static graph **cannot**
> resolve callers or callees. Anything listed here overrides the default
> rule "empty `get_blast_radius` ⇒ no impact".
>
> Read by AI agents before mutating any symbol whose blast radius is
> empty. Maintained by humans, kept short.

## Why this file exists

Lain indexes your codebase with Tree-sitter plus language servers. Both
see syntax and types; neither sees **dynamic dispatch**. The patterns
below produce edges that are invisible to the static graph:

- message buses (`bus.publish`, `EventEmitter.emit`, `kafka.send`)
- DI containers (`container.resolve`, `provider.get`, `@inject`)
- schema-driven routers (FastAPI decorators, Express handlers, gRPC
  `rpc`, OpenAPI `x-router`)
- reflection (`serde_json::Value`, `Box<dyn Any>`, `interface{}`,
  `dynamic`, `*args`/`**kwargs`)

If a touched file sits behind one of these patterns, "no callers" is
not evidence of safety. Mark the file here so the agent treats empty
`get_blast_radius` as **insufficient evidence**, not **no impact**.

## How to use it

1. Run `get_blast_radius` on the symbol you intend to change.
2. If the result is empty, look up the touched file below.
3. If the file is listed → run the broader smoke command before merging.
4. If the file is **not** listed → still run `get_coupling_radar` and
   `find_anchors`; only proceed without a smoke run if all three are
   empty. See `get_agent_strategy` → "Dynamic Dispatch Caveat".

## Registry

### Message buses

| Bus / topic | Producer file(s) | Consumer file(s) | Notes |
|---|---|---|---|
| _example_ | `src/events/order_bus.py` | `src/workers/order_worker.py` | kafka topic `orders.v1` |

### DI containers

| Container | Resolve sites | Notes |
|---|---|---|
| _example_ | `src/di.py::resolve_user_service` | singleton, registered in `app.py` |

### Schema-driven routers

| Router | Decorator / convention | Handler file(s) | Notes |
|---|---|---|---|
| _example_ | `@app.get("/orders/{id}")` | `src/api/orders.py` | FastAPI |

### Reflection / dynamic dispatch

| Pattern | Where | Notes |
|---|---|---|
| _example_ | `serde_json::Value` in `src/parser/mod.rs` | event payload, fields dispatched by `kind` string |

## Conventions

- Keep entries to one row per boundary. If a boundary spans many files,
  link to a directory listing rather than enumerating.
- Update this file when you add a new message bus, container entry
  point, or schema-driven route. The pre-commit hook does not enforce
  it; this is a manual discipline.
- This file is **not** a substitute for the static graph. It is the
  list of places where the static graph is known to be incomplete.
