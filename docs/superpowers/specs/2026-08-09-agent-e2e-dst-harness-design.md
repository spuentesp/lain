# Agent End-to-End DST-Style Test Harness

**Date:** 2026-08-09
**Status:** Design approved for implementation

## Goal

Build a deterministic-simulation-test-style (DST-style) harness that, before any release, drives every installed agent binary through the same scripted scenario and proves that:

1. The agent sees the right MCP tools (Lain in the tool list).
2. The agent's `get_health` call reaches the live HTTP singleton and reports `Operational`.
3. A `query_graph` call returns the same answer regardless of which agent invoked it.
4. A simulated `overlay_insert` round-trip is visible on the agent’s side (or, in this iteration, on a direct `mcp__plugin-lain_lain__query_graph` after a sidecar refresh).
5. The file-watcher path picks up a synthetic edit and surfaces it through the agent’s tools within bounded time.
6. The seven adapter manifests round-trip through the install loop.

The harness must run the same scenario against every installed agent in the same way, with the same assertions, so a regression in any one of them is visible in the same place. Today that is not the case: `dual_instance.rs` proves the wire, the agent-install unit tests prove the install loop, the Python `lain_test.py` proves the JSON-RPC surface, and a one-off Kimi terminal proved the plug-in path manually. None of those is a single test that drives every agent.

## Current failure (the one this plan closes)

The user asked “did we do a deterministic testing end to end that this works with the agents installed?” and the honest answer is no:

- `dual_instance.rs` spawns two `lain` binaries, not an agent plus a `lain`.
- `tests/agents_install.rs` runs `lain agents verify --all` against a temp `lain` instance; the `agents` subcommand is currently not wired into `src/main.rs` on the rebuilt release binary, so the test fails in the most recent green run. It will pass once the `Commands::Agents` clap wiring lands.
- `tests/e2e/lain_test.py` exercises the JSON-RPC surface directly, not through an agent.
- The Kimi terminal proof earlier in this session was a manual run, not a test.

This plan closes the gap by giving the repo a single, deterministic harness that drives every agent through the same scenario.

## Design

### Scope

This is one plan, with a single test binary at `tests/e2e/agent_install.rs` (extending the existing `tests/e2e/` directory). It is not a separate crate. It uses `assert_cmd` (already a transitive dep) and a static list of agent cases.

The harness is **DST-style**, not literal DST: there is no virtual-time scheduler, no simulated network, and no per-test seeded RNG. The “deterministic” part is:

- One fixed scenario script.
- Per-agent run with `--print` (or equivalent), with a fixed timeout.
- A fixed output contract: the agent must print a `mcp__plugin-lain_lain__*` tool name and the literal `Operational`.
- A shared `assert_cmd` runner that runs the same scenario against each agent, in order, with the same assertions.

That is the right size for what you actually need: a regression gate that runs every agent the same way, before every release. It is not a FoundationDB-class simulator; it is a deterministic run that produces a clear pass/fail per agent.

### Test binary: `tests/e2e/agent_install.rs`

One Rust integration test file with one test per agent and one shared helper module. The test is gated on `RUN_E2E_AGENT=1` so the default `cargo test` run stays fast.

#### `AgentCase` table

A `const AGENT_CASES: &[AgentCase]` array, one row per agent. The current set is:

| id           | binary        | run_args (besides `--print`)                          | requires_auth | cwd        |
|--------------|---------------|--------------------------------------------------------|---------------|------------|
| `kimi`       | `kimi`        | `--yolo --print-timeout 60s`                           | no            | langostino |
| `agy`        | `agy`         | `--dangerously-skip-permissions --print-timeout 60s`   | no            | langostino |
| `claude`     | `claude`      | `--dangerously-skip-permissions`                      | yes           | langostino  |
| `cursor`     | `cursor-agent` | `--print`                                             | yes           | langostino  |
| `cline`      | `cline`       | `--yolo --print --output-format json`                  | yes           | langostino  |
| `cn`         | `cn`          | `-p --output-format json`                              | yes           | langostino  |
| `omp`        | `omp`         | `-p --provider ollama --model qwen2.5:latest --yolo`  | no            | langostino  |
| `codex`      | `codex`      | `exec --yolo`                                         | yes           | langostino  |

When you sign in to an additional agent, you remove its `requires_auth = true` and the test stops being `#[ignore]`-marked. The test reports the auth status so a CI run can distinguish “all green” from “all green except auth-gated agents.”

#### One test per agent

