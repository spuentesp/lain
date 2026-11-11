# Parallax requirements — coverage report

**Question:** does Lain deliver what Parallax ("What Parallax requires from
lain") asks for?

**Answer: yes.** Every requirement in that document is delivered on `dev`
(`1a4f4a5a`, the v0.15.0 batch) and most were shipped in v0.13.0/v0.14.0,
which is what Parallax pins today. The requirements were adopted as
modifications and are pinned by Lain's own test battery. Four items must be
handled on Parallax's side to *upgrade* past its v0.13.0 pin — none of them
is a missing capability.

Verified against the tree on 2026-11-10. Each row cites the code or test
that pins it; "the gates" refers to Parallax's own
`pnpm gate:release` / `gate:graphql` / `gate:saleor`.

## 1. Deployment & connection

| Requirement | State | Evidence |
|---|---|---|
| `lain server --config repos.yaml --transport http --port 9999` over a federation listed in `repos.yaml` | Delivered | `docs/FEDERATION.md`; `docs/REPOS_YAML.md` |
| Connect via `LAIN_URL` (`/mcp`), per-workspace `lainUrl` override | Delivered (client-side concern; the endpoint is stable) | `docs/QUICKSTART.md` |
| Full Saleor cold index finishes inside the 15-min gate deadline | Delivered — met since v0.13.0, and materially faster since: the startup cache reuses validated federation checkpoints on unchanged startup, and gRPC declarations are indexed once per file | `docs/STARTUP_CACHE.md`; `docs/benchmarks/nvidia-startup-*.json` (cold medians in seconds per phase) |
| Version pin (`analyzerVersion === '0.13.0'`) | Works today. See "Upgrading" below | `src/server/mcp/contract_tools/mod.rs:124` (`ANALYZER_VERSION = CARGO_PKG_VERSION`) |

## 2. Transport protocol

| Requirement | State | Evidence |
|---|---|---|
| JSON-RPC 2.0 over `POST /mcp`, `tools/list` + `tools/call` | Delivered | `src/server/mcp/handler.rs` |
| Envelope `{analyzer_version, api_version, data, meta, snapshot, view, reproducible}` in `content[0].text` or `structuredContent` | Delivered — all fields present, and the v0.15.0 batch adds schema-valid `structuredContent` alongside the text without removing the text | `src/server/mcp/contract_tools/envelope.rs:164,196`; `docs/tool-schema.json` (25 tools carry `outputSchema`) |
| `api_version` must be `"1"`; missing/incompatible version refused at startup | Delivered — enforced per call (`unsupported_api_version`) | `src/server/mcp/contract_tools/analysis.rs:81-84` (`check_api_version`) |
| `get_code_snippet`, `explain_symbol`, `get_call_sites` return markdown text | Delivered — text preserved | `docs/quickstart-tools.md` ("Raw reads") |
| Cursor pagination `cursor`/`limit`, default 100, max 1000; paginate `list_unresolved` to exhaustion | Delivered — default 100, `range_too_large` beyond 1000, and v0.15.0 pins a deterministic tie-break (name, then id) so exhaustion is stable | `src/server/mcp/contract_tools/contracts.rs:19,902-904`; `docs/query-language.md` |

## 3. Tool surface

| Requirement | State | Evidence |
|---|---|---|
| The 13 contracts tools after `load_package {"package":"contracts"}` | Delivered — the package now has **16** (adds `list_service_refs`, `apply_contract_context`, `trace_flow`; the 13 are unchanged) | `docs/tool-schema.json` (100 tools; all 19 names Parallax calls are present); `src/server/mcp/definitions.rs` |
| `query_graph`, `describe_schema`, `load_package` | Delivered | `docs/tool-schema.json` |
| `query_graph` takes `query` (not `spec`) | Delivered — the schema exposes `query`. Parallax is right; `docs/query-language.md`'s "spec" wording describes the body shape, not the argument name | `docs/tool-schema.json` (`query_graph` args: `query`, `snapshot`) |
| Arg schemas pinned in Parallax's `docs/lain-tool-schemas.json` + Zod mirror | Compatible — inputs unchanged in the v0.15.0 batch; one **output** shape changed (see "Upgrading" #2) | `docs/tool-schema.json` |

## 4. Semantics

| Requirement | State | Evidence |
|---|---|---|
| `snapshot: "live"` \| `"snap_<hex>"`, never inferred; `prepare_snapshot` / `get_snapshot`; `live` is not a diff base/head | Delivered — and strengthened: reads bound to a snapshot now **error** instead of silently falling back to the live tree (reinforces INV 1) | `src/server/mcp/contract_tools/contracts.rs:25`; `src/server/mcp/contract_tools/analysis.rs:171-174` ("live is not supported as diff base or head") |
| `diff_contracts` findings classified `Verified` \| `NeedsInvestigation` \| `NoKnownImpact` | Delivered | `src/server/federation/contracts/diff.rs:1609-1611` |
| `get_coverage` declares omissions/truncation — incomplete coverage never shown as "no impact" (INV 3) | Delivered and strengthened: `answer_scope` (`truncated`, `stale_or_unindexed`, `dynamic_target`, `ambiguous_target`) is now reported across the graph tools, and `truncated` reports the traversal limit independently | `docs/quickstart-tools.md` ("`answer_scope`"); `CHANGELOG.md` (v0.15.0 batch) |
| `resolve_evidence` refs resolvable (repo + sha + path) | Delivered | `docs/quickstart-tools.md` (contracts section) |
| Federation scope in responses: configured_only, reviewed repos with commit + dirty flag, unreviewed list; empty catalog mid-indexing is not "no consumers" | Delivered — readiness states are explicit (`Indexing` / `Ready` / `Degraded`) and answers are qualified while a member is unreviewed | `docs/FEDERATION.md`; `src/server/federation/health.rs` |
| `query_graph` requires `repo_id` scoping in federation mode | Delivered | `docs/FEDERATION.md`; `docs/query-language.md` |
| Binding semantics: `env` declared on the provider service | Delivered | `docs/REPOS_YAML.md` |
| GraphQL: `provides_graphql: false` actually suppresses a provider | Delivered (the v0.13.0-era fix; knob + joiner logic) | `src/server/federation/contracts/config.rs:142`; `src/server/federation/contracts/joiner/endpoints.rs:48` |
| `check_binding` accepts GraphQL endpoint keys (`graphql:query:product`) | Delivered | `src/server/federation/contracts/joiner_tests.rs:1355,1449,1958` |
| Advisor binding YAML round-trips through the config loader | Delivered | `src/server/federation/contracts/joiner_tests.rs` (round-trip pins) |

## 5. Verification

- Lain's side: the full battery (113 suites / 4,178 tests), acceptance 37/37,
  demo 171 checks, schema-drift (100 tools), and the mutation gate cover the
  contract tools, the envelope, and the joiner semantics above.
- Parallax's side: `gate:release` / `gate:graphql` / `gate:saleor` remain the
  consumer's acceptance; nothing in this report replaces them.

## Upgrading past the v0.13.0 pin

Four items, all deliberate — none is a missing feature:

1. **Version pin.** `analyzer_version` is the product version
   (`CARGO_PKG_VERSION`), so v0.14.0 reports `"0.14.0"` and v0.15.0 reports
   `"0.15.0"`; the boot fail-fast and `gate-graphql.ts`'s `=== '0.13.0'` must
   move with the release. Note also that `diff_contracts` refuses base/head
   snapshots recorded under different `analyzer_version`s
   (`analysis.rs:231-237`) — re-prepare snapshots across an upgrade.
2. **`get_contract` provider handlers are now `{repo, file, symbol}` objects**
   (previously strings). Update the Zod mirror in
   `packages/shared/src/lain/tools.ts`.
3. **Unresolved reason vocabulary split.** A runtime-selected path with no
   attributable service is now `dynamic_target` rather than
   `no_route_in_service` (the old label claimed a service had been checked).
   The known-service/dynamic-path case still reports `no_route_in_service`.
   Any classifier matching on the old label needs the two-label rule.
4. **`FEDERATION_GRAPH_VERSION` advanced 4 → 5** (ORM model/table/column
   relations). Run `lain reindex` once per deployment on upgrade or the
   server refuses the old graph with `FederationSchemaMismatch`
   (`docs/FEDERATION.md`, "Graph schema versioning and recovery").

## Beyond the request (in the v0.15.0 batch)

- `answer_scope` on the graph tools — the shared incompleteness vocabulary
  that makes INV 3 mechanical rather than convention.
- `trace_flow` — one path from endpoint/symbol/table across the contract
  boundary, with per-hop provenance and explicit `gaps`.
- ORM model/table/column relations + migration evidence (Django, SQLAlchemy,
  Prisma, Drizzle, TypeORM, Sequelize) for a deeper org service map.
- Source locations and provenance on raw graph results, chained traversal
  paths, deterministic cursors.
- Structured output alongside text on 25 tools — the envelope's
  `structuredContent` is schema-valid, so Parallax can stop parsing the
  JSON-in-text when convenient.
- The startup cache (`LAIN_REQUIRE_STARTUP_CACHE=1` for prepared-cache-only
  launches) for faster CI gates.
