# Documentation

Start with the [quickstart](QUICKSTART.md). It covers installation, a first
single-repository setup, federation, and basic troubleshooting.

## Operate Lain

- [User manual](USER_MANUAL.md): day-to-day operation and troubleshooting
- [Cookbook](COOKBOOK.md): deployment recipes
- [Federation](FEDERATION.md): multi-repository operation
- [`repos.yaml` reference](REPOS_YAML.md): configuration fields and examples
- [Command Center](command-center.md): the browser interface
- [Hot reload](hot-reload.md): config reload behavior
- [Multiplayer](multiplayer.md): agent presence, claims, and coordination
- [Pre-edit hooks](hooks.md): hook installation and behavior
- [Startup cache](STARTUP_CACHE.md): federation checkpoint reuse on
  unchanged startup, validation, and `LAIN_REQUIRE_STARTUP_CACHE`
- [Demo video](VIDEO.md): recorded walkthrough of the capability suite

## Query Lain

- [Tool guide](quickstart-tools.md): MCP tools and request examples
- [Skill bundle](../skills/README.md): agent-loadable skills and role recipes
- [Query tutorial](quickstart-query.md): a short introduction to `query_graph`
- [Query language reference](query-language.md): the full ops-array format
- [`tool-schema.json`](tool-schema.json): generated wire-format schema
- [Dynamic dispatch mitigation](dynamic-dispatch.md): the three-tier plan that closes the static-graph blind spot for message buses, DI containers, and schema-driven routers. Includes the `explain_dispatch` tool, the `dynamic-boundaries.md` template, and the backfill CLI.

## Understand and maintain it

- [Architecture](ARCHITECTURE.md): system boundaries and design choices
- [Protocol coverage](protocol-coverage.md): per-`ContractKey` kind
  sensors, joiner resolvers, binding grammar, and dashboard visibility —
  plus the bus/system detection backlog
- [Technical reference](TECHNICAL.md): modules, data structures, and internals
- [CI](CI.md): checks run in continuous integration
- [Branching](BRANCHING.md): integration, release, and protection policy
- [Distribution](DISTRIBUTION.md): pre-release and published-package checks
- [Release verification](VERIFICATION.md): how to verify a release artifact
- [Supply-chain security](SUPPLY_CHAIN.md): release and dependency controls
- [SafeSkill scoring](SAFESKILL.md): scope and interpretation of the score
- [Contributing as an agent](CONTRIBUTING_AGENTS.md): code-structure rules and
  automated guardrails
- [Agent UX design record](AGENT_UX_ROADMAP.md): stable milestone vocabulary
  referenced by code and tool descriptions
- [Rust gRPC contracts](RUST_GRPC_CONTRACTS.md) and
  [candidates](RUST_GRPC_CANDIDATES.md): generated-client identity and
  provider evidence
- [Confirmed bindings (MCP)](CONFIRMED_BINDINGS_MCP.md) and
  [RPC confirmation acceptance](RPC_CONFIRMATION_ACCEPTANCE.md): the
  binding-confirmation workflow and its acceptance record
- [NAT correctness audit](NAT_CORRECTNESS_AUDIT.md): HTTP consumer
  attribution findings, including the analyzer-revision-10 remediation
- [NVIDIA startup benchmark](NVIDIA_STARTUP_BENCHMARK.md): measured
  startup medians and their raw artifacts
- [Follow-ups](FOLLOWUPS.md): work deliberately deferred from merged
  changes

- [Vulnerability triage](VULNS.md): current OSV findings and remediation
- [OpenSSF Scorecard](SCORECARD.md): current external score and process gaps
- [Graph data-source decision](opinions/graph-tab-data-source.md): why the
  Command Center graph uses `get_workspace_graph`

`METADATA.toml` belongs to the release workflow rather than the reading path.

Completed implementation plans and retired working documents live in
[`docs/archive/`](archive/) — in the tree, out of the maintained reading path.
Older audits and review notes that were never promoted live in Git history.

## License

MIT, Copyright (c) 2026 spuentesp
