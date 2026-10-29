# NeMo Agent Toolkit HTTP Contract Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the NeMo Agent Toolkit workspace show real HTTP contracts — providers the analyzer can see, plus provider declarations an operator or an agent injects at runtime — instead of zero bound contracts and eight garbage routes, and give Lain the runtime-config write path that makes such declarations a first-class capability.

**Architecture:** The consumer half already works (145 HTTP call sites detected). The provider half is dark because the NAT server registers every route with `app.add_api_route(path=<a runtime config field>, methods=[…])` — the literal path lives in a YAML config, never in the code — and because the FastAPI detector only matches decorators. On top of that, the UI's reverse proxy builds its URL into a variable, so the captured template collapses to `/` with no host, which makes even a hand-written binding fail. The fix is four layers: a provider declaration the operator (or an agent) can supply at runtime through a new `repos.yaml#provides` section, an MCP tool that writes that section and reloads the federation, a consumer-URL fix that recovers the host, and a precision fix that stops error strings from becoming routes.

**Tech Stack:** Rust (stable toolchain per `rust-toolchain.toml`), `tree-sitter` 0.25, `serde_yaml` 0.9, `serde_json`, `inventory` for tool registration, `cargo test` (unit tests in-module + `tests/` integration tests).

**Spec:** `docs/protocol-coverage.md` (§http — the resolver ladder, the binding grammar, the coverage matrix), `docs/REPOS_YAML.md` (§services, §http_clients, §bindings), `docs/hot-reload.md` (the reload bus that re-applies the contract config and re-joins).

## Implementation status

Landed with Plan A on `feat/nat-http-contract`, open as PR #381 (both plans in one PR: they share eight files). Measured against the real NVIDIA clones with `scripts/eval/nat-http-contract-smoke.sh` and recorded in `docs/protocol-coverage.md` §http: **12 HTTP endpoints, 1 bound consumer** (a test client calling the declared `http:POST /chat/stream`), 43 unresolved (`no_match` 29, `no_route_in_service` 10, `graphql_no_op` 2, `env_unmapped` 2), the garbage `ANY` routes down from 8 to 0, `http:ANY /api/mcp/clients` detected from the Next.js file route, and the UI proxy attributed to the `nat` service.

| Task | State | Note |
|---|---|---|
| 1 — prerequisite (Plan A Task 1) | done | landed with Plan A |
| 2 — non-URL strings are not routes | done | shipped as "only HTTP verbs and path-shaped strings declare a route" |
| 3 — host from an env-built URL | **substituted, done** | the analyzer does not follow a multi-step variable chain, so the mapping is operator-declared instead: a new `host_aliases:` section maps a host expression (or a repo-relative file) to a service. `apply_contract_context` writes the aliases too, so the runtime path injects both kinds of context |
| 4 — `app.add_api_route` detection | **not built, replaced** | NAT's route paths live in a runtime config file, so `provides:` is the mechanism; the pattern would only catch a literal path, which NAT never has |
| 5 — `provides:` section | done | `ProviderDecl` in the contract config |
| 6 — `apply_contract_context` | done | the runtime write path: validate, back up, write atomically, reload, report. Takes `provides` and `host_aliases`; the `bindings` argument the interface sketch below listed is not implemented |
| 7 — Next.js `pages/api` routes | done | ships as "nextjs pages/api files with a default export are HTTP routes" |
| 8 — NAT smoke + recorded numbers | done | `scripts/eval/nat-http-contract-smoke.sh` |

Three deviations from this plan, all deliberate and all recorded on the branch: Task 3 became operator-declared rather than analyzer-derived (the analyzer cannot follow the chain; the operator can state it), Task 4 was replaced by Task 5's mechanism rather than implemented as a pattern, and Task 6's tool takes `provides` and `host_aliases` only — a hand-written `bindings:` entry still comes from `check_binding`, which emits the YAML for an operator to paste. With the alias declared, the production UI proxy (`proxy/server.js`) is no longer an unresolved `no_match`: it is attributed to the `nat` service with the verdict `no_route_in_service` — a known service, a route that is dynamic because a transparent proxy forwards whatever the client asked for.

One limitation the measurement exposed, and then fixed: the analyzer recorded the proxy's URL as the identifier `targetUrl`, not as the host expression `UPSTREAM_HTTP_ORIGIN` — the const lives in the request handler and the fetch sits in a nested closure, so the expression was not followed. The HTTP client sensor now records assignments per scope and resolves an identifier through the scope chain (innermost first), so a name assigned in an enclosing scope is visible in a nested closure, a name assigned in a sibling scope is not, and a name assigned twice in one scope stays unresolved. The alias therefore names the host expression, which is the natural declaration.

A second step, landed after that: the analyzer now follows the host expression through the file's assignments to the environment variable that configures it — a name resolves to the expression it is assigned, a template to the hole that carries the origin, a member expression to its base (`upstream.protocol` → `upstream`), and a call to its arguments (`new URL(NAT_BACKEND_URL)`) — so the proxy's host resolves to `NAT_BACKEND_URL` on its own and the `nat` service's `env:` list routes it. Only the origin is followed: a literal host stays a literal, and an env var in the path is never mistaken for the host. The operator-declared `host_aliases:` route stays for a host the analyzer cannot trace.

## Measured baseline

Measured on 2026-10-29 with `lain 0.13.0` (commit `0355a95b`, built from `origin/dev`) against shallow clones of `NVIDIA/NeMo-Agent-Toolkit`, `NVIDIA/NeMo-Agent-Toolkit-UI`, `NVIDIA/NeMo-Agent-Toolkit-Examples`, federated with this `repos.yaml`:

```yaml
services:
  - name: nat
    repo: nat
    hosts: [localhost, 127.0.0.1]
    env: [NAT_BACKEND_URL, NAT_SERVER_URL, NAT_URL, NEXT_PUBLIC_NAT_URL]
  - name: nat-ui
    repo: nat-ui
  - name: nat-examples
    repo: nat-examples
```

