# LAIN-mcp

LAIN builds a map of how all the code in your project connects — what calls what, what depends on what, which files tend to change together. Then it lets your AI coding assistant ask questions about that map. So instead of the AI just looking at one file and guessing, it can ask "if I change this function, what else breaks?" and get a real answer. It plugs into any AI agent that supports MCP and runs in the background while you work.

<img width="1511" height="767" alt="Screenshot 2026-04-29 at 9 18 15 PM" src="https://github.com/user-attachments/assets/3bfbfe83-6813-416a-8dfc-c1c17959a00d" />

## How it fits together

```mermaid
flowchart LR
    A["AI Agent<br/>(Claude Code / Kimi / Cursor)"] -->|MCP<br/>JSON-RPC| L["lain"]
    L -->|reads| FS[".lain/<br/>graph.bin"]
    L -->|runs| ENG["LSP / NLP / git<br/>engines"]
    L -->|answers| T["MCP tools<br/>(get_blast_radius,<br/>explain_symbol, …)"]
    A --> T
```

`lain` is a long-running MCP server that indexes your code once and
keeps it fresh while you work. The agent speaks MCP (JSON-RPC over
stdio or HTTP); the server answers structural questions across one
repo (`lain mcp`) or many repos (`lain server --config repos.yaml`).

## Documentation

| Doc | What's in it |
|-----|--------------|
| **[`docs/QUICKSTART.md`](docs/QUICKSTART.md)** | Five-minute tour |
| **[`docs/USER_MANUAL.md`](docs/USER_MANUAL.md)** | Operator + agent manual |
| **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)** | How and why — design rationale |
| **[`docs/TECHNICAL.md`](docs/TECHNICAL.md)** | Source-level internals |
| **[`docs/FEDERATION.md`](docs/FEDERATION.md)** | Multi-repo operating guide |
| **[`docs/REPOS_YAML.md`](docs/REPOS_YAML.md)** | `repos.yaml` schema |
| **[`docs/query-language.md`](docs/query-language.md)** | `query_graph` ops-array reference |
| **[`docs/quickstart-tools.md`](docs/quickstart-tools.md)** | All MCP tools |
| **[`docs/command-center.md`](docs/command-center.md)** | Command Center SPA |
| **[`docs/hot-reload.md`](docs/hot-reload.md)** | Config hot-reload |
| **[`docs/multiplayer.md`](docs/multiplayer.md)** | Multi-agent coordination |
| **[`docs/hooks.md`](docs/hooks.md)** | Pre-edit hooks |
| **[`docs/INDEX.md`](docs/INDEX.md)** | Docs index |

## TL;DR

```bash
# Install (interactive — will add `lain` to PATH)
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash

# Or non-interactive
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --yes

# Configure your project
mkdir -p ~/projects/biller && cd ~/projects/biller
lain repos add auth-svc    https://github.com/acme/auth-svc.git
lain repos add billing-svc https://github.com/acme/billing-svc.git
lain workspaces create biller-core --members auth-svc,billing-svc

# Run the server
lain server --config ./repos.yaml --transport http --port 9999
# Open http://localhost:9999 — that's the Command Center.
```

## What is Lain?

Lain is a persistent code-intelligence MCP server. The headline is
`lain server`: a long-running process that reads a `repos.yaml` config,
indexes every registered repository (locally, by clone, or by shallow
fetch), and answers structural questions across them through MCP
tools. The server also serves a Command Center dashboard at `GET /` for
humans who want to inspect the federation, edit the config, run
queries, and exercise the MCP tool surface directly.

The value over LSP-only or RAG-based approaches is cross-file
structural reasoning: blast radius for proposed changes, transitive
dependency traces, anchor identification, co-change correlation, and
contextual build failure decoration so agents can reason about callers
rather than just the failing line. Written in Rust, persists across
sessions, stays fresh during editing via a file watcher that updates a
volatile overlay layered on top of the static graph, and hot-reloads
its `repos.yaml` / `workspaces.yaml` config without a restart.

---

## The commands

After install, `lain` exposes these subcommands:

