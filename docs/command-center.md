# Command Center

The Command Center is the operator's primary surface for inspecting and steering
`lain server`. It is a self-contained vanilla-JS SPA served from `GET /` by the
MCP HTTP transport. Every panel talks to the running server via the JSON-RPC
`tools/call` endpoint at `POST /mcp` — no separate API, no auth portal.

## Launch

```bash
lain server --config ./repos.yaml --transport http --port 9999
open http://localhost:9999
```

The Command Center shell is served at `/` and pulls `app.js`, `styles.css`, and
the vendored D3 v7 bundle from `/assets/d3.v7.min.js`. Everything else is
fetched from the running server over MCP.

## Sections

- **Topbar** — active project path and active workspace name. The active
  workspace is highlighted in the sidebar list.
- **Workspace switcher** (sidebar) — lists `workspaces.yaml`. The active
  workspace is highlighted. Refreshed on page load.
- **Repo summary** (sidebar) — compact `id (health)` list of every repo known
  to the federation.
- **Recent projects switcher** (sidebar) — projects the operator has used
  recently, with workspace and repo counts pulled live from each project's
  `repos.yaml` / `workspaces.yaml`. Each entry has a *Copy restart cmd* button
  that copies the right `lain server --config <path> --workspace <name>` line
  to the clipboard. The active workspace name is shown when the operator's
  `~/.config/lain/active_workspace` pointer matches that project's path.
- **Overview tab** — server health (`get_health`) and federation health
  (`get_federation_health`) in a single view.
- **Graph tab** — D3 force-directed graph of the active workspace (placeholder
  for the future deeper rendering work; the shell tab is wired in Tasks 4.3 +
  4.4 so the layout doesn't break).
- **Repos tab** — per-repo table with id, path, health, node count, edge
  count. Each row uses `get_repo_info` for the live numbers.
- **Query tab** — runs a `query_graph` call against the federation. Pick a
  repo, an op (currently `find`), a node type, and a limit. The JSON result
  is dumped below the form.
- **Tools tab** — auto-generated MCP tool tester. Calls `tools/list` on load,
  then renders a form for the selected tool by introspecting its
  `inputSchema`. Buttons: *Call* (executes the tool) and *Copy as cURL* (copies
  a `curl -X POST http://localhost:9999/mcp ...` snippet to the clipboard).
- **Status bar** (footer) — pid, transport, repo / workspace counts, and the
  last-sync timestamp. Polled every 2 s via `get_server_status`.

## Wire format

Tool calls are JSON-RPC 2.0:

```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "list_workspaces",
    "arguments": {}
  },
  "id": 1
}
```

Tool responses are wrapped in `result.content[0].text` (with the JSON payload
inside as a string). The `unwrapText` helper in `app.js` extracts the inner
text and `parseJson` decodes it. The status bar also pings the same endpoint
every 2 s so manual YAML changes show up without a refresh.

## Compatibility

The Command Center compiles its tool form from the same `inputSchema` the
MCP `tools/list` returns. Adding a new tool with a `Tool::inputSchema`
automatically makes it available in the Tools tab — no JS changes required.
Scalar fields render as `<input type="text">` (default), `number`, or
`<select>` for booleans. Required fields are marked with `*`.

## Source layout

```
src/server/mcp/command_center/
├── index.html      # SPA shell (topbar, sidebar, tabs, status bar)
├── app.js          # Vanilla JS — MCP helpers + every render fn
├── styles.css      # Light theme, flexbox layout
└── assets/
    └── d3.v7.min.js  # Vendored D3 v7 (for the future Graph tab)
```