| Measurement | Value | Where it came from |
|---|---|---|
| Bound contracts | **0** | every entry of `list_contracts` reports `bound_consumers: 0` |
| `nat` endpoints | 12 — 8 HTTP, 2 topic, 2 table | `list_services` → `endpoints: 12` |
| NAT HTTP providers | 8, of which **6 are in test files**, 1 is an incidental decorator, 1 is an example agent | `list_contracts` → provider paths |
| The real routes | **absent** | `/chat/stream`, `/chat`, `/generate`, `/execute`, `/websocket` appear nowhere |
| `nat-ui` "routes" | 11, of which **8 are garbage** | `/ERROR: Invalid NEXT_INTERNAL_URL format:`, `/Failed to submit feedback:`, `/Error fetching MCP clients:`, `/div`, `/public`, `/x-session-id`, `/⚠️  Port conflict: requested %s, using %s`, … |
| Unresolved consumers | 44 (`nat-ui` 19, `nat` 24, `nat-examples` 1) | `list_unresolved` |
| Unresolved reasons | `no_match` 31, `no_route_in_service` 9, `env_unmapped` 2, `graphql_no_op` 2 | `list_unresolved` |
| The UI's real backend dial | `template: "/"`, `host: null`, verdict `no_match` | `proxy/server.js:234` and `:263` |
| Hand-binding the real link | `check_binding` → `valid: false`, `reasons: ["template_mismatch"]` — for `http:POST /chat/stream`, for `http:POST /chat`, and even for `http:GET /health` (which *does* exist as a provider) | `check_binding` with `consumer: nat-ui:HttpClientCall:proxy/server.js:POST /:263` |
| Coverage ledger (live view) | `error: "coverage ledger unavailable for live view; prepare a pinned snapshot"` | `get_coverage` |
| NAT index time | 59 s ingest + ~9 min post-ingest | `lain server` log |

### Why the provider half is dark — the two exact gaps

1. **NAT never uses a route decorator.** Every production route goes through `app.add_api_route(path=endpoint_path, methods=[endpoint_method], …)` (`NeMo-Agent-Toolkit/packages/nvidia_nat_core/src/nat/front_ends/fastapi/routes/chat.py:84`, and the same shape in `generate.py`, `async_generation.py`, `execution.py`, `v1_chat_completions.py`). The FastAPI detector is a tree-sitter query that matches a *decorator* (`src/server/sensors/patterns/python/fastapi-route.scm`). The only decorator hits in the whole toolkit are in test files and one auth handler (`authentication_flow_handler.py:230`).
2. **The path is a runtime configuration value.** `endpoint_path` is `str | None = None` on a Pydantic config model (`fastapi_front_end_config.py:170-205`); the operator supplies `/chat/stream` in their NAT config file. No static analyzer can read it out of the Python source. This is the structural difference from Cluster A, where the proto file *is* the contract.

### Why the consumer half cannot bind even to a declared provider

The UI's proxy builds the target URL into a variable (`NeMo-Agent-Toolkit-UI/proxy/server.js:212-215`):

```js
const backendPath = pathname.substring(HTTP_PROXY_PATH.length) || '/';
const targetUrl = `${UPSTREAM_HTTP_ORIGIN}${backendPath}${parsedUrl.search || ''}`;
const backendRes = await fetch(targetUrl, { method: req.method, … });
```

`identifier_parts` (`src/server/sensors/http_client_sensor.rs:1823`) resolves `targetUrl` to its template literal and `template_from_string` splits it into literal/hole parts. The literal part is empty (the template starts with a hole), so the captured template is `/` and the host is `null` — which is why `check_binding` refuses even a perfect provider: `template_mismatch`.

## Global Constraints

- No new `ContractKey` variant. The exhaustive `match` over `ContractKey` in `src/server/federation/contracts/coverage_matrix.rs` must keep compiling unchanged.
- No federation-graph schema bump. `FEDERATION_GRAPH_VERSION` in `src/server/federation/graph_backend.rs` stays as it is. The declared providers live in the *contract config*, not in the on-disk graph.
- Ship on `dev`; the PR targets `dev`. Never target `main`. Do not bump any version in this plan.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass on the branch.
- The contract tool schemas under `src/server/mcp/contract_tools/schemas/` are `include_str!`-ed into the binary; after changing any schema, run `make schema` and commit the regenerated `docs/tool-schema.json`.
- **The runtime write path is a side effect on the operator's file.** Every write is atomic, validated before it lands, backup-first, and dry-run by default. A tool that writes `repos.yaml` without those guards is a defect, not a shortcut.
- Determinism: the same workspace and the same declared context must produce the same contract index, byte-for-byte, across runs.

## Review Focus

The failure modes below are implied by the design but not exercised by any task's tests. Each one gets a test in the task that owns the code.

