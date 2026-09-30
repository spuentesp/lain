# `src/server/mcp/` — for AI coding agents

You're editing the MCP transport adapter (`handler.rs`) or one of the
sub-namespace tool files (`presence_tools.rs`, `intent_tools.rs`,
`hook.rs`, `audit_tools.rs`, `federation_tools/*`,
`command_center_assets.rs`).

**Before you write any code, read
[`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md#the-inventory-pattern-in-five-lines).**

The short version:

- The 39 per-repo tools use the `ToolHandler` trait + inventory
  pattern. Adding one is a 5-line shape (see CONTRIBUTING_AGENTS.md).
- The 22 special-case tools (presence / federation / workspace) are
  in `dispatch_tool_call`'s `match name` block **today** but are
  moving to inventory sub-registries (`PresenceToolRegistry`,
  `FederationToolRegistry`, `WorkspaceToolRegistry`) over the next
  few PRs. **Don't add new arms** to `dispatch_tool_call`; if the
  sub-registry for your target sub-namespace isn't wired yet, add
  the handler file in the right sub-namespace and open an issue.
  Intent-layer tools (`lain_intent`, `list_active_intents`) live in
  `intent_tools.rs` and follow the same rule.
- `federation_tools/dto.rs` is being emptied. Don't add a DTO that
  mirrors `schema::*`; extend the schema type instead.
  `scripts/check-no-mirror-dtos.py` rejects it.
- Contract tools (PR 16+) live in `contract_tools/`. They ride
  `ContractToolEntry` — an `inventory::collect!`'d struct that
  pairs a tool name with an async handler
  (`BoxFuture<'static, Result<ToolOutcome, String>>`). Register
  with `inventory::submit!(ContractToolEntry { name, handler })`.
  Dispatch lives in `invoke_contract_inventory` inside
  `handler.rs::dispatch_tool_call` (right after `invoke_inventory`).
  **Don't add a new match arm to `dispatch_tool_call`** —
  `scripts/check-mcp-dispatch-shape.py` rejects it. The PR 16
  contract tools (`list_services`, `get_service`) are the only
  current entries; PR 13 will add the rest of the table.
- `handler.rs::handle_request` is an 800-line HTTP router god
  function. Don't add new routes to it without checking whether
  the request would fit better as an MCP tool — most "HTTP
  endpoint" additions should be MCP tools behind the JSON-RPC
  path. `POST /hook` is the documented exception: it's a
  per-call observation from the agent's host-specific hook layer,
  not a JSON-RPC envelope, so it lives in the router directly.