| Command | Purpose |
|---------|---------|
| `lain server` | Start the MCP server (the headline). Reads `repos.yaml`, serves MCP tools + the Command Center dashboard. Hot-reloads the config when it changes. |
| `lain mcp` | Single-repo MCP server on stdio. Walks up from cwd for `.git` — the stable "drop in a clone and run" entrypoint. No `repos.yaml` required. |
| `lain workspaces` | Manage `workspaces.yaml`. Create, list, show, activate (`use`), forget named groups of repos. |
| `lain repos` | Manage `repos.yaml`. Add, list, remove a repo entry. |
| `lain query` | Run a `query_graph` ops-array against the project's persisted graph. |
| `lain oneshot` | One-shot MCP query: boots a transient `lain mcp` server, sends a single `tools/call`, prints the result as a table, and exits. For "just grep the symbols without keeping a server alive". |
| `lain init` | Scaffold a `repos.yaml` for the current directory. Walks up for `.git`, then writes a minimal config pointing at the discovered workspace. |
| `lain ask` | Single-user LLM-assisted query (uses `semantic_search` + `explain_symbol` heuristics). |
| `lain hooks` | Agent pre-edit hook entry point: `claim` / `release` files, `overlap-check` for commit-time symbol overlap, `lock` / `unlock` for the zero-daemon filesystem-fallback layer. |
| `lain doctor` | "One version of truth" diagnostic. Checks binary version + git SHA, hook script presence, config/hooks dirs (reaping session files older than 30 days), presence registry, and — when `LAIN_URL`/`LAIN_SERVER_URL` is set — both server reachability **and the live MCP surface**, calling `tools/list` and failing if it errors or advertises zero tools. Exits 0 clean, 1 on a hard failure. |
| `lain schema` | Emit the canonical tool-surface schema dump (`dump [--out PATH]` defaults to `./docs/tool-schema.json`). Pair with `make schema && git diff --exit-code docs/tool-schema.json` in CI to fail on schema drift. |
| `scripts/demo.sh` | Capability demonstration and benchmark. Boots a real server against a synthetic repo whose call graph is known by construction, checks lain's answers against that ground truth (not merely that it answered), then benchmarks the same tools against this repo at ~3.5k nodes. `--quick` skips the build and benchmark phases; `--json FILE` writes machine-readable results. Exits non-zero if any check fails. |

The cut surface (`agents`, `hook`, `projects`, top-level `use`) is
gone — those concerns are reached through the commands above. `server`
plus the two config CLIs (`workspaces`, `repos`) cover everything the
prior surface did, scoped to a single project directory that owns a
`repos.yaml`.

This table is checked against `lain --help` by
`tests/cli_surface.rs`, so it cannot drift from the binary again.

---

## Installation

### Quick install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash
```

The installer downloads the `lain` binary to `~/.local/lain`. After that,
configure your agent's MCP config to launch `lain server --config
./repos.yaml --transport stdio` — see [Wire your agent](#wire-your-agent).

**Non-interactive install (with options):**

```bash
# Skip all prompts
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --yes

# Download ONNX model for semantic search (all-MiniLM-L6-v2, ~120MB)
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --download-model --yes
```

When you pipe `install.sh` from a non-TTY (CI, container init, package
post-install), the script auto-detects the missing TTY, prints a
banner, and runs as if you had passed `--yes`. **It will NOT modify
your `~/.bashrc` or `~/.zshrc`** — instead it prints the exact
`export PATH=…` line for you to append manually. Add this to your shell
profile after the install completes:

```bash
echo 'export PATH="$HOME/.local/lain:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

To feed answers via a heredoc instead of skipping prompts, pass
`--interactive` together with a here-document:

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | \
  bash /dev/stdin --interactive <<'EOF'
auto
y
n
EOF
```

After installation:

```bash
# Reload your shell (the installer adds to ~/.zshrc or ~/.bashrc)
source ~/.zshrc   # or ~/.bashrc

# Verify
lain --version

# Show the available commands
lain --help
```

### Homebrew

```bash
brew tap spuentesp/lain https://github.com/spuentesp/lain
brew install lain

