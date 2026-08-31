# Command Center

The Command Center is the operator's primary surface for inspecting and steering
`lain server`. It is a self-contained vanilla-JS SPA served from `GET /` by the
MCP HTTP transport. Every panel talks to the running server via the JSON-RPC
`tools/call` endpoint at `POST /mcp` — no separate API, no auth portal.

[![LAIN Command Center demo](screenshots/spa-demo.gif)](screenshots/spa-demo.mp4)

**Watch in HD** — click the GIF to open the MP4.

```mermaid
flowchart LR
    subgraph BROWSER["Browser"]
        SPA["SPA shell<br/>index.html + app.js"]
        D3["D3 v7<br/>(vendored)"]
        TH["theme.css<br/>(phosphor/paper tokens)"]
    end

    subgraph SERVER["lain server (HTTP transport)"]
        CC["/         → SPA shell"]
        ASSETS["/assets/ → JS/CSS/D3"]
        MCP["/mcp      → JSON-RPC tools/call"]
        EV["/events   → SSE feed"]
        HC["/health   → health JSON"]
    end

    SPA -->|HTTP GET| CC
    SPA -->|HTTP GET| ASSETS
    SPA -->|JSON-RPC POST| MCP
    SPA -->|EventSource| EV
    SPA --> TH
    SPA --> D3
```

Every panel talks to the running server over the same JSON-RPC
endpoint — there is no separate API. The SPA is the *only* consumer;
every state mutation is a tool call.

## Tour

A walkthrough that matches what you see in the demo above:

