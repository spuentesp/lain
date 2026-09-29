# Contract Federation — Implementation Tracker

Tracks delivery of [`CONTRACT_FEDERATION.md`](CONTRACT_FEDERATION.md)
(target LAIN 0.9.0, baseline v0.8.0). Update this file in the same PR
that changes a row's state. Section numbers (§) refer to the design doc.

- **Integration branch:** `feat/contract-federation` (off `dev`); PRs target `dev`.
- **Tag 0.9.0 by:** 2026-10-12 · **Hackathon deadline:** 2026-10-30 10:00 PT
- **Release:** version bumps happen only in the `release/v0.9.0` PR (see `AGENTS.md`).
- **Done means:** the PR's checklist below is ticked, its scenarios pass, and the full CI battery plus the acceptance harness are green.

Status legend: `todo` · `wip` · `review` · `done` · `cut`

## PRs

Listed in delivery order. Week 1 is the live slice; week 2 adds pinned snapshots underneath.

| # | PR | Depends on | Week | Status | Branch / PR | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Fixture script, ground truth, scenario tags (§15.1) | — | Sep 29 | done | | |
| 2 | F1 `GlobalId` encoding (§5.1) | — | Sep 29 | done | | |
| 3 | Schema v3 (§4.2–4.3, §5.4) | 2 | Sep 29 | done | | |
| 4 | F2 `traverse_impact` (§5.2) | 3 | Sep 29 | done | | |
| 5 | Normalizer, route matcher, sensor framework, provider fixes (§4.5, §6.1–6.2, §7.4) | 3 | Sep 29 | todo | | |
| 6 | `http_client_sensor` TS/JS + Python (§6.3) | 5 | Sep 29 | todo | | |
| 7 | F3 joiner, config, `ContractIndex` (§5.3, §7) | 4, 6 | Sep 29 | todo | | Monorepo services: about +1 day |
| 8 | OpenAPI schemas and fields (§6.2, §6.4) | 3 | Sep 29 | todo | | |
| 9 | `field_access_sensor` + field join (§6.5, §7.5) | 7, 8 | Sep 29 | todo | | |
| 16 | Entry points, contract-tool infra, service view, command center (§6.6, §10.1–10.2, §10.9) | 7, 9 | Sep 29 | todo | | Never cut |
| 12 | Diff, classify, evaluate, coverage as pure functions (§9) | 9 | Sep 29 | todo | | |
| 10 | Mirrors, snapshot index mode, index cache (§8.1–8.3) | 3 | Oct 6 | todo | | |
| 11 | Snapshots, `from_snapshot`, residency (§8.4–8.5, §11) | 7, 10 | Oct 6 | todo | | |
| 13 | Remaining tools, full interface, golden tests, docs (§10, §12, §13) | 11, 12, 16 | Oct 6 | todo | | Release PR follows |
| 14 | `http_client_sensor` Rust + Go | 6 | stretch | todo | | Cut 2nd |
| 15 | Events, JSON Schema + proto fields, topic join (§6.7, §7.7) | 7, 8 | stretch | todo | | Cut 1st |
| 17 | `codeowners_sensor` | 16 | stretch | todo | | Cut 4th |
| 18 | Generated-client matching by `operationId` | 6, 8 | stretch | todo | | Cut 3rd; biggest recall gap |

Critical path: 1–13 and 16. Cut order if late: 15 → 14 → 18 → 17 → `check_binding` / `read_source`.
Never cut: envelope, scoped coverage, `get_service`, `resolve_evidence`.

## Per-PR checklists

### PR 1 — Fixture
- [x] `scripts/contracts-fixture.sh <dir>`: four local repos (`orders`, `billing`, `reports`, `platform`) exactly as §15.1, no network
- [x] Generated `repos.yaml` with `workspace_dir` sources, `services` (incl. `platform` paths), env names
- [x] Scenario tags: `s1`, `s2`, `s5`, `s6`, `s11`, `s12`, `s19`, `s21` in `orders`; `s3`, `s5b`, `s20`, `s22` in `billing`
- [x] `tests/fixtures/contracts/ground_truth.yaml`: endpoints, `Binds` with provenance, `ReadsField`, unresolved, entry points, per-scenario expected result

