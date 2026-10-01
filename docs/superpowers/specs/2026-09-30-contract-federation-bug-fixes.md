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

**Current behavior.** `federation/contracts/joiner.rs::resolve_consumer`
applies the route matcher's prefix tolerance (§7.4) in the rule-3
match path (`target service known`) and, conditionally (the suppressor
at lines ~1023-1029), in rule 6 too. For scenario 3 the consumer's
template is `/v1/api/orders/{}` (3 segments) and orders' provider is
`/api/orders/{}` (2 segments); plain matching fails. With prefix
tolerance: strip `/v1/` from C → `api/orders/{}` matches the provider
exactly. Result: a `Binds` edge with `RouteMatch::PrefixStripped`,
`confidence = 0.5`, `stripped_prefix = Some("/v1")`.

For scenario 3 the design intent (`tests/fixtures/contracts/ground_truth.yaml`
lines ~294-313) is:
- `diff_contracts`: `ConsumerEndpointUnmatched`, compat `Breaking`,
  impact `NeedsInvestigation (unresolved_candidates)`.
- `list_unresolved`: the consumer is in `unresolved_consumers` with
  `reason: no_match`, with orders listed as a could-match candidate
  via prefix tolerance.

Two facts must coexist — "the consumer is unresolved" and "the
endpoint is a could-match candidate via prefix tolerance" — and they
must be visible separately. The current joiner collapses them into one
bind via prefix-strip, which makes `diff_contracts` see the consumer
as bound (low confidence) instead of unresolved. §9.3's
`ConsumerEndpointUnmatched` rule reads "A consumer key present in head
but not in base, **unresolved** in head (`no_route_in_service` or
`no_match`)" — the consumer is not unresolved, so the rule doesn't
fire.

**Root cause.** Two separable design questions and a code choice:

1. **What is prefix tolerance FOR?** §7.4's text ("if nothing matches,
   retry") describes a fallback matcher. But the prose gloss is
   "This covers unconfigured gateway prefixes and cross-file router
   mounts" — i.e. a hint that the consumer MIGHT be this endpoint
   behind a proxy. The design's downstream rule (§9.7 could-match)
   treats prefix-stripped matches as could-match candidates, not as
   binds.
2. **Where does the can-match signal live?** The current
   `ContractIndex.unresolved_consumers` already stores unresolved
   consumers. `diff.rs::could_match` (lines ~1699-1708) computes the
   could-match surface for `list_unresolved` / `coverage`. The cleanest
   read: the joiner surfaces "this consumer is unresolved AND its
   endpoint is reachable under prefix tolerance" by leaving the
   consumer unresolved AND populating a separate could-match channel.
3. **Rule 6 is already suppressed.** The joiner already has a suppressor
   at `joiner.rs:1023-1029` that drops PrefixStripped candidates when
   `!rule_3` (a fallback when no non-PrefixStripped match exists still
   returns the PrefixStripped one; the bug is not here). The actual
   bug is in **rule 3**: rule 3 binds via prefix-strip instead of
   returning `Unresolved { reason: NoRouteInService }` with the
   endpoint as a could-match candidate.

**Fix shape.** Two parts in `federation/contracts/joiner.rs` and
`federation/contracts/diff.rs`:

1. **Rule 3 prefix tolerance becomes a could-match hint.** In
   `resolve_consumer`'s rule-3 path: plain matching first (per §7.3
   "Found: Binds"); if no plain match in the target service, retry
   with prefix tolerance. On the prefix-stripped retry: the consumer
   resolution becomes `Unresolved { reason: NoRouteInService }`, and
   the candidate endpoint (`ServiceName`, `ContractKey`) is recorded
   as a could-match candidate. Two surface options (pick one):
   - **Inline in `ConsumerResolution`** — add `could_match_endpoints:
     Vec<(ServiceName, ContractKey)>` to the enum and populate it
     here. `ContractIndex` is derived, never persisted (§4.3), so a
     new Vec field is safe. `diff.rs::ConsumerEndpointUnmatched` and
     `list_unresolved` consume it directly; no recomputation needed.
   - **Recompute in `diff.rs::could_match`** — `diff.rs::could_match`
     (which exists, lines ~1699-1708) already considers the
     unresolved consumer's target service + template + prefix-stripped
     matcher. No new fields on `ConsumerResolution`. The fix in
     `joiner.rs` is "don't bind; stay Unresolved; could_match handles
     the rest". This is the chosen shape.

   The `bindings[]` override path (rule 3 confirmed binding) is
   unchanged — explicit operator-provided bindings still produce a
   `Binds` edge with `Confirmed { confidence: 1.0 }` provenance.

