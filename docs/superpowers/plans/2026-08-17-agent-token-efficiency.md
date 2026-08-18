# Agent Token Efficiency — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make lain cheaper to use than the grep-and-read loop it replaces. Today the standing cost is ~3,500 tokens of tool definitions injected into every agent session before any work happens, several tools cannot be called without first reading a doc, and one returns every result twice.

**Architecture:** A capability/profile filter over the tool list (Task 1), one new aggregate tool (Task 2), a budget parameter convention (Task 3), a schema-quality pass with a single root cause (Task 5), and two new tools that collapse multi-call workflows (Tasks 4, 6). No new Cargo deps.

**Branch:** `feat/agent-token-efficiency` off `main`.

> **Prerequisite:** `2026-08-17-federation-tool-wiring.md` should land first. Until it does, lain saves an agent zero tokens in federation mode, because the tools that would replace the grep loop return empty. Tasks 1, 5, and 7 here are independent and can proceed in parallel.

---

## Measured baseline (main @ `caa0427`)

```
tools/list payload   14,165 bytes  ≈ 3,541 tokens   59 tools
```

Injected into every agent's context at every session start. Most agents need roughly eight of the fifty-nine.

---

## Global Constraints

- **No new Cargo deps.**
- **Additive by default.** Removing a tool from the *default* profile must not remove it from the server — `--tools full` restores it.
- **Never trade correctness for brevity.** A truncated response must say it was truncated and how to get the rest.
- **All existing tests must pass.**

---

## File Structure

```
src/server/
├── mcp/handler.rs                (Task 1: profile filter; Task 5: FEDERATION_TOOL_DEFS schemas)
├── tools/
│   ├── registry.rs               (Task 1: capability + profile metadata)
│   ├── definitions.rs            (Task 3: max_tokens convention)
│   └── handlers/
│       ├── orientation.rs        (new — Task 2)
│       ├── impact.rs             (Task 6: blast_radius_for_diff)
│       └── search.rs             (Task 4: dedupe)
└── federation/federated_index.rs (Task 4: dedupe at the source)

src/cli/
├── orient.rs                     (new — Task 7)
├── impact.rs                     (new — Task 7)
└── mod.rs                        (Task 7: route both)
```

---

## Task 1: Tier the tool surface

**Files:** Modify `src/server/tools/registry.rs`, `src/server/mcp/handler.rs`

**Goal:** Cut the standing per-session cost by roughly 70% without removing capability.

- [ ] **Step 1: Tag every tool with a profile**

Add a `profile: ToolProfile` field to the registry entry and to `special_tool_definitions()`:

- `Core` — the tools an agent reaches for unprompted: `orient` (Task 2), `explain_symbol`, `get_blast_radius`, `get_call_chain`, `semantic_search`, `query_graph`, `search_org`, `trace_dependency`, plus the multiplayer set (`claim_files`, `release_files`, `list_occupancy`).
- `Extended` — useful but situational: `find_dead_code`, `suggest_refactor_targets`, `compare_modules`, `get_layered_map`, `get_coupling_radar`, `architectural_observations`, the testing and coverage tools.
- `Operator` — for the Command Center and humans, not agents: `get_server_status`, `get_reload_status`, `request_reload`, `list_recent_projects`, `get_federation_health`, `register_job_webhook`, `get_job_status`, `sync_state`, `install_language_server`.
- `Hidden` — never listed: `debug_sleep`.

- [ ] **Step 2: Drop the shell duplicates from `Core` and `Extended`**

`run_build`, `run_tests`, and `run_clippy` duplicate a shell that every agent already has. They cost tokens in every session to offer something the agent can already do, and they run in the wrong directory in federation mode besides. Put them in `Operator`.

- [ ] **Step 3: Remove `debug_sleep` from the listing entirely**

