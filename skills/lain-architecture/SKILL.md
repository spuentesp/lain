---
name: lain-architecture
description: Use for system-shape questions through Lain — "map this subsystem", "compare these modules", "trace the dependency chain", "how deep is this in the stack", "where is the god object", "run a raw graph query". Covers the arch and raw packages.
---

# Lain architecture — map the system

The `arch` package answers shape questions the core deliberately
does not. Load it when the question is about structure rather than
one symbol.

## Load it

Call `load_package("arch")` (or set `LAIN_TOOL_PROFILE=arch` at
config time). If the tools do not appear in your list, reconnect
the client and refetch. The `raw` package (`load_package("raw")`)
adds the low-level layer.

## The tools

- `explore_architecture` — file/module tree to a chosen depth. Start
  here for "what does this subsystem look like".
- `compare_modules` — stability and coupling of two modules side by
  side. Use for "which of these do we refactor first".
- `architectural_observations` — patterns, boundary violations,
  high-fan-out hot spots. Use for architecture review, not for one
  change.
- `trace_dependency` — everything a symbol *depends on*, recursively.
  The upstream question; the downstream one is `get_blast_radius`.
- `get_layered_map` / `get_context_depth` — slices of the stack from
  an entry point; how deep a symbol sits.
- `navigate_to_anchor` / `get_anchor_score` — climb from a leaf to
  its controlling anchor; how load-bearing one symbol is.
- `get_master_map` — staleness: when each module last synced. Read
  this before trusting a stale-looking answer.
- `suggest_refactor_targets` — complexity + stability hot spots.

`raw` package, when the high-level tools are not enough:
`query_graph` (JSON ops-array against the graph — read
`describe_schema` first), `get_code_snippet`, `get_call_sites`
(every call with its line), `explain_symbol`,
`get_context_for_prompt`, `get_cross_runtime_callers`,
`semantic_search` (needs a model).

## Rules of thumb

- Depth fields take a number (`3`) or a range (`"1..3"`).
- A layered map is for *shape*; do not use it to answer "who calls
  this" — that is `get_call_chain`.
- `query_graph` is the escape hatch, not the first tool. Its schema
  is in `describe_schema`; prefer the named tools unless you need a
  traversal nobody anticipated.