# Run the server for your project
lain server --config ./repos.yaml
```

### Build from Source

```bash
git clone https://github.com/spuentesp/lain.git
cd lain
cargo build --release    # requires Rust 1.75+

# Binary at ./target/release/lain
```

---

## Quick Start

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash
```

### 2. Configure your project

A project is a directory containing `repos.yaml` (and optionally
`workspaces.yaml`).

```bash
mkdir -p ~/projects/biller && cd ~/projects/biller
lain repos add auth-svc    https://github.com/acme/auth-svc.git
lain repos add billing-svc https://github.com/acme/billing-svc.git
lain workspaces create biller-core --members auth-svc,billing-svc
```

### 3. Run the server

```bash
lain server --config ./repos.yaml --transport http --port 9999
# Open http://localhost:9999 in your browser for the Command Center.
```

### 4. Wire your agent

Add the following to your agent's MCP config (URL/format depends on the
agent). **The single-repo form is the recommended default** — it
walks up from the working directory for `.git`, no `repos.yaml` needed:

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["mcp"]
    }
  }
}
```

If you want the federation tool surface (`list_repos`, `search_org`,
`get_cross_repo_blast_radius`) too, point at a `repos.yaml` instead:

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["server", "--config", "./repos.yaml", "--transport", "stdio"]
    }
  }
}
```

That's it. The next time your agent starts, it sees the workspace
(single-repo form) or the federation, the active workspace, and the
full MCP tool surface.

> **Kimi users:** Kimi's plugin security model pins the MCP subprocess
> cwd to the plugin root, so a naive `lain mcp` invocation would walk
> up from the plugin directory instead of your project. As of 0.6.1,
> `lain mcp` reads `/proc/$PPID/cwd` on Linux (the parent agent's
> cwd) before falling back to its own cwd, so the same
> `{"command":"lain","args":["mcp"]}` config works under Kimi with no
> wrapper. If you're pinned to 0.6.0, ship
> `src/cli/kimi_plugin_wrapper.sh` from the source tree and point the
> plugin manifest at it — the wrapper does the same `/proc/$PPID/cwd`
> lookup outside the binary. macOS is unsupported in either path.

---

## Command Center

When `lain server` runs with `--transport http`, it serves the Command
Center dashboard at `GET /`. It's a self-contained vanilla-JS SPA that
talks back to the running server over the same JSON-RPC endpoint the
MCP tools use. No separate API, no auth portal.

![Command Center — Overview tab](docs/screenshots/command-center-overview.png)

Tabs:

- **Overview** — `get_health` + `get_federation_health` in one view.
- **Graph** — D3 force-directed graph of the active workspace.
- **Repos** — per-repo table (id, path, health, node/edge counts).
- **Query** — runs `query_graph` against the federation.
- **Tools** — auto-generated MCP tool tester. Calls `tools/list`, then
  renders a form per tool by introspecting its `inputSchema`. *Copy as
  cURL* copies a `curl -X POST http://localhost:9999/mcp ...` snippet
  to the clipboard.

![Command Center — Repos tab](docs/screenshots/command-center-repos.png)

The status bar in the footer polls every 2 s for `get_server_status`
and `get_reload_status` so hand-edits to `repos.yaml` /
`workspaces.yaml` show up live.

See [`docs/command-center.md`](docs/command-center.md) for the full
walkthrough.

---

## Hot Reload

`lain server` watches `repos.yaml` and `workspaces.yaml` and rebuilds
its federation state when they change — no restart needed. Both the
`notify` watcher (for hand-edits) and the CLI (via `lain repos add`
or `lain workspaces create`) trigger the same `ReloadBus`.

When you run `lain repos add my-repo …`, the CLI writes the YAML
atomically (write to temp file, then `rename`), then signals the
running server over a Unix socket at
`~/.local/lain/run/<repos-stem>.sock`. The server's rebuild task
diffs the new file against the live federation and applies add / remove
operations against `FederatedIndex`. `get_reload_status` reports the
state (`idle` / `rebuilding` / `failed`); the Command Center status
bar shows it live.

