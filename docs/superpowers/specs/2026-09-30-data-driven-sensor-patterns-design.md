# Data-driven sensor patterns — design

## Intent

Refactor the contract-federation sensors' per-framework detection patterns from hardcoded Rust match arms and tree-sitter node-kind checks into data files. Adding a new framework (Laravel PHP routes, Spring WebFlux, FastAPI Python, Play Scala, Phoenix Elixir, .NET MAUI, etc.) should become a data change — one `.scm` file plus one YAML entry — not a Rust change.

Out of scope: changing the per-language rule semantics in `field_access_sensor.rs` (rules 1–6 from §6.5 — those ARE language-specific semantics, not pattern detection). The read-side walker stays in Rust; this refactor only externalizes pattern detection.

## Current state (why this matters)

**`http_sensor.rs`** — pure regex, one `RoutePattern` per (language, framework) tuple, ~14 patterns in a single function body (`get_route_patterns`, lines 182-366). Adding Spring or Ktor = ~12-15 lines of Rust inside this function. The function is already ~180 lines and growing linearly with each framework.

**`http_client_sensor.rs`** — tree-sitter, one `detect_*_call` per language (8 languages), each 80-130 LoC. ~1000 LoC of per-language detection code; ~30 patterns per language as inline `if` chains on tree-sitter node kinds.

**`field_access_sensor.rs`** — tree-sitter, one `handle_*_node` per language. Plus `RESPONSE_METHOD_DENYLIST` (correctly placed in `util.rs`). Rules 1–6 are encoded in each per-language handler.

**`entry_point_sensor.rs`** — pure regex, one detector per `EntryKind`. Most contained.

**`Lang` enum duplicated** between `util.rs` (10 variants incl. `Ts`/`Tsx`) and `http_client_sensor.rs` (8 variants). `http_client_sensor.rs` re-implements `parse_for_lang` even though `util.rs::parse_for_lang` exists. Doc comment lies about this.

## Goals

- **Adding a framework = a data change**, not a Rust change.
- **Bundled patterns compile at build time** — tree-sitter query compilation is part of the build graph; type and catch errors early.
- **Org-specific overrides load at runtime** from a configurable path (default `<repo>/.lain/patterns/`) — allows individual projects to add or override patterns without rebuilding LAIN.
- **All existing tests stay green**; the `pr13_hermetic_precision_recall_over_t1_fixture` still reports all six metrics at 1.000.
- **No schema version bumps**, no fixture edits, no ground_truth edits, no CONTRACT_ANALYZER_REV bump.

## Non-goals

- **Refactoring rule semantics** in `field_access_sensor.rs` (rules 1–6 from §6.5). Those stay in Rust. Per-language walker dispatch arms stay in Rust. The refactor externalizes pattern *detection* only.
- **Refactoring `Lang` enum unification** beyond what the pattern-detection refactor requires (will happen as a side effect but no architectural decision needed).
- **Replacing tree-sitter's runtime grammar lookup** — `tree_sitter_*::LANGUAGE` stays the parser source; the refactor changes how we *walk* parsed trees, not how we *get* them.
- **Adding new frameworks in this spec.** A follow-up PR adds Laravel / Spring WebFlux / etc. as data-only additions.

## Two-tier pattern model

### Camp A — Regex / text patterns (small, easy)

**File:** `sensors/patterns/frameworks.yaml` (single YAML, language-keyed sections).

**Schema (JSON-Schema-flavored, written informally here):**

