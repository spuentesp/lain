# Agent UX design record

This file preserves the milestone vocabulary referenced by code comments and
tool descriptions. The original implementation plan was retired on 2026-09-20
after its completed and aspirational sections began contradicting one another.
Git history contains the full plan.

For unfinished work, see the issue tracker. For user-facing
behavior, use the [quickstart](QUICKSTART.md), [user manual](USER_MANUAL.md),
and [tool guide](quickstart-tools.md).

## Outcome

Lain should feel like infrastructure an agent can assume exists: install a
published binary, configure an MCP client, discover readiness, ask a small
semantic tool surface for repository context, and recover from partial state
without guessing.

The stable design principles are:

1. Prefer progressive disclosure over advertising every low-level tool.
2. Return actionable readiness and recovery information; avoid dead ends.
3. Make structured agent output primary while keeping human diagnostics clear.
4. Provide a fast structural path before optional semantic enrichment.
5. Keep one canonical implementation path for each transport and capability.

## Milestones

| # | Milestone | Current state |
|---|---|---|
| 1 | Frictionless distribution | Published at npm `latest`; Linux and macOS acceptance pass. Windows clean-room installation is the active defect, tracked in the issue tracker. |
| 2 | Guided `lain setup` | Complete for generic, Claude Code, Codex, Cursor, VS Code, and Continue adapters. |
| 3 | `lain doctor` | Complete; shares the capability/readiness model used by `lain capabilities` and `lain status`. |
| 4 | Zero-config MCP startup | Complete; background indexing, per-repository readiness, cooperative cancellation, blocking-work isolation, and watcher handoff are implemented. The async-only upstream LSP transport remains a separately tracked limitation. |
| 5 | Agent bootstrap context | Complete through `understand_repository`. |
| 6 | Semantic Agent API | Complete through `find_symbol`, `get_context`, `find_related`, `assess_change`, `search_code`, and the dynamic-dispatch synthesis path. |
| 7 | Capability discovery | Complete through `get_capabilities` and readiness notifications. |
| 8 | First-class client recipes | Complete for the six supported setup adapters. |
| 9 | Distribution acceptance | Workflow implemented across three operating systems; the current Windows published-package failure is tracked as active work. |
| 10 | Intent + observability layer | Complete. The intent registry (`lain_intent`), the activity feed (`POST /hook`), the synchronous pre-edit endpoint (`POST /hook/evaluate`), the GREEN / YELLOW / RED coordination engine (with graph-distance refinement), the per-agent-kind observation wrappers (`hooks/{agy,codex,kimi}/pre-tool.sh` + `lain hooks observe` CLI), the system-prompt snippet (`lain setup --agent claude` writes `.lain/PROMPT.md`), the AGY end-to-end harness (`scripts/agy_e2e.sh` → `verdict.json`), the chaos harness (`scripts/agy_chaos.sh` → variant_{1,2,3}.json), and the linearizability stress test (`tests/coordination_linearizability.rs`, 100 iterations × 4 racers + N=10). Open follow-ups: linearizability across server crashes — surfaced by `agy_chaos.sh` variant 1 (OccupancyMap.load does not drop stale-by-agent claims). |

## Wire-surface policy

The default `semantic` profile advertises 15 curated inventory tools, plus
contextual server, workspace, and federation families when applicable. The
`full` profile advertises the complete generated schema. Tool names and counts
are contracts pinned in code and `docs/tool-schema.json`; documentation should
describe the current generated surface rather than preserving old counts.

## Readiness policy

- MCP transport starts before long-running indexing completes.
- Graph-independent tools remain available during warm-up.
- Graph-required calls receive structured loading or failure information.
- Federation readiness is evaluated per target repository and summarized for
  clients that need an aggregate state.
- Cancellation is cooperative across startup, indexing, watchers, and optional
  enrichment work.
- A stale graph is usable only when it is an intact snapshot isolated from an
  in-progress replacement.

## Maintenance

Do not append implementation diaries or completed PR inventories here. Update
the milestone table only when user-visible state changes, and place concrete
unfinished work with evidence and acceptance criteria in the issue tracker.
