# `coverage.unnormalized` Honest Routing — Plan

**Goal:** Make the `coverage.unnormalized` bucket match what the sensor code says it does. Today the bucket is fed by HTTP rule 5 only (`UnresolvedReason::Unnormalized` from the HTTP ladder, `joiner/mod.rs:390-398`), but two sensor code paths advertise they surface here and silently don't:

1. **Dynamic topic names** — `event_sensor.rs:711-718` `continue`s when `site.topic` is `None` (a literal/identifier resolver miss). The module header (`event_sensor.rs:32-36`) and the doc (`protocol-coverage.md:226-231, 376-383`) claim these surface in `coverage.unnormalized`; the bucket is empty for them today. The same drop happens for the `graphql_sensor` query-name miss path, but that's pre-existing — out of scope.
2. **External openapi `$ref`s** — `openapi_sensor.rs:24-28` advertises `unresolved_refs` is a `coverage.unnormalized`-style accessor; the only consumer of `unresolved_refs` is the in-file test `external_ref_response_yields_no_fields` (`openapi_sensor.rs:1256-1310`). It does not feed `coverage.unnormalized` because the openapi path does not produce `ConsumerFact`s — the joiner never sees it. Documented today is misleading.

Both are the same class of bug the SNAPSHOT_MAGIC fix caught: the **doc claims** a behavior the code does not actually have. The fix is to make the bucket honest, in the spirit of `event_sensor.rs:712`'s own comment ("nothing records it either: unnormalized is HTTP rule-5 only"). Two options:

- **Route** dynamic topic names + external `$ref`s into the existing `coverage.unnormalized` bucket. Make the doc true by closing the gap.
- **Document** the discrepancy in code + doc (call it "dropped silently, not surfaced") and route nothing.

The user explicitly asked for the **honest fix** in the F1 conversation ("la honestidad del reporting del coverage es la misma clase de ficción que el bug del magic cazó proactivamente"). We route.

**Architecture:** Two small pushes into `coverage.unnormalized`:

1. **Dynamic topic names** — at the `continue` site (`event_sensor.rs:716`), the sensor has no consumer key to push (it never produced one). The right carrier is the `(repo, file, line)` of the producer/subscription site that lost its topic. We emit a synthetic `ConsumerKey` keyed on the **synthetic per-site node id** the sensor already mints below for static sites (`<TOPIC_READ_PREFIX><path>:<line>`, `event_sensor.rs:773-780`) — even when the topic name is unresolved. The synthetic node is materialized (it is today for static sites; we extend the emission to dynamic sites) so the joiner's existing `unnormalized.push(call_id.clone())` path applies without a new sink.

   But synthetic is materialized in the SAME loop body where the topic name is dropped (it is only emitted when the topic name resolves, `event_sensor.rs:759-779` reads `topic_name` after the `continue`). The cleanest fix: before the `continue`, push a **dynamic-topic marker** (e.g. `(repo, file, line)` into a new `Vec` returned alongside `(nodes, edges)`), and have the joiner or `scan_workspace_event` convert it into a `ConsumerKey` for the bucket. Keep the synthetic node minting unchanged for the resolved case.

2. **External openapi `$ref`s** — `unresolved_refs` is a local `Vec<String>` inside the openapi scan (`openapi_schema.rs`). It already records the `$ref` body. The carrier that reaches `coverage.unnormalized` today is a `ConsumerFact` from a sensor — openapi emits provider `Field`s, not consumer facts. So the joiner has nothing to index. Two cleanest routings:
   - **(a)** Add a new `Coverage.unresolved_refs: Vec<UnresolvedRef>` field (sibling of `unnormalized`) and populate from the openapi scan result. The bucket is read by `claims_from_coverage` (`claims.rs:313-327`) and rendered as a `missing` evidence claim. This is the most honest — it does not pretend an external `$ref` is the same kind of evidence as an unnormalized HTTP URL.
   - **(b)** Map each external `$ref` onto a `ConsumerKey` keyed on the operation `(path, method)` whose response schema it broke. The operation IS a `Consumer` of the foreign schema, so the joiner model fits. But the URL lives outside the workspace; `coverage_complete` and `RepoCoverage` would have to scope it. Option (a) avoids the joiner plumbing.

