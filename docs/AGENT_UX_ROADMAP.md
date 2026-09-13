# Agent UX Roadmap

> Goal: make LAIN feel like infrastructure an agent can assume exists.
>
> A developer should be able to install it, connect an MCP client, and get useful repository intelligence with almost no configuration. An agent should be able to discover what LAIN knows, choose the right capability, and recover from partial readiness without human intervention.

## Product outcome

The target experience is intentionally boring:

```bash
npx @spuentesp/lain-mcp
```

or, after installation:

```bash
lain setup
lain doctor
lain mcp
```

The user should not need to know that LAIN is written in Rust, where the graph is stored, which language server backs a capability, whether an embedding model is installed, or which of dozens of MCP tools should be called first.

The product promise is:

1. **Install without compiling.**
2. **Detect the repository automatically.**
3. **Expose useful structural intelligence immediately.**
4. **Degrade gracefully while optional capabilities warm up.**
5. **Give agents a small semantic interface instead of forcing tool archaeology.**
6. **Make health, freshness, and readiness machine-readable.**
7. **Keep advanced internals available without making them the default UX.**

---

## UX principles

### 1. Progressive disclosure

The default path should contain the fewest concepts possible. Advanced users can still reach federation, custom models, custom LSPs, graph operations, and low-level tools.

Default:

```text
install → setup → agent connects → useful answer
```

Advanced:

```text
custom install → explicit config → federation → low-level graph/LSP tools
```

### 2. Zero dead ends

Every failure should answer three questions:

- What happened?
- What still works?
- What can the user or agent do next?

Bad:

```text
semantic search unavailable
```

Better:

```text
Semantic search is warming up.
Structural search, symbol lookup, call graph, and blast radius are ready.
Run `lain doctor --fix` to install the optional model now.
```

### 3. Agent-readable first, human-readable always

Anything that describes runtime state should have a stable structured form. Human CLI output can remain attractive, but JSON must be available for automation.

```bash
lain doctor --json
lain capabilities --json
lain status --json
```

### 4. Fast path before perfect path

Structural intelligence should become usable before optional enrichment finishes. AST/tree-sitter, Git, and cached graph data should not be blocked by embeddings or slow language-server startup.

### 5. One canonical path

There can be multiple installation mechanisms, but the docs should recommend one. There can be many MCP clients, but setup should present one consistent configuration model.

---

# Milestone 1 — Frictionless distribution

## User story

> I want to try LAIN in an arbitrary repository without installing Rust or reading installation documentation.

## Target UX

Preferred:

```bash
npx @spuentesp/lain-mcp
```

Installed form:

```bash
npm install -g @spuentesp/lain-mcp
lain --version
```

Native packages remain valid secondary paths:

```bash
brew install spuentesp/tap/lain
cargo install lain
```

## Design

The npm package should be a thin native launcher, not an alternate implementation.

```text
npx
 │
 ▼
@spuentesp/lain-mcp
 │
 ├─ detect OS + architecture
 ├─ resolve LAIN version
 ├─ find cached binary
 ├─ verify checksum
 ├─ download release asset if missing
 └─ exec native binary
```

### Release artifacts

Publish deterministic native assets from GitHub Releases:

```text
lain-linux-x64
lain-linux-arm64
lain-darwin-x64
lain-darwin-arm64
lain-windows-x64.exe
SHA256SUMS
```

Add other targets only when CI actually validates them.

### Launcher behavior

Cache binaries under an OS-appropriate per-user cache directory. The launcher should:

- never require administrator privileges;
- verify SHA-256 before execution;
- use an atomic download/rename flow;
- support `LAIN_VERSION` for pinning;
- support offline reuse of an already cached binary;
- print download progress only when attached to a TTY;
- keep MCP stdout protocol-clean by sending diagnostics to stderr.

## Acceptance criteria

- Clean Linux, macOS, and Windows runners can execute `npx @spuentesp/lain-mcp --version`.
- No Rust toolchain is required.
- A second invocation uses the cache and performs no network download.
- Corrupt or mismatched checksums fail closed with a useful error.
- Release CI proves every advertised artifact boots.

---

# Milestone 2 — `lain setup`: guided, modern onboarding

## User story

> I cloned a repository. Configure LAIN for my agent without making me understand MCP internals.

## Target UX

```text
$ lain setup

  LAIN setup

  Repository        ✓ ~/src/my-project
  Languages         ✓ Rust, TypeScript
  Structural index  ✓ ready
  Git history       ✓ ready
  Semantic search   ○ optional model not installed

  Choose an agent
  › Claude Code
    Codex
    Cursor
    VS Code
    Continue
    Generic MCP

  Configuration     ✓ written
  Connection        ✓ verified

  Ready. Ask your agent:
  “What is the blast radius of changing validate_token?”
```