See [`docs/hot-reload.md`](docs/hot-reload.md) for the full picture
(internals, observability, failure modes, caveats).

---

## Multi-project

A **project** is a directory containing `repos.yaml` (and optionally
`workspaces.yaml`). Each project has its own server: change to the
project directory and run `lain server --config ./repos.yaml`, or
keep multiple servers running on different ports. The Command Center
shows recently-used projects in the sidebar with a *Copy restart cmd*
button that copies the right `lain server --config <path> --workspace
<name>` line to the clipboard.

Workspaces are scoped to a single project. Pick one with
`lain workspaces use <name>`; the active name is written to
`~/.config/lain/active_workspace` and is honored at server start via
`--workspace auto`.

---

## Federation mode

For org-wide structural questions — "who else uses this function?",
"what depends on this service?" — run `lain server --config
./repos.yaml`. Federation mode exposes six MCP tools (`list_repos`,
`get_repo_info`, `get_federation_health`, `search_org`,
`get_cross_repo_blast_radius`,
`get_cross_repo_blast_radius_for_repo`) that answer questions
spanning repos. See [`docs/FEDERATION.md`](docs/FEDERATION.md) for
the full guide and [`docs/REPOS_YAML.md`](docs/REPOS_YAML.md) for the
config schema.

---

## Key Features

- **Federation mode** — index N repos and answer org-wide structural questions across them.
- **Command Center** — vanilla-JS SPA at `GET /` for human inspection, config editing, query running, and MCP tool testing.
- **Hot reload** — `repos.yaml` / `workspaces.yaml` changes apply without restarting the server.

### Query Language (`query_graph`)

JSON-based ops array for flexible graph traversals:

```json
{
  "ops": [
    { "op": "find", "type": "Function" },
    { "op": "connect", "edge": "Calls", "depth": { "min": 1, "max": 3 } },
    { "op": "filter", "label": "test" },
    { "op": "semantic_filter", "like": "error handling", "threshold": 0.35 },
    { "op": "limit", "count": 10 }
  ]
}
```

Available ops: `find`, `connect`, `filter`, `semantic_filter`, `group`,
`sort`, `limit`.

### Dependency Intelligence

- **`get_call_chain`** — Shortest path between two functions.
- **`get_blast_radius`** — Everything affected by a change.
- **`trace_dependency`** — What a symbol depends on.
- **`get_coupling_radar`** — Files that change together.

### Architectural Analysis

- **`find_anchors`** — Most-called, most-stable symbols (architectural pillars).
- **`list_entry_points`** — Find `main()`, route handlers, app initialization.
- **`get_context_depth`** — How far from an entry point (abstraction layers).
- **`explore_architecture`** — High-level tree of modules and files.

### Search

- **`semantic_search`** — Find code by meaning, not just names. Uses local ONNX embeddings with hybrid scoring (cosine similarity + stemmed token-overlap) and shows body excerpts in the response. BGE-small-en-v1.5 is the recommended model (better than MiniLM for technical corpora); use a query prefix to enable BGE-style asymmetric retrieval.

### Code Health

- **`find_dead_code`** — Potentially unreachable code (filters trait defaults, common names).
- **`suggest_refactor_targets`** — High-coupling, low-stability nodes.

### Project Management

A project is a directory containing `repos.yaml` (and optionally
`workspaces.yaml`). Manage it directly with the CLI:

- **`lain repos add <name> <url>`** — register a repo in `repos.yaml`.
- **`lain repos list`** — show registered repos.
- **`lain repos remove <name>`** — unregister a repo.
- **`lain workspaces create <name> --members a,b,c`** — declare a named workspace.
- **`lain workspaces list`** — show all workspaces.
- **`lain workspaces use <name>`** — activate a workspace (writes `~/.config/lain/active_workspace`).
- **`lain workspaces current`** — print the active workspace.
- **`lain workspaces forget <name>`** — remove a workspace.

---

## Requirements

| Requirement | Details |
|-------------|---------|
| Rust (build only) | 1.75 or newer |
| Git | Required for co-change analysis |
| ONNX Model | Optional — for `semantic_search` |

