# Dynamic Dispatch Mitigation

Lain's static graph (Tree-sitter + language servers) cannot follow
dynamic dispatch. A function called only via `bus.publish(...)` or
`container.resolve(...)` has zero `Calls` edges even though every
subscriber transitively depends on its signature. `get_blast_radius`
correctly returns an empty list — and an agent that reads "0 callers"
as "0 impact" will mutate the function and ship a regression.

This document describes the three-tier mitigation that turns the
empty blast radius into a *signal* rather than a *void*.

## The problem, in one sentence

Static analysis sees the syntactic call site, not the runtime
receiver. Dynamic dispatch hides the receiver behind indirection
that Tree-sitter and LSP were never designed to resolve.

| Pattern | What static analysis sees | What actually happens at runtime |
|---|---|---|
| `bus.publish('orders.v1', payload)` | A method call on `bus` | Every subscriber registered for the topic gets invoked |
| `container.resolve('user_service')` | A method call on `container` | Whichever implementation is registered gets invoked |
| `@app.get('/orders/{id}')` | A decorator | An HTTP route whose handler is wired at startup |
| `serde_json::Value` | A type | A boxed value dispatched by string key |

The first three produce missing blast-radius edges; the fourth
prevents the type checker from resolving the receiver in the first
place. Every one of them is invisible to `get_blast_radius` until
the agent does something else.

## The three tiers

The mitigation is layered; each tier narrows the false-negative
window the previous one left.

### Tier 1 — protocol composition

**No new code; no new edges. Pure prompt + CLI plumbing.**

An agent that calls `get_blast_radius` and gets an empty list must
not conclude "no impact". It must compose with:

1. `get_coupling_radar` on the touched paths — git co-change partners
   are likely coupled even when no import exists.
2. `find_anchors` on the touched file — high-fan-in symbols are
   likely dispatch hubs.