Non-interactive forms:

```bash
lain setup --agent claude-code
lain setup --agent codex
lain setup --agent cursor
lain setup --agent generic --json
```

## Design

`setup` should be idempotent. Re-running it must inspect and update rather than duplicate configuration.

### Detection phase

Detect:

- Git root;
- languages and frameworks;
- existing `.lain` state;
- installed language servers;
- existing agent configuration;
- semantic model availability;
- whether the repository is already indexed.

### Configuration adapters

Implement client-specific adapters behind one interface:

```text
AgentAdapter
 ├─ detect()
 ├─ config_path()
 ├─ read()
 ├─ plan_changes()
 ├─ apply()
 └─ verify()
```

Adapters should exist for supported clients, while `generic` prints or writes standard MCP JSON.

### Safety

Before modifying a client config:

- parse it structurally;
- preserve unrelated settings;
- show a concise plan in interactive mode;
- create a timestamped backup when modifying a user-owned file;
- expose `--dry-run`;
- expose `--print-config` for users who do not want automatic writes.

## Acceptance criteria

- Re-running setup causes no duplicate MCP entries.
- Every adapter has fixture tests for empty, existing, malformed, and already-configured files.
- `--dry-run` performs no writes.
- Setup can succeed even when semantic search is unavailable.
- The final verification starts LAIN and performs an MCP initialize/tools-list round trip.

---

# Milestone 3 — `lain doctor`: one trustworthy diagnosis surface

`doctor` already exists. The UX milestone is to make it the single authoritative answer to “is LAIN usable here?”

## Target UX

```text
$ lain doctor

  LAIN 0.x.x                 agent-ready

  Core
  ✓ Repository     ~/src/my-project
  ✓ Git            2.5x
  ✓ Graph          current @ 8d5a6fc
  ✓ MCP stdio      healthy

  Intelligence
  ✓ Symbols        ready
  ✓ Call graph      ready
  ✓ Git co-change   ready
  ○ Semantic        optional model not installed

  Integrations
  ✓ Claude Code     configured
  - Cursor          not configured

  Agent-ready: YES
```

Machine form:

```bash
lain doctor --json
```

Suggested schema:

```json
{
  "agent_ready": true,
  "repository": {
    "root": "/repo",
    "head": "8d5a6fc",
    "indexed_commit": "8d5a6fc",
    "working_tree_overlay": true
  },
  "capabilities": {
    "symbols": "ready",
    "call_graph": "ready",
    "git_history": "ready",
    "semantic_search": "unavailable_optional"
  },
  "integrations": {
    "claude_code": "configured"
  },
  "problems": []
}
```

### `--fix`

Where the repair is deterministic and safe:

```bash
lain doctor --fix
```

Examples:

- recreate missing cache directories;
- remove stale session files;
- repair a generated MCP entry;
- download an optional model after confirmation;
- trigger a stale index refresh.

Never silently install unrelated system packages or mutate source files.

## Acceptance criteria

- `doctor --json` is versioned as a compatibility surface.
- Exit codes distinguish ready, degraded-but-usable, and unusable.
- Every reported problem includes a remediation action where possible.
- MCP stdout remains clean when doctor logic is reused during startup.

---

# Milestone 4 — Zero-config MCP startup

## User story

> My agent starts LAIN from somewhere inside the repository. LAIN figures out the rest.

## Target lifecycle

```text
MCP process starts
      │
      ▼
find nearest .git root
      │
      ▼
load existing graph ───────────────┐
      │                            │
      ├─ graph current → ready     │
      │                            │
      └─ graph stale/missing       │
                │                  │
                ├─ expose safe     │
                │  capabilities    │
                └─ refresh async ──┘
```

The first useful tool call should not require a separate `lain init` step.

### Readiness model

Capabilities should have explicit states:

```text
ready
warming_up
stale_usable
unavailable_optional
unavailable_error
```

An agent can therefore decide whether to wait, fall back, or continue.

### Startup rules

- Find the nearest Git root by walking upward.
- Prefer a usable cached graph immediately.
- Apply the working-tree overlay before claiming freshness.
- Reindex in the background when possible.
- Never block structural tools on optional semantic initialization.
- Emit readiness notifications/events when capabilities transition to ready.

## Acceptance criteria

- Starting `lain mcp` inside a nested repository directory works.
- A warm repository exposes structural tools within a small latency budget.
- A missing semantic model does not fail MCP startup.
- Stale state is clearly represented rather than silently treated as current.

