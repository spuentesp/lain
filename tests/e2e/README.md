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

`tests/e2e/agent_install.rs` drives every installed agent binary
through the same scripted scenario and asserts the same five
invariants. Run it with `RUN_E2E_AGENT=1`:

```bash
RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture
```

Per-agent behavior:

- Kimi, agy, omp: the harness exercises the live HTTP singleton end
  to end (install + spawn + tool list + get_health + watcher round-trip).
- claude, cursor, cline, cn, codex: auth-gated. The test installs the
  config and runs the binary; the binary fails to authenticate, the
  stderr-as-fatal check catches the error, and the test reports
  `auth-gated: skipped inner assertions` so a CI run is green except
  for those rows. Once you sign in to the relevant agent, the test
  automatically picks up the working `get_health` reply.

The harness does not require the live HTTP singleton to be
re-installed; it uses `lain agents install --scope user <id>` against
a fresh temp HOME for the round-trip step.

The harness is DST-style: one fixed scenario script, per-agent run,
fixed output contract. It is not a FoundationDB-class simulator; it is
a deterministic run that produces a clear pass/fail per agent.
