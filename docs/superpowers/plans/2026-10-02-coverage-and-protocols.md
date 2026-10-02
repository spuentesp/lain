# Plan: contract-coverage-and-protocols (Phase A → E)

**Spec:** `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md`
**TLA+:** `docs/formal/CoverageClaim.tla` (I3), `docs/formal/IndexGeneration.tla` (I7)
**Branch:** `feat/contract-coverage-and-protocols` (parent `feat/contract-federation`)
**PR:** #272 (draft, against `dev`)
**Date:** 2026-10-02

## Mapping: TLA+ ⇒ Rust (formal-first)

Phase A (and every subsequent phase) follows the same discipline:

1. **Read the TLA+ action as a contract** — the action's guard is a precondition, the primed state is a postcondition, and the invariant is what the implementation must never violate.
2. **Cite the TLA+ module + line** in every Rust function's doc-comment.
3. **Map each TLA+ state variable to a Rust field** (or DB column).
4. **Map each TLA+ action to a Rust function** with the same name where possible.

### CoverageClaim.tla → Phase A

| TLA+ | Rust |
|---|---|
| `analyzed: [repo → BOOLEAN]` | `CoverageLedger.entry(repo).analyzed` |
| `langs_present: [repo → SUBSET LANGUAGES]` | `CoverageLedger.entry(repo).languages_present` |
| `sensors_ran: [repo → SUBSET Sensors]` | `CoverageLedger.entry(repo).sensors_ran` |
| `sensors_failed: [repo → SUBSET Sensors]` | `CoverageLedger.entry(repo).sensors_failed` |
| `unresolved: [repo → SUBSET LANGUAGES]` | `CoverageLedger.entry(repo).unresolved_candidates` |
| `change_in_scope: SUBSET Repos` | `ContractIndex::change_in_scope()` |
| `claim_fired: BOOLEAN` | the verdict being evaluated (no field — local to the evaluator) |
| `Reindex(r)` | the `scan_workspace_*` flow + ledger update |
| `SensorFail(r, s)` | the existing `Sensor::scan` `Err` → `ledger.error` |
| `AddScopeUnindexed(r)` | **forbidden** path: Rust never adds a repo to scope without reindexing first |
| `ReportNoKnownImpact` (guarded) | `evaluate()` returning `NoKnownImpact` — gated by `RepoCoverage::is_complete()` |
| `claim_fired' = FALSE` on reindex/scope-change | verdict re-evaluation must run on every state change |
| `RepoComplete(r)` predicate | `RepoCoverage::is_complete(&self)` |
| `NoKnownImpactSound` invariant | proptest `tests/contracts_properties/soundness.rs` |

### IndexGeneration.tla → Phase A (index-generation machinery)

| TLA+ | Rust |
|---|---|
| `current_gen: Generation` | `ContractIndex::generation: u64` |
| `snapshot: [reader → Generation ∪ {Unloaded}]` | `ToolSnapshot::generation` |
| `dirty: BOOLEAN` | the existing dirty flag |
| `reader_active: [reader → BOOLEAN]` | implicit in tool-call lifetimes |
| `Rejoin` (atomic swap) | the swap site in `rejoin_contracts_if_dirty` |
| `LoadSnapshot(r)` / `ReleaseSnapshot(r)` | tool-call boundaries that pin a generation |
| `IndexGenerationConsistent` invariant | proptest `tests/contracts_properties/index_generation.rs` |

## Phases (delivery order)

### Phase A — Coverage ledger + rule-1 fix (soundness; 0.9 gate)

Per spec §4. New types:

```rust
// Sensor-level ledger
pub struct SensorLedger {
    pub files_seen: usize,
    pub files_analyzed: usize,
    pub files_skipped: Vec<SkipRecord>,   // {reason, count, sample_paths(<=5)}
    pub emitted: usize,
    pub unresolved: Vec<UnresolvedRecord>, // {reason, count, sample_ids}
    pub error: Option<String>,
}

// Repo-level coverage (extends RepoCoverage)
pub struct RepoCoverage {
    pub ledger: BTreeMap<SensorId, BTreeMap<Lang, SensorLedger>>,
    pub languages_present: BTreeSet<Lang>, // extensions in repo, regardless of support
    pub sensor_counts: SensorCounts,      // derived (kept for back-compat)
}
```

Mechanics:
- `walk_workspace` returns `Vec<FileRecord>` (path, lang, ignored, size-capped) — every file accounted for.
- `Sensor::scan` keeps its signature; new `scan_with_report(...) -> ScanReport` default-delegates. Five legacy sensors migrate incrementally; unmigrated → `ledger: unknown`, never clean.
- `run_all` returns `(SensorCounts, CoverageLedger)`. Sensor `Err` → `ledger.error`.
- Persist `CoverageLedger` in per-commit index cache; `RepoCoverage` gains `ledger`.
- Tri-state query result: `found | not_found_analyzed | not_analyzed(reasons)`.
- Verdict change: `evaluate()` downgrades `NoKnownImpact` → `NeedsInvestigation` when any in-scope repo is incomplete. `coverage.complete` derived from same predicate.
- Rule-1 fix: wrapper candidates without `http_clients` match become `ConsumerTarget::Unresolved{WrapperUnconfigured}`.