---

# Milestone 5 — Agent bootstrap context

## Problem

A new agent should not spend several calls learning what repository it is in, which LAIN capabilities are available, and which tools are appropriate.

## Proposed MCP tool

```text
bootstrap_repository_context
```

Example response:

```json
{
  "repository": {
    "name": "lain",
    "languages": ["Rust", "JavaScript"],
    "head": "8d5a6fc",
    "dirty": false
  },
  "architecture": {
    "entry_points": ["src/main.rs"],
    "anchors": ["Server", "GraphDatabase", "Indexer"],
    "important_paths": ["src/server", "src/graph", "tests/use_cases"]
  },
  "capabilities": {
    "structural": "ready",
    "semantic": "warming_up",
    "git_history": "ready"
  },
  "recommended_actions": [
    {
      "intent": "understand a symbol",
      "tool": "get_context"
    },
    {
      "intent": "assess a change",
      "tool": "assess_change"
    }
  ]
}
```

The payload should respect a token/size budget:

```json
{ "budget_tokens": 3000 }
```

## Design constraints

- deterministic ordering;
- stable top-level schema;
- no giant source dumps;
- include freshness and confidence metadata;
- favor repository anchors, entry points, and architectural boundaries;
- use semantic enrichment only when already available.

## Acceptance criteria

- A fresh MCP client can orient itself with one call.
- The tool stays useful when semantic search is absent.
- Responses remain bounded and deterministic enough for regression tests.

---

# Milestone 6 — A small semantic Agent API

## Problem

LAIN's detailed MCP surface is powerful, but a large tool list increases selection cost and makes client behavior less predictable.

Do not remove the low-level tools. Add a preferred semantic layer.

## Proposed primary tools

```text
understand_repository
find_symbol
get_context
find_related
assess_change
search_code
query_graph
get_health
```

Possible intent mapping:

| Agent intent | Primary tool | Internal composition |
|---|---|---|
| “Where is X?” | `find_symbol` | symbol index, aliases, paths |
| “Explain X” | `get_context` | symbol, callers, callees, source, docs |
| “What is connected to X?” | `find_related` | graph edges, co-change, semantic links |
| “What breaks if I change X?” | `assess_change` | blast radius, call sites, tests, co-change |
| “Find code that does Y” | `search_code` | lexical + structural + optional semantic |
| “Understand this repo” | `understand_repository` | bootstrap/anchors/entry points |
| Advanced graph question | `query_graph` | existing graph query surface |
| Trust/readiness | `get_health` | structured capability state |

### Tool descriptions matter

Each primary tool description should tell the model:

- when to use it;
- when not to use it;
- expected cost;
- whether it needs semantic readiness;
- which low-level alternatives exist.

### Composition over duplication

Primary tools should orchestrate existing internal capabilities instead of introducing parallel indexing or duplicate business logic.

## Acceptance criteria

- The primary tool set covers common repository navigation and change-analysis tasks.
- Existing specialized tools remain accessible.
- Agent benchmark prompts select the semantic tool more reliably than before.
- Tool schemas remain concise enough for MCP clients with limited context budgets.

---

# Milestone 7 — Capability discovery and readiness

Introduce a cheap capability endpoint/tool that an agent can call before planning.

```json
{
  "server_version": "0.x.x",
  "repository": "my-project",
  "capabilities": {
    "symbols": { "state": "ready" },
    "call_graph": { "state": "ready" },
    "git_history": { "state": "ready" },
    "semantic_search": {
      "state": "warming_up",
      "optional": true
    }
  },
  "freshness": {
    "head": "abc123",
    "indexed_commit": "abc123",
    "working_tree_overlay": true
  }
}
```

This should be substantially cheaper than a full health diagnostic and safe to call frequently.

## Acceptance criteria

- Agents never need to infer readiness from error strings.
- Capability state transitions are testable.
- Freshness is explicit.
- Optional capability absence is distinguishable from failure.

---

# Milestone 8 — First-class client recipes

Support a small set of integrations well rather than documenting dozens poorly.

Initial matrix:

| Client | Install | Auto-config | Verified in CI/manual contract |
|---|---:|---:|---:|
| Claude Code | yes | yes | yes |
| Codex | yes | yes | yes |
| Cursor | yes | yes | yes |
| VS Code | yes | yes | yes |
| Continue | yes | yes | yes |
| Generic MCP | yes | config output | yes |

Every integration page should fit this shape:

```text
1. Install LAIN
2. Run `lain setup --agent <name>`
3. Restart client if required
4. Ask one known-good question
```

