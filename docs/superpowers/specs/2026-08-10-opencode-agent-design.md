> **Status:** Superseded by `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md`.

# Design: OpenCode as a first-class Lain agent

## Goal

Make a single `lain init --agent opencode` install (or `lain agents install opencode`) configure the OpenCode terminal/IDE agent to use the Lain MCP server across any git repository. The MCP config and an awareness doc are written so OpenCode both *can* and *knows when to* reach for Lain.

## Background

OpenCode ([opencode.ai](https://opencode.ai)) is an open-source AI coding agent by Anomaly that runs in the terminal, as a desktop app, and as an IDE extension. It supports MCP servers starting in its core config.

`lain` today supports Claude Code, Kimi, Gemini, Cursor, Windsurf, Cline, Antigravity, Codex, Continue, and `omp` (oh-my-pi). OpenCode is the next natural target: open-source, MCP-native, local-first philosophy aligned with Lain, and growing fast. Adding it follows the same pattern as the other first-class agents.

`omp` is **not** OpenCode. `omp` is the oh-my-pi agent with its own config at `~/.omp/mcp.json` and a different MCP shape (`mcpServers` + absolute `command` path). The `omp` adapter stays untouched; this work adds a new `opencode` agent id.

## Verified config shape

The OpenCode local MCP server shape is taken directly from the [OpenCode MCP servers reference](https://opencode.ai/docs/mcp-servers/). Critical detail: **`command` is an Array** of `[executable, arg1, arg2, ...]`, not a string with a separate `args` field. Confirmed against the schema table in the reference.

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "lain": {
      "type": "local",
      "command": [
        "lain",
        "--workspace", "auto",
        "--transport", "stdio",
        "--embedding-model", "/home/<user>/.local/lain/models/all-MiniLM-L6-v2.onnx"
      ],
      "enabled": true,
      "timeout": 30000
    }
  }
}
```

**Load-bearing choices:**

- `type: "local"` — required for stdio servers.
- `command` is an **array** — must include the executable as element 0, then its args. This is different from every other agent we support.
- `--workspace auto` — lets Lain resolve the git root from the MCP subprocess's cwd. OpenCode launches MCP servers with cwd set to the project root, so `auto` works without a wrapper (unlike Kimi, which needs the `/proc/$PPID/cwd` wrapper).
- `command: "lain"` (bare name on PATH). OpenCode does not silently reject absolute paths the way Claude Code does, but the bare name keeps the config portable.
- `enabled: true` — explicit so OpenCode doesn't skip it.
- `timeout: 30000` — overrides the 5-second default. Lain needs ~5–10 seconds to load the NLP model on cold start; the default would race the health check and look like a connection failure.

## Design

### Files to add

- `agents/manifest.toml` — add an `opencode` entry.
- `src/cmds/agents/adapters/opencode.rs` — new `OpenCodeAdapter` for the `lain agents install` path.
- `src/cmds/agents/adapters/mod.rs` — register the new adapter and extend `adapter_for`.
- `src/cmds/init.rs` — new `init_opencode` function; extend `run_init` to dispatch `opencode`.
- `src/main.rs` — add `--scope` to the `Init` subcommand; thread it into `run_init` and `init_opencode`.
- `hooks/opencode/AGENTS.md` — bundled awareness doc, included via `include_str!`.
- `tests/e2e_opencode.rs` — end-to-end test that runs the real `lain init` and verifies the produced `opencode.json`.

### Manifest entry (`agents/manifest.toml`)

```toml
[[agent]]
id = "opencode"
display_name = "OpenCode"
binary = "opencode"
detect_paths = ["~/.config/opencode"]
config_user = "~/.config/opencode/opencode.json"
config_project = "opencode.json"
config_format = "jsonc"
mcp_section = "mcp"
mcp_name = "lain"
transport = "stdio"
command = "lain"
default_args = []
headless_probe = ["opencode", "--version"]
```

Notes:

- `config_format = "jsonc"` — OpenCode supports JSONC (JSON with comments); we write plain JSON which is a valid JSONC subset.
- `command = "lain"` (bare name) and `default_args = []` — the adapter builds the `command` **array** directly (see below), so the generic `server_for`/`render_args` path that produces `command: "string", args: [...]` is not used here. The adapter writes the `mcp.lain` entry by hand to guarantee the array shape.
- `mcp_section = "mcp"` — OpenCode's root key, not `mcpServers`.
- `transport = "stdio"` — OpenCode's local MCP transport is stdio.

### `OpenCodeAdapter` (`src/cmds/agents/adapters/opencode.rs`)

Responsibilities (mirrors the shape of the other adapters in this directory):

- `id() -> "opencode"`.
- `install(entry, scope)`:
  - Resolve target path: `scope == User` → `~/.config/opencode/opencode.json`; `scope == Project` → `<project>/opencode.json`; `Workspace` is unsupported.
  - Read existing file if present; parse as JSON. If the file is malformed, start from `{}` and log a warning.
  - Ensure the `mcp` key is an object. Merge: preserve any existing MCP servers, set/overwrite `mcp.lain` to the new entry.
  - Build the `lain` entry as:
    ```rust
    json!({
        "type": "local",
        "command": ["lain", "--workspace", "auto", "--transport", "stdio",
                    "--embedding-model", <path-or-omitted>],
        "enabled": true,
        "timeout": 30000
    })
    ```
    The `command` is built as a `Vec<String>`; this is the array shape OpenCode requires. Do **not** reuse `server_for`/`render_args` because those produce a `command: "string"` with separate `args`.
  - Write back with `serde_json::to_string_pretty`.
- `read(entry, scope)` — symmetric read for `lain agents list` and the adapter round-trip tests; returns the `mcp.lain` value or `Value::Null` if absent.
- `remove(entry, scope)` — drop the `mcp.lain` key while preserving other servers.

`adapter_for` in `src/cmds/agents/adapters/mod.rs` gets a new arm: `Some("opencode") => Ok(Box::new(OpenCodeAdapter))`.

### `init_opencode` (`src/cmds/init.rs`)

Signature:

```rust
fn init_opencode(
    workspace: &Path,
    embedding_model: Option<&Path>,
    transport: &str,
    port: u16,
    yes: bool,
    scope: &str,  // "project" or "user"
) -> Result<()>
```

Behavior:

1. Validate `scope` is `"project"` or `"user"`; error otherwise.
2. Resolve the target path:
   - `project` → `<workspace>/opencode.json`.
   - `user` → `~/.config/opencode/opencode.json`.
3. If the file exists, parse and merge. If the `lain` entry already exists and `yes` is false, prompt to overwrite. If `yes`, overwrite silently.
4. Write the verified config (same builder as the adapter, or call into a shared helper).
5. If `scope == "project"`, also write `AGENTS.md` to the workspace root using the bundled `hooks/opencode/AGENTS.md`. If the file exists, prompt (or skip when `yes`).
6. If `scope == "user"`, skip `AGENTS.md` — it's a per-project convention, not appropriate to write a global one without a clear project context.
7. Print a clear summary: where the config was written, whether `AGENTS.md` was written, and the command to restart OpenCode.

`run_init` dispatches `opencode` to `init_opencode` after the existing `omp`/`kimi`/etc. branches.

### `--scope` on the `Init` subcommand (`src/main.rs`)

Add to the `Init` clap variant:

```rust
Init {
    // existing fields...
    #[arg(long, default_value = "project", value_parser = ["project", "user"])]
    scope: String,
}
```

Thread `scope` through `run_init` into `init_opencode` (and the other inits that gain scope support later). For agents that don't yet support `scope` (claude, kimi, etc.), the value is ignored or rejected with a clear error — `claude` and `kimi` are user-only, so `scope=project` on those should be a clear "not supported" error rather than a silent fallback.

For this work, `scope` is only honored by `init_opencode` and `init_claude`/`init_kimi` (which are inherently user-scope). Other inits ignore it; future work can extend it.

### Awareness doc (`hooks/opencode/AGENTS.md`)

Bundled and included via `include_str!("../../hooks/opencode/AGENTS.md")` (path relative to `src/cmds/init.rs`). Modeled on the Kimi skill and the expanded Claude `LAIN.md`:

- **When to use lain** — trigger phrases ("Where should I start?", "If I change X, what breaks?", "Where do we do X?", "Is there unused code?", etc.).
- **The most useful tools** — table of MCP tool names with one-line guidance (`get_health`, `find_anchors`, `get_blast_radius`, `trace_dependency`, `semantic_search`, `explain_symbol`, `get_code_snippet`, `find_dead_code`, `get_coupling_radar`).
- **Workflows** — "I'm new here", "I'm about to refactor X", "Where do we do X?" (semantic), "What calls X? / What does X call?", "Read this symbol".
- **Caveats** — cold-call latency, workspace scope, no embedding model.
- **Don't** — semantic_search with literal symbol names, tools against paths outside the workspace, repeated `get_health`.

OpenCode reads `AGENTS.md` from the project root automatically. Writing this in the project root gives OpenCode the same "knows when to use lain" behavior Claude and Kimi get.

## Data flow

```
user runs:
  lain init --agent opencode --scope project
        |
        v
  main::run_init
        |  (resolves workspace, --workspace auto, embedding model)
        v
  cmds::init::init_opencode(workspace, model, "stdio", 0, yes, "project")
        |
        +--> merge into <workspace>/opencode.json
        |       mcp.lain = { type: "local", command: ["lain", ...], ... }
        |
        +--> write <workspace>/AGENTS.md
        |
        v
  user restarts OpenCode in the project
        |
        v
  OpenCode reads opencode.json, launches `lain` per the mcp.lain
  entry, discovers tools, makes them available in chat
```

## Edge cases

- **`opencode.json` already has other MCP servers**: preserve them, insert/update `mcp.lain`.
- **`AGENTS.md` already exists**: prompt to overwrite; if `--yes`, overwrite. The bundled doc replaces it.
- **`scope=user` writes to a global path**: use `dirs::home_dir()` to resolve `~/.config/opencode/opencode.json`. If the directory doesn't exist, create it. Do not write `AGENTS.md`.
- **`--scope` is neither `"project"` nor `"user"`**: clap rejects at parse time (`value_parser = ["project", "user"]`). Belt-and-suspenders: `init_opencode` also asserts.
- **Embedding model path doesn't exist on disk**: init does not validate the model's existence; the existing `run_init` flow already warns. No new check needed.
- **OpenCode not installed**: `lain agents install opencode` returns the installer's normal error. `lain init --agent opencode` is best-effort for the config file regardless of whether OpenCode is installed; a warning is printed so the user knows the config is on disk for when they install it.
- **The `--workspace auto` flag in Lain**: this depends on the MCP subprocess's cwd being the project root. OpenCode launches MCP servers with cwd set to the project root, so this works without a wrapper. (Kimi required a `/proc/$PPID/cwd` wrapper because Kimi pins the subprocess cwd to the plugin root; OpenCode does not.)

## Testing

### Unit tests (`src/cmds/init.rs`, `src/cmds/agents/adapters/`)

- `init_opencode_writes_verified_mcp_config` — writes to a temp dir; parses the resulting `opencode.json`; asserts `mcp.lain.type == "local"`, `command` is a JSON array, `command[0] == "lain"`, the args contain `--workspace auto` and `--transport stdio`, `enabled == true`, `timeout == 30000`.
- `init_opencode_writes_agents_md_with_awareness` — asserts `AGENTS.md` is written in the project root and contains the key trigger phrases and tool names (regression pin, same pattern as `claude_awareness_doc_contains_key_guidance`).
- `init_opencode_scope_user_writes_global_config` — with `scope="user"`, writes to a temp HOME's `~/.config/opencode/opencode.json` and does not create `AGENTS.md` in the workspace.
- `init_opencode_merges_with_existing_opencode_json` — pre-seed `opencode.json` with another MCP server, run init, assert the other server is preserved and `mcp.lain` is added.
- `OpenCodeAdapter::install_round_trip` (in `adapters/opencode.rs`) — `install` then `read` returns the same `mcp.lain` value; `remove` drops it; preserves other servers.

### e2e test (`tests/e2e_opencode.rs`)

- `lain_init_opencode_writes_real_opencode_json` — run the real `lain init --agent opencode --yes` in a temp git repo, read the resulting `opencode.json`, assert it matches the verified schema. This is the real-install proof and would have caught the `omp`/OpenCode confusion if the manifest entry were wrong.

### Behavior test (gated, lower priority)

- `claude_style_opencode_get_health_resolves_temp_repo` — gated `#[ignore]` + `RUN_E2E_BEHAVIOR=1`; spawns `opencode` headless against a temp repo with the Lain MCP server configured, sends an MCP `get_health` request, asserts the response contains `Operational`. Only run if the user has OpenCode installed and authed; otherwise the test skips. This mirrors `tests/e2e_agents.rs`.

## Out of scope

- **Remote/HTTP MCP servers** — OpenCode supports them; Lain is stdio only.
- **OAuth flows** — only relevant for remote servers.
- **Migrating the `omp` (oh-my-pi) adapter** — they are different agents. The `omp` adapter stays untouched.
- **Windows-specific path handling** — the resolved `~/.config/opencode/opencode.json` uses `dirs::home_dir()` which already handles Windows correctly (`%APPDATA%`).
- **Multi-agent scope (`--scope project` for Claude/Kimi)** — the `--scope` flag is added but only `init_opencode` honors it meaningfully. Other inits ignore it for now; extending them is a separate piece of work.

## Acceptance criteria

- `lain init --agent opencode --yes` produces a valid `opencode.json` with `mcp.lain.type: "local"` and a `command` array starting with `"lain"`, in a temp git repo.
- The same install produces an `AGENTS.md` in the project root with the awareness content.
- `lain init --agent opencode --scope user --yes` writes to `~/.config/opencode/opencode.json` and does not create an `AGENTS.md`.
- The existing `omp` (oh-my-pi) install path is unchanged.
- All new unit tests pass. `cargo test --lib` and `cargo test --bin lain cmds::init::tests` are green. `tests/e2e_opencode.rs` passes when run.
