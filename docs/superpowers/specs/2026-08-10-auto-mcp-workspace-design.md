# Design: Auto-detect workspace for MCP server startup

## Goal

Make a single `lain init --agent <agent>` configuration reusable across any git
repository. When an agent starts the Lain MCP server, Lain should discover the
repository root from the agent's current working directory instead of relying
on a hardcoded `--workspace` path written at install time.

## Background

`lain init` currently writes an absolute `--workspace <path>` argument into each
agent's MCP settings. This pins that agent instance to one repository. To use
Lain in a second repository, the user must re-run `lain init` from that repo and
overwrite the agent config.

The project registry (`lain projects add/use/list`) already makes CLI-level
project switching convenient, but it does not affect the MCP server because the
`--workspace` flag always wins.

## User-facing behavior

### One-time setup

```bash
cd ~/projects/repo-a
lain init --agent claude --yes
# ~/.claude/settings.json now references --workspace auto
```

### Everyday use

```bash
cd ~/projects/repo-a
claude
# MCP server starts with cwd=repo-a; Lain serves repo-a

cd ~/projects/repo-b
claude
# Same config; cwd=repo-b; Lain serves repo-b
```

A single agent configuration now works for every git repository.

### Edge cases

- **Outside a git repo:** `lain` exits fast with:
  ```
  error: --workspace auto requires a git repository, but none was found from <cwd>
  Pass an explicit --workspace <path> or run inside a git repo.
  ```
- **Inside a subdirectory of a repo:** Lain walks up to `.git` and serves the
  repo root.
- **Explicit override:** `--workspace /path/to/repo` continues to win, so users
  who want a pinned workspace can edit the config back to a hardcoded path.

## Design

### Sentinel value

Introduce `--workspace auto` as a sentinel. When `args.workspace == "auto"`,
Lain resolves the workspace at startup. An explicit path is used unchanged.

We use an explicit sentinel rather than changing the default value so that:

1. Existing hardcoded configs keep working unchanged.
2. The CLI default for commands such as `lain query` remains backward
   compatible with the current active-project/cwd fallback logic.

### Resolution helper

Add `resolve_auto_workspace() -> Result<PathBuf, LainError>` near the existing
registry resolution code in `src/state.rs`:

1. Call `git2::Repository::discover(".")`.
2. On success, return `repo.workdir()` canonicalized.
3. On failure (`NotFound` or any libgit2 error), return a clear user-facing
   error that mentions `--workspace auto` and suggests passing an explicit path.

`Repository::discover` already walks parent directories until it finds a `.git`
folder, which gives the correct repo root when the agent is opened in a
subdirectory.

### Startup path

In `src/main.rs`, resolve `auto` **before dispatching any subcommand**, so that
`init`, `query`, and the default server path all benefit:

```rust
if args.workspace.as_os_str() == "auto" {
    args.workspace = resolve_auto_workspace()?;
}
```

After this point the rest of the startup code treats `args.workspace` as a
normal, explicit path.

The server logs the resolved workspace at startup so the user sees what was
selected:

```
info!(workspace = %args.workspace.display(), "Serving repo")
```

### Agent installers

Update `src/cmds/init.rs` so every agent's MCP args begin with:

```json
["--workspace", "auto", "--transport", "stdio"]
```

instead of the absolute workspace path. All other args (transport, port,
embedding model) remain unchanged.

### Tests

- **Unit tests** for `resolve_auto_workspace`:
  - inside a git repo root → returns that path
  - inside a subdirectory → returns repo root
  - outside any git repo → returns an error
  - bare repo without `workdir()` → falls back to the repo path or returns a
    clear error
- **Update** `tests/e2e-sandboxed.sh`, which currently asserts that the
  hardcoded fake workspace appears in the installed MCP args. It should now
  expect `--workspace auto`.
- **Existing tests** that pass explicit `--workspace <path>` continue to work
  unchanged.

## Out of scope

- Removing `lain init` entirely. Auto-detection still requires the agent config
  to be installed once.
- Using the active project registry to drive the MCP server. The agent serves
  the repository it is opened in, not the project last selected with
  `lain use`.
- Windows-specific wrapper scripts. The `git2`-based implementation is
  cross-platform.

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Agent does not set cwd to the project root | The sentinel fails fast with a clear message; user can fall back to explicit `--workspace`. |
| Nested git repositories (e.g. a repo inside another repo) | `git2::Repository::discover` stops at the nearest `.git`, which is the expected behavior. |
| Breaking existing e2e-sandboxed assertion | Update the test to match the new args. |

## Acceptance criteria

- `lain init --agent claude --yes` writes `"args": ["--workspace", "auto", ...]`.
- Running `lain --workspace auto --transport stdio` from inside a git repo
  starts the server for that repo.
- Running the same command outside a git repo exits with a clear error.
- `cargo test --lib` passes, including new unit tests.
- `tests/e2e-sandboxed.sh` passes.