```yaml
languages:
  python:
    - id: fastapi-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
      # The actual regex stays in Rust for now (http_sensor) or migrates
      # to a `.scm` for the structural parts.
    - id: httpx-outbound
      kind: outbound
      lib_match: '^(requests|httpx|httpx\.Client|.*\.Client)$'
      deny_methods: [json, text, data, body]
  rust:
    - id: axum-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
    - id: reqwest-outbound
      kind: outbound
      lib_match: '^(reqwest|.*reqwest::Client.*|ureq.*)$'
  go:
    - id: stdlib-http
      kind: outbound
      lib_match: '^http\.(Get|Post|NewRequest|.*)$'
      deny_methods: [decode, Decode, Close, Body, StatusCode, Status, Header, Proto, Request, TLS, Trailer, ContentLength]
  java:
    - id: spring-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
      annotation: '(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)\b'
    - id: okhttp-outbound
      kind: outbound
      lib_match: '^(OkHttpClient|.*OkHttpClient.*|okhttp3\..*)$'
  csharp:
    - id: aspnet-route
      kind: route
      verbs: [get, post, put, delete, head, options, patch]
      attribute: '\[(HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch|HttpHead|HttpOptions|HttpRequest)\]'
    - id: httpclient-outbound
      kind: outbound
      lib_match: '^(HttpClient|.*HttpClient.*|WebClient.*)$'
  ruby:
    - id: sinatra-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
    - id: rails-route
      kind: route
      source: 'config/routes.rb'   # text-parsed by language; not tree-sitter
    - id: net-http-outbound
      kind: outbound
      lib_match: '^(Net::HTTP|HTTParty|Faraday|RestClient)$'
  kotlin:
    - id: ktor-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
    - id: ktor-client-outbound
      kind: outbound
      lib_match: '^(HttpClient|OkHttpClient)$'
```

This YAML is **loaded at runtime** by a single `Patterns::from_yaml()` parser. Tree-sitter queries (Camp B) are referenced by `id` but the query bodies live in `.scm` files.

### Camp B — Tree-sitter structural patterns

**Files:** `sensors/patterns/{rust,go,java,csharp,ruby,kotlin}/<framework>.scm` — one file per (language, framework).

**Example: `sensors/patterns/rust/axum-route.scm`** (HTTP GET route detection):

```scheme
(call_expression
  function: (identifier) @Router_new
  arguments: (arguments
    (call_expression
      function: (field_expression
        object: (identifier) @self
        field: "route")
      arguments: (arguments
        (string_literal
          (string_content) @path)
        (call_expression
          function: (identifier) @verb_method
          arguments: (arguments
            (identifier) @handler))))))
```

**Example: `sensors/patterns/rust/reqwest-outbound.scm`** (outbound GET):

```scheme
(call_expression
  function: (scoped_identifier
    name: (identifier) @lib
    path: (identifier_path
      (identifier) @reqwest)
    scope: (identifier) @http_method)
  arguments: (arguments
    (string_literal) @url)
```

Tree-sitter query captures are bound to Rust identifiers (`@Router_new`, `@path`, `@verb_method`, `@handler`, `@url`, `@lib`). The sensor walker turns captures into `HttpRoute` / `Field` / `HttpClientCall` nodes.

**Build pipeline:**

- `tree_sitter_query_assets!()` macro from `tree-sitter-query` crate (or hand-rolled with `include_str!` + `Query::new`) compiles each `.scm` at build time into a static `Query` instance.
- A `build.rs` script in `sensors/patterns/` enumerates the `.scm` files and validates them at build time (via `tree_sitter_cli` or `Query::new` per file).
- Compile errors fail the build. Missing captures are caught at first use (runtime), but the sensor logs them clearly.
- Cargo recompiles when an `.scm` file changes (via `include_str!`).

**Runtime override path:** A second `Patterns::load_overrides(path)` method reads `<repo_root>/.lain/patterns/*.yaml` + `*.scm` at sensor init. Override files have the same schema as the bundled ones. They REPLACE (not augment) entries with matching `id`. Missing keys from the override fall back to bundled.

**Override wiring (post-`runtime-override wire-in` pass, 2026-10-02):** Before this PR, override bodies were validated and stored by `Patterns::load_overrides` but never reached the walkers — no `scan_workspace_*` ever called `load_overrides`, and `compiled_queries()` returned only the bundled static. This was a load-bearing gap: per-repo overrides at `<repo_root>/.lain/patterns/` were checked at test time but never reached production scans.