It is a job-infrastructure test fixture — "Sleep for the given number of seconds (useful for testing job infrastructure)" — and it is currently advertised to every connected client. Gate it behind `#[cfg(any(test, feature = "test-utils"))]` or the `Hidden` profile.

- [ ] **Step 4: Add the `--tools` flag and config key**

`lain server --tools core|extended|full` (default `core`), also settable as `tools_profile:` in `repos.yaml` so a project pins it once. `full` is today's behavior exactly.

- [ ] **Step 5: Test** that `--tools core` produces a strictly smaller `tools/list` than `--tools full`, that every `Core` tool is present in all profiles, and that `debug_sleep` appears in none.

**Verification:**
```bash
lain server --config repos.yaml --transport http --port 9871 --tools core &
curl -s -X POST localhost:9871/mcp -d '{"jsonrpc":"2.0","method":"tools/list","id":1}' \
  -H 'Content-Type: application/json' | wc -c
# target: under 5,000 bytes (from 14,165)
```

---

## Task 2: One orientation call

**Files:** Create `src/server/tools/handlers/orientation.rs`

**Goal:** Replace an agent's first fifteen exploratory tool calls with one.

**Context:** Landing in an unfamiliar repo, an agent currently needs `get_agent_strategy`, then `explore_architecture`, then `find_anchors`, then `get_layered_map` — four calls plus the reasoning to sequence them, plus a dozen `Read`/`Grep` calls to fill the gaps. That whole phase is the same every time and can be one response.

- [ ] **Step 1: Define `orient`**

```json
{
  "name": "orient",
  "description": "One-call orientation for an unfamiliar codebase: entry points, architectural anchors, module tree, and the files that change together. Call this first.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "max_tokens": {"type": "integer", "default": 1500},
      "focus": {"type": "string", "description": "Optional path or module to orient within, e.g. 'src/server/federation'"}
    }
  }
}
```

- [ ] **Step 2: Compose it from the existing handlers**

Entry points from `find_entry_points`, anchors from `find_anchors(limit)`, the module tree from `explore_architecture(max_depth=2)`, co-change clusters from `get_co_change_partners`. No new graph algorithms — this is composition and budget allocation.

- [ ] **Step 3: Budget the sections**

Divide `max_tokens` across the four sections with fixed proportions, and truncate each independently so one huge module tree cannot starve the anchors. Every truncated section says so and names the tool that returns the rest.

- [ ] **Step 4: Write for an agent reader.** Prose headers, one line per item, file paths clickable as `path:line`. Not JSON — this response is read, not parsed.

- [ ] **Step 5: Test** that `orient` on the lain repo returns under `max_tokens`, names `src/main.rs` among entry points, and mentions the federation module.

**Verification:** Manual read-through — the test is whether it actually orients you.

---

## Task 3: Make output budget a parameter, not a constant

**Files:** Modify `src/server/tools/handlers/*.rs`, `src/server/tools/definitions.rs`

**Goal:** Callers control response size, and always know when they got a partial answer.

**Context:** `get_context_for_prompt` already takes `max_tokens` — the right pattern, used in exactly one place. Everywhere else the caps are hardcoded and invisible to the caller: `take(20)` in `architecture.rs:43`, `take(5)` in `metrics.rs:247`, `take(8)` at `:266` and `:284`, `take(15)` at `architecture.rs:296`, `take(3)` at `context.rs:69`. An agent cannot tell a 20-item list from a 20-item truncation of 500.

- [ ] **Step 1: Add `max_tokens` to every list-returning tool's schema**, optional, with the current hardcoded cap as the documented default. No behavior change when omitted.

- [ ] **Step 2: Return truncation state.** Every truncated response ends with an explicit line: `[truncated: showing 20 of 347 — raise max_tokens or narrow with focus]`. `context.rs:78` already does this; make it universal.

- [ ] **Step 3: Add a cursor where the full set is genuinely useful** — `search_org`, `find_dead_code`, `suggest_refactor_targets`. `{items, truncated, next_cursor}`; a follow-up call with the cursor resumes. Skip this for tools whose full output is never wanted.

