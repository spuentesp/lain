# LAIN: GraphQL Config Bridge — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the LAIN engine gaps that keep a real GraphQL consumer→provider edge (storefront → saleor-api) from resolving: scope the GraphQL provider surface per service, and make `bindings:` / `check_binding` work for GraphQL consumers.

**Architecture:** Two engine gaps, both verified against the code and a live probe. (1) Provider-surface pollution: every `.graphql`/`.gql` SDL file mints provider endpoints for its service, so repos that only carry schema copies (codegen inputs) duplicate every `(op, field)` key and push consumers to `graphql_no_op`; a new `services[].provides_graphql: false` knob scopes it. (2) Explicit bindings are HTTP-only at three choke points (key grammar, `confirmed.rs` matching, `check_binding` gate); all three gain a GraphQL arm. The consuming software (Parallax) side is out of this repo — its requirements are in `docs/superpowers/plans/2026-10-14-parallax-requirements.md`.

**Tech Stack:** Rust (existing `src/server/federation/contracts`, `src/server/mcp/contract_tools`, tree-sitter sensors).

**Spec:** `docs/REPOS_YAML.md` (services/bindings schema) and `docs/superpowers/plans/2026-10-14-parallax-requirements.md` (the integration contract this engine work must satisfy). Diagnosis evidence: §Diagnóstico below.

## Global Constraints

- Open PRs against `dev`, never `main` (`AGENTS.md`). No version bumps in feature PRs.
- Every user-visible behavior change gets a `CHANGELOG.md` entry naming the recovery/migration path if any.
- Tool output schema changes require editing `src/server/mcp/contract_tools/schemas/check_binding.out.json` and regenerating `docs/tool-schema.json` (`lain schema` / `make schema`); `tests/contracts_golden.rs` and the CI `schema-drift` job must stay green.
- Do NOT bump `FEDERATION_GRAPH_VERSION` — these changes are additive; the on-disk graph format is unchanged.
- `cargo test` (workspace) green before each commit; claim files you edit via `hooks/<agent>/` if contention appears.
- Commit subjects in English, matching `git log`.
- Do not modify the Parallax repo or its `test-repos/` checkouts. They are read-only inputs for the manual verification recipe in Task 6 at most.

## Diagnóstico (evidence this plan argues from)

Verified on disk at `/data/agents/orca/parallax/test-repos/` (read-only) and in LAIN source:

1. **Storefront operations exist as full-text `.graphql` documents.** `storefront/src/graphql/*.graphql` has 47 files (43 git-tracked; `storefront/.gitignore:5` only ignores `src/checkout/graphql/generated/`) plus multi-operation files under `src/checkout/graphql/` (`checkout.graphql`, `user.graphql`, `payment.graphql`, `order.graphql`). ~32 of the `src/graphql/` files are full operations (e.g. `ProductDetails.graphql`: `query ProductDetails($slug: String!) { product(slug: …) { … } }`); the rest are fragment-only (e.g. `OrderDetailsFragment.graphql`). No `${` interpolation in any of them.
2. **LAIN already has a `.graphql` consumer sensor.** `graphql_consumer_sensor.rs:110-121` scans `*.graphql|*.gql` and mints one consumer node per top-level field per operation, named `graphql-call:{op}:{field}` (`src/server/sensors/util.rs:523`), carrying `ContractFact::GraphqlConsumer { op, field }`. Fragment-only documents are marked `dynamic: true` and mint **nothing** (and leave no ledger record — `UnresolvedReason` has no `DynamicOperation` variant).
3. **Schema copies pollute the provider key space.** `saleor/saleor/graphql/schema.graphql` (the real API surface) plus FOUR SDL copies in the dashboard repo: `schema.graphql`, `schema-main.graphql`, `schema-staging.graphql`, `schema-directives.graphql`. `graphql_provider_sensor` mints `GraphqlProvider` facts for every SDL file it finds; `assign_services` (`src/server/federation/contracts/joiner/mod.rs:623-668`) attributes them to the service whose `paths` prefix matches — and a service with empty `paths` matches its whole repo (prefix 0). In the current Saleor config, `saleor-dashboard` has no `paths`, so its 4 SDL copies register as provider endpoints of service `saleor-dashboard`.
4. **`graphql_no_op` means "0 or ≥2 providers for `(op, field)`"**, not "fragment without operation". `resolve_by_key` (`consumer_protocol.rs:98-120`) skips the consumer's own service, then binds only when exactly one candidate remains; `n != 1` → `Unresolved { reason: GraphqlNoOp }`. Consequences on the Saleor config:
   - dashboard consumers: own SDL copies are skipped → only `saleor-api` remains → resolves, except fields missing from the pinned API SDL (version skew against `schema-staging.graphql`) → `graphql_no_op` (the 500-item bucket);
   - storefront consumers: both `saleor-api` and `saleor-dashboard` provide the same `(op, field)` keys → `n = 2` → `graphql_no_op`. The arrow cannot appear even though detection works, unless the provider surface is scoped.