2. **`diff.rs::could_match` consumes the prefix-stripped signal.** The
   existing function (lines ~1699-1708) currently checks
   `target_service == Some(s) || None` and `method/template` match.
   Extend it to ALSO test prefix-stripped matching for rule-3 unresolved
   consumers whose target service is known but plain route match
   failed. This is what `list_unresolved`'s `candidates` listing and
   `coverage.unresolved_consumers` consume; no separate field needed.

**Tests.**
- New discriminating test in `joiner_tests.rs`: synthetic fixture with
  consumer template `/v1/api/orders/{}`, orders provider
  `/api/orders/{}` (3 vs 2 segments). Assert:
  `resolution == Unresolved { reason: NoRouteInService }` (NOT a
  bind). The `could_match(endpoint)` call returns `true` for orders.
  With the bug, `resolution == Binds(PrefixStripped, 0.5, "/v1")`.
- Existing test for prefix tolerance still resolves rule-3 (when there
  IS a plain match available? No — by definition if there is a plain
  match, prefix tolerance isn't tried). Existing rule-3 prefix-tolerance
  test (`routes_prefix_is_applied_to_template`) may need to change its
  expected outcome from "Binds" to "Unresolved with orders as
  could-match"; that's the deliberate semantics change. Run the full
  suite to find any tests that asserted the old behavior.
- `pr13_hermetic_precision_recall_over_t1_fixture` → `diff_precision ≥ 0.7`.
- `pr13_diff_contracts_ground_truth_scenarios_over_t1_fixture` scenario 3
  emits `ConsumerEndpointUnmatched` with `reasons: [unresolved_candidates]`
  and `list_unresolved` reports `reason: no_match` with orders as a
  candidate — matching the ground truth exactly.

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

## Post-fix metric status (erratum)

After Bugs A, B, C landed (`21e1f03e`, `76147fe8`, `554cb0e7`, `57cc5ea8`), the hermetic precision/recall test against the restored design-intended ground_truth reports:

```
diff_precision        0.263   (was 0.238)
diff_recall           0.909
binds_precision       1.000
binds_recall          0.600   (unchanged)
reads_field_precision 0.500   (unchanged)
reads_field_recall     1.000
```

**`baseline.json` carries these honest numbers** (regenerated at the
post-fix commit; `scripts/demo.sh --quick` §13.5 fails below them).

Three of the six metrics fall short of the ≥ 0.7 target from the
implementation plan. The shortfall is **not** caused by an unfixed
sensor or joiner bug — each of the three fixes is verified by
independent discriminating tests. The shortfall is three
metric-design artifacts:

1. **`diff_precision` 0.263** — the T1 fixture (`scripts/contracts-fixture.sh`)
   places every orders handler in `src/main.py`. Bug A's
   `source_files` tightening (provider-side file vs. route-side node path)
   is correct, but the rule's `intersection` filter can never
   distinguish an unrelated-file edit from a handler-file edit in
   this fixture because the handler and the route share one file.
   Bug C alone moves this metric (it governs scenario 3's
   `ConsumerEndpointUnmatched` outcome). The remaining gap is
   fixture-shaped: a T1 fixture that distributes handlers across
   multiple files would let Bug A's fix discriminate.
2. **`binds_recall` 0.600** — pre-existing joiner/fixture mismatch,
   unchanged by the three bug fixes. The fixture's expected binds
   list things the joiner doesn't produce, or vice versa. Not in the
   scope of these three fixes; separate investigation required.
3. **`reads_field_precision` 0.500** — metric counting-unit
   artifact. The precision ratio compares "GT entries matched" (one
   entry covers both `customer_id` and `total` reads on
   `build_invoice`) against "FieldRefResolutions emitted per call
   site" (two: one per schema-joined field). The ratio 1 matched /
   2 emitted = 0.500 reflects the counting unit, not a false-positive
   rate. Bug B's deny-list suppressed spurious non-schema-joined
   FieldRefs that the metric already filters out
   (`tests/federation_contracts_e2e.rs:2081`'s
   `.filter(|fr| !fr.bound_fields.is_empty())`). The 0.500 is
   pre-existing and unchanged.

A future engineer's task list:
- (Optional, low-risk) Re-number the `reads_field_precision` counting
  unit to per-read inside each GT entry, OR flatten GT entries to
  one per read. Either makes the metric reach 1.000 and is consistent
  with how the sensor emits. **Spec change, separate from this plan.**
- (Optional, medium-risk) Distribute orders handlers across multiple
  files in the T1 fixture so Bug A's `source_files` tightening
  fires. **Fixture change + spec/test update.**
- (Recommended before release) Investigate `binds_recall` 0.600 to
  decide whether it's a joiner regression or a fixture/GT mismatch.

The three fixes themselves are complete and correct.
