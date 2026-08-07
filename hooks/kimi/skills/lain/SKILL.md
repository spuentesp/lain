---
name: lain
description: |
  Structural code intelligence for AI coding agents. Use this skill
  when the user wants to understand how a codebase is organized
  (modules, call graphs, file dependencies), find where to start
  reading, trace the impact of a change, find code by meaning, or
  understand what a symbol does in its full context. Do NOT use this
  skill for simple file reading or trivial tasks that don't require
  understanding structural relationships.
---

# lain — Codebase Intelligence

The lain MCP server is available at tool prefix `mcp__lain__*`. The
server indexes the workspace and exposes tools for structural queries,
semantic search, and code navigation.

## 1. When to use lain

Prefer lain when the question is about **structure**:
- "Where should I start reading this codebase?"
- "What does X depend on?" / "Who calls X?"
- "If I change X, what else breaks?"
- "Where do we do X?" (semantic search by meaning)
- "What is this function/class doing?"
- "Is there unused code?"
- "What files change together?"

Skip lain for simple file reads or single-line searches.

## 2. The most useful tools

| Tool | When to use it |
|------|----------------|
| `mcp__lain__get_health` | First call in any session. |
| `mcp__lain__find_anchors` | "Where should I start reading?" — most-called, most-stable symbols. |
| `mcp__lain__list_entry_points` | Find `main()`s and entry points. |
| `mcp__lain__explore_architecture` | High-level module/file tree. |
| `mcp__lain__get_blast_radius` | "If I change X, what breaks?" — transitive callers. |
| `mcp__lain__trace_dependency` | "What does X depend on?" |
| `mcp__lain__semantic_search` | Find code by meaning: "error handling", "rate limiting", etc. Body excerpts included. |
| `mcp__lain__explain_symbol` | "What is this symbol?" — source + callers + callees + anchor. |
| `mcp__lain__find_dead_code` | Unused definitions (filters false positives). |
| `mcp__lain__get_coupling_radar` | "What files change with X?" |

## 3. Workflows

### "I'm new here, where do I start?"

```
1. get_health
2. find_anchors limit=5
3. explain_symbol <top anchor>
4. get_blast_radius <top anchor>
```

### "I'm about to refactor X"

```
1. get_blast_radius <X> depth=2
2. trace_dependency <X>
3. get_coupling_radar <X path>
```

### "Where do we do X?" (semantic)

```
1. semantic_search <natural query> limit=5
2. explain_symbol <top result>
```

## 4. Caveats

- **Cold-query latency**: first call after server start is 5-10s
  (loads bge embedding, runs cross-encoder reranker). Don't panic.
  Subsequent calls are sub-second.
- **Workspace scope**: lain is bound to the workspace passed at install
  time. For other repos, re-run the install.
- **`get_call_chain` may hang** in v0.4.0 (known bug). Use
  `trace_dependency` and `get_blast_radius` instead.

## 5. Don't

- Don't use `semantic_search` with literal symbol names. Use natural
  language: "where do we retry failed uploads" not "retry_upload".
- Don't ask lain to analyze a different repo — workspace is hardcoded.
- Don't repeatedly call `get_health` — once per session is enough.