5. **EMPIRICAL PROBE (2026-10-14, `lain` debug build, single-repo workspace over `test-repos/storefront`):** a fresh `lain reindex` + `list_unresolved` returns 78 items — **63 of them GraphQL consumers minted from `.graphql` files** (e.g. `storefront:Function:src/checkout/graphql/checkout.graphql:graphql-call:mutation:checkoutLinesUpdate:205`; consumer paths include `src/checkout/graphql/checkout.graphql` (19), `src/checkout/graphql/user.graphql` (7), `src/graphql/AccountAddressCreate.graphql` (1)), plus 3 `env_unmapped` and 12 `no_match`. **The storefront is visible to LAIN.** The "ítems con storefront: 0" observation in the field report was a pagination artifact: `list_unresolved` pages at `limit` (default 100, max 1000 — `contracts.rs:709-731`) and the aggregated breakdown of one 500-item page only showed the dashboard bucket. Reproduce with `list_unresolved { "repo": "storefront", "limit": 1000 }`.
6. **The "155 candidates" figure is an artifact.** `candidate_endpoints_for` (`contracts.rs:1014-1062`) parses a consumer node's name as `"<METHOD> <template>"`; a `graphql-call:query:x` name has no whitespace, so every HTTP endpoint in the index is listed as a "candidate". It says nothing about GraphQL providers.
7. **`bindings:` cannot express GraphQL today.** `confirmed.rs:70` matches only `ContractFact::Consumer` (HTTP). `parse_contract_key` (`joiner/mod.rs:784-791`) prefixes `http:` to any key without `http:`/`topic:`, so `graphql:query:x` becomes `http:graphql:query:x` → error. `validate_key` (`contracts/config.rs:587-614`) requires a space (`"missing space"`). `run_check_binding` (`contracts.rs:616-627`) hard-rejects non-HTTP keys ("topic endpoints not supported in 0.9").
8. **`check_binding` DOES emit YAML.** `render_bindings_entry` (`contracts.rs:1134-1147`) returns `bindings_entry` when `valid: true`; `check_binding.out.json` documents it. (Consequence for the consuming side — see the requirements doc: its mirrored response type is missing this field.)
9. **What LAIN needs from Parallax** (scan for invisible patterns, propose validated `repos.yaml` entries, paginate `list_unresolved`) is specified as a contract in `docs/superpowers/plans/2026-10-14-parallax-requirements.md`. No Parallax implementation is planned in this document.
10. **`env: [NEXT_PUBLIC_SALEOR_API_URL]` was necessary but not sufficient** for the HTTP path: `services[].env` is only consulted when a consumer fact with `HostPart::Env` exists. For the GraphQL edge it is not consulted at all — the edge comes from document text, not from the HTTP call shape (`fetchWithTimeout` opacity is therefore out of scope for this plan; see Out of Scope).

## Review Focus

1. **SDL copies in consumer repos** (every field of the API schema is provided by ≥2 services) — a reasonable person expects one provider per field; without Task 2 the storefront stays `graphql_no_op` no matter what else is fixed. Pinned by Task 2 test `provides_graphql_false_shrinks_provider_key_space`.
2. **Fragment-only `.graphql` files are invisible** (no node, no ledger record) — the engine must keep that behavior explicitly pinned so a future "improvement" does not silently change it. Pinned by Task 1 test `fragment_only_graphql_file_mints_no_consumer`.
3. **`check_binding` with a GraphQL consumer and an HTTP endpoint key** must return a clean reason (`op_mismatch`/`field_mismatch`), not a panic and not a misleading `template_mismatch`. Pinned by Task 5 test `pr14_check_binding_rejects_key_kind_mismatch`.
4. **Stale GraphQL bindings** (consumer node renamed/removed after the YAML was written) must land in `stale_bindings` with `NoConsumer`, never a silent drop. Pinned by Task 4 test `confirmed_graphql_binding_no_consumer_marks_stale`.
5. **`provides_graphql` default must preserve prior behavior** — configs that do not know the field must load and resolve exactly as before. Pinned by Task 2 tests `parses_provides_graphql_default_true` / `parses_provides_graphql_false`.
6. **Round-trip: the YAML `check_binding` emits must actually work** — fed back as `repos.yaml#bindings`, it must pass `validate_key` and match the consumer node (same grammar on both sides is where a drift would hide). Pinned by Task 6 test `check_binding_yaml_round_trip_binds_graphql_consumer`.

## File Structure

- `src/server/federation/contracts/config.rs` — `ServiceDecl.provides_graphql`, binding key grammar (`validate_key`).
- `src/server/federation/contracts/joiner/endpoints.rs` — provider-surface filter in `build_endpoints`.
- `src/server/federation/contracts/joiner/mod.rs` — `parse_contract_key` accepts `graphql:`.
- `src/server/federation/contracts/joiner/confirmed.rs` — GraphQL arm in `apply_confirmed_binding`.
- `src/server/mcp/contract_tools/contracts.rs` — `run_check_binding` GraphQL arm; `render_bindings_entry` (unchanged shape).
- `src/server/mcp/contract_tools/schemas/check_binding.out.json` + `docs/tool-schema.json` — new reasons.
- `docs/REPOS_YAML.md`, `CHANGELOG.md` — documentation.
- Tests: `tests/graphql_resolution.rs`, `src/server/federation/contracts/joiner_tests.rs`, `tests/federation_contracts_e2e.rs`, `tests/contracts_golden.rs` (must stay green).