The wire-in closes the gap:
1. `Patterns::with_overrides(root)` is a new helper that clones the bundled singleton and runs `load_overrides(root)` on it.
2. Every `scan_workspace_*` (routes, clients, field_access, entry_points) opens its scan with `Patterns::with_overrides(root)?` after the read-only guard. The walker code consumes the per-scan instance via `&patterns`, replacing every `Patterns::patterns()` call site that was previously reading the bundled singleton.
3. `Patterns::compiled_queries()` now returns `Result<Cow<'static, [(key, lang, framework, body)]>, PatternsError>` — `Cow::Borrowed(generated::QUERIES)` when no overrides are loaded (zero cost fast path), `Cow::Owned(merged)` when overrides are loaded. Override bodies are `Box::leak`'d once per unique body so they fit the `&'static str` tuple field.
4. `field_access_sensor.rs`'s deny-gate (`util::is_deny_method`) is the one accessor that can't easily take a `&Patterns` parameter (it's called from deep walker helpers 8 levels deep in `handle_python_node` / `handle_attribute`). The wire-in installs a thread-local `Patterns` reference at the top of `scan_workspace_field_access` via `util::with_current_patterns`; `is_deny_method` reads from there (falling back to the bundled singleton when no thread-local is set).

End-to-end coverage: `tests/sensors/override_end_to_end.rs` pins both halves (YAML override flips `json` off the Python deny surface; `.scm` override replaces the bundled `axum-route.scm` body).

## File layout (after refactor)

```
src/server/sensors/
├── patterns/                              # NEW — data-driven patterns
│   ├── frameworks.yaml                    # Camp A (regex/text + deny_methods)
│   ├── build.rs                           # NEW — compile & validate .scm files
│   ├── rust/
│   │   ├── axum-route.scm
│   │   ├── actix-route.scm
│   │   ├── reqwest-outbound.scm
│   │   └── ureq-outbound.scm
│   ├── go/
│   │   ├── stdlib-http-outbound.scm
│   │   └── gin-route.scm                   # Camp B structural — Gin route + handler
│   ├── java/
│   │   ├── spring-route.scm               # @GetMapping pattern detection
│   │   ├── jaxrs-route.scm
│   │   └── okhttp-outbound.scm
│   ├── csharp/
│   │   ├── aspnet-core-route.scm
│   │   ├── minimal-api-route.scm
│   │   └── httpclient-outbound.scm
│   ├── ruby/
│   │   ├── sinatra-route.scm
│   │   └── net-http-outbound.scm
│   └── kotlin/
│       ├── ktor-route.scm
│       └── ktor-client-outbound.scm
├── util.rs                                # existing — Lang enum, lang_for_path, walker; add Patterns loader + dispatcher
├── http_sensor.rs                         # refactored: queries + YAML dispatch
├── http_client_sensor.rs                  # refactored: same
├── field_access_sensor.rs                 # MINIMAL change — denylist now from util.rs (already); only the per-language walker dispatch arms stay; pattern detection uses query + YAML for `.method()` deny lists etc.
└── entry_point_sensor.rs                  # refactored: scheduled/cli/main from YAML; per-framework regex stays in Rust for now (or split to .scm for Spring/ASP.NET/Ruby annotations)
```

## How each sensor is refactored

### `http_sensor.rs` — pure regex today, becomes query-driven

Current: `get_route_patterns()` returns a `BTreeMap<&'static str, RoutePattern>` of ~14 entries, each with three regexes (verb, path, handler). The walker scans each file and applies each pattern.

Refactored:
- `RoutePattern` becomes a struct loaded from `frameworks.yaml`. Each pattern has: `kind: route`, `verbs`, `path: <tree-sitter query fragment OR regex>`, `handler: <field>`, `prefix_kind: <regex OR query>`.
- The scanner asks `Patterns::for_language(lang)` to get the set of patterns, then iterates and emits `HttpRoute` nodes the same way it does today.
- New patterns (e.g., FastAPI Python, Spring WebFlux) = one YAML entry + one `.scm` file. No Rust changes.

### `http_client_sensor.rs` — tree-sitter today, becomes query + YAML