### PR 2 — F1 GlobalId encoding
- [x] `GlobalId::new` percent-encodes `%` and `:` in path and name; `parse` requires 5 segments; accessors decode
- [x] Hand-built/split ids routed through `GlobalId` (`repo_id.rs` splits, `repo_id.rs:280`, `global_id_str`, `rg` sweep of federation + mcp)
- [x] `is_known_node_kind` → `NodeType::all()`
- [x] Proptest round-trip (`:`, `%`, `::`, `%3A`); regression for `GET /orders/:id`

### PR 3 — Schema v3
- [x] `NodeType`: `HttpClientCall`, `Field`, `FieldRef`; `EdgeType`: `SendsHttp`, `RequestSchema`, `ResponseSchema`, `PayloadSchema`, `HasField`, `ReadsField`, `ReadsFrom`, `Binds`; `all()` and `is_indexed()` updated
- [x] `GraphNode.contract: Option<ContractFact>`, `GraphNode.entry: Option<EntryKind>`, `GraphEdge.site`, `GraphEdge.detail`; `EdgeProvenance::Confirmed`
- [x] `federation/contracts/model.rs` types (§4.3), externally tagged enums only
- [x] `FEDERATION_GRAPH_VERSION` 2 → 3; `PATH_FORMAT_VERSION` 3 → 4
- [x] `CHANGELOG.md` migration note (install → `lain reindex` → enable `contracts` → use)

### PR 4 — F2 traverse_impact
- [x] `traverse_impact(starts, depth, cap, min_confidence)` with deterministic BFS and tie-breaks (§5.2)
- [x] Exhaustive `impact_propagation`, only `Calls` on
- [x] `get_cross_repo_blast_radius` rebuilt on it; regression test untouched and green

| Edge types switched on | PR | Done |
| --- | --- | --- |
| `Calls` | 4 | [x] |
| `CallsHttp`, `SendsHttp`, `Binds` | 7 | [ ] |
| `RequestSchema`, `ResponseSchema`, `HasField` | 8 | [ ] |
| `ReadsField` | 9 | [ ] |
| `PayloadSchema`, `Produces`, `Consumes` | 15 | [ ] |

### PR 5 — Normalizer, matcher, sensor framework
- [ ] `federation/contracts/normalize.rs` implementing §4.5 steps 1–5; property tests
- [ ] Route matcher (§7.4): method rules, segment rules, specificity, prefix tolerance
- [ ] `Sensor::phase`, `run_all` sorted by `(phase, name)`; new `SensorCounts` fields
- [ ] `GraphDatabase::replace_sensor_output` + `SensorOwner`; `http_sensor` and `openapi_sensor` switched to it (fixes stale routes)
- [ ] `util::enclosing_symbol`
- [ ] `http_sensor`: normalizer, `ContractFact::Provider`, `go-std` → `ANY`, same-file router prefixes (FastAPI, Flask, axum, actix), `HashMap` → `BTreeMap`

### PR 6 — http_client_sensor (TS/JS, Python)
- [ ] Every call shape in §6.3, tree-sitter based
- [ ] URL expression → parts; one-step identifier resolution; env/settings host resolution
- [ ] Wrapper candidates recorded as `CallVia::Receiver`
- [ ] `httpx.Client(base_url=…)` host propagation
- [ ] `HttpClientCall` node + `SendsHttp` edge with `site`; module-level calls attach to `File`

