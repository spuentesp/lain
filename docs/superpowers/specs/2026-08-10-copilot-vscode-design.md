> **Status:** Superseded by `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md`.

# Design: GitHub Copilot in VS Code as a first-class Lain agent

## Goal

Make a single `lain init --agent copilot` install (or `lain agents install copilot`) configure the GitHub Copilot Chat extension in VS Code to use the Lain MCP server. Because VS Code's MCP config and Copilot Chat share the same file, one install covers both. The MCP config and an awareness doc are written so the agent both *can* and *knows when to* reach for Lain.

## Background

VS Code 1.102+ has native MCP support; GitHub Copilot Chat (an MCP client inside VS Code) reads the same MCP config. Verified from the official docs:

- [VS Code MCP servers](https://code.visualstudio.com/docs/agent-customization/mcp-servers) — primary reference.
- [GitHub Copilot MCP](https://docs.github.com/en/copilot/customizing-copilot/using-model-context-protocol/extending-copilot-chat-with-mcp) — confirms Copilot reads the same `.vscode/mcp.json`.

VS Code supports two MCP config locations:

1. **Workspace**: `<workspace>/.vscode/mcp.json` (commit it; travels with the repo).
2. **User**: `~/.copilot/mcp-config.json` (a dedicated MCP config for the Copilot Agent Host — **not** VS Code's general `settings.json`, which the docs explicitly distinguish as the portable Agent Host path).

Both files use the same shape: root key `servers`, local server is just `command` + `args` (no `type: "stdio"` field needed — local is the default). Verified example from the VS Code docs:

```json
{
  "servers": {
    "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp" },
    "playwright": { "command": "npx", "args": ["-y", "@microsoft/mcp-server-playwright"] }
  }
}
```

The `omp` and `claude` mishaps (absolute-path bug, `mcpServers` vs `mcp` vs `servers` confusion) informed two load-bearing choices in this design:

- The manifest entry uses `mcp_section = "servers"` to match the actual root key. (OpenCode uses `mcp`; Claude Code uses `mcpServers` from `~/.claude.json`; VS Code uses `servers`.)
- `command: "lain"` is a bare PATH-resolvable name. VS Code does **not** silently ignore absolute paths the way Claude Code does, but the bare name keeps the config portable across hosts.

`omp` (oh-my-pi) and the existing `init_omp` adapter are **not** related to this work and stay untouched. This adds a new `copilot` id.

## Verified config shape

```json
{
  "servers": {
    "lain": {
      "command": "lain",
      "args": [
        "--workspace", "auto",
        "--transport", "stdio",
        "--embedding-model", "/home/<user>/.local/lain/models/all-MiniLM-L6-v2.onnx"
      ]
    }
  }
}
```

**Load-bearing choices:**

- Root key is `servers` (top-level, in both `.vscode/mcp.json` and `~/.copilot/mcp-config.json`). Not `mcp`, not `mcpServers`.
- Local server uses `command` (string) + `args` (array of strings). This is the string-with-args shape, distinct from OpenCode's array-`command` shape.
- `--workspace auto` works because VS Code launches MCP subprocesses with cwd set to the project root. No `/proc/$PPID/cwd` wrapper needed.
- `command: "lain"` (bare name on PATH).

## Design

### Files to add

- `agents/manifest.toml` — add a `[[agent]]` block for `copilot`.
- `src/cmds/agents/adapters/copilot.rs` — new `CopilotAdapter` and a `pub fn build_copilot_lain_entry`.
- `src/cmds/agents/adapters/mod.rs` — register the adapter in `adapter_for`; re-export the builder.
- `src/cmds/init.rs` — add `init_copilot`, dispatch in `run_init`, tests, awareness content pin.
- `src/main.rs` — add `copilot` to `SUPPORTED_AGENTS` so `run_init` accepts it. The `--scope` flag is already on the `Init` subcommand (added in the opencode work); no new CLI surface.
- `hooks/copilot/copilot-instructions.md` — bundled awareness doc, included via `include_str!`.
- `tests/e2e_copilot.rs` — end-to-end test that runs the real `lain init` and verifies the produced files.

### Manifest entry (`agents/manifest.toml`)

```toml
[[agent]]
id = "copilot"
display_name = "GitHub Copilot in VS Code"
binary = "code"
detect_paths = ["~/.config/Code", "~/.vscode"]
config_user = "~/.copilot/mcp-config.json"
config_project = ".vscode/mcp.json"
config_format = "jsonc"
mcp_section = "servers"
mcp_name = "lain"
transport = "stdio"
command = "lain"
default_args = []
headless_probe = ["code", "--version"]
```

Notes:

- `config_format = "jsonc"` — both files are plain JSON (a valid JSONC subset); we write plain JSON.
- `mcp_section = "servers"` — root key in both locations. Distinct from opencode's `mcp` and Claude's `mcpServers`.
- `mcp_name = "lain"` — the entry name. Used by the adapter's `read` and `remove`.
- `command = "lain"` and `default_args = []` — the adapter and `init_copilot` build the entry directly via `build_copilot_lain_entry` (the string-command + `args` shape) rather than going through the generic `server_for`/`render_args` path. The manifest's `command`/`default_args` are advisory for this row, same convention as the opencode row.

### `CopilotAdapter` (`src/cmds/agents/adapters/copilot.rs`)

Responsibilities:

- `id() -> "copilot"`.
- `build_copilot_lain_entry(embedding_model: Option<&Path>) -> Value` (pub fn, shared with `init_copilot`):
  ```rust
  json!({
      "command": "lain",
      "args": {
          let mut args = vec![
              "--workspace".to_string(),
              "auto".to_string(),
              "--transport".to_string(),
              "stdio".to_string(),
          ];
          if let Some(model) = embedding_model {
              args.push("--embedding-model".to_string());
              args.push(model.to_string_lossy().to_string());
          }
          args
      }
  })
  ```
- `install(entry, scope)`:
  - Resolve target path: `scope == User` → `~/.copilot/mcp-config.json`; `scope == Project` → `<project>/.vscode/mcp.json`; `Workspace` is unsupported.
  - Read existing file if present; parse as JSON. If malformed, start from `{}` and log a warning.
  - Ensure the root object has an `entry.mcp_section` key (i.e. `servers`) as an object. Merge: preserve existing servers, set/overwrite `<entry.mcp_section>.<entry.mcp_name>` to the new entry.
  - **Use `entry.mcp_section.clone()` and `entry.mcp_name.clone()`** in `install`, `read`, **and** `remove` — fixing the lesson from the opencode fix wave (the opencode adapter initially hardcoded `"mcp"` / `"lain"` in `install`/`remove`; the final review caught it and required the entry fields everywhere).
  - Write back with `serde_json::to_string_pretty`.
- `read(entry, scope)` — symmetric read for `lain agents list` and the adapter round-trip tests; returns `<root>.<entry.mcp_section>.<entry.mcp_name>` or `Value::Null` if absent.
- `remove(entry, scope)` — drop `<root>.<entry.mcp_section>.<entry.mcp_name>` while preserving other servers.

`adapter_for` in `src/cmds/agents/adapters/mod.rs` gets a new arm: `Some("copilot") => Ok(Box::new(CopilotAdapter))`.

### `init_copilot` (`src/cmds/init.rs`)

Signature:

```rust
fn init_copilot(
    workspace: &Path,
    embedding_model: Option<&Path>,
    _transport: &str,
    _port: u16,
    yes: bool,
    scope: &str,
) -> Result<()>
```

Behavior:

1. Validate `scope` is `"project"` or `"user"`; error otherwise.
2. Resolve the target path:
   - `project` → `<workspace>/.vscode/mcp.json`.
   - `user` → `~/.copilot/mcp-config.json`.
3. Create parent dirs (e.g., `~/.copilot/` for user scope; `.vscode/` for project scope).
4. If the file exists, parse and merge. If `<root>.<entry.mcp_section>.<entry.mcp_name>` already exists and `yes` is false, skip overwrite and print a notice (matches the opencode `init_opencode` behavior). If `yes`, overwrite silently.
5. Write the entry using `build_copilot_lain_entry(embedding_model)`.
6. If `scope == "project"`, also write `<workspace>/.github/copilot-instructions.md` using the bundled `hooks/copilot/copilot-instructions.md`. If the file exists and `yes` is false, skip and print a notice; if `yes`, overwrite. If `scope == "user"`, skip the awareness doc (per-repo convention, inappropriate to write globally).
7. Print a clear summary: where the config was written, whether the awareness doc was written, and the command to restart VS Code / reload Copilot Chat.

`run_init` dispatches `copilot` to `init_copilot` after the `opencode` arm (or wherever fits the surrounding order). Add `copilot` to `SUPPORTED_AGENTS` in `src/cmds/init.rs` so the early "Unknown agent" check passes. The `--scope` flag (added for opencode) is already threaded through `run_init`; this work just consumes it.

### Awareness doc (`hooks/copilot/copilot-instructions.md`)

Bundled and included via `include_str!("../../hooks/copilot/copilot-instructions.md")` from `src/cmds/init.rs`. Same structure as the opencode `AGENTS.md` and the Claude `LAIN.md` (modeled on the Kimi skill):

- **When to use lain** — trigger phrases ("Where should I start reading?", "If I change X, what breaks?", "Where do we do X?", "Is there unused code?", etc.).
- **The most useful tools** — table of MCP tool names with one-line guidance (`get_health`, `find_anchors`, `get_blast_radius`, `trace_dependency`, `semantic_search`, `explain_symbol`, `get_code_snippet`, `find_dead_code`, `get_coupling_radar`).
- **Workflows** — "I'm new here", "I'm about to refactor X", "Where do we do X?" (semantic), "What calls X? / What does X call?", "Read this symbol".
- **Caveats** — cold-call latency, workspace scope, no embedding model.
- **Don't** — semantic_search with literal symbol names, tools against paths outside the workspace, repeated `get_health`.

GitHub Copilot Chat reads `.github/copilot-instructions.md` for repo-level instructions (per the Copilot docs). Writing this gives VS Code / Copilot the same "knows when to use lain" behavior as Claude and Kimi.

## Data flow

```
user runs:
  lain init --agent copilot --scope project
        |
        v
  main::run_init
        |  (resolves workspace, --workspace auto, embedding model)
        v
  cmds::init::init_copilot(workspace, model, "stdio", 0, yes, "project")
        |
        +--> merge into <workspace>/.vscode/mcp.json
        |       servers.lain = { command: "lain", args: [...] }
        |
        +--> write <workspace>/.github/copilot-instructions.md
        |
        v
  user reloads VS Code window
        |
        v
  VS Code + Copilot Chat read the config, launch `lain` per the
  servers.lain entry, discover tools, make them available in chat
```

For `--scope user`, the same flow targets `~/.copilot/mcp-config.json` and skips the awareness doc.

## Edge cases

- **`.vscode/mcp.json` already has other servers**: preserve them, insert/update `servers.lain`.
- **`.github/copilot-instructions.md` exists**: skip with a notice unless `--yes`; matches the opencode `AGENTS.md` skip pattern (added in the opencode fix wave).
- **`--scope user` writes to a global path**: create `~/.copilot/` if missing. Do not write the awareness doc.
- **The `--workspace auto` flag in Lain**: depends on the MCP subprocess's cwd being the project root. VS Code sets cwd to the project root, so this works without a wrapper.
- **OpenCode not installed**: `lain agents install copilot` returns the installer's normal error. `lain init --agent copilot` is best-effort for the config file regardless of whether VS Code is installed; a warning is printed so the user knows the config is on disk for when they install it.
- **`--scope` is neither `"project"` nor `"user"`**: clap rejects at parse time (`value_parser = ["project", "user"]`). Belt-and-suspenders: `init_copilot` also asserts.

## Testing

### Unit tests

- `init_copilot_writes_verified_mcp_config` — writes to a temp dir; parses the resulting `.vscode/mcp.json`; asserts `servers.lain.command == "lain"`, `servers.lain.args` is an array, args contain `--workspace auto` and `--transport stdio`.
- `init_copilot_includes_embedding_model_when_provided` — checks the args array contains `--embedding-model <path>` when the model is supplied.
- `init_copilot_writes_copilot_instructions_md_in_project_root` — asserts `.github/copilot-instructions.md` is written and contains the trigger phrase and `find_anchors`.
- `init_copilot_scope_user_writes_global_config` — with `scope="user"`, writes to a temp HOME's `~/.copilot/mcp-config.json` and does NOT create `.vscode/mcp.json` or `.github/copilot-instructions.md`.
- `init_copilot_merges_with_existing_mcp_json` — pre-seeds `.vscode/mcp.json` with another `servers.<other>`, runs init, asserts the other server is preserved and `servers.lain` is added.
- `init_copilot_does_not_overwrite_existing_instructions_md_without_yes` — pre-creates `.github/copilot-instructions.md` with custom content, calls `init_copilot` with `yes=false`, asserts the custom content is preserved. (Mirror pin for `yes=true` overwrites.)
- `copilot_instructions_md_contains_key_guidance` — content regression on the bundled doc (triggers, tool table, Workflows, Caveats). Same pattern as the opencode `opencode_agents_md_contains_key_guidance` test.
- `copilot_adapter_install_read_remove_round_trip`, `copilot_adapter_preserves_other_servers`, `copilot_adapter_remove_drops_only_lain` — adapter tests using the **shared** `HOME_LOCK` (promoted to `pub` in the opencode fix wave) and **the** `mcp_section` / `mcp_name` fields everywhere — no hardcoded literals.

### e2e test (`tests/e2e_copilot.rs`)

- `lain_init_copilot_writes_verified_mcp_json_and_instructions_md` — runs the real `lain init --agent copilot --yes` in a temp git repo, reads `.vscode/mcp.json`, asserts the verified shape; reads `.github/copilot-instructions.md`, asserts the trigger phrase.
- `lain_init_copilot_scope_user_writes_global_only` — sets `HOME` to a tempdir (using the `HomeGuard` Drop-guard pattern from the opencode fix wave, duplicated locally — the two test binaries can't share), runs with `--scope user`, asserts `~/.copilot/mcp-config.json` exists in the temp HOME and the project files do NOT.

## Out of scope

- **Migrating any existing agent** — `copilot` is a new id.
- **VS Code's `settings.json` MCP namespace** (legacy `chat.mcp.discovery` import from Claude Desktop). The new dedicated `~/.copilot/mcp-config.json` is the supported portable path per the current docs.
- **Sandboxed MCP server config** (`sandboxEnabled`, sandbox filesystem/network rules) — not needed for Lain.
- **Remote/HTTP MCP servers** — Lain is stdio only.
- **OAuth / registry flows** — only relevant for remote servers.
- **Windows-specific path quirks** — `dirs::home_dir()` already handles Windows; `~/.copilot/mcp-config.json` expands correctly.
- **Live VS Code / Copilot behavior test** — gated `#[ignore]` + `RUN_E2E_BEHAVIOR=1`, spawns `code` with a prompt to call a Lain tool. Only run if the user has VS Code + Copilot auth configured. Mirrors `tests/e2e_behavior.rs` for the opencode path.

## Acceptance criteria

- `lain init --agent copilot --yes` produces a valid `.vscode/mcp.json` with `servers.lain.command == "lain"` and `servers.lain.args` containing `--workspace auto`, in a temp git repo.
- The same install produces a `.github/copilot-instructions.md` in the project root with the awareness content.
- `lain init --agent copilot --scope user --yes` writes to `~/.copilot/mcp-config.json` and does not create the project files.
- The adapter uses `entry.mcp_section` and `entry.mcp_name` in `install`, `read`, and `remove` (no hardcoded literals).
- The bundled `copilot-instructions.md` content regression test passes.
- All new unit tests pass. `cargo test --lib`, `cargo test --bin lain cmds::init::tests cmds::agents`, and the e2e suites are green.