1. **A declared provider that names an undeclared service.** The write must be refused — a provider attached to a service nobody can consume against is a silent no-op that reads as "no contract".
2. **A declared provider with a malformed key.** The write must be refused with the key grammar's own error message, not accepted and silently dropped by the joiner.
3. **A declared provider whose service also owns real, detected providers.** The declared one must not shadow or duplicate them — `list_contracts` must show one endpoint with the detected provider, and the declared entry must be reported as its source.
4. **Two agents declaring the same provider concurrently.** The second write must not lose the first (`repos.yaml` read-modify-write under a lock, or a compare-and-swap on the file's mtime/hash).
5. **A write that fails validation.** The operator's `repos.yaml` must be byte-identical to what it was before the attempt — a rejected write must leave no trace.
6. **A reload that fails to re-join.** The tool must report the failure instead of returning a view that claims the declaration took effect.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/server/federation/contracts/config.rs` | modify | `ContractFederationConfig.provides: Vec<ProviderDecl>` + validation |
| `src/server/federation/contracts/joiner/endpoints.rs` | modify | Inject declared providers into the endpoint table |
| `src/server/mcp/contract_tools/context.rs` | create | The `apply_contract_context` tool: validate, merge, write, reload, report |
| `src/server/mcp/contract_tools/schemas/apply_contract_context.{in,out}.json` | create | Tool schemas |
| `src/server/mcp/definitions.rs` | modify | Advertise the new tool in `CONTRACT_TOOL_DEFS` |
| `src/server/mcp/contract_tools/mod.rs` | modify | Register the tool handler |
| `src/server/sensors/http_client_sensor.rs` | modify | Recover the host from a template hole that resolves to an env var |
| `src/server/sensors/patterns/tsjs/fetch-outbound.scm` | modify | Reject captured strings that are not URL-shaped |
| `src/server/sensors/patterns/python/fastapi-add-api-route.scm` | create | Tree-sitter query for `app.add_api_route(...)` |
| `src/server/sensors/patterns/frameworks.yaml` | modify | Register the new pattern id |
| `src/server/sensors/http_sensor.rs` | modify | Next.js file-based API routes (`pages/api/**`) |
| `docs/REPOS_YAML.md` | modify | Document `provides:` and the runtime write path |
| `tests/federation_declared_providers_e2e.rs` | create | End-to-end tests for declared providers and the write path |
| `scripts/eval/nat-http-contract-smoke.sh` | create | Reproducible smoke run over the real NAT workspace |

---

### Task 1: Prerequisite — the indexing foundation from Plan A

This plan inherits Plan A's Task 1 (`docs/superpowers/plans/2026-10-29-openshell-rpc-contract.md`, Task 1): an independent per-repo index budget (`LAIN_INDEX_TIMEOUT`), concurrent repo indexing bounded by `max_concurrent_indexers`, and a barrier before the cross-repo glue pass.

Without it, indexing the NAT toolkit takes ~9 minutes of post-ingest work with no bound, and the three-repo federation does not complete — the smoke run in Task 7 cannot be trusted.

- [ ] **Step 1: Land Plan A Task 1 first**

Run: `git log --oneline -1` and confirm the commit `fix(index): per-repo index budget independent of startup budget; index repos concurrently` is an ancestor of `HEAD`.
Expected: the commit is present. If it is not, implement Plan A Task 1 before continuing.

- [ ] **Step 2: Verify the toolkit indexes within a bounded time**

Run: `LAIN_INDEX_TIMEOUT=600 ./target/release/lain server --config <nats-only repos.yaml> --transport http --port 9972 &` then poll `/health`.
Expected: the repo reaches `ready` and the log shows `fully indexed in <60s>`.

---

### Task 2: Stop non-URL strings from becoming routes

Eight of the eleven "routes" the UI produced are error messages and template fragments captured as paths. They are not merely noise: they are the visible face of a detector that accepts any string, and they make every contract report untrustworthy.

**Files:**
- Modify: `src/server/sensors/patterns/tsjs/fetch-outbound.scm`
- Test: `src/server/sensors/http_client_sensor.rs` (tests module)

**Interfaces:**
- Consumes: the pattern file format (validated at build time by `src/server/sensors/patterns/build.rs`).
- Produces: a `fetch`/`axios` call whose URL argument is a non-URL string is no longer captured as a call site.

- [ ] **Step 1: Write the failing test**

Add to the tests module of `src/server/sensors/http_client_sensor.rs`:

```rust
    /// An error message is not a URL. Capturing it as a path
    /// produced routes like "/ERROR: Invalid NEXT_INTERNAL_URL
    /// format:" in the contract view.
    #[test]
    fn fetch_with_a_non_url_string_is_not_a_call_site() {
        let src = "\
const res = await fetch('ERROR: Invalid NEXT_INTERNAL_URL format:');
";
        let calls = crate::server::sensors::http_client_sensor::detect_calls(
            std::path::Path::new("proxy.ts"),
            src,
            crate::server::sensors::http_client_sensor::Lang::TsJs,
            crate::server::sensors::patterns::Patterns::load_default(),
        );
        assert!(
            calls.iter().all(|c| !c.url.template.as_deref().unwrap_or("").contains("ERROR")),
            "an error message was captured as a path: {:?}",
            calls.iter().map(|c| &c.url.template).collect::<Vec<_>>()
        );
    }
```

> `detect_calls(path, content, lang, patterns)` is the module's pure entry point
> (`src/server/sensors/http_client_sensor.rs:208`); `Patterns::load_default()`
> returns the bundled pattern set (`src/server/sensors/patterns/mod.rs:736`).
> `HttpClientCall.url` is a `NormalizedUrl { host: HostPart, template:
> Option<String> }` — `None` means a dynamic path.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib http_client_sensor`
Expected: FAIL — the error string is captured.

- [ ] **Step 3: Tighten the pattern**

In `src/server/sensors/patterns/tsjs/fetch-outbound.scm`, add a predicate on the captured string. The current query captures the string content; add a guard that the value looks like a URL or a path:

```scheme
; A URL-ish argument starts with "/", "http://", "https://", or
; "ws://" — optionally followed by an interpolation. Anything else
; (an error message, a log line, a prose string) is not a call
; site.
((#match? @url "^([\"'`])(/|https?://|wss?://)")
 (#not-match? @url "^[\"'`]\\s*$"))
```

Use the query predicate syntax the existing `.scm` files already use (`#match?`, `#not-match?`, `#eq?`, `#not-eq?` — see `tsjs/fetch-outbound.scm` and `python/fastapi-route.scm`).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib http_client_sensor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/patterns/tsjs/fetch-outbound.scm src/server/sensors/http_client_sensor.rs
git commit -m "fix(sensors): a non-URL string is not an HTTP call site"
```

---

### Task 3: Recover the host when the URL is a template built from an environment variable

The UI's proxy dials `${UPSTREAM_HTTP_ORIGIN}` (derived from `process.env.NAT_BACKEND_URL`). The captured consumer has `host: null`, so the joiner cannot route it to a service — and with the template reduced to `/`, no binding is possible even by hand.

**Files:**
- Modify: `src/server/sensors/http_client_sensor.rs` (`parts_from_node_inner`, `template_from_string`, `identifier_parts`)
- Test: `src/server/sensors/http_client_sensor.rs` (tests module)

**Interfaces:**
- Consumes: `UrlPart` (`Literal` / `Hole`), `FileContext::resolve_identifier`, `crate::server::sensors::util::host_env_name`.
- Produces: a consumer whose URL template contains `process.env.NAT_BACKEND_URL` carries that env var as its host evidence, so `services[].env` can dispatch it.

- [ ] **Step 1: Write the failing test**

```rust
    /// The NAT UI proxy builds its target URL from an env var, but
    /// through two indirections: the fetch argument is a template
    /// whose first hole is a const, and that const is built from
    /// `process.env.NAT_BACKEND_URL`. The consumer must carry that
    /// env var as host evidence so the joiner can route it to the
    /// service that declares it.
    #[test]
    fn fetch_template_resolves_through_consts_to_the_env_var_host() {
        let src = "\
const NAT_BACKEND_URL = process.env.NAT_BACKEND_URL;
const upstream = new URL(NAT_BACKEND_URL);
const UPSTREAM_HTTP_ORIGIN = `${upstream.protocol}//${upstream.host}`;
const backendPath = '/chat/stream';
const targetUrl = `${UPSTREAM_HTTP_ORIGIN}${backendPath}`;
const res = await fetch(targetUrl, { method: 'POST' });
";
        let calls = crate::server::sensors::http_client_sensor::detect_calls(
            std::path::Path::new("proxy/server.js"),
            src,
            crate::server::sensors::http_client_sensor::Lang::TsJs,
            crate::server::sensors::patterns::Patterns::load_default(),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].url.host,
            lain::federation::contracts::model::HostPart::Env(vec!["NAT_BACKEND_URL".to_string()])
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib http_client_sensor`
Expected: FAIL — the host is `HostPart::Expr("UPSTREAM_HTTP_ORIGIN")`, not `HostPart::Env`.

- [ ] **Step 3: Implement the transitive resolution**

The env-host machinery already exists: `host_for` (`src/server/sensors/http_client_sensor.rs:1932`) collects the leading hole text and maps it through `host_env_name` (`src/server/sensors/util.rs:459`, which already knows `process.env.X`) to `HostPart::Env`. What is missing is the *indirection*: the leading hole is the identifier `UPSTREAM_HTTP_ORIGIN`, not the env expression itself.

Resolve the hole through same-file identifiers before giving up. Thread the file context into `host_for` and recurse:

```rust
/// Resolve the leading hole of a URL to a host. The hole may be an
/// env expression (`process.env.X` → `HostPart::Env`), or an
/// identifier that resolves — in the same file — to one
/// (`UPSTREAM_HTTP_ORIGIN` → a template built from
/// `upstream.protocol` / `upstream.host` → `new URL(NAT_BACKEND_URL)`
/// → `process.env.NAT_BACKEND_URL`).
///
/// Depth is bounded so a cyclic definition cannot spin.
fn resolve_host_hole(
    ctx: &FileContext,
    hole_text: &str,
    enclosing_fn_line: Option<u32>,
    depth: usize,
) -> Option<HostPart> {
    const MAX_DEPTH: usize = 4;
    if depth > MAX_DEPTH {
        return None;
    }
    let t = hole_text.trim();
    if let Some(name) = crate::server::sensors::util::host_env_name(t) {
        return Some(HostPart::Env(vec![name.to_string()]));
    }
    if !t.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let resolved = ctx.resolve_identifier(t, enclosing_fn_line)?;
    resolve_host_hole(ctx, &resolved, enclosing_fn_line, depth + 1)
}
```

A resolved identifier can itself be a template literal with holes (`upstream.protocol`), so the recursion must also expand a template's *first* hole. Keep the recursion on the leading hole text of the resolved expression:

```rust
    let resolved = ctx.resolve_identifier(t, enclosing_fn_line)?;
    // The resolved expression may itself be a template whose first
    // hole carries the host: `${upstream.protocol}//${upstream.host}`.
    let first_hole = template_from_string(&resolved)
        .into_iter()
        .find_map(|p| match p {
            UrlPart::Hole(h) => Some(h),
            UrlPart::Literal(_) => None,
        });
    match first_hole {
        Some(h) => resolve_host_hole(ctx, &h, enclosing_fn_line, depth + 1),
        None => resolve_host_hole(ctx, &resolved, enclosing_fn_line, depth + 1),
    }
```

Then `host_for` takes the context and uses the resolver:

```rust
fn host_for(parts: &[UrlPart], ctx: &FileContext, enclosing_fn_line: Option<u32>) -> HostPart {
    let mut hole_text = String::new();
    for part in parts {
        match part {
            UrlPart::Hole(h) => hole_text.push_str(h),
            UrlPart::Literal(_) => break,
        }
    }
    if hole_text.is_empty() {
        return HostPart::None;
    }
    resolve_host_hole(ctx, &hole_text, enclosing_fn_line, 0)
        .unwrap_or_else(|| HostPart::Expr(hole_text))
}
```

Update the two call sites (the framework-fallback call and the normal call) to pass `ctx` and `enclosing_fn_line`.

> The *path* of this URL stays dynamic — `backendPath` is whatever the client
> asked for. The honest verdict for this consumer is therefore "known target
> service, unknown route" (`no_route_in_service`), not a bind. That is the
> correct outcome for a transparent reverse proxy; see the plan's "Out of
> scope" section.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib http_client_sensor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/http_client_sensor.rs
git commit -m "feat(sensors): carry the env vars an HTTP URL depends on as host evidence"
```

---

### Task 4: Detect `app.add_api_route` providers

The NAT server registers every route this way. Even with a dynamic path, the *call shape* is worth detecting: a route whose path is a literal is a first-class provider, and a route whose path is a variable is a provider whose path must be supplied by configuration (Task 5).

**Files:**
- Create: `src/server/sensors/patterns/python/fastapi-add-api-route.scm`
- Modify: `src/server/sensors/patterns/frameworks.yaml`
- Modify: `src/server/sensors/http_sensor.rs` (route extraction for the new pattern)
- Test: `src/server/sensors/http_sensor.rs` (tests module) and `tests/` e2e

**Interfaces:**
- Consumes: the pattern registry (`Patterns` in `src/server/sensors/patterns/mod.rs`), the route-pattern mechanism the `django-route` entry uses (a YAML entry plus an `.scm` query — see the comment at `frameworks.yaml:44-60`).
- Produces: a `route` pattern id `fastapi-add-api-route` that emits an HTTP route fact for `app.add_api_route("/literal", …)`.

- [ ] **Step 1: Write the failing test**

```rust
    /// FastAPI's non-decorator registration form.
    #[test]
    fn fastapi_add_api_route_with_a_literal_path_is_a_route() {
        let src = "\
from fastapi import FastAPI
app = FastAPI()

def add_routes(app):
    app.add_api_route(
        path=\"/health\",
        endpoint=handler,
        methods=[\"GET\"],
    )
";
        let routes = detect_http_routes(src, "py", "routes.py");
        assert!(
            routes.iter().any(|r| r.path == "/health"
                && r.method == HttpMethod::Get),
            "expected GET /health, got {:?}",
            routes
        );
    }
```

> `HttpRoute` (`src/server/sensors/http_sensor.rs:76`) is
> `{ method: HttpMethod, path: String, handler_path: String, handler_name: String,
> line: u32 }` — the raw pre-normalization path is `path`, and `HttpMethod`
> lives in `src/server/federation/contracts/model.rs:574`. Adapt
> `detect_http_routes` to the module's real detection entry point.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib http_sensor`
Expected: FAIL — no route is detected.

- [ ] **Step 3: Add the pattern**

Create `src/server/sensors/patterns/python/fastapi-add-api-route.scm`:

```scheme
; FastAPI's non-decorator registration form:
;   app.add_api_route(path="/health", endpoint=handler, methods=["GET"])
; Captures the path literal and the method list.
(call
  function: (attribute
    attribute: (identifier) @verb)
  arguments: (argument_list
    .
    (keyword_argument
      name: (identifier) @_path_kw
      value: (string
        (string_content) @path))
    .
    (keyword_argument
      name: (identifier) @_methods_kw
      value: (list
        (string
          (string_content) @method))))
  (#eq? @verb "add_api_route")
  (#match? @path "^/"))
```

Register it in `src/server/sensors/patterns/frameworks.yaml` under `languages.python`:

```yaml
    # FastAPI's non-decorator registration. `path=` is often a
    # runtime config value; when it is a literal the route is a
    # provider; when it is a variable the route needs a declared
    # path (see `repos.yaml#provides`).
    - id: fastapi-add-api-route
      kind: route
      verbs: [get, post, put, delete, patch, head, options]
      path_regex: '\badd_api_route\s*\(\s*path\s*=\s*["''](/[^"'']*)["'']'
      handler_regex: 'endpoint\s*=\s*(\w+)'
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib http_sensor`
Expected: PASS. (The build script validates every `.scm` at compile time — a malformed query fails `cargo build`, which is the intended feedback loop.)

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/patterns/python/fastapi-add-api-route.scm \
        src/server/sensors/patterns/frameworks.yaml src/server/sensors/http_sensor.rs
git commit -m "feat(sensors): detect fastapi add_api_route providers"
```

---

### Task 5: Declare providers the analyzer cannot see — `repos.yaml#provides`

This is the structural fix. The NAT server's paths live in an operator's YAML config; a static analyzer will never read them. So the operator (or an agent) states them, and the contract layer treats the declaration exactly like a detected provider.

The key grammar is **reused**, not extended: `http:POST /chat/stream`, `ws:/websocket`, `topic:kafka/orders.created`, `rpc:grpc:openshell.v1.OpenShell:Health` are the same strings `check_binding` already accepts, so a declaration and a hand-written binding speak one language.

**Files:**
- Modify: `src/server/federation/contracts/config.rs` (`ContractFederationConfig`, `validate`)
- Modify: `src/server/federation/contracts/joiner/endpoints.rs` (`build_endpoints`)
- Modify: `docs/REPOS_YAML.md`
- Test: `tests/federation_declared_providers_e2e.rs` (create)

**Interfaces:**
- Consumes: `ContractKey` parsing (the grammar the config validation already uses), `EndpointProviderRecord` (`joiner/endpoints.rs`), `GlobalId::new(repo, node_type, path, name, line_start)`.
- Produces:
  - `pub struct ProviderDecl { pub service: String, pub key: String, pub source: Option<String> }`
  - `ContractFederationConfig.provides: Vec<ProviderDecl>`
  - `build_endpoints` emits one endpoint record per declaration, with the provider id `repo:HttpRoute:repos.yaml#provides:<key>:0` and the path shown to the operator as `repos.yaml#provides[<i>]`.

- [ ] **Step 1: Write the failing test**

Create `tests/federation_declared_providers_e2e.rs`:

```rust
//! A provider the analyzer cannot see (a runtime-configured route)
//! can be declared; the declaration must land in the endpoint table
//! and be bindable by a consumer.

use lain::federation::contracts::config::{ContractFederationConfig, ProviderDecl, ServiceDecl};
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use std::sync::Arc;

#[tokio::test]
async fn declared_provider_lands_in_the_endpoint_table() {
    let root = tempfile::tempdir().unwrap();
    let repo_dir = root.path().join("nat");
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(repo_dir.join("src/.keep"), b"").unwrap();

    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));

    let repo_id = RepoId::new("nat").unwrap();
    let source: Box<dyn RepoSource> = Box::new(WorkspaceDirSource::with_config(
        repo_id.clone(),
        repo_dir.clone(),
        lain::federation::config::SourceConfig::WorkspaceDir { path: repo_dir.clone() },
    ));
    fed.add_repo(source).await.unwrap();

    fed.set_contract_config(ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "nat".into(),
            repo: "nat".into(),
            paths: vec![],
            hosts: vec!["localhost".into()],
            env: vec![],
            base_path: None,
            route_prefixes: vec![],
            provides_graphql: true,
        }],
        provides: vec![ProviderDecl {
            service: "nat".into(),
            key: "http:POST /chat/stream".into(),
            source: Some("nat config.yaml".into()),
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    });

    fed.rejoin_contracts_if_dirty().unwrap();
    let index = fed.contract_index().expect("contract index");
    // `ContractIndex.endpoints` maps an `EndpointId` to an
    // `Endpoint { id, method, template, providers, schemas }`.
    let declared = index
        .endpoints
        .values()
        .any(|e| e.template == "/chat/stream");
    assert!(declared, "the declared provider is missing from the endpoint table");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test federation_declared_providers_e2e`
Expected: FAIL — `ProviderDecl` does not exist.

- [ ] **Step 3: Add the config section**

In `src/server/federation/contracts/config.rs`:

```rust
/// A provider the analyzer cannot see. The service's API surface
/// often lives in a runtime configuration file (FastAPI's
/// `add_api_route(path=…)`, a gateway route table, a service mesh
/// spec); the operator states it and the contract layer treats it
/// like a detected provider.
///
/// `key` uses the same grammar `check_binding` accepts:
/// `http:<METHOD> <template>`, `graphql:<op>:<field>`,
/// `topic:<broker>:<name>`, `rpc:<system>:<service>:<method>`,
/// `ws:<route>`, `table:<name>`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderDecl {
    /// A service declared in `services`.
    pub service: String,
    /// The endpoint key, in the shared key grammar.
    pub key: String,
    /// Where the declaration came from (a config file path, a URL,
    /// a ticket). Shown to the operator; free-form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
```

Add the field to `ContractFederationConfig` with `#[serde(default)]`, and to `validate`:

```rust
    // Rule: every declared provider names a declared service and a
    // key in the shared grammar. A provider attached to a service
    // nobody can consume against is a silent no-op that reads as
    // "no contract", so it is rejected at load time.
    for (i, p) in self.provides.iter().enumerate() {
        if !known_services.contains(p.service.as_str()) {
            return Err(LainError::Config(format!(
                "provides[{i}]: unknown service '{service}'",
                service = p.service
            )));
        }
        if parse_contract_key(&p.key).is_none() {
            return Err(LainError::Config(format!(
                "provides[{i}]: malformed key '{key}'",
                key = p.key
            )));
        }
    }
```

> `validate` returns `Result<(), LainError>` and reports failures as
> `LainError::Config(String)` (`src/server/federation/contracts/config.rs:271`) —
> there is no separate error enum. Reuse the config module's existing key-parsing
> helper: the `bindings` validation already parses the same grammars
> (`http:…`, `graphql:…`, `topic:…`, `rpc:…`, `ws:…`, `table:…`).

- [ ] **Step 4: Inject the declarations into the endpoint table**

In `src/server/federation/contracts/joiner/endpoints.rs::build_endpoints`, after the node loop:

```rust
    // Declared providers: the surface the analyzer could not see.
    // They enter the endpoint table exactly like detected providers,
    // so bindings, coverage and the dashboard treat them the same.
    for (i, decl) in config.provides.iter().enumerate() {
        let Some(key) = parse_contract_key(&decl.key) else {
            continue; // validation rejects this before we get here
        };
        let Some(service_decl) = config.services.iter().find(|s| s.name == decl.service) else {
            continue; // validation rejects this before we get here
        };
        let Ok(repo_id) = RepoId::new(&service_decl.repo) else { continue };
        let svc = ServiceName(decl.service.clone());
        let (template, method) = template_and_method_for(&key);
        let id = GlobalId::new(
            &repo_id,
            crate::schema::NodeType::HttpRoute,
            &format!("repos.yaml#provides[{i}]"),
            &decl.key,
            Some(i as u32),
        );
        table
            .entry((svc, key))
            .or_default()
            .push(EndpointProviderRecord {
                id,
                fact: None,
                template,
                method,
            });
    }
```

> Real signatures: `GlobalId::new(repo: &RepoId, kind: NodeType, path: &str, name: &str,
> line_start: Option<u32>)` (`src/server/federation/repo_id.rs:98`) and
> `EndpointProviderRecord { id: GlobalId, fact: Option<ContractFact>, template: String,
> method: HttpMethod }` (`joiner/endpoints.rs:34`). `template_and_method_for` is a new
> helper that maps a `ContractKey` to the endpoint's display template and method —
> for `ContractKey::Http { method, template }` it returns the template and the
> method; for the other kinds it returns the key's identifying string and
> `HttpMethod::Any`, matching how the detected-provider arms above populate the
> record.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --test federation_declared_providers_e2e`
Expected: PASS.

- [ ] **Step 6: Document the section**

Add to `docs/REPOS_YAML.md`:

```yaml
provides:
  # The NAT server registers its routes from a runtime config file,
  # so the paths are not in the source. State them here.
  - service: nat
    key: "http:POST /chat/stream"
    source: "nat config.yaml (general.endpoint.path)"
  - service: nat
    key: "ws:/websocket"
    source: "nat config.yaml (general.endpoint.websocket_path)"
```

And a paragraph explaining that the key grammar is the same one `check_binding` accepts, so a declaration and a hand-written binding are interchangeable.

- [ ] **Step 7: Commit**

```bash
git add src/server/federation/contracts/config.rs \
        src/server/federation/contracts/joiner/endpoints.rs \
        docs/REPOS_YAML.md tests/federation_declared_providers_e2e.rs
git commit -m "feat(contracts): declare providers the analyzer cannot see"
```

---

### Task 6: Inject contract context at runtime — the `apply_contract_context` tool

The operator's file is already hot-reloaded: the watcher on `repos.yaml` fires, `run_rebuild` re-reads the file, `set_contract_config` applies it and `rejoin_contracts_if_dirty` re-runs the joiner (`src/server/reload.rs:246-270`). So the *reload* half exists. What is missing is a write half an agent can call: today `check_binding` emits YAML text for a human to paste.

This task adds the write path, so an agent can declare "service nat exposes POST /chat/stream" and immediately get a contract to trace consumers against.

**Files:**
- Create: `src/server/mcp/contract_tools/context.rs`
- Create: `src/server/mcp/contract_tools/schemas/apply_contract_context.in.json`
- Create: `src/server/mcp/contract_tools/schemas/apply_contract_context.out.json`
- Modify: `src/server/mcp/definitions.rs` (`CONTRACT_TOOL_DEFS`)
- Modify: `src/server/mcp/contract_tools/mod.rs` (registration)
- Test: `tests/federation_declared_providers_e2e.rs` (extend) and `src/server/mcp/contract_tools/context.rs` (unit tests)

**Interfaces:**
- Consumes: `crate::cli::io::write_file_atomic` (used by `src/cli/repos.rs:86`), `crate::server::federation::contracts::config::ContractFederationConfig::load_from_str` + `validate`, the reload bus (`request_reload`'s path in `src/server/mcp/federation_tools/`), `ContractToolEntry` registration.
- Produces:
  - MCP tool `apply_contract_context` with arguments `{ snapshot, provides: [{service, key, source}], bindings: [...], dry_run: bool }`.
  - `pub fn merge_context(current: &ContractFederationConfig, incoming: &ContextPatch) -> ContractFederationConfig` — pure, unit-testable.
  - `pub fn apply_context_to_file(path: &Path, cfg: &ContractFederationConfig) -> Result<(), LainError>` — validate, back up, write atomically.

- [ ] **Step 1: Write the failing unit test for the merge**

In the new `src/server/mcp/contract_tools/context.rs` file's tests module:

```rust
    #[test]
    fn merge_appends_a_provider_without_dropping_the_existing_ones() {
        let current = ContractFederationConfig {
            services: vec![svc("nat")],
            provides: vec![ProviderDecl {
                service: "nat".into(),
                key: "http:POST /chat".into(),
                source: None,
            }],
            ..ContractFederationConfig::default()
        };
        let patch = ContextPatch {
            provides: vec![ProviderDecl {
                service: "nat".into(),
                key: "http:POST /chat/stream".into(),
                source: Some("agent".into()),
            }],
            bindings: vec![],
        };
        let merged = merge_context(&current, &patch);
        assert_eq!(merged.provides.len(), 2);
    }

    #[test]
    fn merge_is_idempotent_for_an_identical_patch() {
        // Applying the same declaration twice must not duplicate it.
        let current = ContractFederationConfig { services: vec![svc("nat")], ..Default::default() };
        let patch = ContextPatch {
            provides: vec![ProviderDecl {
                service: "nat".into(),
                key: "http:POST /chat/stream".into(),
                source: None,
            }],
            bindings: vec![],
        };
        let once = merge_context(&current, &patch);
        let twice = merge_context(&once, &patch);
        assert_eq!(once.provides.len(), 1);
        assert_eq!(twice.provides.len(), 1);
    }
```

> `ContractFederationConfig` does not derive `Default` today — add `#[derive(Default)]` (every field is already `#[serde(default)]`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib contract_tools::context`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement the merge and the write**

```rust
//! Runtime injection of contract context: an operator or an agent
//! declares the providers and bindings the analyzer cannot see, and
//! the federation picks them up without a restart.

use crate::federation::contracts::config::{ContractFederationConfig, ProviderDecl};

/// The patch an agent may apply.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ContextPatch {
    #[serde(default)]
    pub provides: Vec<ProviderDecl>,
    #[serde(default)]
    pub bindings: Vec<crate::federation::contracts::config::ConfirmedBinding>,
}

/// Merge a patch into the current config. Idempotent: a declaration
/// that is already present (same service, same key) is not added
/// twice.
pub fn merge_context(
    current: &ContractFederationConfig,
    patch: &ContextPatch,
) -> ContractFederationConfig {
    let mut out = current.clone();
    for p in &patch.provides {
        if !out.provides.iter().any(|e| e.service == p.service && e.key == p.key) {
            out.provides.push(p.clone());
        }
    }
    for b in &patch.bindings {
        if !out.bindings.iter().any(|e| {
            e.consumer.repo == b.consumer.repo
                && e.consumer.path == b.consumer.path
                && e.consumer.symbol == b.consumer.symbol
                && e.consumer.key == b.consumer.key
        }) {
            out.bindings.push(b.clone());
        }
    }
    out
}

/// Write the config atomically, after validation, keeping a backup.
///
/// The write is the only side effect this tool has on the
/// operator's machine, so it is deliberately narrow: validate, back
/// up, write, and report exactly what changed.
pub fn apply_context_to_file(
    path: &std::path::Path,
    cfg: &ContractFederationConfig,
    repo_ids: &[String],
) -> Result<(), crate::error::LainError> {
    cfg.validate(repo_ids).map_err(|e| {
        crate::error::LainError::Config(format!("apply_contract_context: {e}"))
    })?;
    let yaml = serde_yaml::to_string(cfg)
        .map_err(|e| crate::error::LainError::Config(format!("serialize config: {e}")))?;
    // Backup first: a rejected or corrupted write must be recoverable.
    if path.exists() {
        let backup = path.with_extension("yaml.bak");
        std::fs::copy(path, &backup).map_err(|e| {
            crate::error::LainError::Config(format!("backup config: {e}"))
        })?;
    }
    crate::cli::io::write_file_atomic(path, yaml.as_bytes())
        .map_err(|e| crate::error::LainError::Config(format!("write config: {e}")))
}
```

> `ContractFederationConfig` is only the *contract* half of `repos.yaml`; the file also carries `repos:` and `services:` at the top level. Write the merged contract sections back into the existing file rather than serializing the contract config alone — read the whole file, replace the contract sections, and write the whole document. Adjust `apply_context_to_file` accordingly; the sketch shows the shape, not the final serialization.

- [ ] **Step 4: Write the tool handler**

```rust
inventory::submit!(ContractToolEntry {
    name: "apply_contract_context",
    handler: apply_contract_context_handle,
});

/// `apply_contract_context` — declare providers and bindings at
/// runtime, then report the contract view that results.
///
/// `dry_run` defaults to true: the tool returns the YAML it would
/// write and touches nothing. With `dry_run: false` it writes
/// `repos.yaml` atomically (after validation, with a `.bak`) and
/// waits for the federation to re-join.
pub fn apply_contract_context_handle<'a>(
    ctx: &'a McpContext<'a>,
    args: serde_json::Value,
) -> ContractToolFuture<'a> {
    let args_map = object_or_empty(args);
    Box::pin(async move {
        let dry_run = args_map
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let patch: ContextPatch = match serde_json::from_value(
            args_map.get("context").cloned().unwrap_or(serde_json::json!({})),
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(error_outcome(
                    "invalid_argument",
                    "context patch does not parse",
                    Some(serde_json::json!({ "detail": e.to_string() })),
                    &snapshot_label(&args_map),
                    Instant::now(),
                ));
            }
        };
        // … read the live config, merge, validate, write, reload …
        Ok(success_envelope_with_view(/* the new list_contracts view */))
    })
}
```

The handler needs three things from the context: the path of the `repos.yaml` it is serving (the same path the config watcher watches), the live `ContractFederationConfig`, and the reload trigger. Follow `request_reload` (`src/server/mcp/federation_tools/federation.rs`) for the reload trigger and `spawn_config_watcher` (`src/server/watcher.rs`) for the config path.

- [ ] **Step 5: Advertise the tool**

In `src/server/mcp/definitions.rs::CONTRACT_TOOL_DEFS`, add:

```rust
    ToolDef {
        name: "apply_contract_context",
        description: "Declare contract context the analyzer cannot see — providers (a runtime-configured route, a mesh route) and confirmed bindings — and apply it to repos.yaml. Dry-run by default: returns the YAML that would be written. With dry_run=false it writes atomically and waits for the federation to re-join.",
        required_args: &["snapshot"],
        optional_args: &["context", "dry_run"],
        input_schema: Some(include_str!("contract_tools/schemas/apply_contract_context.in.json")),
        output_schema: Some(include_str!("contract_tools/schemas/apply_contract_context.out.json")),
    },
