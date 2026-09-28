# Contract Federation — Implementation Tracker

Tracks delivery of [`CONTRACT_FEDERATION.md`](CONTRACT_FEDERATION.md)
(target LAIN 0.9.0, baseline v0.8.0). Update this file in the same PR
that changes a row's state.

- **Integration branch:** `feat/contract-federation` (off `dev`); PRs target `dev`.
- **Tag 0.9.0 by:** 2026-10-12 · **Hackathon deadline:** 2026-10-30 10:00 PT
- **Release:** version bumps happen only in the `release/v0.9.0` PR (see `AGENTS.md`).

Status legend: `todo` · `wip` · `review` · `done` · `cut`

## PRs

| # | PR | Depends on | Week | Status | Branch / PR | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Fixture org (`orders`, `billing`, `reports`) and ground-truth manifest | — | Sep 29 | todo | | |
| 2 | F1: `GlobalId` percent-encoding + round-trip proptest | — | Sep 29 | todo | | |
| 3 | Schema v3: new node/edge types, `ContractMeta`, `SourceSite`, `Confirmed` provenance, derived kind check, migration notes | 2 | Sep 29 | todo | | |
| 4 | F2: `traverse_impact` + propagation table; blast radius migrated, tests unchanged | 3 | Sep 29 | todo | | |
| 5 | Shared template normalizer + `util::enclosing_symbol` | 3 | Sep 29 | todo | | |
| 6 | `http_client_sensor` (TypeScript, Python) | 5 | Sep 29 | todo | | |
| 7 | F3: `services` / `http_clients` / `bindings` config, `ContractJoiner::join_http`, `rejoin_contracts`, reconciliation skips `Binds` | 4, 6 | Sep 29 | todo | | |
| 8 | OpenAPI request/response schemas and fields | 3 | Oct 6 | todo | | |
| 9 | `field_access_sensor` + field join | 7, 8 | Oct 6 | todo | | |
| 10 | `GitRevisionSource`, worktree cache, per-commit index cache | 3 | Oct 6 | todo | | |
| 11 | `Snapshot`, `FederatedIndex::from_snapshot`, job queue; `prepare_snapshot`, `get_snapshot` | 7, 10 | Oct 6 | todo | | |
| 12 | `diff_contracts`, `classify`, `evaluate`, coverage | 9, 11 | Oct 6 | todo | | |
| 13 | `contracts` profile: envelope, `api_version`, error codes, remaining tools, schema dump, golden tests | 11, 12 | Oct 6 | todo | | Release PR follows |
| 14 | `http_client_sensor` (Rust, Go) | 6 | stretch | todo | | Cut 2nd |
| 15 | JSON Schema + proto fields, `event_sensor`, topic join | 7, 8 | stretch | todo | | Cut 1st |

Critical path: 1–13. Cut order if late: 15 → 14 → `check_binding` / `read_source`.
Never cut: versioned envelope, coverage reporting, `resolve_evidence`.

## Foundational checklist

### F1 — GlobalId encoding (PR 2)
- [ ] `GlobalId::new` percent-encodes `%` and `:` in path and name; `parse` requires 5 segments; accessors decode
- [ ] All hand-built / hand-split ids (`split(':')`, `format!`, `global_id_str`) routed through `GlobalId`
- [ ] `is_known_node_kind` replaced by `NodeType::all()` check
- [ ] Proptest round-trip (names with `:`, `%`, `::`)
- [ ] Regression: `:id` route projects and is found by name

### F2 — Typed impact traversal (PR 4)
- [ ] `traverse_impact(start, depth, cap) -> ImpactResult` with predecessor map
- [ ] Exhaustive `impact_propagation` table, only `Calls` = `Incoming` initially
- [ ] `get_cross_repo_blast_radius` rebuilt on it; response shape unchanged
- [ ] Later PRs flip their edge types from `Stop` (track below)

| Edge type | Flipped in PR | Done |
| --- | --- | --- |
| `Calls` | 4 | [ ] |
| `CallsHttp`, `SendsHttp`, `Binds` | 7 | [ ] |
| `RequestSchema`, `ResponseSchema`, `HasField` | 8 | [ ] |
| `ReadsField` | 9 | [ ] |
| `PayloadSchema`, `Produces`, `Consumes` | 15 | [ ] |

