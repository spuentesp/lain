---
name: lain-federation
description: Use when answering questions across repositories with Lain — "which repos know about X", "what breaks across repos if I change this", "what is in this workspace group". Covers the federation and workspace packages.
---

# Lain federation — org-wide questions

Federation tools appear automatically when the server runs multiple
repositories; workspace tools when a `workspaces.yaml` is
configured. This skill covers how to use them well.

## The tools

- `search_org` — symbol/path search across every indexed repo. The
  answer to "which repos know about X" and the first call before
  anything cross-repo.
- `get_cross_repo_blast_radius` — what breaks across repos if a
  symbol changes, grouped by repo. **Pin `repo_id` when the name
  exists in more than one repo** (the error tells you the
  candidates). `depth` takes a number or range.
- `list_repos` / `get_repo_info` / `get_federation_health` — what
  is indexed, where it came from, and whether anything failed to
  load (unreachable repos are listed, not fatal).
- `get_workspace_graph` / `list_workspaces` / `get_active_workspace`
  — workspace-group views when repos are managed as named sets.

## Rules of thumb

- Call chains (`get_call_chain`) stay **within one repository** —
  by design. For the cross-repo question use
  `get_cross_repo_blast_radius`.
- A cold federation answers `warming_up` first; cross-repo edges
  appear once both sides are projected. Retry, do not conclude
  "no connections".
- Ids are five segments (`repo:Kind:path:name:line_start`); the
  line segment distinguishes same-named methods at different lines
  in one file. Ids returned by one tool work as handles in the next.
- If a graph refuses to load after a Lain upgrade, that is the
  schema gate doing its job: run `lain reindex` (backs up the old
  graph first). See FEDERATION.md.
