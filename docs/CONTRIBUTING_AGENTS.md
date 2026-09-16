# Contributing to LAIN — for AI coding agents

You are an AI agent (Kimi / Claude / Codex / Cursor / Copilot / Gemini /
Agy / Cline / Windsurf / opencode / Claude Code) editing Lain source
on behalf of a human maintainer. This document is the canonical "before
you write code under `src/`" checklist. The full architectural
rationale lives in [`docs/ARCHITECTURE.md`](ARCHITECTURE.md); the
high-level agent workflow lives in [`AGENTS.md`](../AGENTS.md).

## TL;DR — five rules, no exceptions

1. **Adding a per-repo tool?** Use `inventory::submit!(ToolHandlerEntry(&…))`. Don't add to `dispatch_tool_call`.
2. **Adding a sensor?** Use the `Sensor` trait + `inventory::submit!(SensorEntry(&…))`. Don't copy the walker from another sensor.
3. **Adding a node/edge DTO?** Extend `schema::GraphNode` / `schema::GraphEdge`. Don't create a parallel struct in `federation_tools/dto.rs`.
4. **Adding a time/duration formatter?** Add it to `tools/utils.rs`. Don't redefine in your handler.
5. **Adding a CLI timestamp helper?** Use `crate::server::time::unix_secs_u64`. Don't redefine `chrono_now_unix` in your file.

The CI guards in `scripts/check-*.py` enforce each of these. They run
on every PR. A "creative workaround" to bypass one of them is exactly
the kind of drift this document exists to prevent — please don't.

## The inventory pattern in five lines

```rust
pub struct XxxHandler;
#[async_trait::async_trait]
impl crate::server::tools::registry::ToolHandler for XxxHandler {
    fn name(&self) -> &'static str { "xxx" }
    fn description(&self) -> &'static str { "one-line summary" }
    fn input_schema(&self) -> &'static str { r#"{ "type":"object", … }"# }
    fn capability(&self) -> crate::server::tools::registry::ToolCapability {
        crate::server::tools::registry::ToolCapability::ReadOnly
    }
    async fn call(&self, ctx: &crate::server::tools::registry::ToolContext,
                  args: &serde_json::Map<String, serde_json::Value>)
        -> Result<String, crate::server::error::LainError> { … }
}
inventory::submit!(crate::server::tools::registry::ToolHandlerEntry(&XxxHandler));
```

That's the whole pattern. No central registry to edit, no enum arm to
add. The dispatcher (`ToolRegistry::dispatch`) iterates the inventory
collection at runtime — the registration is the side effect of the
`inventory::submit!` line being compiled in.

If you find yourself wanting to add an arm to a `match name` block,
**stop and re-read this section**. The match is the wrong place.

## Don't do these things

### ❌ Don't add a new arm to `dispatch_tool_call`

`src/server/mcp/handler.rs::dispatch_tool_call` is being phased out in
favour of `inventory`-registered sub-handlers. Today it still has
~22 arms that will move into `PresenceToolRegistry`,
`FederationToolRegistry`, and `WorkspaceToolRegistry` over the next few
PRs. **Do not add new arms to it.** If you need to expose a new
federation/presence/workspace tool:

1. Add a struct in `src/server/mcp/<presence,federation,workspace>_tools.rs`.
2. Implement `McpToolHandler` (or whatever the current registry trait is in that file).
3. Call `inventory::submit!(…(&Handler))`.
4. The dispatcher picks it up automatically.

If the inventory pattern is not yet wired in your target sub-namespace,
add the handler file in the right sub-namespace but **don't** also add
an arm to `dispatch_tool_call` — open an issue instead, and someone
will wire up the sub-registry.

### ❌ Don't copy the sensor walker

The five protocol sensors (`proto`, `openapi`, `graphql`, `http`,
`websocket`) all need to walk files under `root`, skip read-only
graphs, mint `(kind, path, name, namespace)` ids, and upsert one node
plus one edge per detected operation. They share **all** of that
pipeline; only the regex / AST logic differs.

If you're adding a sixth sensor (e.g. `thrift_sensor.rs`):

1. Create `src/server/sensors/thrift_sensor.rs`.
2. `pub struct ThriftSensor;` and `impl Sensor`.
3. Put **only** the detection logic in the impl body — the walker
   shell, the id-mint, the `is_read_only` guard, the
   `to_snake_case`/`to_camel_case` helpers, and the
   `find_handler_in_graph` resolver all live in
   `src/server/sensors/util.rs` and the trait default impls. Import
   them.
4. `inventory::submit!(SensorEntry(&ThriftSensor));`

If you copy the walker into your new file instead of importing it,
`scripts/check-no-duplicate-sensors.py` will fail.

### ❌ Don't define a DTO that mirrors `schema::*`