```

Create the two schema files with the shapes the handler accepts and returns.

- [ ] **Step 6: Regenerate the tool schema dump**

Run: `make schema`
Expected: `docs/tool-schema.json` changes and includes `apply_contract_context`. Commit it.

- [ ] **Step 7: Run the tests**

Run: `cargo test --lib contract_tools && cargo test --test federation_declared_providers_e2e && cargo test --test schema_dump_smoke`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/server/mcp/contract_tools/context.rs \
        src/server/mcp/contract_tools/schemas/apply_contract_context.in.json \
        src/server/mcp/contract_tools/schemas/apply_contract_context.out.json \
        src/server/mcp/definitions.rs src/server/mcp/contract_tools/mod.rs \
        src/server/federation/contracts/config.rs docs/tool-schema.json
git commit -m "feat(contracts): apply_contract_context injects providers and bindings at runtime"
```

---

### Task 7: Next.js file-based API routes

The UI's own API surface lives in file paths (`pages/api/mcp/clients.ts` → `/api/mcp/clients`), not in decorators. Without this, the UI's providers are only the eight garbage strings from Task 2, and the intra-repo contract `utils/api/mcpClient.ts → /api/mcp/clients` cannot bind.

**Files:**
- Modify: `src/server/sensors/http_sensor.rs` (new file-route extraction for Next.js)
- Test: `src/server/sensors/http_sensor.rs` (tests module)

