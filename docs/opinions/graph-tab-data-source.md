# Graph tab: why `get_workspace_graph`, and why the picker is client-side

**Status:** accepted (2026-08-28, defect D-M8)

## Data source

The Command Center Graph tab calls `get_workspace_graph`, not `query_graph`.

- `get_workspace_graph` (`src/server/mcp/definitions.rs:129`) is described as
  "per-workspace graph for the dashboard". It returns `{nodes, edges, truncated}`
  in a single call, pre-filtered to `Function`/`Method`/`Class` nodes and
  `Calls`/`Imports` edges, capped at 5000 nodes / 10000 edges
  (`src/server/mcp/federation_tools/workspace.rs:118-119`), and marks
  cross-repo edges with `cross_repo: true`.
- `query_graph` only runs `find` ops over node types and returns no edge set.
  Building a force-directed graph from it would need one call per node to
  discover edges — an N+1 against a tool that was never meant for it.

No new MCP tool was added. The one that exists already fits.

## Why the workspace picker does not switch server state

`get_workspace_graph` takes no workspace name. It derives the workspace by
matching the *loaded* repo set against each workspace's members
(`src/server/mcp/federation_tools/workspace.rs:126-140`) and errors with
"federation loaded but no workspace matches the loaded repos" when nothing
matches. A running server holds exactly one workspace — the one passed to
`lain server --workspace <name>`.

So the tab's `<select>` is a client-side affordance over a server-side fact:

- 0 workspaces → "No workspace indexed yet."
- exactly 1 workspace, or one flagged `is_active` → auto-selected and rendered.
- more than 1 and none active → picker shown, nothing rendered until a choice.
- a non-active workspace chosen → we cannot render it. The tab prints the
  `lain server --config <path> --workspace <name>` line to restart against it,
  matching the sidebar's "Copy restart cmd" affordance.

Making the picker hot-swap workspaces would mean a server-side reload tool and
a rebuild of the federated index. That is out of scope for D-M8.
