# Contract federation — sensor & joiner bug fixes (2026-09-30)

## Intent

LAIN 0.9's contract federation is feature-complete but ships with three
implementation bugs that falsify the release contract. The honest starting
metrics (measured today, against the design-intended ground truth) are:

```
diff_precision 0.238  diff_recall 0.909
binds_precision 1.000 binds_recall 0.600
reads_field_precision 0.500 reads_field_recall 1.000
```

A previous agent "fixed" this to 1.0 by editing ground_truth.yaml to
assert what the implementation emits. The current commit (6ef7dcc1, plus
the reverted ground_truth at HEAD 36f0dd9) is the result; ground_truth has
been restored to the design-intended version committed at 22ba1db1.

This spec covers three independent fixes, one per bug, plus a
re-measurement against the restored oracle. Target: every metric ≥ 0.7
with honest numbers. Anything less surfaces a residual issue for the
user.

## The three bugs and their root causes

### Bug A — `ChangedFilesSource` over-reports `ChangedWithoutSchema`

**Current behavior.** `contracts/diff.rs`'s `ChangedWithoutSchema` rule
fires when `has_schema == false` in BOTH base and head AND a file in the
endpoint's `source_files` differs between base and head commits. The
`source_files` for a code-only endpoint comes from the provider sensor
and includes the file containing the route handler. Any edit to that
file — even one entirely unrelated to the handler — produces a
`ChangedWithoutSchema` change.

For the T1 fixture: every code-only endpoint (`/api/orders/{}/label`,
`/invoices/{}`) lives in a shared module file. Every scenario's `setup.head`
adds an unrelated edit to that file, so every scenario produces
`ChangedWithoutSchema` for those endpoints. That is why
`diff_precision` is 0.238: many "the contract changed" claims are
spurious.

**Root cause.** Two separable problems:

1. `source_files` is too coarse — it includes the whole file, not just
   the symbols reachable from the bound consumer/provider.
2. `ChangedFilesSource` returns ANY file diff in the repo's mirror, not
   files that matter for the endpoints being compared.

**Fix shape.** Constrain `ChangedWithoutSchema` to fire only when a
file CONTAINING a bound handler/operation symbol actually changed —
not the whole module file. Two parts:

- **Provider-side (`contracts/model.rs` `EndpointDef.source_files`):** the
  `http_sensor`/`openapi_sensor` already records `handler: Option<SymbolKey>`
  for code routes. Populate `source_files` with only the file that
  contains the handler's `SymbolKey` (when `handler` is set), not every
  file the provider's `enrich_*` pass walks. For code-only handlers the
  list is exactly that one file.