```rust
#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_kimi() { run_case(&AGENT_CASES[0]) }
#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_agy() { run_case(&AGENT_CASES[1]) }
#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_claude() { run_case(&AGENT_CASES[2]) }
...
```

The body is the same one-liner that dispatches to `run_case(&AgentCase)`. Every per-agent test uses the same helper. This is the “deterministic” part: one function, eight call sites.

#### `run_case` semantics

For each `AgentCase`:

1. **Install.** Run `lain agents install --scope user <id>` against a temp `HOME`. The temp `HOME` is a fresh `tempfile::tempdir()`. Capture the install’s stdout/stderr.
2. **Spawn the agent.** Use `assert_cmd::Command::new(case.binary)` with `args(case.run_args)` and a stdin pipe. The test prompt is fixed:

   ```
   list the MCP tools you have, then call get_health on the one named lain, and print both the tool list and the get_health response verbatim
   ```

3. **Timeout.** Wait up to `case.timeout` (default 90 s) for the process to exit. Read the captured stdout and stderr into `String`.
4. **Assertions.** Apply the same five assertions regardless of agent:
   - **Tools list contains a Lain tool.** Assert that the captured stdout matches `mcp__plugin-lain_lain__\w+` (regex) at least once. Failure: agent did not load the Lain MCP server.
   - **`Operational` is in the output.** Assert the literal `Operational` (case-insensitive) appears in stdout. Failure: agent did not actually call `get_health`, or the call failed.
   - **`get_health` body looks right.** Assert the output contains `static_nodes` and a number greater than 1000. Failure: agent called `get_health` but the body was not the live one.
   - **Agent process exited cleanly.** `assert_cmd` reports non-zero exit; assert success. If the agent process needed a TTY or auth and failed to start, this catches it.
   - **Stderr has no fatal errors.** Assert that stderr does not contain `error sending request` or `connect error`. Failure: agent’s MCP handshake failed.

5. **Cleanup.** Remove the temp `HOME`. The next test gets a fresh one.

If `case.requires_auth`, the test still runs but the `Operational` assertion is dropped (the agent likely never got past auth). The test reports `auth-gated: skipped inner assertions` so a CI run can be `all green except auth-gated`.

#### File-edit round-trip (the user’s other ask)

After the basic scenario runs, the test also exercises the file-watcher path. The test creates a temp file `trigger.py` inside the watched workspace, waits up to 5 s, and asserts that `query_graph` (or a sidecar `get_health`) now reports a `Last Enriched Commit` or `Volatile Nodes` count that is non-zero. This is a deterministic test of the watcher + overlay path through the agent.

Concretely:

```rust
let trigger = temp_workspace.join("e2e_trigger.py");
std::fs::write(&trigger, "# lain e2e trigger\n").unwrap();
// Wait up to 5s for the watcher to pick it up.
let deadline = Instant::now() + Duration::from_secs(5);
let mut last = String::new();
while Instant::now() < deadline {
    last = call_get_health_over_singleton();
    if last.contains("Operational") && last.contains("e2e_trigger.py") {
        break;
    }
    std::thread::sleep(Duration::from_millis(500));
}
assert!(last.contains("e2e_trigger.py"), "watcher did not surface trigger file in get_health body");
```

This is a deterministic test of the watcher, run through the same agent-driven scenario.

#### Adapter round-trip (the user’s other ask)

For each `AgentCase`, the test re-runs the install loop (step 1) on a different `tempfile::tempdir()` and asserts that the resulting config file is valid JSON, contains the `mcpServers.lain` key, and points at the same command/URL that `lain agents install` would write. This is the same round-trip the existing `cmds::agents` unit tests do, but it is run as part of the same harness.

#### Coverage matrix

A summary printed at the end of the run:

```text
e2e_agy        PASS  (tools: mcp__plugin-lain_lain__*, get_health: Operational)
e2e_kimi       PASS  (tools: mcp__plugin-lain_lain__*, get_health: Operational, watcher: trigger visible)
e2e_claude     SKIP  (requires auth)
...
```

The summary is the report you read before each release.

## Data flow

