# LAIN e2e Tests

End-to-end tests for the LAIN MCP server.

## Setup

```bash
pip install -r requirements.txt
```

## Running

### Start the server first

```bash
# In one terminal:
cargo run -- --workspace /path/to/project --transport http --port 9999
```

### Run tests

```bash
# Option 1: Direct run
python tests/e2e/test_lain.py

# Option 2: With pytest
python -m pytest tests/e2e/ -v
```

### With custom binary/workspace

```bash
LAIN_BINARY=target/release/lain LAIN_WORKSPACE=/path/to/project python tests/e2e/test_lain.py
```

## Tests

- `test_health` - Server health check
- `test_sync_state` - Graph sync
- `test_get_tools` - Tool registry
- `test_explore_architecture` - Architecture exploration
- `test_list_entry_points` - Entry point detection
- `test_get_blast_radius` - Impact analysis
- `test_trace_dependency` - Dependency tracing
- `test_semantic_search` - Semantic search
- `test_get_file_diff` - Git diff
- `test_get_commit_history` - Git history
- `test_query_graph` - Graph query interface
- `test_confidence_field` - Confidence metadata

## Agent end-to-end harness

`tests/e2e/agent_install.rs` drives every supported agent through the same
scripted scenario and asserts the same invariants. Run it with
`RUN_E2E_AGENT=1`:

```bash
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

What it verifies for every agent:

1. `lain agents install --scope user <id>` writes valid config.
2. The configured MCP server is reachable via the shared HTTP singleton.
3. A file-edit watcher round-trip increases `Volatile Nodes (Overlay)`.
4. A fresh temp-HOME adapter round-trip reproduces the same config.

Per-agent behavior:

- **kimi**, **antigravity**, **omp**: config-only verification (`skip_live: true`).
  - Kimi is verified separately by `scripts/smoke-test-kimi.sh`, which spawns
    `kimi -p` and confirms Lain is loaded as a native MCP plugin.
- **claude**, **cursor**, **cline**, **cn**, **codex**: auth-gated. The harness
  installs the config and asserts the adapter round-trip; the live spawn is
  skipped. Once signed in, `scripts/smoke-test-claude.sh` verifies the real
  end-to-end path.

## Live agent smoke tests

Two helper scripts exercise the real agent binaries against the running Lain
singleton:

```bash
# Requires `claude` to be signed in.
scripts/smoke-test-claude.sh

# Requires `kimi` to be signed in.
scripts/smoke-test-kimi.sh
```

Both scripts:

- Install the Lain config for the agent if missing.
- Prompt the agent to list its MCP tools and call `get_health` on Lain.
- Assert the output contains Lain tools and an `Operational` health response.

## Notes

- The agent harness uses the shared HTTP singleton on port 9999 (override with
  `LAIN_PORT`). Make sure an owner is running before invoking the harness or
  smoke tests.
- Kimi's plugin security model requires stdio MCP `command` and `cwd` to be
  `./` paths inside the plugin root. The Kimi adapter therefore generates a
  wrapper script at `~/.kimi-code/plugins/managed/lain/bin/lain` and references
  it as `./bin/lain` in `kimi.plugin.json`.