Current: 8 `detect_*_call` functions, each 80-130 LoC of `if` chains on tree-sitter node kinds.

Refactored:
- One `detect_calls(lang, tree) -> Vec<HttpClientCall>` walks the tree, finds all outbound-call nodes, classifies via:
  1. Run the lang's outbound `*.scm` query → matches `(library, verb, url_subtree)` triples.
  2. The matched `url_subtree` is parsed by the existing language-agnostic `parts_from_node_inner` → `UrlPart[]` → `UrlPart::Literal | UrlPart::Hole` → `NormalizedUrl`.
  3. The library name + verb determine the kind (`Library` / `Receiver` / etc.) and the deny-method list (looked up from `RESPONSE_METHOD_DENYLIST` via `Patterns::deny_methods_for(library, verb)`).
- `Client { lib_name, base_url }` is captured by a separate query per language (`<lib>_client_ctor.scm`) → instance-name + base_url.
- The framework-specific shape (`.get(url)` vs `reqwest::get(url)` vs `client.get(url)`) is encoded in each language's outbound queries.
- Per-language detect functions disappear.

### `field_access_sensor.rs` — minimal change

Current: per-language `handle_*_node` walkers, each 100+ LoC. Rules 1-6 encoded in `handle_*_assignment` etc.

Refactored:
- `RESPONSE_METHOD_DENYLIST` already in `util.rs` (correctly placed by Workstream 5).
- Per-language walker dispatch arms stay (rules 1-6 ARE language-specific semantics).
- New: the **`field_access_sensor.rs` deny gate** looks up deny methods via `Patterns::deny_methods_for(lang, recv_type)` — same logic as today but data-driven.
- The walker itself (the bound-tracking, the escape set, the FieldRef emission) stays in Rust. No structural rewrite.

### `entry_point_sensor.rs` — pure regex today, becomes YAML-driven for Spring/ASP.NET/Ruby

Current: 7 detectors (HttpHandler / Scheduled / Cli / Main), each per-framework regex.