- [ ] **Step 4: Test** that a tool with `max_tokens: 200` returns a materially smaller response than the default and that the truncation notice appears.

---

## Task 4: Stop returning every symbol twice

**Files:** Modify `src/server/federation/federated_index.rs`, `src/server/tools/handlers/search.rs`

**Goal:** Halve `search_org`'s response for zero information loss.

**Context:** `search_org` returns each hit twice — once keyed by a UUID `global_id`, once by the structured `repo:Kind:path:name` form — for the identical symbol:

```json
[{"global_id":"02fef78d-7a09-5c48-b7af-6f86dcd92439","name":"run_doctor","path":"…/doctor.rs"},
 {"global_id":"lain:Function:/…/doctor.rs:run_doctor","name":"run_doctor","path":"…/doctor.rs"}]
```

- [ ] **Step 1: Find why both id forms exist in the backend.** Two writers are producing ids for the same symbol — likely `upsert_node` (which parses an existing id) versus `upsert_node_global` (which constructs one). Establish which is canonical.

- [ ] **Step 2: Converge on one.** The structured form is self-describing and human-readable; prefer it. Migrate or dedupe the UUID form at the projection boundary (`project_repo`), not in the search handler — fixing it at the source also fixes `list_nodes`, `all_edges`, `resolve_symbol`, and the Command Center graph.

- [ ] **Step 3: Add a defensive dedupe in `search_org`** on `(repo_id, path, name, kind)` regardless, so a future writer cannot reintroduce it silently.

- [ ] **Step 4: Test** that a symbol with both id forms in the backend yields exactly one search hit.

---

## Task 5: Fix the federation tool schemas at their root cause

**Files:** Modify `src/server/mcp/handler.rs` (`FEDERATION_TOOL_DEFS` and its two synthesizers)

**Goal:** Federation tool schemas stop misleading agents and stop causing retries.

**Context:** All of the following come from **one** place. `FEDERATION_TOOL_DEFS` is a `(name, description, required)` tuple table carrying no per-parameter information, so the schema synthesizer at `handler.rs:437` — duplicated verbatim at `handler.rs:1683` — invents both the type and the description for every parameter:

```rust
p.insert("type".into(), serde_json::Value::String("string".into()));
p.insert("description".into(), serde_json::Value::String(format!("{req} of the repo to look up")));
```

That single template produces every one of these:

| Symptom | Consequence |
|---|---|
| `search_org.limit` typed `string`, description "limit of the repo to look up" | Agent sends a string; description is nonsense |
| `search_org.limit` in `required` | Agent must invent a limit to search at all |
| `get_cross_repo_blast_radius.depth` typed `string` holding `"1..3"` | A string DSL where `{min,max}` would self-validate |
| `get_cross_repo_blast_radius.symbol` described as "symbol of the repo to look up" | Actively misleading |

- [ ] **Step 1: Give `FEDERATION_TOOL_DEFS` real parameter schemas.** Replace the `required: &[&str]` field with a proper per-parameter list carrying name, JSON type, description, and whether it is required. This is the whole fix; everything below follows from it.

- [ ] **Step 2: De-duplicate the synthesizer.** The same block exists at `:437` and `:1683`. Extract one function — they have already drifted once in spirit and will again.

- [ ] **Step 3: Correct the specific schemas.**
  - `search_org.limit` → `integer`, optional, default 20.
  - `get_cross_repo_blast_radius.depth` → `{"type":"object","properties":{"min":{"type":"integer"},"max":{"type":"integer"}}}`, accepting the legacy `"1..3"` string for one release.
  - Write real descriptions for every parameter.

- [ ] **Step 4: Give `query_graph` a discoverable schema.**

