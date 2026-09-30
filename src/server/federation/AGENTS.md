# `src/server/federation/` — for AI coding agents

You're editing the multi-repo coordination layer (`federated_index.rs`,
`repo_index.rs`, `repo_source.rs`, `cross_repo.rs`, `workspace.rs`,
`manifest.rs`, `loader.rs`, etc.).

**Before you write any code, read
[`docs/CONTRIBUTING_AGENTS.md`](../../../docs/CONTRIBUTING_AGENTS.md).**

The short version:

- The federation has one entry point (`FederatedIndex`) and one
  trait (`RepoSource`). Adding a new way to source a repo (e.g.
  `GitWorktreeSource`) means `impl RepoSource for X` + a constructor
  on `FederatedIndex` — nothing else.
- The graph backend is a trait (`GraphBackend`) with one impl
  (`PetgraphBackend`). Don't reach into petgraph directly from
  `FederatedIndex`; go through the trait. A `MemgraphBackend` is
  the deferred escape hatch.
- `cross_repo.rs` joins symbol edges across repos; the
  contract-federation `Binds` edges are owned by
  `contracts/joiner.rs` (PR 7). Don't add cross-repo joins in
  `repo_index.rs`.
- Federation tools are MCP tools — see `src/server/mcp/AGENTS.md`
  for how to register them.
- Contract tools (`list_services`, `get_service` — PR 16) live in
  `mcp/contract_tools/`. They ride the `ContractToolEntry`
  inventory (see `mcp/AGENTS.md`) and read the federation's
  `ContractIndex` after `rejoin_contracts_if_dirty`. New contract
  tools should add a `ToolDef` to `CONTRACT_TOOL_DEFS`
  (`mcp/definitions.rs`), a handler in `mcp/contract_tools/`, and an
  inventory `submit!`. Don't extend `dispatch_tool_call`.

The federation is the only place in Lain where per-process and
cross-process state can disagree. Be conservative: prefer reading
through a lock to taking a snapshot.