3. `trace_dependency` for the dispatcher if the symbol sits near a hub.
4. `explain_dispatch <symbol>` — synthesizes everything below into a
   single `verdict` field. See [the tool](#tier-3--the-explain_dispatch-tool).

If all four are empty, treat as **no static evidence** and run the
smoke command before merging. The `get_agent_strategy` tool returns
this protocol as part of its decision-flow section.

**Operator files:**

- [`docs/dynamic-boundaries.md`](dynamic-boundaries.md) — per-repo
  registry of known dispatch points. Maintain manually.
- `hooks/claude-code/pre-commit.sh` — refuses to commit when
  `LAIN_SMOKE_CMD` is set and the smoke fails. Otherwise just runs
  the existing `lain hooks overlap-check`.

### Tier 2 — heuristic edges

**New edge types and a regex-based sensor.**

The `dynamic_dispatch_sensor` scans the workspace and emits
heuristic edges for files that match one of five convention patterns:

| Detector | Confidence | Patterns |
|---|---:|---|
| `message_bus_publisher` | 0.7 | `bus.publish`, `kafka.send`, `EventEmitter.emit`, `producer.send` |
| `message_bus_subscriber` | 0.7 | `bus.subscribe`, `EventBus.on`, `@consumer.listen` |
| `container_resolve` | 0.6 | `container.resolve`, `@inject`, `provider.get` |
| `schema_router` | 0.5 | `@app.get`, `@router.post`, FastAPI/Express/gRPC decorators |
| `serde_value` | 0.4 | `serde_json::Value`, `Box<dyn Any>`, `interface{}`, `dynamic` |

Each edge carries a `provenance` field with the detector name and
the per-edge confidence. The target is a synthetic `Hub:<detector>`
node so a publisher→hub→subscriber chain exists in the graph even
when the receiver can't be resolved.

`get_blast_radius` honours these edges when `include_weak_edges=true`
or when the per-edge confidence clears `LAIN_HEURISTIC_MIN_CONFIDENCE`
(default 0.5). Below-threshold edges are filtered by default to keep
the default view clean.

### Tier 3 — runtime observations

**The honest answer when static + heuristic still say "I don't know".**

The `runtime_trace` module holds a process-global
`RuntimeTraceStore`. Future OTLP adapters (or test fixtures) feed
`SpanRecord`s into it; each parent→child span pair with both
endpoints resolvable in the static graph becomes a `RuntimeCall`
edge with `provenance = Runtime { trace_id, last_seen_unix }` and a
TTL configured by `LAIN_TRACE_TTL_SECS` (default 3600).

The OTLP gRPC listener itself is not shipped in this milestone —
the heavy `tonic` + `opentelemetry-*` deps are deferred to a
follow-up PR. The store API is the contract adapters must fulfil.

## The `explain_dispatch` tool

`explain_dispatch <symbol>` is the user-facing answer to "do I
actually *know* everything that touches this?". It returns four
parallel signal arrays plus a single `verdict`:

| Verdict | Meaning |
|---|---|
| `static_only` | Static graph sees callers; heuristic and runtime are empty. |
| `heuristic_only` | Only the heuristic sensor matched. Treat with care — patterns can false-positive. |
| `runtime_only` | Only OTLP spans observed the dispatch. Strongest signal but limited to recent activity. |
| `runtime_confirmed` | Both static and runtime agree. Highest confidence. |
| `insufficient_evidence` | Every signal is empty. **This is the case Tier 1 teaches agents to refuse to treat as safe.** |

The tool is in the curated `semantic` tool profile (`README.md`
and `tools/list` by default).

### When to call it

Call `explain_dispatch` whenever:

- `get_blast_radius` returns empty and you're about to mutate the symbol.
- You are debugging a regression in a file that uses message buses
  or DI containers.
- The repo's `dynamic-boundaries.md` lists the touched file as a
  known dispatch point.

### Output shape

```text
Dispatch summary for 'handle_order' (src/api/handle_order.py):
  verdict: runtime_confirmed
  static_callers:    0
  heuristic_callers: 1
  runtime_callers:   1
  co_change_partners:2

--- JSON ---
{
  "symbol": "handle_order",
  "target_id": "...",
  "target_path": "src/api/handle_order.py",
  "static_callers": [],
  "heuristic_callers": [
    { "target_id": "...", "detector": "message_bus_subscriber", "confidence": 0.7 }
  ],
  "runtime_callers": [
    { "source_id": "...", "trace_id": "abc123", "last_seen_unix": 1734567890 }
  ],
  "co_change_partners": ["src/api/orders.py", "src/workers/order_worker.py"],
  "verdict": "runtime_confirmed"
}
```

## The `dynamic-boundaries.md` file

Per-repo registry of known dispatch points. Place at
`docs/dynamic-boundaries.md` at the repo root. The
[`docs/dynamic-boundaries.md`](dynamic-boundaries.md) file in this
repo (lain itself) is the template; copy and adapt.

Keep the entries to one row per boundary. For each bus, container,
or router, list the producer/resolver/handler files and a one-line
note about the convention. Updating this file when adding a new
dispatch point is a manual discipline — there is no automated check.

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `LAIN_HEURISTIC_MIN_CONFIDENCE` | `0.5` | Minimum confidence a heuristic edge needs to appear in `get_blast_radius` without `include_weak_edges=true`. Clamped to `[0.0, 1.0]`. |
| `LAIN_TRACE_TTL_SECS` | `3600` | TTL for runtime edges in `RuntimeTraceStore`. Expired edges are purged on the next sweep. |
| `LAIN_TRACE_MAX_EDGES` | `100000` | Maximum number of runtime edges the store keeps in memory. When exceeded, oldest are dropped first. |

`LAIN_TRACE_RUNTIME=true` is reserved for the future OTLP listener
adapter; the store is always available via
`RuntimeTraceStore::global()`.

## CLI: backfilling heuristic edges

```bash
lain hooks backfill-heuristics --workspace /path/to/repo [--graph path] [--dry-run]
```

Re-runs the heuristic sensor against an existing
`.lain/graph.bin` without paying for a full re-index of static
edges. Idempotent: re-running on the same workspace adds zero new
edges because the sensor emits deterministic UUID v5 identifiers.

Use after upgrading a lain install that predates Tier 2, or after
adding a new detector to the sensor's regex table.

## What this mitigation does NOT solve

- **Type-level dynamic dispatch** (Rust trait objects behind
  `Box<dyn Trait>`, Swift `Any`, Java interfaces) — the static
  graph plus LSP get partial coverage. Tier 3's runtime traces
  cover what static cannot.
- **Schema-driven configurations** (route tables in YAML, OpenAPI
  specs, gRPC service definitions in `.proto`). Tier 2 catches
  decorators but not external schema files. The OTLP ingest path
  is the right escape hatch here.
- **Reflection at runtime** (Python `getattr`, Ruby
  `method_missing`). The static graph cannot model these; runtime
  traces catch them when the application is exercised.

## See also

- [`docs/dynamic-boundaries.md`](dynamic-boundaries.md) — the
  per-repo template.
- The `get_agent_strategy` MCP tool — the agent-facing rules.
- `src/server/runtime_trace/` — the in-memory store.
- `src/server/sensors/dynamic_dispatch_sensor.rs` — the detector
  implementation.