**Interfaces:**
- Consumes: the repo root the sensor walks, the HTTP route fact (`ContractFact::Provider`).
- Produces: one route fact per `pages/api/**` file, with the template derived from the path and the method `ANY` (Next.js handlers decide the method from the request).

- [ ] **Step 1: Write the failing test**

```rust
    /// Next.js file-based API routes: pages/api/mcp/clients.ts
    /// serves /api/mcp/clients.
    #[test]
    fn nextjs_pages_api_files_are_routes() {
        let routes = detect_nextjs_file_routes("pages/api/mcp/clients.ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/api/mcp/clients");
        assert_eq!(routes[0].method, HttpMethod::Any);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib http_sensor`
Expected: FAIL.

- [ ] **Step 3: Implement the extraction**

```rust
/// Next.js file-based API routes: `pages/api/x/y.ts` serves
/// `/api/x/y`; `pages/api/x/y/index.ts` serves `/api/x/y`. The
/// `pages/` root is the app root, so the path under `pages/` is the
/// route.
fn detect_nextjs_file_routes(rel_path: &str) -> Vec<HttpRoute> {
    let mut parts: Vec<&str> = rel_path.split('/').collect();
    if parts.first() != Some(&"pages") {
        return Vec::new();
    }
    parts.remove(0);
    if parts.first() != Some(&"api") {
        return Vec::new();
    }
    parts.remove(0);
    let last = parts.pop().unwrap_or_default();
    let stem = last.split('.').next().unwrap_or_default();
    if !matches!(stem, "ts" | "tsx" | "js" | "jsx") {
        return Vec::new();
    }
    ...
}
```

