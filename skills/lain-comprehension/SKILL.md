---
name: lain-comprehension
description: Use when reading, locating, or assessing code through Lain's MCP tools — "where is X", "explain this", "what breaks if I change this", "what should I read first", "is this answer trustworthy". Covers the default core surface and the honest-answer contract.
---

# Lain comprehension — orient, understand, assess

The default Lain surface is small on purpose: these tools answer
almost every reading question. Reach for them in this order.

## The flow

1. **First contact** — `understand_repository` once. It returns
   identity, top anchors, entry points, and capability states in one
   payload. Do not scatter exploratory calls before it.
2. **Locate** — `find_symbol` when you know the name (it returns
   every match with ids and paths); `search_code` when you know the
   idea ("where is auth handled").
3. **Understand** — `get_context` on one symbol: definition, callers,
   callees, source excerpt. This is the "quote it back" payload.
4. **Before editing** — `assess_change` (dependents + untested
   dependents + a low/medium/high risk verdict). Use `get_blast_radius`
   only when you want the raw dependent list instead of the verdict,
   and `get_call_chain from=A to=B` when you need one exact path.
5. **Read strategically** — `find_anchors` ("what is load-bearing"),
   `list_entry_points` ("where does execution start"),
   `get_coupling_radar` ("what changes together") before refactors,
   `find_dead_code` before cleanup. `find_related` bundles
   graph + co-change + semantic neighbours for "what is connected to X".

## The honesty contract

`explain_dispatch` exists for the trap case: an impact query that
returns *surprisingly little*. When `get_blast_radius` looks wrong,
call `explain_dispatch` — its verdict distinguishes "nothing calls
this" from "static analysis cannot see the dispatcher"
(`insufficient_evidence`). Treat `insufficient_evidence` as **do not
assume safe**, and say so in your answer. Likewise "still indexing"
(`warming_up`, `retry_after_ms`) means *retry later*, not *empty*.

## When you need more

Testing, git state, raw graph queries, architecture deep-dives, and
multiplayer plumbing live in packages beyond the default: call
`list_packages`, then `load_package("<package>")`, then refetch tools
(or reconnect the client) — see `lain-verify`, `lain-architecture`,
`lain-coordination`, `lain-federation`.