### PR 7 — F3 joiner and config
- [ ] Config sections `services` (`paths`, `hosts`, `env`, `base_path`, `route_prefixes`), `http_clients`, `generic_keys`, `schemas`, `bindings`; every validation error of §7.1; `config_hash`
- [ ] `ContractJoiner::run` steps 1–7 (§7.2); consumer resolution table (§7.3); built-in generic keys
- [ ] `ContractIndex` (§4.3); contract node ids recorded by projection
- [ ] `rejoin_contracts` with batch apply; `contracts_dirty`; every trigger of §5.3; live tools call `rejoin_contracts_if_dirty`
- [ ] `project_edges` reconciliation skips `Binds`
- [ ] Confirmed bindings keyed by `(repo, path, symbol, key)`; `stale_bindings`
- [ ] `federation/AGENTS.md` names `contracts/joiner.rs` alongside `cross_repo.rs`
- [ ] Tests: order independence, rebind on rename, provider removal, hot reload, line move keeps binding; invariants of §7.8; rejoin budget on tokio + bytes

### PR 8 — OpenAPI schemas
- [ ] `operationId` rename fix; `head`, `options`; `servers[].url` / `basePath` prefix
- [ ] Request, 2xx-union response, `$query` parameters (§6.4)
- [ ] Flattening: properties, `[]`, `{}`, `allOf`, `oneOf`/`anyOf`, `$ref`, cycles, external refs
- [ ] Types, nullability (3.0, 3.1, Swagger 2), requiredness, enum stringification
- [ ] Pointer → line index for YAML and JSON
- [ ] `Schema`, `Field` nodes; `RequestSchema`, `ResponseSchema`, `HasField` edges

### PR 9 — Field reads
- [ ] Binding rules 1–6 and scope (§6.5), tree-sitter `Calls` only
- [ ] Read forms incl. destructuring and `match`; chains
- [ ] Escape rules → `reads_complete`
- [ ] `FieldRef` + `ReadsField` + `ReadsFrom`
- [ ] Field join (§7.5): exact, suffix, ambiguous, unknown-field reads, `schemaless_endpoints`

### PR 16 — Service view
- [ ] `entry_point_sensor` (§6.6), clears and resets `entry` each run
- [ ] `Package::Contracts`; `CONTRACT_TOOL_DEFS`; `ToolDef.input_schema` / `output_schema`; `ContractToolEntry` + `ToolOutcome`; dispatch hook after `invoke_inventory`; no new `match` arms
- [ ] Envelope (§10.2) and scope sentence rendering
- [ ] `list_services`, `get_service` with `used_by` (§10.9) on `live`
- [ ] Live scope from `RepoHealth` mapping (§8.7)
- [ ] Command center Services tab

### PR 12 — Diff and evaluation
- [ ] `ContractSurface` from `ContractIndex`, keyed per service (§9.1)
- [ ] Provider-side kinds (§9.2) incl. `PathChanged` via `SymbolKey`/`operationId`, `FieldRenamed`, nested-field rule, `ChangedWithoutSchema`
- [ ] Consumer-side kinds (§9.3)
- [ ] `classify` table (§9.4)
- [ ] `evaluate`: base/head tracing, qualifying prefix, per-consumer table, could-match rule, scoped `NoKnownImpact` (§9.5–9.7)
- [ ] Tested on two in-process fixture states

### PR 10 — Mirrors and cache
- [ ] Mirrors under `<data_dir>/mirrors`; ref resolution; one fetch then `ref_not_found`; `fetch_failed`
- [ ] Worktrees + per-repo `File::lock`; `prune` at startup
- [ ] `IndexMode::Snapshot`; `IndexRequest.lsp_pool` / `overlay` optional; skips LSP, overlay, resolver, co-change, NLP
- [ ] Index cache layout, manifest, atomic write, LRU with holds
- [ ] `CONTRACT_ANALYZER_REV`, canonical digest, `tests/contracts_analyzer_digest.rs`, determinism test