Refactored:
- Spring `@GetMapping` detector becomes a query (`spring-route-annotation.scm`) — captures `@GetMapping` annotation + the next non-blank method declaration within 6 lines.
- ASP.NET `[HttpGet]` detector likewise.
- Ruby Rails controller actions detector stays regex-based (whole-file scan for `class …Controller < ApplicationController` lines, then `def` lines within).
- Scheduled/Cli/Main detectors stay regex-based (cron schedule strings, NestJS `@Cron` annotations, `def main`).
- New `MainDetector` for Frameworks (Spring Boot's `public static void main(String[] args)`, Go `func main()`, Rust `fn main()`, .NET `static void Main(string[] args)`) added.

### `Lang` enum deduplication

Free side-effect of this work: `http_client_sensor.rs::Lang` disappears; the sensor imports `util::Lang` directly. The doc-comment fix is automatic.

## Build & test plan

### Build

- New `sensors/patterns/build.rs`:
  ```rust
  fn main() {
      for entry in walkdir("sensors/patterns").filter(|e| e.path().extension() == Some("scm")) {
          let src = fs::read_to_string(entry.path()).unwrap();
          let lang = tree_sitter_query_assets::compile(&src, /* language hint from path */)
              .unwrap_or_else(|e| panic!("query {} failed to compile: {}", entry.path().display(), e));
          println!("cargo:rerun-if-changed={}", entry.path().display());
      }
  }
  ```
  (Simplified; actual build.rs uses `tree-sitter-cli` crate to compile each `.scm` against the right grammar, storing results in `OUT_DIR`.)
- `tree-sitter-cli` is already an indirect dep (via `tree-sitter`); may need to add as `[build-dependencies]`.

### Test

Per-pattern assertion: `tests/sensors/patterns.rs` loads each pattern + a synthetic source file containing the matching construct + a non-matching construct; asserts the matching construct is detected and the non-matching is not. This is the regression guard.

Plus per-scenario regression coverage:
- `cargo test --test federation_contracts_e2e pr13_hermetic_precision_recall_over_t1_fixture` — all six metrics at 1.000.
- `cargo test --workspace` exit 0.
- `cargo clippy --all-targets -- -D warnings` exit 0.
- `cargo fmt --check` clean.
- `scripts/check-*.py` clean.
- Existing per-sensor test suites (http_sensor, http_client_sensor, field_access_sensor, entry_point_sensor) — all still green.

## Migration plan (task ordering in writing-plans)

1. **Foundation:** Add `sensors/patterns/build.rs` + `sensors/patterns/frameworks.yaml` (existing patterns encoded as data) + `sensors/patterns/{rust,go,java,csharp,ruby,kotlin}/` skeleton. Verify build succeeds; verify runtime loader loads patterns; verify existing tests still pass (no behavior change yet — patterns loaded but not yet consumed).

2. **Migrate `http_sensor.rs`:** Replace the inline `get_route_patterns()` body with a runtime-loaded `Patterns::route_patterns(lang)` query. Existing tests should pass without change. Add new pattern tests in `sensors/patterns.rs`.

3. **Migrate `http_client_sensor.rs`:** Delete the 8 `detect_*_call` functions. Replace `detect_calls` body with a tree-sitter-walker that consumes the per-language outbound `*.scm` queries. The shared `parts_from_node_inner` and `host_for_envizer` stay. `Lang` deduplicates. Existing tests pass.

4. **Migrate `entry_point_sensor.rs`:** Replace Spring + ASP.NET + Ruby controller-action detectors with query-driven detection. Scheduled/Cli/Main stay regex. New `Main` cross-language helper.

5. **`field_access_sensor.rs` — minimal change:** No rewrite of walker logic. Add a data-driven deny-methods lookup that reads from `frameworks.yaml` (replacing the hardcoded `RESPONSE_METHOD_DENYLIST`). All existing tests pass; deny list behavior identical.

6. **Build-time compilation:** `build.rs` validates every `.scm` file at compile time; query parse errors break the build.

7. **Runtime override path:** `Patterns::load_overrides(root)` reads `<repo>/.lain/patterns/` if present; replaces entries by `id`. Test with a fake override directory.

8. **End-to-end verification:** All gates green; metrics stay at 1.000; a new framework can be added (as a data change) and a regression test added without touching Rust.

## Risks

- **Tree-sitter query syntax bugs.** `.scm` files use a Lisp-like syntax; mistakes are easy. The build-time `tree-sitter-cli` compile pass catches them, but query capture names must match what the sensor walker expects. Mitigation: per-pattern regression test in `tests/sensors/patterns.rs`.
- **Tree-sitter query coverage gaps.** A framework may need multiple queries (handler name capture, route capture, prefix capture). Each `*.scm` file is small; one file per concern. Mitigation: each framework ships with multiple `.scm` files if needed.
- **Performance.** Tree-sitter query execution on every parse is fast (microseconds per node). For LAIN's T1 fixture (a few hundred files), this is negligible. For large monorepos, the walker cost is dominated by parsing + tree construction, not query execution. Mitigation: profile.
- **Doc-comment drift.** Many sensor files have outdated doc-comments about which rules or sub-features are implemented. Re-read each file's doc-headers before committing; fix or remove obsolete claims.
- **Coverage gaps the refactor doesn't fix.** Per the inventory, `Sinatra` handler-name resolution, `kotlin-ktor` in-source detection, and Java outbound URL binding (always emits synthetic URL) are still partial. These are independent improvements.

## Out of scope

- Adding new frameworks (Laravel, Spring WebFlux, FastAPI, Play, Phoenix, .NET MAUI, etc.). A follow-up PR adds data-only entries once the refactor lands.
- Refactoring `Lang` enum unification beyond what the pattern refactor needs (free side-effect).
- Replacing per-language walker dispatch arms in `field_access_sensor.rs` (rules 1-6 stay in Rust; the refactor only changes the deny-method lookup).
- Cross-cutting changes to the joiner, diff, contract-tool surface, snapshot pipeline, or precision/recall metrics.