```
RUN_E2E_AGENT=1 cargo test --test agent_install -- --nocapture
  └ for each AgentCase in AGENT_CASES:
       ├─ install: lain agents install --scope user <id> in temp HOME
       ├─ spawn: <binary> <run_args> with the fixed test prompt on stdin
       ├─ wait: up to 90 s for exit
       ├─ assert: mcp__plugin-lain_lain__* in stdout
       ├─ assert: "Operational" in stdout
       ├─ assert: "static_nodes" + number > 1000 in stdout
       ├─ assert: process exit code == 0
       ├─ assert: no fatal errors in stderr
       ├─ round-trip: re-run install in a fresh temp HOME, assert config JSON shape
       ├─ watcher: write a trigger file in the watched workspace, poll get_health for 5 s, assert trigger visible
       └─ cleanup: remove temp HOME
```

## Error handling

- **Agent process does not start.** `assert_cmd` reports the exit code and stderr. The test fails with a clear message: `agent <id> exited with code N; stderr: ...`.
- **Agent process is hung.** The test uses a `std::process::Child` with a `WaitTimeout` of 90 s. If the process does not exit, the test kills it and reports `agent <id> timed out after 90s; stderr: ...`.
- **MCP handshake fails.** The stderr assertion catches the `error sending request` line. Failure includes the stderr in the assertion message.
- **Auth-gated agent.** The test reports `auth-gated: skipped inner assertions` for `requires_auth = true` cases. CI is green except for those rows, and the report names them.

## Testing

### Automated

- `tests/e2e/agent_install.rs` — the new test file. 8 tests, one per agent. Each test is `#[ignore]`-marked by default and runs only with `RUN_E2E_AGENT=1`.
- `cargo test --test agent_install` without the env var runs the test-binary build but skips every test. CI runs `RUN_E2E_AGENT=1 cargo test --test agent_install -- --nocapture` before each release.

### Live verification

After the test passes:

1. Run `RUN_E2E_AGENT=1 cargo test --test agent_install -- --nocapture` and read the per-agent summary.
2. Run `cargo test --all-targets` to confirm no existing test regresses.
3. Run the live verify: `lain agents verify --all --json` against the live HTTP singleton, and confirm every installed agent is `Operational`.

## Acceptance criteria

- `tests/e2e/agent_install.rs` exists and is wired into `cargo test --all-targets` (the test binary builds; tests skip without the env var).
- With `RUN_E2E_AGENT=1`, the test binary runs and produces a per-agent summary.
- `tests/e2e/agent_install.rs` is gated so it does not run by default in `cargo test`. The gate is the env var.
- For non-auth-gated agents (Kimi, agy, omp), the test passes and reports `Operational` in the captured output.
- For auth-gated agents (Claude, Cursor, Cline, Continue, Codex), the test reports `auth-gated: skipped inner assertions` and the CI run is green except for those rows.
- The watcher round-trip asserts that a trigger file written inside the watched workspace shows up in `get_health` within 5 s.
- The adapter round-trip asserts that the install loop produces a valid JSON config with `mcpServers.lain` and the expected command/URL.
- Existing tests in `cargo test --all-targets` remain green (no regressions in `dual_instance`, `agents_install`, `cmds::agents`, watcher, graph, sidecar, or `lain-mcp-probe`).

## Unchanged behavior

- The `dual_instance.rs` test continues to pass. The new harness does not replace it; it adds a higher-level harness on top.
- The `cmds::agents` unit tests continue to pass.
- The `lain agents` subcommand continues to work the same way (the test exercises it, but does not change its behavior).
- No production code changes. The harness lives in `tests/` and uses only public APIs.

## Out of scope

- A real FoundationDB-class DST framework (virtual time, simulated network, fault injection). The harness is a one-process deterministic scenario run, not a multi-host simulator.
- A new install loop. The test exercises the existing install loop; it does not change it.
- New agent support. Adding a new agent is a one-line addition to the `AGENT_CASES` array.
- Visual companion. There is no UI surface to design.

## Trade-offs accepted

- The test takes up to 90 s per non-auth-gated agent (so the full harness can be 8 × 90 s = 12 minutes worst case). The default `cargo test` run skips the test entirely via `#[ignore]`. CI runs the test in a separate step with the env var, after the unit tests pass.
- The test depends on the live HTTP singleton. If the singleton is not running, the test fails. The test assumes the live `lain` is at `http://localhost:9999/mcp`. A `LAIN_PORT` env var can override this. A `LAIN_URL` env var can override the URL entirely.
- The watcher round-trip is the only one that depends on real timing (5 s poll). All other assertions are deterministic.
- The harness is Rust-only. A Python equivalent (`tests/e2e/lain_test.py`) already exists; this plan does not replace it. The Python one is useful for ad-hoc inspection; the Rust one is the CI gate.
