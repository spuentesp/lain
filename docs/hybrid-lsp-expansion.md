# Plan — Hybrid LSP Expansion

## Goal

Push `lsp-bridge` past the current "structural edges only" floor so
that Rust trait objects, Go interfaces, Java/Kotlin interfaces, and
Swift protocols resolve to their concrete implementations. After this
lands, `get_blast_radius` on a method behind a trait object lists
the real call sites instead of an empty list, and `explain_dispatch`
no longer needs to lean on `heuristic_only` for code that the LSP
already understands.

## Why deferred, why now

Tier 3's heuristic sensor closes the dynamic-dispatch gap for
**convention patterns** (message buses, DI containers, FastAPI
decorators). What it doesn't close is **type-resolved dispatch**:
when `tracer.record(event)` is called on `dyn Tracer`, the static
graph sees one call to a `dyn Trait` receiver and zero concrete
implementations. Tier 2 has no view of "what implements this trait
in this workspace"; the LSP does, via the `textDocument/implementation`
request.

The current `lsp-bridge` (`src/server/lsp.rs`) only exposes
`documentSymbol`. Adding `implementation`, `typeDefinition`, and
`prepareTypeHierarchy` (the LSP requests that resolve dynamic
dispatch) lifts the static graph's coverage from "syntactic calls"
to "syntactic + type-resolved calls". The plan keeps the per-binary
circuit breaker, the cold-boot prewarm, and the request timeout
that the existing code already enforces.

## Scope

In scope:

- New `LspMultiplexer` methods: `implementation`,
  `type_definition`, `incoming_calls`, `outgoing_calls`. The
  last two are LSP 3.17 (`callHierarchy/incomingCalls` and
  `callHierarchy/outgoingCalls`) and give the call hierarchy that
  the current static scanner approximates via tree-sitter-only
  name matching.
- Trait-object resolution for `rust-analyzer`:
  `textDocument/implementation` returns the concrete
  `impl` for `dyn Trait` and `impl Trait for T` receivers.
- Interface resolution for `clangd` (C++), `kotlin-language-server`
  (Kotlin), `dart-lang-sdk` analysis server (Dart).
- New LSP registrations: `kotlin-language-server`,
  `dart-analysis-server`. Kotlin and Dart are picked because
  they share the same trait-object + interface dispatch pattern
  as Rust and the binaries are easy to install.
- A new edge type `Implements` already exists (`schema.rs:170`);
  this plan reuses it and adds a new variant `DispatchesTo` for
  the LSP-resolved dynamic calls (analogous to a `Calls` edge
  with `provenance = Static { source: Lsp }`).

Out of scope:

- Java LSP (Eclipse JDT-LS): heavyweight install, low ROI for the
  current user base.
- TypeScript: `typescript-language-server` doesn't reliably resolve
  dynamic dispatch through union types; skipped.
- Python: `pylsp` lacks `textDocument/implementation` coverage;
  the heuristic sensor already covers the relevant patterns.
- Swift / Objective-C: `sourcekit-lsp` requires Xcode integration;
  deferred.

## LSP request additions

For each new method, the `LspMultiplexer` API is:

```rust
impl LspMultiplexer {
    pub async fn implementation(
        &self,
        file_uri: &Url,
        line: u32,
        character: u32,
    ) -> Result<Vec<GraphNode>, LainError>;

    pub async fn type_definition(
        &self,
        file_uri: &Url,
        line: u32,
        character: u32,
    ) -> Result<Vec<GraphNode>, LainError>;

    pub async fn incoming_calls(
        &self,
        file_uri: &Url,
        line: u32,
        character: u32,
    ) -> Result<Vec<GraphEdge>, LainError>;

    pub async fn outgoing_calls(
        &self,
        file_uri: &Url,
        line: u32,
        character: u32,
    ) -> Result<Vec<GraphEdge>, LainError>;
}
```

Each method goes through the same circuit-breaker + timeout +
cold-boot prewarm path that `get_document_symbols_hierarchical`
already uses (`src/server/lsp.rs:768`). The prewarm pass is
extended to issue one `implementation` call against a sentinel
file per language, parallel to the existing
`documentSymbol` prewarm.

