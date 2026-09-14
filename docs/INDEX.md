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

## Query Lain

- [Tool guide](quickstart-tools.md): MCP tools and request examples
- [Query tutorial](quickstart-query.md): a short introduction to `query_graph`
- [Query language reference](query-language.md): the full ops-array format
- [`tool-schema.json`](tool-schema.json): generated wire-format schema

## Understand and maintain it

- [Architecture](ARCHITECTURE.md): system boundaries and design choices
- [Technical reference](TECHNICAL.md): modules, data structures, and internals
- [CI](CI.md): checks run in continuous integration
- [Release verification](VERIFICATION.md): how to verify a release artifact
- [Supply-chain security](SUPPLY_CHAIN.md): release and dependency controls
- [SafeSkill scoring](SAFESKILL.md): scope and interpretation of the score
- [Agent UX roadmap](AGENT_UX_ROADMAP.md): active install and onboarding work
- [Graph data-source decision](opinions/graph-tab-data-source.md): why the
  Command Center graph uses `get_workspace_graph`

`METADATA.toml` belongs to the release workflow rather than the reading path.

Completed plans, audits, review notes, and the academic SRS package live in Git
history. Keeping them out of the current tree prevents old implementation notes
from competing with maintained documentation.

## License

MIT, Copyright (c) 2026 spuentesp