Filter to the API subtree only; `pages/index.tsx` is a page, not an endpoint.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib http_sensor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/http_sensor.rs
git commit -m "feat(sensors): nextjs pages/api files are HTTP routes"
```

---

### Task 8: Reproducible smoke run over the real NAT workspace

**Files:**
- Create: `scripts/eval/nat-http-contract-smoke.sh`
- Modify: `docs/protocol-coverage.md` (a "measured on real repositories" note under §http)

**Interfaces:**
- Consumes: `lain server --config <repos.yaml> --transport http --port <port>`, the `contracts` MCP tools over `POST /mcp`.
- Produces: a script that prints a pass/fail line per expectation and exits non-zero on failure.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Smoke: the NeMo Agent Toolkit workspace must show real HTTP
# contracts — declared providers, no garbage routes, and a consumer
# that resolves to the right service.
#
# Usage: scripts/eval/nat-http-contract-smoke.sh <workspace-dir> [port]
set -euo pipefail

LAIN_BIN="${LAIN_BIN:-./target/release/lain}"
WS="${1:?usage: $0 <workspace-dir> [port]}"
PORT="${2:-9972}"
WORK="$(mktemp -d)"
trap 'kill "${SERVER_PID:-0}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/.lain/federation
max_concurrent_indexers: 3
repos:
  - id: nat
    source:
      type: workspace_dir
      path: $WS/NeMo-Agent-Toolkit
  - id: nat-ui
    source:
      type: workspace_dir
      path: $WS/NeMo-Agent-Toolkit-UI
  - id: nat-examples
    source:
      type: workspace_dir
      path: $WS/NeMo-Agent-Toolkit-Examples
services:
  - name: nat
    repo: nat
    hosts: [localhost, 127.0.0.1]
    env: [NAT_BACKEND_URL, NAT_SERVER_URL, NAT_URL]
  - name: nat-ui
    repo: nat-ui
  - name: nat-examples
    repo: nat-examples
provides:
  - service: nat
    key: "http:POST /chat/stream"
    source: "nat config.yaml"
  - service: nat
    key: "http:POST /chat"
    source: "nat config.yaml"
  - service: nat
    key: "ws:/websocket"
    source: "nat config.yaml"
EOF

"$LAIN_BIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" \
  > "$WORK/server.log" 2>&1 &
SERVER_PID=$!

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' -H 'Accept: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}

for _ in $(seq 1 240); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/health" && break
  sleep 1
done

echo "--- declared providers present ---"
call list_contracts '{"snapshot":"live","kind":"http","limit":200}' \
  | python3 -c '
import json,sys
items=json.load(sys.stdin).get("data",{}).get("items",[])
keys={i["endpoint"]["key"] for i in items}
for want in ("http:POST /chat/stream","http:POST /chat"):
    print(("OK   " if want in keys else "MISS "), want)
'

echo "--- no garbage routes ---"
call list_contracts '{"snapshot":"live","kind":"http","limit":200}' \
  | python3 -c '
import json,sys
items=json.load(sys.stdin).get("data",{}).get("items",[])
bad=[i["endpoint"]["key"] for i in items if "ERROR:" in i["endpoint"]["key"] or "Failed to" in i["endpoint"]["key"]]
print("garbage routes:", bad or "none")
sys.exit(1 if bad else 0)'
```