Avoid client-specific product logic inside the core server. Client adapters belong at the configuration boundary.

---

# Milestone 9 — Distribution acceptance CI

The real acceptance test is not “Cargo tests pass.” It is “a stranger can install LAIN in a clean environment and an MCP client can talk to it.”

Add a clean-room matrix:

```text
Linux x64
macOS arm64
Windows x64
```

For each target:

1. install only Node where required for the npx path;
2. run the public install command;
3. verify `lain --version`;
4. create/clone a tiny fixture repository;
5. run `lain doctor --json`;
6. launch MCP stdio;
7. perform `initialize`;
8. list tools;
9. call capability discovery;
10. call one structural query;
11. assert protocol-clean stdout.

Add a separate release-gate job that downloads exactly the published release artifacts rather than workspace-built binaries.

## Acceptance criteria

A release cannot be considered distribution-green unless the clean-room path passes on every advertised platform.

---

# Visual and interaction language

The CLI should feel calm and intentional rather than verbose.

## Human output

Use:

- short headings;
- aligned status rows;
- `✓`, `○`, `!`, `×` where terminals support Unicode;
- a `NO_COLOR` fallback;
- one recommended next action;
- no animated spinner when stdout/stderr is not a TTY.

Example:

```text
  LAIN setup

  Repository        ✓ ~/src/lain
  Structural graph  ✓ current
  Semantic model    ○ optional
  Claude Code       ✓ configured

  Ready.
```

## Structured output

Every important operational command should support JSON without ANSI escapes:

```bash
lain setup --json
lain doctor --json
lain capabilities --json
lain status --json
```

## Errors

Prefer typed remediation:

```text
× Could not update Claude Code configuration
  ~/.config/claude/config.json contains invalid JSON.

  Nothing was changed.
  Fix the file and run:
    lain setup --agent claude-code
```

Avoid stack traces by default. Offer `--verbose` / `RUST_LOG` for diagnostics.

---

# Suggested implementation order

## Phase A — Distribution foundation

1. Release artifact naming and checksums.
2. npm launcher with platform detection and cache.
3. clean-room `npx ... --version` CI.

**Why first:** every other UX improvement is easier to adopt once installation is trivial.

## Phase B — Trust and onboarding

4. Normalize structured capability/readiness state.
5. modernize `doctor` around that state.
6. implement `setup` core and generic MCP adapter.
7. add client adapters incrementally.

**Why second:** setup should consume a reliable health/readiness model instead of inventing a parallel one.

## Phase C — Agent-native interface

8. bootstrap repository context.
9. semantic primary tool layer.
10. tool descriptions and recommendation metadata.
11. agent selection benchmarks.

**Why third:** simplify the agent surface after the server can reliably report what is ready.

## Phase D — Polish and release gates

12. capability transition notifications.
13. optional background model acquisition UX.
14. cross-platform clean-room MCP contract tests.
15. update Quickstart to the new canonical path.

---

# Proposed issue breakdown

Keep implementation PRs small and independently shippable.

1. **Release: publish native binaries and SHA256SUMS**
2. **npm: native launcher with verified binary cache**
3. **CI: clean-room launcher smoke tests**
4. **Core: structured capability/readiness model**
5. **CLI: redesign `lain doctor` + stable JSON output**
6. **CLI: implement idempotent `lain setup` framework**
7. **Setup: generic MCP adapter**
8. **Setup: Claude Code adapter**
9. **Setup: Codex adapter**
10. **Setup: Cursor / VS Code / Continue adapters**
11. **MCP: bootstrap repository context tool**
12. **MCP: primary semantic agent API**
13. **MCP: capability discovery tool/resource**
14. **Bench: agent tool-selection benchmark suite**
15. **CI: end-to-end MCP distribution acceptance matrix**
16. **Docs: replace install-first quickstart with setup-first onboarding**

Each issue should define UX screenshots/transcripts, implementation constraints, and acceptance tests before coding.

---

# Definition of done for the overall initiative

LAIN reaches the intended UX bar when a developer on a clean supported machine can:

```bash
cd existing-repository
npx @spuentesp/lain-mcp setup --agent <supported-agent>
```

and then ask the agent a repository question without manually installing Rust, editing MCP JSON, prebuilding an index, configuring an embedding model, or learning LAIN's internal tool catalog.

The connected agent can independently determine:

- where the repository is;
- what LAIN capabilities are ready;
- whether its knowledge is fresh;
- which high-level tool to call;
- what fallback remains if an optional subsystem is unavailable.

That is the product boundary: **powerful internals, nearly invisible setup.**