**Decision: route dynamic topics into `coverage.unnormalized` (existing bucket) + add `Coverage.unresolved_refs` (new bucket, sibling).** The two surfaces honestly: dynamic topic names are unnormalized consumer evidence at the topic layer; external `$ref`s are an openapi-shape gap that does not fit the consumer-keyed bucket.

**Tech Stack:** Rust (event_sensor push, openapi_sensor push, joiner/consumer_http.rs::resolve_consumer plumbing for synthetic sites, diff.rs::Coverage struct addition, claims.rs rendering), no new deps.

**Spec:** The §9.7 `Coverage` shape (`src/server/federation/contracts/diff.rs:1623-1638`); the `consumer_http.rs:resolve_consumer` return shape (current `ConsumerResolution`); the `claims.rs:313-327` three-bucket iteration that names "unresolved", "ambiguous", "unnormalized" as separate `missing`-class claims.

## Global Constraints

- Branches cut from `dev`, merged back to `dev`; English commit subjects; no version bumps.
- CHANGELOG entry for user-visible change: a new coverage bucket (`unresolved_refs`) is a `tool-schema.json` schema change, must regenerate + `tests/contracts_golden.rs` green.
- `cargo test --workspace` green + `cargo fmt --all -- --check` + CI clippy flags (`-D warnings -A clippy::style -A clippy::complexity -A clippy::perf -A clippy::pedantic -A unused -A dead_code`).
- Host: `TMPDIR=/data/agents/tmp-lain-tests` on every cargo call; `/data` free space guard.
- Sensor ownership rule (src/server/sensors/AGENTS.md): the new bucket emission MUST NOT mutate a node another sensor owns. The dynamic-topic synthetic node extends `event_sensor`'s own emission — no cross-sensor risk.

## Review Focus

1. **Dynamic topic evidence must be representable in the bucket.** The existing `ConsumerKey` shape (`caller: SymbolKey + target: ...`) is symbol-shaped; a dynamic topic has no symbol+endpoint key. We must either map onto an existing key shape or extend it. Decide before coding.
2. **The synthetic node minting for the dynamic-topic case must NOT create an orphan.** Today the synthetic node is emitted alongside the hub; if the hub isn't emitted (the whole point), the synthetic stands alone. `prune_orphans` is path-keyed and won't drop it (it is owned by `event_sensor`), so the synthetic node lives in the graph alone with no edges — this is honest evidence, but verify a `get_workspace_graph` doesn't trip on it.
3. **`unresolved_refs` schema addition must round-trip** through `claims_from_coverage` and the JSON serialization (`tool-schema.json`). A schema drift breaks CI's `schema-drift` job.
4. **`coverage_complete` gate** must be extended if `unresolved_refs` is treated as a coverage gap (currently the gate looks at `unresolved_consumers`, `ambiguous`, `unnormalized` and per-repo `is_complete`).

## File Structure

- `src/server/sensors/event_sensor.rs` — push dynamic-topic marker before `continue` (Task 1).
- `src/server/federation/contracts/joiner/mod.rs` — collect markers, push to `unnormalized` (Task 1).
- `src/server/sensors/openapi_sensor.rs` — return `unresolved_refs` upward (Task 2).
- `src/server/sensors/openapi_schema.rs` — accumulator already present; plumb out (Task 2).
- `src/server/federation/contracts/diff.rs` — `Coverage.unresolved_refs` field + `build_coverage` population (Task 2).
- `src/server/claims.rs` — `claims_from_coverage` fourth bucket (Task 2).
- `docs/protocol-coverage.md` — drop the "dynamic topic names are dropped silently" caveat (Tasks 1, 2).
- `docs/tool-schema.json` — regenerated; CI schema-drift gate.
- Tests: `tests/event_hub_e2e.rs` (dynamic topic surfaces in unnormalized), `tests/openapi_e2e.rs` (external `$ref` surfaces in unresolved_refs), `tests/federation_contracts_e2e.rs` (claims rendering).