---

### Task 1: Pin storefront document shapes as regression tests

**Files:**
- Modify: `tests/graphql_resolution.rs` (append; reuse `fixed_workspace` / `write_file` helpers and the `scan_workspace_graphql_consumer` harness used by `f7_graphql_field_lineage_and_field_binds` at `:647`)

**Interfaces:**
- Consumes: existing test helpers `fixed_workspace(tag) -> PathBuf`, `write_file(root, rel, content)`, `scan_workspace_graphql_consumer(&graph, &root, &ns)` (see `tests/graphql_resolution.rs:106-194`).
- Produces: nothing consumed by later tasks; this task is the diagnostic anchor for Review Focus #2 and the evidence base for Task 2.

- [ ] **Step 1: Write the failing tests**

Add to `tests/graphql_resolution.rs`, imitating the fixture style of `f7_graphql_field_lineage_and_field_binds`:

```rust
#[test]
fn storefront_graphql_op_file_mints_one_consumer_per_top_field() {
    let root = fixed_workspace("sf_op_files");
    write_file(
        &root,
        "src/graphql/ProductDetails.graphql",
        "query ProductDetails($slug: String!) {\n  product(slug: $slug) {\n    id\n    name\n  }\n}\n",
    );
    let mut graph = Graph::default();
    let ns = Namespace::new("sf_op_files");
    scan_workspace_graphql_consumer(&graph, &root, &ns);
    let names: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"graphql-call:query:product"),
        "expected consumer node graphql-call:query:product, got {names:?}"
    );
}

#[test]
fn fragment_only_graphql_file_mints_no_consumer() {
    let root = fixed_workspace("sf_frag_file");
    write_file(
        &root,
        "src/graphql/OrderDetailsFragment.graphql",
        "fragment OrderDetails on Order {\n  id\n  number\n}\n",
    );
    let mut graph = Graph::default();
    let ns = Namespace::new("sf_frag_file");
    scan_workspace_graphql_consumer(&graph, &root, &ns);
    assert!(
        graph.nodes.is_empty(),
        "fragment-only docs must mint no consumers, got {:?}",
        graph.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}
```

Adjust struct/namespace construction to the exact helper signatures in the file; the assertions are the contract.

- [ ] **Step 2: Run the tests**