### Optional: Semantic Search

For `semantic_search` to work, you need an ONNX embedding model. The
easiest setup uses the provided install script with `--download-model`.
Otherwise, drop a model into `.lain/models/`:

```bash
mkdir -p .lain/models

# Option A: bge-small-en-v1.5 (recommended — better MTEB scores, 384d, ~120MB)
curl -L https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/onnx/model.onnx \
  -o .lain/models/model.onnx
curl -L https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/tokenizer.json \
  -o .lain/models/tokenizer.json

# Option B: all-MiniLM-L6-v2 (smaller, 384d, ~80MB)
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx \
  -o .lain/models/model.onnx
curl -L https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json \
  -o .lain/models/tokenizer.json
```

Export the model path so the server picks it up:

```bash
export LAIN_EMBEDDING_MODEL=$PWD/.lain/models/model.onnx
```

For BGE-style asymmetric retrieval (better for short queries), set
the query prefix in `.lain/tuning.toml`:

```toml
query_prefix = "Represent this sentence for searching relevant passages: "
```

Without the model, `semantic_search` returns "unavailable" but all
other features work.

---

## MCP Transport Modes

| Mode | Command | Use Case |
|------|---------|----------|
| `stdio` | `--transport stdio` | Claude Code, MCP clients |
| `http` | `--transport http --port 9999` | Command Center dashboard + curl-driven MCP |

The HTTP transport is no longer combined with stdio in a single
`both` mode — start two `lain server` processes (or use the HTTP
transport and exercise tools via `curl` against `/mcp`).

---

## Troubleshooting

**Hand-edit not picked up?**

The hot-reload watcher is non-recursive and uses atomic rename.
Editing the file in place (`vim repos.yaml`) triggers a notify event
within ~1 s. If you've moved the file across directories, save it
back into the same directory.

**Repo stuck in `indexing` / `degraded` / `unavailable` / `missing`?**

```bash
# Check federation health
curl -s -X POST http://localhost:9999/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_federation_health","arguments":{}},"id":1}'
```

The Command Center's Overview tab shows the same numbers in a single
view.

**Force a reload:**

```bash
curl -s -X POST http://localhost:9999/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"request_reload","arguments":{}},"id":1}'
```

**View all available tools:**

```bash
curl -s -X POST http://localhost:9999/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_agent_strategy","arguments":{}},"id":1}'
```

**`run_build` / `run_tests` fail with "not found"?**

The server inherits the environment of whatever launched it, and an
editor-launched MCP server usually has no version-manager shims on
`PATH`. lain searches the toolchain's known install locations (rustup,
nvm, pyenv, volta, mise, asdf and friends) before giving up, and the
error names every way to fix it. To teach it a manager it doesn't know,
add `program_dirs` / `program_resolver` to that toolchain's profile —
see [`toolchains/README.md`](toolchains/README.md).

**Answers look stale, or a symbol "doesn't exist" that clearly does?**

`lain mcp` blocks on the first re-index before its stdio loop comes
up, so the first tool call after `initialize` already sees a
populated graph (or `LAIN_REINDEX_TIMEOUT` was exceeded — see below).
The legacy "second call works, first doesn't" footgun is gone.

If you still see stale or missing symbols, check `get_health`:

- **`Build:`** tells you the version and git SHA of the process
  answering, and warns when a newer binary is on disk. An MCP stdio
  server is spawned once by its client and outlives every rebuild, so
  it can be older than your source tree — restart the client to pick up
  a new build.
- **`Status:`** reads `Degraded ⚠` when the last re-index failed OR
  timed out, which means "not in this graph", not "does not exist". A
  timeout banner means `LAIN_REINDEX_TIMEOUT` (default 300s) was too
  short for your working tree — raise it and restart.

**Two agents not seeing each other?**

They must share one workspace. Presence is exchanged through the state
file under `~/.local/lain/state/`, so agents on the same repo see each
other's claims even when each console spawned its own stdio server.
`list_active_agents` and `list_occupancy` are the quickest check.

---

## License

MIT — Copyright (c) 2026 spuentesp