---

## Task 1: Dynamic topic names surface in `coverage.unnormalized`

**Files:**
- Modify: `src/server/sensors/event_sensor.rs:710-718` (push marker before `continue`).
- Modify: `src/server/federation/contracts/joiner/mod.rs:341-403` (consume marker, push to bucket).
- Modify: `docs/protocol-coverage.md:226-231, 376-383` (drop the silent-drop language, point at the bucket).
- Test: `tests/event_hub_e2e.rs` (new test: a producer whose topic name is a same-file const miss → bucket has one entry keyed on the synthetic site).

**Step 1: Add a dynamic-topic marker type to the sensor.**

- In `event_sensor.rs`, define `pub(crate) enum TopicSiteMarker { Dynamic { path: String, line: u32, broker: String } }` in the existing `crate::server::sensors::event_sensor` module.
- Change the return shape of the helper that walks `sites` (currently `(Vec<GraphNode>, Vec<GraphEdge>)`) to add a third `Vec<TopicSiteMarker>` output.
- At the `continue` site (`event_sensor.rs:716`), push `TopicSiteMarker::Dynamic { path: graph_path.to_string(), line: site.line, broker: site.broker.clone() }` instead of `continue`ing. **Wait** — that changes the emission model. The honest choice is: drop the topic (no hub, no synthetic node, no edge — same as today) BUT record the marker so the joiner can put it in the bucket. So `continue` stays; the marker push happens before it.

**Step 2: Plumb the marker to the joiner.**

- The event scan returns the marker list. `scan_workspace_event` aggregates them across files into a `Vec<TopicSiteMarker>` on the per-repo scan summary.
- The `FederatedIndex::add_repo` path already pushes per-repo metadata; add the marker list as a sibling.
- The joiner (`joiner/mod.rs:341-403`) iterates **per call site id**, not per topic/dyn/marker. We need to convert each `TopicSiteMarker` into a `ConsumerKey` keyed on... what?

  The existing `ConsumerKey` is `{ caller: SymbolKey, target: TargetKey, reason: UnresolvedReason }` (`diff.rs` consumer side). For a dynamic topic, the caller is the synthetic per-site node (the SAME id shape `event_sensor.rs:773-780` would mint IF the topic name had resolved). Map `Dynamic { path, line, broker }` → synthetic id `<TOPIC_READ_PREFIX><path>:<line>` (call `GraphNode::generate_id(NodeType::Function, &path, &name, Some(line), namespace)` with `name = "<TOPIC_READ_PREFIX><path>:<line>"`); the consumer key's `caller.id` is that hash.

  The `reason` is a new variant `UnresolvedReason::DynamicTopic`. Add it next to `Unnormalized` in `index.rs:158` and the label map (`contracts.rs:1453`).

- Joiner pushes the new `ConsumerKey` to `unnormalized` directly (the bucket is generic `Vec<ConsumerKey>`).

**Step 3: Test.**