Run: `cargo test --test graphql_resolution storefront_graphql_op_file_mints_one_consumer_per_top_field -- --nocapture`
and `cargo test --test graphql_resolution fragment_only_graphql_file_mints_no_consumer -- --nocapture`
Expected: both PASS (the sensor already parses standalone docs — `parse_document` at `graphql_consumer_sensor.rs:269-329` — and the live probe in Diagnóstico #5 confirms it on real files). If the first test FAILS, record the actual names in the failure output and continue — the fix belongs in `graphql_consumer_sensor.rs`/`parse_document`, and the observed gap defines it. Do not delete the test.

- [ ] **Step 3: Run the whole file to confirm no collateral damage**

Run: `cargo test --test graphql_resolution`
Expected: PASS (all pre-existing F1–F7 scenarios plus the two new tests).

- [ ] **Step 4: Commit**

```bash
git add tests/graphql_resolution.rs
git commit -m "test: pin storefront .graphql document shapes (ops vs fragment-only)"
```

### Task 2: `services[].provides_graphql` — scope the GraphQL provider surface

**Files:**
- Modify: `src/server/federation/contracts/config.rs:60-73` (`ServiceDecl`) and validation in `validate()` (~`:249-528`)
- Modify: `src/server/federation/contracts/joiner/endpoints.rs:43-138` (`build_endpoints`)
- Modify: `docs/REPOS_YAML.md` (services section), `CHANGELOG.md`
- Test: `tests/graphql_resolution.rs`, `src/server/federation/contracts/config.rs` `mod tests`

**Interfaces:**
- Consumes: `ContractFact::GraphqlProvider` facts (already minted by `graphql_provider_sensor`), `assign_services` attribution (unchanged).
- Produces: `ServiceDecl { …, pub provides_graphql: bool }` (serde default `true`). Later tasks rely on YAML key `services[].provides_graphql: false` being accepted by `ContractFederationConfig::load_from_str`. The requirements doc hands this key to the consuming side.

- [ ] **Step 1: Write the failing tests**

In `tests/graphql_resolution.rs`, extend the existing `config_with_two_services` builder area (`:141-190`) with a variant that sets `provides_graphql: false` on the second service:

```rust
#[test]
fn provides_graphql_false_shrinks_provider_key_space() {
    // Two services mint GraphQL provider endpoints for (query, product):
    // the API (real SDL) and a consumer repo carrying a schema copy.
    // With provides_graphql: false on the copy, exactly one provider
    // remains and the consumer must Binds instead of graphql_no_op.
    let nodes = vec![
        graphql_provider_node("api", "query:product", GraphqlOp::Query, "product"),
        graphql_provider_node("dashboard", "query:product", GraphqlOp::Query, "product"),
        graphql_consumer_node("storefront", "graphql-call:query:product", GraphqlOp::Query, "product"),
    ];
    let cfg = config_with_two_services_provides_graphql(false);
    let out = run(&nodes, &cfg);
    assert!(out.binds.iter().any(|e| e.consumer.name() == "graphql-call:query:product"));
}
```

In `src/server/federation/contracts/config.rs` `mod tests` (next to `allows_disjoint_paths_in_service`, `:722`), in the file's inline-`r#"…"#` + `load_from_str` style:

```rust
#[test]
fn parses_provides_graphql_default_true() {
    let yaml = r#"
services:
  - name: orders
    repo: alpha
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    assert!(cfg.services.iter().all(|s| s.provides_graphql));
}

#[test]
fn parses_provides_graphql_false() {
    let yaml = r#"
services:
  - name: orders
    repo: alpha
    provides_graphql: false
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    assert!(!cfg.services[0].provides_graphql);
}
```

(The file's service-parse tests use exactly this inline-`r#"…"#` + `load_from_str` + `validate(&repo_ids())` style — see `allows_single_path_in_service` at `config.rs:713`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test graphql_resolution provides_graphql_false_shrinks_provider_key_space` → Expected: FAIL (unknown field `provides_graphql` on deserialize, or the consumer resolves `graphql_no_op`).
Run: `cargo test --lib parses_provides_graphql` → Expected: FAIL (field does not exist).

- [ ] **Step 3: Implement the config field**

In `ServiceDecl` (`config.rs:60-73`):

```rust
    /// Whether this service's `.graphql`/`.gql` SDL files mint GraphQL
    /// provider endpoints. Set `false` for repos that only carry schema
    /// copies (codegen inputs) so they do not pollute the `(op, field)`
    /// provider key space. Defaults to `true`.
    #[serde(default = "default_provides_graphql")]
    pub provides_graphql: bool,
```

with `fn default_provides_graphql() -> bool { true }` next to the other serde defaults.

- [ ] **Step 4: Implement the filter in `build_endpoints`**

In `endpoints.rs`, where a `ContractFact::GraphqlProvider` becomes an endpoint entry (the block around `:125-138`), skip minting when the owning service is suppressed. Add a helper and use it:

```rust
fn service_provides_graphql(config: &ContractFederationConfig, svc: &ServiceName) -> bool {
    config
        .services
        .iter()
        .find(|s| ServiceName(s.name.clone()) == *svc)
        .map(|s| s.provides_graphql)
        .unwrap_or(true)
}
```

Call it in the `GraphqlProvider` arm before pushing to the endpoint table: `if !service_provides_graphql(config, &svc) { continue; }`. Nodes that fall through to the implicit repo-id service keep default `true` (a repo with no `ServiceDecl` still exposes its SDL).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test graphql_resolution provides_graphql_false_shrinks_provider_key_space` → PASS.
Run: `cargo test --lib parses_provides_graphql` → PASS.
Run: `cargo test` (workspace) → PASS (the golden contracts tests and joiner tests must be unaffected).

- [ ] **Step 6: Document and commit**

Add to `docs/REPOS_YAML.md` services section:

```yaml
services:
  - name: saleor-dashboard
    repo: saleor-dashboard
    # Schema copies only (codegen inputs) — not a GraphQL provider.
    provides_graphql: false
```

Add a `CHANGELOG.md` entry: "config: `services[].provides_graphql` scopes which services mint GraphQL provider endpoints; set `false` for repos that only carry SDL copies. No migration needed (default `true` preserves prior behavior)."

```bash
git add -A src/server/federation/contracts/config.rs src/server/federation/contracts/joiner/endpoints.rs tests/graphql_resolution.rs docs/REPOS_YAML.md CHANGELOG.md
git commit -m "feat: services[].provides_graphql scopes GraphQL provider surface"
```

### Task 3: Accept `graphql:` keys in binding config grammar

**Files:**
- Modify: `src/server/federation/contracts/joiner/mod.rs:784-791` (`parse_contract_key`)
- Modify: `src/server/federation/contracts/config.rs:587-614` (`validate_key`)
- Modify: `docs/REPOS_YAML.md` (bindings section), `CHANGELOG.md`
- Test: `src/server/federation/contracts/config.rs` `mod tests`, `src/server/federation/contracts/joiner/mod.rs` tests if present

**Interfaces:**
- Consumes: `ContractKey::FromStr` which already parses `"graphql:query:ProductDetails"` → `ContractKey::Graphql { op: Query, field: "ProductDetails" }` (`model.rs:793-808`; op must be `query|mutation|subscription`).
- Produces: `parse_contract_key("graphql:query:product") -> Ok(ContractKey::Graphql { … })`; `validate_key` accepting `graphql:` keys. Tasks 4–5 rely on these.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parse_contract_key_accepts_graphql_prefix() {
    let key = parse_contract_key("graphql:query:product").unwrap();
    assert_eq!(
        key,
        ContractKey::Graphql { op: GraphqlOp::Query, field: "product".into() }
    );
}

#[test]
fn validate_key_accepts_graphql_key() {
    assert!(validate_key("bindings.consumer.key", "graphql:query:product").is_ok());
}

#[test]
fn validate_key_rejects_graphql_key_with_unknown_op() {
    let err = validate_key("bindings.consumer.key", "graphql:queryish:product").unwrap_err();
    assert!(err.to_string().contains("unknown graphql op"), "got: {err}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test parse_contract_key_accepts_graphql_prefix validate_key_accepts_graphql_key` → Expected: FAIL (`http:graphql:query:product` → `"malformed http key"`; `"missing space"`).

- [ ] **Step 3: Implement**

`parse_contract_key` (`joiner/mod.rs:784-791`):

```rust
pub(crate) fn parse_contract_key(s: &str) -> Result<ContractKey, String> {
    let normalized = if s.starts_with("http:")
        || s.starts_with("topic:")
        || s.starts_with("graphql:")
    {
        s.to_string()
    } else {
        format!("http:{s}")
    };
    normalized.parse()
}
```

`validate_key` (`config.rs:587-614`): before the existing `split_once(' ')` HTTP path, add:

```rust
    if key.starts_with("graphql:") {
        match ContractKey::from_str(key) {
            Ok(ContractKey::Graphql { field, .. }) if !field.is_empty() => return Ok(()),
            Ok(_) => return Err(LainError::Config(format!("{field}: '{key}' is not a graphql key"))),
            Err(e) => return Err(LainError::Config(format!("{field} '{key}': {e}"))),
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test parse_contract_key_accepts_graphql_prefix validate_key_` → PASS. Then `cargo test` (workspace) → PASS.

- [ ] **Step 5: Document and commit**

`docs/REPOS_YAML.md` bindings section: key grammar becomes `"<METHOD> <template>"` **or** `"graphql:<query|mutation|subscription>:<field>"`. CHANGELOG: "config: `bindings[].{consumer,provider}.key` accepts `graphql:op:field` keys (previously HTTP-only grammar)."

```bash
git add -A src/server/federation/contracts/joiner/mod.rs src/server/federation/contracts/config.rs docs/REPOS_YAML.md CHANGELOG.md
git commit -m "feat: accept graphql:op:field keys in binding config grammar"
```

### Task 4: `confirmed.rs` — apply confirmed bindings to GraphQL consumers

**Files:**
- Modify: `src/server/federation/contracts/joiner/confirmed.rs:28-151` (`apply_confirmed_binding`)
- Test: `src/server/federation/contracts/joiner_tests.rs` (§7.6 confirmed bindings block, `:935-1055`)

**Interfaces:**
- Consumes: `parse_contract_key` (Task 3), `ContractFact::GraphqlConsumer(GraphqlConsumerFact { op, field })` (`model.rs:247-251`), `ContractKey::Graphql`, `EndpointTable` lookup `endpoints.get(&(ServiceName, ContractKey))` which already works for any key variant.
- Produces: a `BindsEdge` with `EdgeProvenance::Confirmed { source: "repos.yaml#bindings[<i>]" }` for GraphQL consumers; `StaleReason::{NoConsumer, NoEndpoint}` unchanged. Task 5's `check_binding` YAML must match nodes by the same rules (repo + path + symbol where symbol = node name `graphql-call:{op}:{field}`).

- [ ] **Step 1: Write the failing tests**

In `joiner_tests.rs`, next to `confirmed_binding_matches_consumer_and_records_provenance` (`:937`), imitating its builders:

```rust
#[test]
fn confirmed_graphql_binding_binds_consumer_to_provider_service() {
    let provider = graphql_provider_node("api", "query:product");
    let consumer = graphql_consumer_node("storefront", "graphql-call:query:product");
    let cfg = config_with_binding(ConfirmedBinding {
        consumer: ConfirmedBindingConsumer {
            repo: "storefront".into(),
            path: consumer.path.clone(),
            symbol: "graphql-call:query:product".into(),
            key: "graphql:query:product".into(),
        },
        provider: ConfirmedBindingProvider {
            service: "api".into(),
            key: "graphql:query:product".into(),
        },
    });
    let out = ContractJoiner::run(&[provider, consumer], &[], &cfg);
    assert!(out.binds.iter().any(|e| matches!(e.provenance, EdgeProvenance::Confirmed { .. })));
}

#[test]
fn confirmed_graphql_binding_no_consumer_marks_stale() {
    // same config, but no consumer node
    let provider = graphql_provider_node("api", "query:product");
    let out = ContractJoiner::run(&[provider], &[], &cfg);
    assert!(out.index.stale_bindings.iter().any(|s| s.reason == StaleReason::NoConsumer));
}

#[test]
fn confirmed_graphql_binding_no_endpoint_marks_stale() {
    // consumer present, provider absent
    let consumer = graphql_consumer_node("storefront", "graphql-call:query:product");
    let out = ContractJoiner::run(&[consumer], &[], &cfg);
    assert!(out.index.stale_bindings.iter().any(|s| s.reason == StaleReason::NoEndpoint));
}
```

Use the file's existing builder style; if `joiner_tests.rs` lacks GraphQL node builders, mirror `graphql_provider_node` / `graphql_consumer_node` from `tests/graphql_resolution.rs:81/:104` (duplicate minimal builders — match local style).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib confirmed_graphql_binding` → Expected: FAIL (binding never matches a `GraphqlConsumer` node → stale `NoConsumer`).

- [ ] **Step 3: Implement the GraphQL arm**

In `apply_confirmed_binding` node loop, replace the fact gate at `:70` with a two-arm match producing the node's contract key:

```rust
let node_key = match node.contract.first() {
    Some(ContractFact::Consumer(consumer)) => {
        let Some(template) = consumer.url.template.clone() else { continue };
        ContractKey::Http { method: consumer.method, template }
    }
    Some(ContractFact::GraphqlConsumer(fact)) => {
        ContractKey::Graphql { op: fact.op, field: fact.field.clone() }
    }
    _ => continue,
};
if node_key != consumer_key { continue; }
```

Keep the existing filters (repo `:76`, path `:79`, symbol `:82-84` — `node.container.unwrap_or(node.name) == binding.consumer.symbol` works for synthetic GraphQL nodes whose `container` is `None` and whose name is `graphql-call:{op}:{field}`). Keep the HTTP-only `consumer.url.template.is_some()` check inside the HTTP arm only. On match, push the same `BindsEdge` / resolution replacement as `:102-133`; on no match, the existing `StaleReason::NoConsumer` path `:135-150`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib confirmed_graphql_binding` → PASS (all three).
Run: `cargo test` (workspace) → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/federation/contracts/joiner/confirmed.rs src/server/federation/contracts/joiner_tests.rs
git commit -m "feat: apply repos.yaml bindings to GraphQL consumers"
```

### Task 5: `check_binding` for GraphQL consumers (reasons + YAML emission)

**Files:**
- Modify: `src/server/mcp/contract_tools/contracts.rs:512-668` (`run_check_binding`) and `:1064-1100` (`parse_consumer_call_shape` area) — add a GraphQL branch keyed on the `graphql-call:` name prefix (`GRAPHQL_CALL_PREFIX`, `src/server/sensors/util.rs:523`)
- Modify: `src/server/mcp/contract_tools/schemas/check_binding.out.json` — reasons enum += `op_mismatch`, `field_mismatch`
- Regenerate: `docs/tool-schema.json` (`make schema` / `lain schema`)
- Test: `tests/federation_contracts_e2e.rs`, `tests/contracts_golden.rs` (existing golden tests must stay green)

**Interfaces:**
- Consumes: Task 3 key grammar; `view.consumer(&consumer_id)` and `view.endpoints.get(&endpoint_id)` (GraphQL provider endpoints are already in that map keyed `(ServiceName, ContractKey::Graphql)`).
- Produces: `check_binding` accepts `endpoint.key = "graphql:query:product"` and, when `valid: true`, `bindings_entry` YAML that Task 4's matching rules accept verbatim (consumer `symbol: graphql-call:query:product`, keys `graphql:query:product` on both sides). The requirements doc documents this output for the consuming side.

- [ ] **Step 1: Write the failing tests**

In `tests/federation_contracts_e2e.rs`, imitating `pr13_check_binding_emits_bindings_entry_for_valid_link` (`:888`):

```rust
#[tokio::test]
async fn pr14_check_binding_emits_graphql_bindings_entry() {
    // federation with a GraphQL provider endpoint (saleor-api style)
    // and a graphql-call consumer node in another repo
    let out = check_binding_handle(&ctx, json!({
        "snapshot": "live",
        "consumer": "<global id of graphql-call:query:product>",
        "endpoint": {"service": "saleor-api", "key": "graphql:query:product"}
    })).await;
    assert_eq!(out["data"]["valid"], json!(true));
    let yaml = out["data"]["bindings_entry"].as_str().unwrap();
    assert!(yaml.contains("key: \"graphql:query:product\""));
    assert!(yaml.contains("symbol: graphql-call:query:product"));
}

#[tokio::test]
async fn pr14_check_binding_rejects_key_kind_mismatch() {
    let out = check_binding_handle(&ctx, json!({
        "snapshot": "live",
        "consumer": "<graphql consumer id>",
        "endpoint": {"service": "saleor-api", "key": "http:POST /graphql"}
    })).await;
    assert_eq!(out["data"]["valid"], json!(false));
    let reasons = out["data"]["reasons"].as_array().unwrap();
    assert!(reasons.iter().any(|r| r == "op_mismatch" || r == "field_mismatch"));
}

#[tokio::test]
async fn pr14_check_binding_rejects_wrong_graphql_field() {
    // endpoint key graphql:query:somethingElse vs consumer graphql-call:query:product
    let out = check_binding_handle(&ctx, json!({ ... })).await;
    assert_eq!(out["data"]["valid"], json!(false));
    assert!(out["data"]["reasons"].as_array().unwrap().iter().any(|r| r == "field_mismatch"));
}
```

(Reuse `build_three_repo_federation` / `ctx_for` from the file's existing setup at `:77`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test federation_contracts_e2e pr14_` → Expected: FAIL (current gate: "topic endpoints not supported in 0.9" error outcome).

- [ ] **Step 3: Implement the GraphQL branch**

In `run_check_binding`, replace the provider destructure at `:615-635` (the `let (provider_method, provider_template) = match &key { … }` block plus the `methods_compatible` / `templates_compatible` statements right after it) with a single match that produces both comparison labels. The real tail of the function (`let valid = reasons.is_empty() && endpoint_def.is_some();` at `:639` onward) stays untouched:

```rust
    let (consumer_method, consumer_template) = parse_consumer_call_shape(&consumer_id);
    let (method_match, template_match) = match &key {
        ContractKey::Http { method, template } => {
            let mm = methods_compatible(&consumer_method, method);
            if !mm {
                reasons.push("method_mismatch");
            }
            let (tc, label) = templates_compatible(consumer_template.as_deref(), template);
            if !tc {
                reasons.push("template_mismatch");
            }
            (mm, label)
        }
        ContractKey::Graphql { op, field } => {
            // GraphQL consumers are synthetic nodes named
            // `graphql-call:{op}:{field}`; parse_consumer_call_shape cannot
            // see them (no whitespace in the name). Compare (op, field)
            // against the name and reuse the same verdict fields.
            let name = consumer_id.name();
            let rest = name.strip_prefix(GRAPHQL_CALL_PREFIX).unwrap_or("");
            let (cop, cfield) = rest.split_once(':').unwrap_or((rest, ""));
            let op_ok = cop == op.to_string();
            let field_ok = cfield == field;
            if !op_ok {
                reasons.push("op_mismatch");
            }
            if !field_ok {
                reasons.push("field_mismatch");
            }
            (op_ok, if field_ok { "exact" } else { "none" }.to_string())
        }
        _ => {
            return Err(error_outcome(
                "invalid_argument",
                "only http and graphql endpoints are supported in 0.9",
                Some(json!({"arg": "endpoint.key"})),
                &snap_label,
                started,
            ));
        }
    };
```

`GRAPHQL_CALL_PREFIX` is `"graphql-call:"` (`src/server/sensors/util.rs:523`) — import it if visible, otherwise inline the literal with a comment. Match the `reasons` vec's element type to what the file already pushes (the current code pushes `&'static str` via `reasons.push("already_bound");`). The `template_match` label for GraphQL is `exact`/`none` (field equality); `method_match` is op equality. `render_bindings_entry` (`:1134-1147`) needs no change: `ContractKey::Display` already prints `graphql:query:product` (`model.rs:691-693`) and `symbol` is the node name (`consumer_id.name()`), which is exactly what Task 4's symbol filter matches on.

- [ ] **Step 4: Update the schema and regenerate**

Add `"op_mismatch"`, `"field_mismatch"` to the `reasons` enum in `src/server/mcp/contract_tools/schemas/check_binding.out.json`. Run `make schema` (or `lain schema`) to regenerate `docs/tool-schema.json`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test federation_contracts_e2e pr14_` → PASS (3 tests).
Run: `cargo test --test contracts_golden` → PASS (envelope + per-tool schema goldens).
Run: `cargo test` (workspace) → PASS.

- [ ] **Step 6: CHANGELOG and commit**

CHANGELOG: "`check_binding` accepts GraphQL endpoints (`graphql:op:field`); new reasons `op_mismatch`/`field_mismatch`. Tool schema updated — regenerate `docs/tool-schema.json` consumers if you mirror it."

```bash
git add -A src/server/mcp/contract_tools/contracts.rs src/server/mcp/contract_tools/schemas/check_binding.out.json docs/tool-schema.json tests/federation_contracts_e2e.rs CHANGELOG.md
git commit -m "feat: check_binding supports graphql endpoints and emits bindings YAML"
```

### Task 6: Round-trip acceptance — emitted YAML resolves the edge

**Files:**
- Test: `tests/federation_contracts_e2e.rs` (append)
- No production code. This task proves the whole chain works together and is the gate the requirements doc hands to Parallax.

**Interfaces:**
- Consumes: `run_check_binding` + `render_bindings_entry` (Task 5), `ContractFederationConfig` binding parsing (Task 3), `apply_confirmed_binding` (Task 4), `provides_graphql` (Task 2).
- Produces: a green e2e test asserting (a) the two-SDL-provider scenario resolves after `provides_graphql: false`, and (b) a `check_binding`-emitted YAML entry parses as `repos.yaml#bindings` and binds the consumer. The manual recipe below is what the demo in the requirements doc verifies end-to-end.

- [ ] **Step 1: Write the round-trip test**

```rust
#[tokio::test]
async fn check_binding_yaml_round_trip_binds_graphql_consumer() {
    // 1. Build the Saleor shape: service "api" (SDL provider),
    //    service "dashboard" (SDL copies, provides_graphql: false),
    //    a graphql consumer in repo "storefront".
    let fed = build_saleor_shape_federation(root.path()).await; // reuse build_three_repo_federation helpers
    // 2. Assert the consumer is unresolved graphql_no_op WITHOUT the flag,
    //    and Binds once provides_graphql: false lands on "dashboard".
    // 3. check_binding(consumer_global_id, {service:"api", key:"graphql:query:product"})
    //    -> valid: true with bindings_entry.
    // 4. Feed bindings_entry back: parse it as a YAML bindings item
    //    (serde_yaml::from_str::<Vec<ConfirmedBinding>> on "- consumer: ..." form
    //    with the leading "- " stripped, or wrap it in "bindings:\n" + entry),
    //    run ContractJoiner with it, and assert EdgeProvenance::Confirmed.
    // Assertions are the contract; adapt builders to the file's style.
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --test federation_contracts_e2e check_binding_yaml_round_trip` → Expected: PASS after Tasks 2–5; if it fails, the failing assertion names which link of the chain broke (config parse → joiner → tool output → re-parse).

- [ ] **Step 3: Manual verification recipe (read-only against real checkouts)**

For the demo referenced in the requirements doc, verify against the real Saleor checkouts without modifying them:

```bash
# config lives OUTSIDE the fixture repos (e.g. /tmp/saleor-verify/repos.yaml):
# repos: saleor, saleor-dashboard, storefront (workspace_dir paths, read-only);
# services: saleor-api   {repo: saleor, paths: ["saleor"], env: [NEXT_PUBLIC_SALEOR_API_URL]}
#           saleor-dashboard {repo: saleor-dashboard, provides_graphql: false}
#           saleor-storefront {repo: storefront}
lain reindex --config /tmp/saleor-verify/repos.yaml   # slow in debug: 3 repos
# then via `lain server --config ... --transport http` + tools/call:
#   list_unresolved {"snapshot":"live","repo":"storefront","limit":1000}
#     -> expect NO graphql_no_op items for storefront
#   list_contracts  {"snapshot":"live","limit":1000}
#     -> expect graphql:query:product with bound_consumers >= 1
```

Expected: the storefront's `graphql-call:{op}:{field}` consumers bind to `saleor-api`; the dashboard's schema copies no longer act as providers. If the debug build times out on the 3-repo reindex, run the two-repo variant (`saleor` + `storefront`) which still proves the key-space fix.

- [ ] **Step 4: Commit**

```bash
git add tests/federation_contracts_e2e.rs
git commit -m "test: round-trip check_binding YAML binds GraphQL consumer (acceptance)"
```

## Out of Scope (deliberate)

- **Parallax implementation** — its own repo; what LAIN needs from it is the contract in `docs/superpowers/plans/2026-10-14-parallax-requirements.md`.
- **Wrapper HTTP call detection in the engine** (`fetchWithTimeout` opacity): irrelevant for the GraphQL edge, which is derived from document text. The per-repo pattern-override mechanism (`<repo>/.lain/patterns/`, `http_client_sensor.rs:103-109`) remains the escape hatch for HTTP-only attribution work.
- **Auto-applying configuration** — the engine consumes config; applying it is the operator's/consumer's step.
- **Web UI**, and any change to the on-disk graph format (`FEDERATION_GRAPH_VERSION` stays put).
- **The `candidates` list for GraphQL consumers** (`candidate_endpoints_for` mis-parses `graphql-call:` names and lists every HTTP endpoint). Cosmetic in `list_unresolved`; fix only if a task above needs it.

## Review Focus → owning tests (self-review)

| Failure mode | Owning task / test |
|---|---|
| SDL copies pollute `(op, field)` providers | Task 2 `provides_graphql_false_shrinks_provider_key_space` |
| Fragment-only docs invisible to LAIN | Task 1 `fragment_only_graphql_file_mints_no_consumer` |
| `check_binding` key-kind mismatch | Task 5 `pr14_check_binding_rejects_key_kind_mismatch` |
| Stale GraphQL bindings | Task 4 `confirmed_graphql_binding_no_consumer_marks_stale` |
| `provides_graphql` default changes existing behavior | Task 2 `parses_provides_graphql_default_true` / `_false` |
| Emitted YAML does not round-trip into the joiner | Task 6 `check_binding_yaml_round_trip_binds_graphql_consumer` |