Acceptance (from spec §4):
- Fixture repo containing a language with no sensor ⇒ `not_analyzed`, `NeedsInvestigation`, not `NoKnownImpact`.
- Corrupt/oversized/unreadable file ⇒ in `files_skipped`.
- Wrapper call with no config ⇒ in `unresolved`.
- Snapshot round-trip preserves the ledger.
- `pr13_hermetic_precision_recall_over_t1_fixture` stays at 1.000 × 6.
- Each golden test that flips from `NoKnownImpact` to `NeedsInvestigation` is called out in the commit message.

### Phase B — Wrapper & base-URL resolution (stretch)

Per spec §5. Cross-file client registry (TS/JS `axios.create`, `ky.create`, `got.extend`; Python existing; later Java/Kotlin/C#/Go). Composition: `normalize(base_parts ++ call_path_parts)`. `CallVia::Receiver` gains `base: Option<BaseOrigin>`.

Resolution precedence (I6 total order): confirmed binding → code-derived base+host → `http_clients` config → `operationId` → heuristic → unresolved.

### Phase C — Env aliases

Per spec §6. New `env_sensor` (phase 0) reads `.env*`, docker-compose `environment:`, helm `values.yaml`, k8s `env:` → `EnvBinding{var, host, source}`. Join: `HostPart::Env(vars)` → host → services[]. Unmapped vars in ledger as `env_unmapped`. Conflicting values → ambiguous.

### Phase D — SQL tables

Per spec §7. New `NodeType::Table`, `EdgeType::ReadsTable`, `EdgeType::WritesTable` (folded into v3, no version bump). `sql_sensor` (phase 1) recognizes sqlx/rusqlite/cursor/JDBC shapes with literal statements; parses with a SQL parser crate (subject to `docs/VULNS.md` policy); CTEs/subselects/joins → reads; INSERT/UPDATE/DELETE/MERGE → writes. ORMs and column lineage: out of scope.

### Phase E — gRPC then GraphQL

Per spec §8. `ContractKey::Rpc { system: Grpc, service, method }`, `ContractKey::Graphql { op, field }` (folded into v3).

**E-gRPC first**: rewrite `proto_sensor` onto a real proto grammar (tree-sitter-proto) emitting `service Foo { rpc Bar }` as providers; `package` part of identity (`pkg.Foo/Bar`). Server registration via `RegisterFooServer` / `add_FooServicer_to_server` / `@GrpcService` links provider to handler.

Generated stub calls (`FooClient.Bar` / `stub.Bar`) resolved to `pkg.Foo` via the channel address (`grpc.NewClient("orders:50051")`, env) reusing Phase B/C host resolution. Join: exact `(pkg.Service, method)` match within target service; no URL prefix tolerance. Unknown stub → ledger candidate `rpc_stub_unknown`.

**E-GraphQL second**: rewrite SDL parsing via real parser (graphql-parser crate, subject to VULNS policy). Resolver linkage by naming convention per framework (Apollo resolver maps, graphql-java `DataFetcher`, gqlgen, Strawberry). Consumer: `gql`/`graphql` tagged templates, `.graphql` documents, persisted operations → top-level selection fields. Fragment-only or interpolated documents → `dynamic_operation`. Endpoint target: `/graphql` HTTP route resolution via Phase B/C; join on `(op, field)`. If several services expose the same root field (federation/gateway) ⇒ ambiguous, never single-bound.

## Delivery cadence

- **One PR per phase, all stacked on #272** (per the user: "MERGE TO THIS PR. NOT TO DEV.").
- Each phase lands as commits on this branch; the PR description is updated when scope changes.
- Each phase: `cargo test`, `clippy --all-targets` 0/0, fmt, fresh-named ci-probe full battery + acceptance harness before "done".
- Bugs found en route are fixed in place.
- Hard-rules compliance: no schema version bumps, no fixture edits, no `Cargo.toml [dependencies]` runtime version bumps. CHANGELOG + `lain reindex` cover the v3-fold-in.

## Tooling

- TLA+: `tools/tla/tlc` wrapper, `tla2tools.jar` v1.8.0. CI runs `./tools/tla/tlc docs/formal/*.tla` when `docs/formal/` changes.
- New parser crates (tree-sitter-proto, graphql-parser, sql-parser) — license/vuln policy check via `docs/VULNS.md` before adoption.

## What's not in this plan

- TLA+ for I1, I2, I4, I5, I6 — these get proptest cases (partition, shuffle, monotone precedence) per spec §9. No TLA+ models needed.