### PR 11 — Snapshots
- [ ] Records with `join_config`; id from `{repos, excluded, config_hash, analyzer_version}`; derived snapshots inherit `join_config`
- [ ] Job queue: dedup, `LAIN_SNAPSHOT_WORKERS`, 64-job limit → `busy`; restart re-enqueue; retention
- [ ] `PetgraphBackend::ephemeral`; `project_graph` shared with live; `from_snapshot` + rejoin
- [ ] Residency (`LAIN_SNAPSHOT_RESIDENT`, holds, `busy` after `wait_ms`)
- [ ] `prepare_snapshot` (`repos`, `exclude`, `from`, `max_base_age_s`, `wait_ms`), `get_snapshot`
- [ ] Memory ceiling committed; check wired into the `main` battery

### PR 13 — Interface complete
- [ ] Remaining tools over snapshots and `live` (table below)
- [ ] Versioning, paging and cursors, limits, all 15 error codes with required `details`
- [ ] Output schemas in `docs/tool-schema.json`; golden JSON per tool validated with dev-only `jsonschema`
- [ ] stdio/HTTP byte parity; default profile still ≤ 18 tools
- [ ] Docs (below)

## MCP tools (`contracts` package)

| Tool | PR | Golden test | Status |
| --- | --- | --- | --- |
| `list_services` | 16 | [ ] | todo (never cut) |
| `get_service` | 16 | [ ] | todo (never cut) |
| `prepare_snapshot` | 11 | [ ] | todo |
| `get_snapshot` | 11 | [ ] | todo |
| `list_contracts` | 13 | [ ] | todo |
| `get_contract` | 13 | [ ] | todo |
| `list_unresolved` | 13 | [ ] | todo |
| `check_binding` | 13 | [ ] | todo (cuttable) |
| `diff_contracts` | 12 (logic) / 13 (tool) | [ ] | todo |
| `trace_impact` | 13 | [ ] | todo |
| `get_coverage` | 13 | [ ] | todo |
| `resolve_evidence` | 13 | [ ] | todo (never cut) |
| `read_source` | 13 | [ ] | todo (cuttable) |

## Verification scenarios (`tests/federation_contracts_e2e.rs`)

| # | Scenario | Expected | PR | Passing |
| --- | --- | --- | --- | --- |
| 1 | `orders` removes `customer_id` | `Verified`; affected `billing`; path reaches `reports` | 12/13 | [ ] |
| 2 | `orders` adds optional `currency` | not reported; `compatible_changes = 1` | 12/13 | [ ] |
| 3 | `billing` URL from unmapped variable | `NeedsInvestigation` (`unresolved_candidates`); `list_unresolved` candidate | 13 | [ ] |
| 4 | `exclude: [reports]`, change with no reviewed consumer | `NoKnownImpact` with `unreviewed = [reports: excluded]`; text names it | 13 | [ ] |
| 5 | Enum value added; `billing` doesn't read `status` / does (`s5b`) | `NoKnownImpact` / `NeedsInvestigation` (`needs_review`) | 12/13 | [ ] |
| 6 | Path renamed, same handler | `PathChanged`, `Verified` | 12/13 | [ ] |
| 7 | Forged ref to `resolve_evidence` | `exists: false`, `no_such_node` | 13 | [ ] |
| 8 | `prepare_snapshot` twice | same id, one job | 11 | [ ] |
| 9 | Derived head, one override | only that repo indexed | 11 | [ ] |
| 10 | Binding added, no commit moved | new id; `Confirmed`; call bound | 11/13 | [ ] |
| 11 | Field renamed, same type | one `FieldRenamed`, `Verified` | 12/13 | [ ] |
| 12 | Field renamed with type change | `FieldRemoved` + `FieldAdded` | 12 | [ ] |
| 13 | Literal `/api/orders/me` | binds `GET /api/orders/me` | 7 | [ ] |
| 14 | Call to `api.stripe.com` | `coverage.external`; not unresolved | 7 | [ ] |
| 15 | Lines inserted above a confirmed call | binding still resolves | 7 | [ ] |
| 16 | `shipping` → `inventory` in `platform` | one `Binds`, `cross_repo = false` | 7 | [ ] |
| 17 | `get_service(billing)` | `reports` with both `used_by` entries | 16 | [ ] |
| 18 | `get_service(orders)` with `reports` `Indexing` | `unreviewed` has `reports: not_ready` | 16 | [ ] |
| 19 | Optional request field type change | `BreakingIfSent` → `NeedsInvestigation` | 12/13 | [ ] |
| 20 | `billing` reads `discount` | `ConsumerFieldUnmatched`, `Verified` | 12/13 | [ ] |
| 21 | Code-only route handler changed | `ChangedWithoutSchema` → `NeedsInvestigation` | 12/13 | [ ] |
| 22 | Response cached at module level, then scenario 1 | `NeedsInvestigation` (`reads_not_fully_traced`) | 12/13 | [ ] |