It is the flagship query tool and its schema is `{"query": {"type": "object"}}` with `required: []`. An agent cannot call it without first fetching `docs/query-language.md` — a doc read that costs more tokens than the schema would. Put the ops array in the schema: an array of objects with `op` as an enum of `find | connect | filter | semantic_filter | group | sort | limit`, each op's parameters described inline, and one worked example in the tool description.

- [ ] **Step 5: Add a schema lint test.** Assert that no tool's parameter description matches `/of the repo to look up$/`, that no parameter is typed `string` while its description says it is parsed as a number, and that every tool with required parameters documents each one. This is cheap and it pins the whole class.

**Verification:**
```bash
call search_org '{"query":"run_doctor"}'   # no limit → must succeed
cargo test --test schema_lint
```

---

## Task 6: `blast_radius_for_diff`

**Files:** Modify `src/server/tools/handlers/impact.rs`

**Goal:** Answer the question agents actually have, in one call.

**Context:** An agent almost never wants "what calls `foo`". It wants "I am about to change these three hunks — what breaks?". Today that requires reading the diff, identifying each changed symbol by hand, calling `get_blast_radius` per symbol, and merging the results.

- [ ] **Step 1: Define the tool.** Optional `base` (default: working tree vs `HEAD`), optional `paths` filter, `max_tokens`.

- [ ] **Step 2: Map hunks to symbols.** `GitSensor` gives the diff with line ranges; `GraphDatabase::get_node_at_location(path, line)` maps each changed line to its enclosing symbol. Deduplicate.

- [ ] **Step 3: Return the union blast radius**, grouped by changed symbol, with each affected symbol's distance. Note which changed lines resolved to no symbol — that is a real signal (config, docs, generated code), not a gap to hide.

- [ ] **Step 4: Reuse it for multiplayer.** This is also the commit-time overlap primitive from `docs/wish-list.md` — the same machinery answers "do these two branches touch the same symbols?". Keep the symbol-set extraction in a helper both can call.

- [ ] **Step 5: Test** on a fixture repo with a known call chain: modify the leaf, assert the caller appears.

---

## Task 7: CLI parity for short-lived workers

**Files:** Create `src/cli/orient.rs`, `src/cli/impact.rs`; modify `src/cli/mod.rs`

**Goal:** A subagent that lives twenty seconds should not pay an MCP handshake plus 3,500 tokens of tool definitions to ask one question.

- [ ] **Step 1: Add `lain orient [--focus <path>] [--max-tokens N]`** — the Task 2 handler over the persisted graph, printed to stdout.

- [ ] **Step 2: Add `lain impact <symbol>`** and `lain impact --diff` — the Task 6 handler.

- [ ] **Step 3: Reuse the handlers, do not reimplement.** Both commands should call the same functions the MCP tools call. `lain query` and `lain ask` are the precedent.

- [ ] **Step 4: Update the README.** It currently says lain exposes "exactly five subcommands"; the binary already has seven (`hooks`, `doctor`), and this adds two more. Replace the fixed count with the generated list.

**Verification:** `lain orient` and `lain impact run_doctor` both produce useful output with no server running.

---

## What this plan does *not* fix

- **Response caching.** Repeated identical tool calls re-compute. Worth measuring before building.
- **Streaming responses.** MCP supports it; nothing here needs it yet.
- **The `get_agent_strategy` guide** stays as-is; Task 2's `orient` is the replacement in practice, and the guide can be retired once `orient` proves out.

## Definition of done

1. `tools/list` under `--tools core` is under 5,000 bytes, down from 14,165.
2. `debug_sleep` appears in no profile.
3. `orient` returns a usable orientation for an unfamiliar repo within its token budget.
4. `search_org` returns each symbol once, and works without a `limit`.
5. `query_graph` is callable from its schema alone, without reading `docs/query-language.md`.
6. `blast_radius_for_diff` returns the affected set for an uncommitted change.
7. The schema lint test passes and no parameter description reads "… of the repo to look up".