1. **Server boot** — the terminal at the top of the clip runs `lain repos add …`, `lain workspaces create tokio-stack --members bytes,tokio`, then `lain server --config ./repos.yaml --transport http --port 9931`. Watch the federation reach `ready`.
2. **Overview tab** — `get_health` + `get_federation_health` in one view. Federation totals: `total_repos`, `ready`, `indexing`, `degraded`, `total_nodes`, `total_edges`.
3. **Repos tab** — the per-repo table (id, path, health, node count, edge count). Both `bytes` and `tokio` show `ready`.
4. **Query tab** — `find` op against `bytes`, type `Function`, limit 50. The JSON result dumps below the form.
5. **Tools tab** — `find_anchors` against the `bytes` repo, take the top result, then `get_cross_repo_blast_radius` on that symbol with depth `1..3`. The result pane shows real cross-repo call chains into `tokio` (e.g. `bytes::Buf` callers across tokio's I/O codec and runtime).
6. **Graph tab** — D3 force-directed layout settles on the same workspace-graph nodes the Tools tab just returned. Hover a node to see its name, repo, kind, and path. (Cross-repo edges don't render yet — the federation's per-repo projection drops them; see `tests/federation_e2e.rs::get_cross_repo_blast_radius_traverses_boundaries` for the gap.)

The sections below describe the same surface in prose.

## Launch

```bash
lain server --config ./repos.yaml --transport http --port 9999
open http://localhost:9999
```

The Command Center shell is served at `/` and pulls `app.js`, `theme.css`,
`styles.css`, and the vendored D3 v7 bundle from `/assets/d3.v7.min.js`.
Everything else is fetched from the running server over MCP.

### Screenshots

| Tab | What it shows |
|-----|---------------|
| **Overview** | `get_health` + `get_federation_health` output side-by-side. Federation row totals: `total_repos`, `ready`, `indexing`, `degraded`, `total_nodes`, `total_edges`, `healthy`. |

![Command Center — Overview](screenshots/command-center-overview.png)

| Tab | What it shows |
|-----|---------------|
| **Repos** | Per-repo table (id, path, health, node count, edge count). Each row is a live `get_repo_info` call. |

![Command Center — Repos](screenshots/command-center-repos.png)

| Tab | What it shows |
|-----|---------------|
| **Tools** | Auto-generated tester from `tools/list` + per-tool `inputSchema`. Click any tool on the left to render a form; *Call* executes it; *Copy as cURL* copies a copy-pasteable `curl` against `POST /mcp`. |

![Command Center — Tools](screenshots/command-center-tools.png)

## Sections

```mermaid
flowchart TB
    subgraph TOP["Topbar"]
        PROJ["active project path"]
        WS["active workspace"]
        TH2["theme toggle"]
    end

    subgraph SIDE["Sidebar"]
        WSSW["workspace switcher"]
        REPOSW["repo summary (id+health)"]
        RECENT["recent projects switcher"]
    end

    subgraph TABS["Tabs"]
        OVW["Overview<br/>get_health + get_federation_health"]
        GRAPH["Graph<br/>D3 force-directed"]
        REPOSTAB["Repos<br/>id, path, health, counts"]
        Q["Query<br/>query_graph form"]
        TOOLS["Tools<br/>auto-generated form per tool<br/>(from inputSchema)"]
    end

    subgraph FOOT["Footer"]
        SB["status bar (2s poll)<br/>pid, transport, counts"]
    end

    TOP --- SIDE
    SIDE --- TABS
    TABS --- FOOT
```

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
- **Overview** — *(see [Tour step 2](#tour) for what this looks like)* `get_health` + `get_federation_health` in one view.
- **Overview tab** — server health (`get_health`) and federation health
  (`get_federation_health`) in a single view.
- **Graph** — *(see [Tour step 6](#tour) for what this looks like)* D3 force-directed graph of the active workspace.
- **Graph tab** — D3 force-directed graph of the active workspace, drawn from
  `get_workspace_graph` (Function/Method/Class nodes, Calls/Imports edges,
  capped at 5000 nodes / 10000 edges server-side). Cross-repo edges are drawn
  in the warning colour; hovering a node shows its name, repo, kind and path;
  nodes are draggable. Labels are drawn only when the graph has 150 nodes or
  fewer. The header line reports node / edge / cross-repo counts and flags a
  truncated response.

  Workspace selection follows the shape of the federation:

  | `list_workspaces` returns | Graph tab shows |
  | --- | --- |
  | nothing (or no `workspaces.yaml`) | *"No workspace indexed yet."* plus the `lain server --config … --workspace …` line |
  | exactly one workspace | that workspace, drawn immediately; the picker is hidden |
  | several, one flagged `is_active` | the active one, drawn immediately; the picker is shown |
  | several, none active | *"Pick a workspace above to draw its graph."* and nothing drawn |

  The picker is a **client-side** affordance. `get_workspace_graph` takes no
  workspace name — it derives the workspace from the repos the server actually
  loaded — and a server holds exactly one workspace at a time, the one passed
  to `lain server --workspace <name>`. Picking a workspace the server has not
  loaded therefore prints the restart command instead of a graph. The
  reasoning is written up in
  [docs/opinions/graph-tab-data-source.md](opinions/graph-tab-data-source.md).
- **Repos** — *(see [Tour step 3](#tour) for what this looks like)* per-repo table (id, path, health, node count, edge count).
- **Repos tab** — per-repo table with id, path, health, node count, edge
  count. Each row uses `get_repo_info` for the live numbers.
- **Query** — *(see [Tour step 4](#tour) for what this looks like)* runs `query_graph` against the federation.
- **Query tab** — runs a `query_graph` call against the federation. Pick a
  repo, an op (currently `find`), a node type, and a limit. The JSON result
  is dumped below the form.
- **Tools** — *(see [Tour step 5](#tour) for what this looks like)* auto-generated MCP tool tester.
- **Tools tab** — auto-generated MCP tool tester. Calls `tools/list` on load,
  then renders a form for the selected tool by introspecting its
  `inputSchema`. Buttons: *Call* (executes the tool) and *Copy as cURL* (copies
  a `curl -X POST http://localhost:9999/mcp ...` snippet to the clipboard).
- **Status bar** (footer) — pid, transport, repo / workspace counts, and the
  last-sync timestamp. Polled every 2 s via `get_server_status`. Rendered in
  reverse video, like a terminal status line.
- **Theme toggle** (topbar, right) — flips between *phosphor* (dark) and
  *paper* (light). See [Theme](#theme).

## Theme

The palette is an 80s console look ported from the pre-SPA UI
(`src/mcp/front_end_monitor.html`, dropped in 49f5f82). Both themes are
monospace throughout, square-cornered, with hairline borders and letterspaced
uppercase panel labels.

| | Surface | Ink |
|---|---|---|
| **phosphor** (dark) | near-black `#05080f` | cyan `#00e5cc` |
| **paper** (light) | warm paper `#e6e1d3` | teal ink `#14322e` |

Dark is the default — it is lain's identity — and light applies when the
system asks for it. Three states, in precedence order:

1. an explicit `[data-theme]` on `<html>`, set by the topbar toggle and
   persisted to `localStorage` under `lain-theme`
2. otherwise `prefers-color-scheme: light` → paper
3. otherwise → phosphor

Two knobs carry the CRT treatment, both neutralised in the light theme:
`--glow` (phosphor bloom on accent text) and `--scanlines` (the overlay
opacity behind `.crt-scanlines`). The blinking block cursor after the wordmark
respects `prefers-reduced-motion`.

Every colour lives in `theme.css` as a custom property; no other stylesheet
hardcodes one, so both themes come from a single rule set and only the token
values swap. The standalone `/ui/*` detail views load the same `theme.css` and
read the same `localStorage` key, so a choice made in the Command Center
carries over to them. `command_center_assets_tests.rs` fails the build if a
consumer hardcodes a colour or if the two light-theme declaration blocks
drift apart.

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
├── theme.css       # 80s console palette (phosphor + paper), shared
├── styles.css      # Layout and components; reads tokens from theme.css
└── assets/
    └── d3.v7.min.js  # Vendored D3 v7 (for the future Graph tab)
```

`theme.css` is also served to the standalone detail views under `src/ui/`
(`blast-radius.html`, `call-chain.html`, `coupling.html`), which is why it is a
separate file rather than a block at the top of `styles.css`.