Other gates:
- [ ] Ground-truth precision/recall baseline committed (`tests/fixtures/contracts/baseline.json`); `demo.sh --quick` contracts phase fails below it
- [ ] Canonical-digest determinism; byte-identical `diff_contracts` for the same snapshot pair
- [ ] `federation_blast_radius_regression.rs` and `federation_e2e.rs` unchanged and passing
- [ ] Memory ceiling check on `main`

## Docs to update

- [ ] `docs/REPOS_YAML.md` — `services` (`paths`, `hosts`, `env`, `base_path`, `route_prefixes`), `http_clients`, `generic_keys`, `schemas`, `bindings` (symbol-keyed), validation errors
- [ ] `docs/FEDERATION.md` — services and monorepos, contract joins, scoped "no known impact", snapshots, what snapshots do not contain
- [ ] `docs/command-center.md` — Services tab
- [ ] `docs/quickstart-tools.md` — `contracts` package; "who uses this service" walkthrough with `get_service`
- [ ] `CHANGELOG.md` — 0.9.0 entry with migration order and the `read_source` threat model (walked files only, secret denylist, binaries refused, single trust domain)
- [ ] `src/server/federation/AGENTS.md`, `src/server/mcp/AGENTS.md` — joiner location; contract-tool registration

## Log

| Date | Change |
| --- | --- |
| 2026-09-28 | Tracker created; design committed as `docs/CONTRACT_FEDERATION.md` |
| 2026-09-28 | Review round 1: joiner module, worktree lock, derivation from any `from` state, topic coverage rule, `FieldRenamed`, `read_source` threat model, residency, `range_too_large` details, migration order |
| 2026-09-28 | Review round 2: rename request-side classification, response-required wording, count-only residency, `read_source` range clamping, `path_rejected` reasons |
| 2026-09-28 | Revision 2 (silo-breaking vision): scoped `NoKnownImpact`; services incl. monorepos as the join unit; `list_services` / `get_service` + `used_by` in 0.9; CODEOWNERS and `operationId` as stretch; live slice first |
| 2026-09-28 | Implementation-ready rewrite, checked against v0.8.0 code: config-free sensors + `ContractIndex`; bincode compatibility via version bumps only; sensor phases and self-replacing output; `IndexMode::Snapshot`; LAIN-owned mirrors; canonical digest; recorded `join_config`; contract-tool infra (`ContractToolEntry`, schemas on `ToolDef`); bound-identifier field tracking with escapes; consumer-side diff; `ChangedWithoutSchema`; nested-field rule; full signatures, limits, error codes; decisions replace open questions |
| 2026-09-28 | PR 1 done: `scripts/contracts-fixture.sh`, ground truth, scenario tags; review clean after one fix round |
| 2026-09-29 | PR 2 done: F1 `GlobalId` percent-encoding; review clean after one fix round |
| 2026-09-29 | PR 3 done: schema v3 (types, version bumps, migration note); review clean, no fix round |
| 2026-09-29 | PR 4 done: F2 `traverse_impact` (§5.2) — exhaustive `impact_propagation` table with only `Calls` on; BFS + tie-breaks + sort; `get_cross_repo_blast_radius` rebuilt on it (response shape unchanged); `federation_blast_radius_regression.rs` untouched and green |