### F3 — Join ownership (PR 7)
- [ ] `FederatedIndex::rejoin_contracts()` — full desired set, diff, apply
- [ ] `project_edges` reconciliation skips `EdgeType::Binds`
- [ ] Called after loader Phase 2, hot-reload apply, `add_repo`, `remove_repo`, `from_snapshot`; under `projection_lock`
- [ ] Tests: order A,B == B,A; provider rename rebinds; provider removal unbinds; hot reload updates

## MCP tools (`contracts` profile)

| Tool | PR | Golden test | Status |
| --- | --- | --- | --- |
| `prepare_snapshot` | 11 | [ ] | todo |
| `get_snapshot` | 11 | [ ] | todo |
| `list_contracts` | 13 | [ ] | todo |
| `get_contract` | 13 | [ ] | todo |
| `list_unresolved` | 13 | [ ] | todo |
| `check_binding` | 13 | [ ] | todo (cuttable) |
| `diff_contracts` | 12/13 | [ ] | todo |
| `trace_impact` | 13 | [ ] | todo |
| `get_coverage` | 13 | [ ] | todo |
| `resolve_evidence` | 13 | [ ] | todo (never cut) |
| `read_source` | 13 | [ ] | todo (cuttable) |

Interface cross-cutting:
- [ ] Envelope (`api_version`, `analyzer_version`, `snapshot`, `meta`)
- [ ] `unsupported_api_version` negotiation
- [ ] All 13 error codes, one test each
- [ ] stdio/HTTP byte parity
- [ ] Output schemas in `docs/tool-schema.json` under drift check
- [ ] Default profile still ≤ 18 tools

## Verification scenarios (`tests/federation_contracts_e2e.rs`)

| # | Scenario | Expected | Passing |
| --- | --- | --- | --- |
| 1 | `orders` removes `customer_id` | `Verified`, path reaches `billing` + `reports` | [ ] |
| 2 | `orders` adds optional `currency` | no reported change | [ ] |
| 3 | `billing` URL from unmapped variable | `NeedsInvestigation`; `list_unresolved` lists `orders` candidate | [ ] |
| 4 | Snapshot excludes `reports` | `coverage.complete = false`; no `NoKnownImpact` | [ ] |
| 5 | Enum value added to `status` | `NeedsInvestigation` via `NeedsReview` | [ ] |
| 6 | `/api/orders/{}` → `/api/order/{}`, same handler | `PathChanged`, `Verified` for `billing` | [ ] |
| 7 | Forged ref to `resolve_evidence` | `exists: false` + reason, no error | [ ] |
| 8 | `prepare_snapshot` twice, same inputs | same id, one indexing job | [ ] |
| 9 | Head derived `from` base + one override | only overridden repo indexed | [ ] |
| 10 | Scenario 3 binding added to `bindings` | `Confirmed`; `Verified` if field read | [ ] |

Other gates:
- [ ] Ground-truth precision/recall baseline committed; CI fails below it
- [ ] Determinism: byte-identical `diff_contracts` for same snapshot pair
- [ ] `federation_blast_radius_regression.rs` and `federation_e2e.rs` unchanged and passing
- [ ] Full CI battery + acceptance harness green before calling any PR done

## Docs to update

- [ ] `docs/REPOS_YAML.md` — `services`, `http_clients`, `bindings`, `schemas`
- [ ] `docs/FEDERATION.md` — contract joins, snapshots
- [ ] `docs/quickstart-tools.md` — `contracts` profile
- [ ] Schema v3 migration note (`lain reindex`)
- [ ] `CHANGELOG` entry for 0.9.0

## Open questions

- [ ] Snapshot record retention: 7 days, or allow pinning?
- [ ] Accept `live` as `diff_contracts` head?
- [ ] Use OpenAPI `operationId` for `PathChanged` when handlers differ?

## Log

| Date | Change |
| --- | --- |
| 2026-09-28 | Tracker created; design committed as `docs/CONTRACT_FEDERATION.md` |