`schema::GraphNode { id, kind, name, path, … }` and
`schema::GraphEdge { edge_type, source_id, target_id, … }` are the
canonical types. If you need a federation-specific field
(`repo_id`, `cross_repo`), **add it to the schema type as an
`Option<…>`** and serialize it. Do not create a parallel struct in
`src/server/mcp/federation_tools/dto.rs`.

`scripts/check-no-mirror-dtos.py` rejects any new DTO whose field set
is a subset of an existing `schema::*` struct.

### ❌ Don't redefine `format_duration`

`fn format_duration(seconds: i64) -> String` lives in
`src/server/tools/utils.rs`. It already has callers in
`tools/handlers/architecture.rs` and `tools/handlers/gitops.rs`. If you
need a third caller, import it; don't redefine.

`scripts/check-format-duration-once.py` rejects a second definition
in any other handler file. The same applies to the `<3600`/`<60`/
`<86400` time-ladder literal — if you need it, the function is the
canonical home.

### ❌ Don't redefine `chrono_now_unix` or `now_unix`

`src/server/time.rs` is the canonical place for "now → unix seconds"
helpers. Use:

```rust
use crate::server::time::unix_secs_u64;
let now = unix_secs_u64();
```

The four CLI files (`cli/setup.rs`, `cli/hooks.rs`, `cli/mcp.rs`,
`cli/doctor.rs`) currently each have a private one-liner. Don't add a
fifth. If `server::time` doesn't expose what you need, add it there
and have everyone import it.

### ❌ Don't reach for `map_err` when `?` would do

`LainError` already has `From` impls for `std::io::Error`,
`serde_json::Error`, `git2::Error`, `toml::de::Error`,
`toml::ser::Error`, `bincode::Error`, and `serde_yaml::Error`.
Inline `map_err(|e| LainError::X(e.to_string()))` is only acceptable
when the conversion needs extra context (e.g. `"bincode: {e}"`).

If you find yourself writing `.map_err(|e| LainError::Io(e.to_string()))?`,
stop — check that the `From` impl exists, and if it doesn't, add it
to `src/server/error.rs` rather than working around it.

## When you're tempted to duplicate, ask first

The audit that motivated this document found three duplication
incidents:

1. `format_duration` redefined in two handler files.
2. `GraphNode` / `GraphEdge` DTOs in `federation_tools/dto.rs` that
   mirrored `schema::*` 1:1.
3. `to_snake_case` / `to_camel_case` redefined in three sensor files.

Each of those started as "I just need this one thing, it'll be
quicker to inline it." Two of them ended up in shipped code with
subtle disagreements (one used `<3600`, the other used `<60 * 60`;
one looked up the handler by snake_case only, the other also tried
camel_case). If you have the same feeling, **stop and ask** — the
canonical version probably already exists.

## Module-level AGENTS.md

Some directories have their own short `AGENTS.md`. If the file you're
editing has one, read it before writing — they have directory-specific
pointers (e.g. `src/server/sensors/AGENTS.md` will redirect you here
before you touch a sensor).

| Directory | Its `AGENTS.md` |
|---|---|
| `src/server/sensors/` | "Adding a sensor? Read CONTRIBUTING_AGENTS.md#sensor-pattern" |
| `src/server/mcp/` | "Adding a presence/federation/workspace tool? Read CONTRIBUTING_AGENTS.md#inventory-pattern" |
| `src/server/tools/handlers/` | "Adding a per-repo tool? Read CONTRIBUTING_AGENTS.md#inventory-pattern" |
| `src/server/federation/` | "Adding a federation source? Implement `RepoSource` and submit it via inventory." |

## What agents are NOT expected to do

You are not expected to:

- Refactor `LainServer` (the 30-field god struct). That's a
  multi-PR job; for now, only add fields with the section comments
  in `src/server/ingest/server.rs` as a guide.
- Split `src/server/graph.rs` (2,177 lines). It will move to
  `graph/persist.rs`, `graph/query.rs`, and `graph/freshness.rs`
  sub-modules; until that lands, don't *add* new persistence or
  freshness code to the top of `graph.rs` — open an issue.
- Replace the inventory pattern with `Box<dyn Trait>` or a
  proc-macro. The inventory pattern is intentional and isn't
  changing.
- Add dependencies. The Scorecard `Vulnerabilities` check is
  sensitive to new transitive deps; if you need one, open an issue
  and tag the maintainer.

## Questions?

If you're not sure whether your edit violates a rule, the safest
move is to:

1. Read the relevant `AGENTS.md` in the target directory.
2. Grep the canonical file (`rg -n 'fn <thing>' src/`) to see if it
   already exists.
3. If still unclear, stop and ask the maintainer.

The cost of asking is small. The cost of another `format_duration`
incident is a 200-line cleanup PR.