## Graph integration

`schema.rs` gains a new variant:

```rust
DispatchedBy,  // LSP-resolved dynamic call: dyn Trait receiver
                // → concrete impl. EdgeType enum, sibling to
                // DynamicDispatch but with provenance = Static.
```

`EdgeProvenance::Static { source: StaticSource::Lsp }` already
exists; the new edges carry that provenance. The Tier 2 / Tier 3
heuristic / runtime paths do not produce `DispatchedBy`; only
`LspMultiplexer::implementation` and `LspMultiplexer::type_definition`
do. The blast-radius filter already accepts `Lsp` provenance
through the existing `is_static_dependency` check, so no change to
`impact.rs` is needed for the default path. The
`LAIN_HEURISTIC_MIN_CONFIDENCE` knob does not apply (these edges
are 1.0 confidence).

## Per-language resolution matrix

| Language | LSP | Trait / interface resolution | Cold-boot cost | Install |
|---|---|---|---|---|
| Rust | `rust-analyzer` (existing) | `textDocument/implementation` ✓ | 2-5 s | `rustup component add rust-analyzer` |
| C / C++ | `clangd` (existing) | `textDocument/implementation` partial (virtual methods) | 1-3 s | `apt install clangd` / brew |
| Kotlin | `kotlin-language-server` (new) | `textDocument/implementation` ✓ | 2-4 s | `brew install kotlin-language-server` |
| Dart | `dart analyze --lsp` (new) | `textDocument/implementation` ✓ | 1-2 s | bundled with Dart SDK |

The `LANGUAGE_MAP` table in `src/server/lsp.rs:133` gains two
entries; the install command is gated on the same platform
detection that the existing five entries use.

## Performance budget

| Operation | Budget | Notes |
|---|---|---|
| `implementation` round-trip | 1 s | existing `LSP_REQUEST_TIMEOUT` reused |
| Cold-boot prewarm | 30 s per binary | existing `lsp_prewarm_timeout_secs` reused |
| Indexing pass surface area | +5 % worst case | added `implementation` calls per file in `build_core_memory` |
| Tool-call latency (`get_blast_radius` on a hub) | +50 ms p95 | one extra LSP round-trip when the symbol is in a hub |

The `+5 %` budget is the one that needs operational monitoring. If
it slips, the fallback is to run `implementation` lazily — on the
first `get_blast_radius` call against a hub, not during indexing.

## Failure isolation

The existing per-binary circuit breaker (3 failures → `unavailable`
for the rest of the process lifetime, with a restart budget for
crashes) handles LSP flakiness. New risks specific to the
expanded requests:

- `implementation` returning empty for legitimate trait objects:
  the LSP just doesn't know. Treat as no-op; the heuristic sensor
  picks up the slack.
- `kotlin-language-server` requiring JDK on PATH at startup:
  pre-flight `which kotlin-language-server` and `which java`; if
  either is missing, mark the binary `unavailable` with the
  install hint.
- Cold-boot cost on a Kotlin-heavy workspace (200+ files): the
  prewarm phase runs one `implementation` per sentinel file;
  bound by `lsp_prewarm_max_files` (default 50) like the existing
  prewarm.

## Tests

