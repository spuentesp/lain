# `src/server/tools/handlers/` — for AI coding agents

You're editing one of the per-repo tool handler modules
(`architecture.rs`, `navigation.rs`, `search.rs`, `impact.rs`,
`metrics.rs`, `query.rs`, `cross_runtime.rs`, `enrichment.rs`,
`execution.rs`, `context.rs`, `gitops.rs`, `testing.rs`) or adding a
new domain.

**Before you write any code, read
[`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md#the-inventory-pattern-in-five-lines).**

The short version:

- Each domain module holds pure-domain logic for one tool family.
  One tool = one `pub fn` inside the module.
- The MCP-side glue (`ToolHandler` impl + `inventory::submit!`) lives
  in `registry_impl.rs`. When you add a `pub fn` to your domain
  module, the corresponding `ToolHandler` impl block goes there.
- Arg-extraction helpers (`str_arg`, `usize_arg`, `bool_arg`, etc.)
  live in `../utils.rs`. Import them; don't redefine.
- `format_duration` lives in `../utils.rs`. Don't redefine it in
  your handler. `scripts/check-format-duration-once.py` rejects a
  second definition.
- Don't add `let mut` shadowing of `ctx` or pass `ctx` through
  bespoke wrappers — `ToolContext` is the only context a handler
  sees.