- New test in `tests/event_hub_e2e.rs`: producer calls `producer.send({ topic: SOME_CONST, ... })` where `SOME_CONST` is defined in a same-file const BUT THE PATTERN DOES NOT MATCH the constant-extraction map` (e.g. the const is in a different file the pattern extractor doesn't see). After scan + projection, `coverage.unnormalized` has one entry with `reason = DynamicTopic` and the synthetic site id.
- Test harness fixture: skip the constant-extraction shortcut by using a producer in one file and a const in a different file (the `constants` table is per-file in `event_sensor.rs:530`).

**Step 4: Commit.**

- Subject: `fix: dynamic topic names surface in coverage.unnormalized`
- CHANGELOG entry: `event_sensor` now reports dynamic topic names in `coverage.unnormalized` with `reason=DynamicTopic`; previously dropped silently.
- Verify: `cargo test --workspace` green, `tool-schema.json` regenerated.

## Task 2: External openapi `$ref`s surface in a sibling bucket

**Files:**
- Modify: `src/server/sensors/openapi_sensor.rs:1256-1310` (the existing test already accumulates `unresolved_refs`).
- Modify: `src/server/sensors/openapi_schema.rs` (plumb `unresolved_refs` upward through the scan).
- Modify: `src/server/federation/contracts/diff.rs:1623-1638` (`Coverage.unresolved_refs: Vec<UnresolvedRef>`).
- Modify: `src/server/federation/contracts/diff.rs:2489-2542` (`build_coverage` collects per-repo `unresolved_refs`).
- Modify: `src/server/claims.rs:313-327` (fourth bucket, "unresolved external $ref" note).
- Modify: `docs/tool-schema.json` + `src/server/mcp/contract_tools/schemas/*.json` (regenerated).
- Modify: `docs/protocol-coverage.md:224-231` (the external-`$ref` caveat drops to a one-line note pointing at `unresolved_refs`).
- Test: `tests/openapi_e2e.rs` (new test: external `$ref` on response schema surfaces in `coverage.unresolved_refs`).

**Step 1: Plumb `unresolved_refs` out of the openapi scan.**

- `openapi_schema.rs` already accumulates `unresolved_refs` (a `Vec<String>` of the foreign URLs). Thread it through `scan_workspace`'s return for `openapi_sensor` to a new field on the sensor's per-repo summary (the same shape `event_sensor`'s marker list takes).

**Step 2: `Coverage.unresolved_refs`.**

- New type: `pub struct UnresolvedRef { repo: String, path: String, ref_url: String }` in `diff.rs`.
- New field on `Coverage`: `pub unresolved_refs: Vec<UnresolvedRef>`.
- `build_coverage` aggregates from the per-repo marker lists.
- `coverage_complete` returns `false` if any `unresolved_refs` is non-empty for an in-scope repo (mirrors `unnormalized`).
- `RepoCoverage` gets a sibling field `unresolved_refs: Vec<String>` for per-repo completeness.

**Step 3: `claims.rs` fourth bucket.**

- `claims_from_coverage` adds `(&coverage.unresolved_refs, "unresolved external schema ref")` and renders an `AFFECTED: <repo>:<ref_url>  EVIDENCE: missing` line for each entry.

**Step 4: Test.**

- New test in `tests/openapi_e2e.rs`: spec with one external `$ref` on a response schema. After scan, `coverage.unresolved_refs` has one entry with the foreign URL and the spec path.

**Step 5: Commit + regenerate `tool-schema.json`.**

- Subject: `schemas: coverage.unresolved_refs surfaces external openapi $ref gaps`
- `make schema` regenerates `docs/tool-schema.json`; `tests/contracts_golden.rs` updated.
- CHANGELOG entry: `coverage.unresolved_refs` is a new bucket surfacing external schema refs that the Joiner cannot resolve.

## Out of scope

- GraphQL query-name drop (separate sensor, separate plumbing).
- The `unresolved_refs` accumulator was the easy part because it already exists; the **honest** part of this fix is admitting the bucket is openapi-only, not consumer-shaped.
- The dynamic-topic case does NOT create a synthetic node (it stays dropped); the marker list is the record. A future sensor extension could mint a "dynamic topic" hub node if we wanted to visualize them, but that's a separate decision (today the doc says "no node, no edge, no record"; this task makes "no record" into "in the bucket").

## Verification

- `cargo test --workspace` exit 0.
- New tests pass; existing event_hub_e2e and openapi_e2e tests green (no regression in the resolved-case path).
- `tool-schema.json` drift check: `make schema && git diff --exit-code docs/tool-schema.json src/server/mcp/contract_tools/schemas/*.json` clean.
- Manual end-to-end: `tests/fixtures/saleor` reindex → `coverage.unnormalized` count includes the new dynamic-topic entries; `coverage.unresolved_refs` non-empty if any openapi spec references an external schema.