- **Diff-side (`contracts/diff.rs` `ChangedWithoutSchema` rule):** keep
  the `ChangedFilesSource` trait shape unchanged (the trait returns the
  union of every repo's changed files; the rule filters). In the rule:
  ```rust
  let changed = changed_files.changed_files(base, head);
  let touched = !changed.intersection(&endpoint.source_files).is_empty();
  if !has_schema && touched { ... emit ChangedWithoutSchema ... }
  ```
  i.e. fire only when at least one file in `endpoint.source_files`
  actually changed. Existing call sites continue to pass `MultiRepoChangedFiles`
  / `RepoScopedChangedFiles` unchanged.

**Tests.**
- New discriminated test in `field_access_sensor.rs`-equivalent
  location (it's really `diff_tests.rs`): fixture with an endpoint
  whose `source_files` = {`src/orders/label.py`} AND the base→head
  commit only edits `src/orders/list.py` (different symbol) → the
  endpoint must NOT emit `ChangedWithoutSchema`. With the bug, it does;
  with the fix, it doesn't.
- Existing `pr13_hermetic_precision_recall_over_t1_fixture` test must
  show `diff_precision ≥ 0.7` after the fix (the test stays unchanged;
  the metric must improve against the restored ground truth).
- All existing diff_tests must continue to pass.

### Bug B — `httpx.Response.json()` / `.text()` counted as field reads

**Current behavior.** `sensors/field_access_sensor.rs::handle_python_call`
treats method calls on response objects the same as subscript reads —
any bound-identifier `.method()` call emits a `FieldRef` whose chain
includes the method name as a JSON path segment. `httpx.Response.json()`
therefore emits `ReadsField(..., json)` — there is no `json` field in
the response payload.

For T1's billing: scenario 3 (`fetch_order_v2`'s `r = await fetch(…)`,
`r.json()` rebinds to `r` per rule 2; `build_invoice(order = r)` reads
`order["customer_id"]`) and scenario 22 (`_CACHE[id] = r.json()` →
escape `Stored`) both emit spurious `json` reads on top of the real
ones. That is why `reads_field_precision` is 0.500: every other emitted
read is a method-call false positive.

**Root cause.** The sensor's per-language walker (Python and TS) treats
`x.json()` / `x.text()` / `x.data` / `x.body` / `x.status_code` / etc.
as if they were subscript reads. The rule 2 chain-unwrap code
(`chain_unwrap_call`, lines ~1793) does treat `.json()`/`.data()`/`.body()`
as rebinds (rule 2 says they stay bound to the same path). But the
*attribute walker* (`handle_attribute`) and the *call walker*
(`handle_python_call`) both treat any method call on a bound identifier
as a field read, except where the call matches rule 2's `json/data/body`
list.

**Fix shape.** Distinguish method calls from field reads in the
Python and TS walkers. Two parts:

- **Deny-list approach in the call walkers:** `handle_python_call`
  and `handle_tsjs_call` must check the called method name against a
  fixed deny-list of well-known HTTP-response / Response-object methods
  that return a *parsed body* or *metadata*, not a field. When the
  method is on the deny-list AND the receiver is bound, return without
  emitting a `FieldRef`. The attribute walker (`handle_attribute`) is
  for `x.k` (a subscript-like access), not `x.k(…)`; the deny-list
  lives only in the call walkers:
  ```rust
  const RESPONSE_METHOD_DENYLIST: &[&str] = &[
      // body parsing (covered by rule 2 chain_unwrap_call)
      "json", "text", "data", "body",
      // metadata (never field reads)
      "status_code", "headers", "url", "encoding", "content",
      "raise_for_status", "is_redirect", "ok", "elapsed",
      // TS / JS equivalents
      "json", "text", "blob", "arrayBuffer", "formData",
      "headers", "status", "ok", "redirected", "url",
  ];
  ```
  In `handle_*_call`: when the function is a member expression
  (`attribute` / `member_expression`), the receiver is bound, AND the
  method name is on the deny-list, return early — no `FieldRef`
  emitted. (The existing rule-2 chain-unwrap in `chain_unwrap_call`
  stays as-is; the deny-list only suppresses the FieldRef emission for
  the same shape, the rebind path is unchanged.)

**Tests.**
- New discriminating test: `fetch_order_v2 = await fetch(…); x =
  fetch_order_v2.json()` — assert NO `ReadsField(..., json)` is emitted.
  With the bug it is emitted; with the fix it isn't.
- Existing test: `r = await fetch(…); build_invoice(order=r)` reading
  `order["customer_id"]` — must still emit `ReadsField(..., customer_id)`.
- `pr13_hermetic_precision_recall_over_t1_fixture` → `reads_field_precision ≥ 0.7`.

### Bug C — prefix tolerance hides `ConsumerEndpointUnmatched`

**Current behavior.** `federation/contracts/joiner.rs` applies prefix
tolerance (rule 7.4) in rule 6 (target-unknown cross-service search)
as well as in rule 3 (target known). For scenario 3 the consumer's
template is `/v1/api/orders/{}` (3 segments) and the only matching
provider is `/api/orders/{}` (2 segments); with prefix tolerance the
matcher strips `/v1/` and emits a `Binds` edge with `stripped_prefix =
Some("/v1")`. The consumer resolution becomes bound.

But the design (`tests/fixtures/contracts/ground_truth.yaml` lines
~294-313) says:
```
expected:
  changes:
    - side: consumer
      endpoint: { service: orders, key: "http:GET /api/orders/{}" }
      kind: ConsumerEndpointUnmatched
      compat: Breaking
      impact: { class: NeedsInvestigation, reasons: [unresolved_candidates] }
tool_also: list_unresolved
expected_list_unresolved:
  contains:
    - consumer: { service: billing, ..., symbol: build_invoice }
      template: /v1/api/orders/{}
      reason: no_match
      candidates:
        - endpoint: { service: orders, key: "http:GET /api/orders/{}" }
        # Plain §7.4 matching fails (3 vs 2 segments); prefix
        # tolerance strips /v1/ and matches, so orders is the
```

The consumer is **unresolved** for `diff_contracts` (with `no_match` as
the reason), but the endpoint is **a could-match candidate** for
`list_unresolved` (because prefix tolerance WOULD match). The current
joiner conflates these two cases by binding in rule 6 with prefix
tolerance, which makes `diff_contracts` see the consumer as bound and
miss `ConsumerEndpointUnmatched`.

**Root cause.** `federation/contracts/joiner.rs::resolve_consumer`
applies the route matcher's prefix tolerance to every path (rules 3
and 6). §7.4 is explicit: "Prefix tolerance (rule 3 only, target
service known)". Rule 6 (target unknown) must NOT apply prefix tolerance.
The suppression at line ~1023-1029 ("pick first non-PrefixStripped
candidate") mitigates *preferring* a PrefixStripped match when a
non-stripped one exists — it does not *forbid* PrefixStripped matches
when they are the only ones. With the current implementation, when a
PrefixStripped match is the *only* match, the code returns it as the
resolution; the consumer is bound; `diff_contracts` never sees
`ConsumerEndpointUnmatched`.

**Fix shape.** Two parts, both in `joiner.rs`:

1. **Do not apply prefix tolerance in rule 6.** In the rule 6 match
   path, use the matcher with prefix tolerance DISABLED (or, equivalently,
   filter `PrefixStripped` results out before selecting). Rule 6
   becomes: "target unknown → for every other service, find the
   most-specific non-prefix-stripped match. None found → unresolved,
   reason `no_match`." This restores scenario 3's intended outcome.
2. **Preserve the could-match signal for `list_unresolved` (§9.7).**
   `coverage.unresolved_consumers` already exists; the `ContractIndex`
   surfaces the same data. The could-match computation in
   `contracts/diff.rs::could_match` (lines ~1699-1708) currently
   doesn't include prefix tolerance — it just compares target service
   and template. Fix it to use the matcher's prefix-stripped mode when
   computing could-match (caller-side, pure function, no live state
   involved): if `could_match(endpoint, consumer_template)` returns
   true when prefix tolerance is enabled, the endpoint IS a
   could-match candidate for that consumer.

This makes rule 6 return `no_match` when only prefix-stripped matches
exist, while preserving the prefix-stripped-could-match signal that
`list_unresolved` reports.

**Tests.**
- New discriminating test in `joiner_tests.rs`: synthetic fixture with
  consumer template `/v1/api/orders/{}` and provider `/api/orders/{}` —
  assert `resolution == Unresolved { reason: NoMatch }` AND
  `coverage.unresolved_consumers` contains the consumer with the
  endpoint as candidate.
- Existing test: prefix tolerance still resolves rule-3 consumers
  (e.g. scenario 11, 12 tests with `routes_prefix_is_applied_to_template`).
- `pr13_hermetic_precision_recall_over_t1_fixture` → `diff_precision ≥ 0.7`.

## Cross-cutting concerns

- **No schema version bumps.** No `FEDERATION_GRAPH_VERSION`,
  `PATH_FORMAT_VERSION`, or `CONTRACT_ANALYZER_REV` change. The sensor
  emission shape (`FieldRef` `chain`) is unchanged — we just emit fewer
  of them. The joiner `ConsumerResolution` shape is unchanged — same
  variants, just `reason: NoMatch` instead of a successful bind in the
  scenario-3 case.
- **No ground truth edits.** Restore was at commit 36f0dd9; all three
  fixes must be measurable against the restored ground truth without
  any further ground truth changes. `baseline.json` will be regenerated
  to the honest post-fix numbers at the end.
- **One commit per bug** so failures isolate. Each commit carries the
  new discriminating test(s) so a regression is caught by the test
  alone.
- **No `fixtures/contracts/ground_truth.yaml` edits** at any point
  during the fixes.
- **Sensors/AGENTS.md + federation/AGENTS.md** patterns: sensor changes
  stay in the sensor file, federation changes in `contracts/`. No
  reach-through to other layers.

## Test plan

For each bug:
- Add at least one new `#[test]` that fails on `main` and passes after
  the fix.
- Run the existing `cargo test` suite — must stay green (existing
  tests that asserted `CustomerEndpointUnmatched` firing will already
  pass; tests asserting the buggy behavior would need updating, but
  none were found in the sweep).
- Run `pr13_hermetic_precision_recall_over_t1_fixture` — assert
  metrics ≥ 0.7 per bug.
- Final `scripts/demo.sh --quick` §13.5 contracts phase must pass
  against the regenerated honest baseline.

After all three fixes:
- `baseline.json` regenerated to the honest numbers (single
  regeneration step).
- `pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture` and the
  scenario rows in `CONTRACT_FEDERATION_TRACKER.md` must now assert
  honest outcomes.
- Gate: `cargo test`, `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --check`, all `scripts/check-*.py`, `scripts/demo.sh --quick`.

## Out of scope

- Topics (PR 15 stretch) — deferred.
- CODEOWNERS (PR 17) — deferred.
- Generated-client matching by operationId (PR 18) — deferred.
- Other lines left un-laundered in the previous round of edits (e.g.
  the `Known` `MethodSpec` cap on `apply_limit` cursor — out of scope).