- [ ] **Step 2: Run it against the real workspace**

Run: `bash scripts/eval/nat-http-contract-smoke.sh /data/agents/tmp-lain-tests/nvidia-eval 9972`
Expected: both declared providers present, no garbage routes.

- [ ] **Step 3: Record the numbers in the docs**

Add a "Measured" paragraph to `docs/protocol-coverage.md` §http with the counts the script printed, the Lain commit, and the date.

- [ ] **Step 4: Commit**

```bash
git add scripts/eval/nat-http-contract-smoke.sh docs/protocol-coverage.md
git commit -m "test(eval): nat http contract smoke run over the real NVIDIA workspace"
```

---

## Out of scope

- **Reading the operator's NAT config file automatically.** A config-aware sensor would be repo-specific and would silently couple Lain to one vendor's file layout. The `provides` section makes the declaration explicit and works for any framework whose routes are configured at runtime.
- **A general-purpose dynamic-path join.** The UI's proxy is a transparent reverse proxy: its URL path is whatever the client asked for. The honest verdict for it is "known target service, unknown route" (`no_route_in_service`), not a bind. Making that verdict reachable needs Task 3's host recovery; making it *bind* would need a wildcard route concept, which is a design change and out of scope here.
- **The Rust provider in `NeMo-Fabric`** — that is Plan A's surface, not this one.