| Test | What it proves |
|---|---|
| `rust_analyzer_resolves_dyn_trait` | Code fixture: `fn handle(t: &dyn Tracer)` + `impl Tracer for T1` + `impl Tracer for T2`. After indexing, `LspMultiplexer::implementation` returns both `T1` and `T2` as `GraphNode` entries. |
| `clangd_resolves_virtual_method` | C++ fixture with `class Base { virtual void f(); }; class Derived : public Base { void f() override; }`. `implementation` at `f()` returns `Derived::f`. |
| `incoming_calls_returns_callers` | Fixture: `a()` calls `b()` calls `c()`. `incoming_calls` at `b()` returns `a()` with the right `EdgeType::Calls`. |
| `circuit_breaker_opens_after_three_failures` | Three timeouts in a row on `implementation` mark the binary `unavailable`; subsequent calls return `Err(LspUnavailable)`. |
| `cold_boot_prewarm_runs_implementation` | `prewarm_outcomes()` records `PrewarmOutcome::Ok` for the new request on the sentinel fixture. |
| `dispatched_by_edge_has_lsp_provenance` | A node resolved via `implementation` produces a `DispatchedBy` edge with `EdgeProvenance::Static { source: Lsp }`. |
| `kotlin_language_server_install_and_invoke` | Integration test (skipped if `kotlin-language-server` not on PATH): round-trip a Kotlin interface implementation through the listener. |
| `graceful_degradation_when_no_lsp_for_hub` | Rust trait object without rust-analyzer on PATH: `explain_dispatch` returns `heuristic_only` instead of erroring. |

The Kotlin and Rust tests live in `tests/lsp_dispatch.rs`; the
graceful-degradation test pins the contract that a missing LSP
falls through to Tier 2 rather than failing the request.

## Documentation updates

- `docs/TECHNICAL.md`: append a "Hybrid LSP" section describing
  the `implementation` / `typeDefinition` / `incomingCalls` /
  `outgoingCalls` methods and how they feed `EdgeType::DispatchedBy`.
- `docs/dynamic-dispatch.md`: replace the "Hybrid LSP" caveat with
  a link to `docs/hybrid-lsp-expansion.md` and a one-paragraph
  summary of which languages are covered and which fall back.
- `docs/QUICKSTART.md`: a "language coverage" subsection listing
  Rust + C++ + Kotlin + Dart as LSP-supported, others as
  heuristic-only.

## Verification

1. Build the test fixture (Rust + C++ + Kotlin + Dart).
2. `cargo test -p lain --test lsp_dispatch` → green.
3. Index the fixture repo and confirm the graph contains
   `DispatchedBy` edges with `provenance = Static { source: Lsp }`.
4. Call `get_blast_radius` on a method behind a `dyn Trait` and
   confirm the impls appear as direct callers.
5. Stop `rust-analyzer` mid-run and confirm the circuit breaker
   keeps the rest of the workspace healthy.
6. `cargo build --release` clean; `cargo test -p lain` green.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `LSP_REQUEST_TIMEOUT` (1 s) too tight for some implementations requests | Per-language override via `lsp_request_timeout_overrides: HashMap<String, Duration>` in `tuning.toml`; default still 1 s. |
| Kotlin / Dart binaries missing on operator machines | `LspConfig::install_cmd` reuses the existing `install_language_server` flow; the CLI surfaces clear error messages. |
| Per-file LSP round-trip during indexing scales poorly | Lazy fallback: `implementation` runs on the first `get_blast_radius` against a hub, not during indexing. Switch the default via a feature flag. |
| LSP responses vary across versions | Pin LSP binary versions in `toolchains.toml`; document the floor (e.g., `rust-analyzer >= 2024-01-01`). |
| Federation: per-repo LSP install | Reuse `install_language_server --extensions auto`; one CLI command bootstraps everything. |

## Estimated effort

One engineer, three to four weeks. Most time is in the Kotlin and
Dart language registrations (the install paths and platform
detection each take a day), plus the `implementation` request
handling per LSP. The Rust trait-object resolution is well-trodden
ground.

## Out-of-plan follow-ups

- Java via Eclipse JDT-LS. Deferred until user demand.
- Swift via sourcekit-lsp. Requires Xcode integration.
- TypeScript union-type dispatch via `typescript-language-server`.
  Coverage is incomplete; the heuristic sensor already covers
  the relevant patterns.

## What this plan does NOT retroactively fix

Tier 2's `dynamic_dispatch_sensor` and Tier 3's `explain_dispatch`
are independent of this work. They continue to operate on
convention patterns (Tier 2) and runtime observations (Tier 3
once the OTLP adapter lands). This plan only adds a new
*coverage axis*: type-resolved dynamic calls, which neither Tier
2 nor Tier 3 can produce. Operators who don't install Kotlin or
Dart see no change in behavior.
