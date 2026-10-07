# Mutation-Testing Credibility + Coverage Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the mutation score a number worth acting on, then drive it to a stated floor — with zero unclassified survivors in the code that turns source files into contract facts.

**Architecture:** Three phases in order, because you cannot drive down a number you cannot measure. Phase 1 repairs the measurement (wrong filters, no equivalent-mutant memory, 1–2h runtime). Phase 2 closes survivor *classes*, not individual mutants — one fixture per class kills a dozen. Phase 3 makes the harness a gate so the rate cannot silently regress. The harness already exists (`scripts/mutation-check.py`, 4 operators, `try/finally` restore); this plan extends it rather than replacing it.

**Tech Stack:** Python 3 (harness), Rust 2021 (tests under `tests/`, `src/**/*_tests.rs`), `cargo test` as the oracle. No new crates.

**Spec:** `docs/superpowers/plans/2026-10-07-remaining-work.md` Task 9 (the risk-ordered survivor list this plan replaces) and `scripts/mutation-check.py`'s docstring (the measured baseline).

## Global Constraints

- **The oracle is `cargo test`, and only `cargo test`.** A mutant is *killed* iff the scoped suite exits non-zero. Do not weaken a test to make a mutant survive less — that is inverting the instrument.
- **Never leave a mutation in production code.** Two agents already did (`src/server/tools/handlers/metrics.rs` carried a `==` → `!=` for a full commit cycle). Every mutation must be reverted by **editing the line back**, never by `git checkout`/`git reset`. Before every commit run `git diff --stat src/` and confirm every remaining change is intended.
- **No destructive git.** `git reset --hard`, `git checkout --`, `git stash`, `git clean`, `git restore` are forbidden. (Three parallel subagents have destroyed work with these.)
- A **survivor must be classified** — `equivalent` (with the reason) or `gap` (with the test that would kill it). An unclassified survivor is a finding, not a statistic.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must both be clean at every commit.
- PRs target `dev`. No version bumps (`AGENTS.md`).

## Review Focus

1. **A mutation score can be gamed by narrowing the test filter.** If a task "improves" a number by removing a target or a test, that is a regression. Every score change must be explained by killed mutants or by an explicit filter correction recorded in `mutation-baseline.json`.
2. **Equivalent mutants are invisible without a durable record.** If the same mutant is re-triaged as `equivalent` every run, the number will never converge and reviewers cannot audit the calls. The triage file is the artefact; a chat message is not.
3. **A fast harness that lies is worse than a slow one.** Parallelism must not change which mutants are killed — same mutants, same oracle, same scoping. Any speed change needs a before/after score equality check.
4. **Killing a mutant with a test that cannot fail is the same as not killing it.** Several tests written for the earlier fixes pin the *shape* of the fix but not the surrounding behaviour; verify each new test by applying the mutation and watching it fail, not by watching the counter go up.
5. **`graph/mod.rs` is 3669 lines measured through a 9-test filter.** Any claim about its coverage is a claim about the filter, not the code. Fix the measurement before drawing conclusions.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `scripts/mutation-check.py` | Harness: mutate → run oracle → classify. Gains parallelism, per-target filter assertions, operator set, and triage lookup |
| `scripts/mutation-baseline.json` | **NEW.** Durable record: per-target counts + the equivalent-mutant triage (`path:line:op` → reason) so decisions persist across runs |
| `tests/parsers_adversarial.rs` | Proto / Avro / JSON-Schema byte-boundary and shape fixtures |
| `tests/graphql_resolution.rs` | GraphQL parser byte-boundary fixtures |
| `src/server/graph/mod.rs` | `#[cfg(test)]` only — BFS depth-gate fixtures |
| `src/server/federation/contracts/joiner_tests.rs` | Joiner boundary fixtures (zero/two/one candidate, foreign host) |
| `src/server/federation/contracts/field_join.rs` | `#[cfg(test)]` only — `is_suffix` and unique-suffix fixtures |
| `.github/workflows/ci.yml` | The gate: floor check on `mutation-baseline.json` |

---

## Phase 1 — Make the number mean something

### Task 1: Establish the true baseline on current HEAD

The last measurement predates Task 8 (idioms → data) and Task 9 (32 mutants killed). The score has certainly moved. Everything downstream depends on the real number.

**Files:**
- Modify: `scripts/mutation-check.py` (add `--json` output)
- Create: `scripts/mutation-baseline.json`

**Interfaces:**
- Produces: `mutation-baseline.json` shape — `{"generated_at": …, "targets": [{path, filter, kind, mutants, killed, survived}], "triage": {"<path>:<line>:<op>": "equivalent: <reason>"}}` — which every later task reads and writes.

- [ ] **Step 1: add machine-readable output to the harness.** In `scripts/mutation-check.py`, after the per-target loop, write the summary to `scripts/mutation-baseline.json` and print a one-line digest. Do not change what is mutated or how the oracle runs — this is output plumbing only.

```python
# at the end of the main loop, replacing the current print block
import json, datetime
doc = {
    "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    "operators": [label for _, _, label in MUTATIONS],
    "targets": results,           # [{path, filter, kind, mutants, killed, survived}]
    "triage": load_triage(),      # from scripts/mutation-baseline.json if present
}
with open(os.path.join(ROOT, "scripts", "mutation-baseline.json"), "w") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
print("MUTATION_SUMMARY "
      f"{sum(r['killed'] for r in results)}/{sum(r['mutants'] for r in results)} killed, "
      f"{sum(r['survived'] for r in results)} survived")
```

- [ ] **Step 2: run it and record the baseline.**

Run: `nohup setsid python3 scripts/mutation-check.py > /tmp/mut-baseline.log 2>&1 &` then wait (expect 60–120 min).
Expected: `MUTATION_SUMMARY <killed>/<mutants> killed, <survived> survived`, and `scripts/mutation-baseline.json` written.

- [ ] **Step 3: sanity-check the number against the code, not against the old score.** For each target, read the `survived` count and answer one question: *is this count explained by the filter, by equivalent mutants, or by real gaps?* Write the answer into the task notes. If a target's count looks wrong, that is Task 2's job — do not fix it here.

- [ ] **Step 4: commit.**

```bash
git add scripts/mutation-check.py scripts/mutation-baseline.json
git commit -m "test(mutation): machine-readable baseline on current HEAD"
```

---

### Task 2: Fix the filters so each target measures what it claims

Two targets are measured through filters that cannot reach the code.

**Evidence (from the last run):**
- `src/server/graph/mod.rs` — **3669 lines**, filter `sensor_coexistence` (9 integration tests that only drive `sensor_owner_of`). 35 of 37 mutants survive. This is a filter artefact.
- `src/server/federation/contracts/field_join.rs` — **1107 lines**, filter `joiner_tests`. The tests that pin `is_suffix` live in `field_join::tests::is_suffix_*`, which `joiner_tests` never runs. Those mutants survive *despite* having tests.

**Files:**
- Modify: `scripts/mutation-check.py` (`TARGETS` + a filter-reachability assertion)

**Interfaces:**
- Consumes: `scripts/mutation-baseline.json` from Task 1.
- Produces: corrected `TARGETS`, plus a `--check-filters` mode asserting each filter actually runs tests that exercise the target file.

- [ ] **Step 1: write the failing reachability check.** Add a `--check-filters` mode that, per target, runs the scoped suite and asserts it is non-trivially exercising the file. The cheapest honest proxy: the filter must run **at least one test whose name or module path mentions the target's primary symbol family**, and the suite must not be empty.

```python
def check_filters():
    """A filter that runs no test touching the target file makes every
    mutant survive, which reads as 'no coverage' when it means 'wrong
    filter'. Fail loudly instead."""
    problems = []
    for relpath, filt, kind in TARGETS:
        args = [CARGO, "test", "--lib" if kind == "lib" else "--test", filt, "--", "--list"]
        r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=300)
        listed = [l for l in r.stdout.splitlines() if ": test" in l]
        if not listed:
            problems.append(f"{relpath}: filter '{filt}' lists zero tests")
            continue
        stem = os.path.basename(relpath).rsplit(".", 1)[0]
        module_guess = stem.replace(".rs", "")
        if not any(module_guess in l or "tests::" in l for l in listed):
            problems.append(
                f"{relpath}: filter '{filt}' lists {len(listed)} tests but none "
                f"appear to target '{module_guess}' — mutants will all survive"
            )
    return problems
```

- [ ] **Step 2: run it and confirm it FAILS on the two known-bad targets.**

Run: `python3 scripts/mutation-check.py --check-filters`
Expected: FAIL listing `graph/mod.rs: filter 'sensor_coexistence' …` and `field_join.rs: filter 'joiner_tests' …`.

- [ ] **Step 3: fix the two filters.**

```python
TARGETS = [
    ...
    # graph/mod.rs is 3.6k lines. `sensor_coexistence` only reaches
    # `sensor_owner_of`; the BFS/entry-point code needs the lib suite.
    ("src/server/graph/mod.rs", "graph", "lib"),
    # field_join's `is_suffix` tests live in `field_join::tests`, which
    # the `joiner_tests` filter never runs.
    ("src/server/federation/contracts/field_join.rs", "field_join::tests", "lib"),
    ...
]
```

- [ ] **Step 4: run `--check-filters` and confirm it passes.**

Run: `python3 scripts/mutation-check.py --check-filters`
Expected: `PASS`, zero problems.

- [ ] **Step 5: re-run the full measurement and compare.** Expect `graph/mod.rs`'s survivor count to change sharply (its true coverage is much higher than 5%) and `field_join.rs`'s to drop. **Record both deltas in `mutation-baseline.json`'s `triage` as `filter_correction: …` notes** so a future reader knows the number moved because the measurement changed, not the code.

- [ ] **Step 6: commit.**

```bash
git add scripts/mutation-check.py scripts/mutation-baseline.json
git commit -m "test(mutation): fix filters that could not reach their target files"
```

---

### Task 3: Durable equivalent-mutant triage

Right now "equivalent" lives in a chat message from a previous agent. It must live in the repo, or every run re-opens the same questions and the achievable ceiling stays invisible.

**Files:**
- Modify: `scripts/mutation-check.py` (`load_triage`, skip/annotate pre-classified mutants)
- Modify: `scripts/mutation-baseline.json` (the `triage` map)

**Interfaces:**
- Consumes: the `triage` map from Task 1.
- Produces: `mutation-baseline.json["triage"]` with keys `<relpath>:<line>:<op>` and values `equivalent: <reason>` or `gap: <proposed test>`. Later tasks append to it.

- [ ] **Step 1: write the failing test** — a mutant already classified `equivalent` must not be re-reported as an open survivor.

```python
# in scripts/mutation-check.py, unit-checked via --self-test
def classify_survivor(relpath, line, op, triage):
    key = f"{relpath}:{line}:{op}"
    return triage.get(key, "gap: unclassified")
```

- [ ] **Step 2: run `--self-test` and confirm it fails** (no `--self-test` mode yet).

Run: `python3 scripts/mutation-check.py --self-test`
Expected: FAIL — mode not implemented.

- [ ] **Step 3: implement `--self-test` + triage lookup.** Survivors classified `equivalent` are reported under a separate `equivalent` heading and **do not count toward the gap rate**; survivors classified `gap` carry their proposed test in the report.

- [ ] **Step 4: seed the triage from the known-equivalent mutants** already identified by hand. At minimum these, from the last run's analysis:

```json
{
  "src/server/ingest/resolve.rs:648:ge->gt": "equivalent: max_edges budget enforced by four guards; flipping one leaves three holding the cap",
  "src/server/ingest/resolve.rs:661:ge->gt": "equivalent: same four-guard budget",
  "src/server/ingest/resolve.rs:665:ge->gt": "equivalent: same four-guard budget",
  "src/server/ingest/resolve.rs:683:ge->gt": "equivalent: same four-guard budget",
  "src/server/audit.rs:262:ge->gt": "equivalent: single break site; the next iteration exits on the empty list anyway",
  "src/server/sensors/payload_schema.rs:645:and->or": "equivalent: both branches consume the literal \"map\" and read the same field name",
  "src/server/sensors/payload_schema.rs:696:eq->ne": "equivalent: third arm of an || chain; the first two cover non-underscore",
  "src/server/sensors/payload_schema.rs:723:eq->ne": "equivalent: newline counter; no fixture asserts that exact line count",
  "src/server/sensors/payload_schema.rs:735:eq->ne": "equivalent: || with 736; flipping one arm still exits via the other",
  "src/server/sensors/payload_schema.rs:736:eq->ne": "equivalent: reached only when 735 is true; loop exits either way",
  "src/server/federation/contracts/field_join.rs:599:le->lt": "equivalent: confidence tie-break; the fixture only ever has equal confidences"
}
```

- [ ] **Step 5: verify.** Run `--self-test` (PASS), then the full run; confirm the reported gap count drops by exactly the number of seeded entries.

- [ ] **Step 6: commit.**

```bash
git add scripts/mutation-check.py scripts/mutation-baseline.json
git commit -m "test(mutation): durable equivalent-mutant triage"
```

---

### Task 4: Make it fast enough to be a gate

The harness runs `cargo test` once per mutant serially: 291 mutants × a test invocation. That is why it takes 1–2 hours. It cannot be a CI gate until it is minutes.

**Files:**
- Modify: `scripts/mutation-check.py`

**Interfaces:**
- Consumes: `mutation-baseline.json`.
- Produces: identical scores, shorter wall-clock. Any score difference is a bug in this task.

- [ ] **Step 1: write the failing test** — a parallel run must produce a byte-identical `mutation-baseline.json` (minus `generated_at`) to a serial run.

```python
# --self-test addition
def parallel_matches_serial():
    """Parallelism must not change which mutants die."""
    a = run_mutations(workers=1)
    b = run_mutations(workers=os.cpu_count() or 4)
    a.pop("generated_at", None); b.pop("generated_at", None)
    return a == b
```

- [ ] **Step 2: confirm it fails** (no parallelism exists yet).

Run: `python3 scripts/mutation-check.py --self-test`
Expected: FAIL — `run_mutations` takes no `workers` argument.

- [ ] **Step 3: implement.** Two speedups, both score-preserving:
  - **Skip mutants the triage already marks `equivalent`** — no oracle run needed.
  - **Run mutants for one target in parallel worker processes**, each with its own `CARGO_TARGET_DIR` so `cargo`'s lock does not serialise them. Keep the `try/finally` restore per worker; a killed worker must never leave a mutated file behind.

- [ ] **Step 4: verify score equality.** Run `--self-test` (PASS), then one full run; the per-target `killed`/`survived` must match Task 3's baseline exactly. Record wall-clock before and after in the commit message.

- [ ] **Step 5: commit.**

```bash
git add scripts/mutation-check.py
git commit -m "test(mutation): parallel workers + triage skip; score-preserving speedup"
```

---

## Phase 2 — Close survivor classes

Each of these is a *class* of mutants, not one mutant. One well-chosen fixture kills a dozen. Verify each by applying the mutation by hand and watching the test fail.

### Task 5: Parser byte-boundary class (the highest risk)

`graphql_consumer_sensor.rs` and `payload_schema.rs` are the **only** path from a source file to a `ContractFact`. One byte off silently drops fields.

**Files:**
- Modify: `tests/parsers_adversarial.rs`
- Modify: `tests/graphql_resolution.rs`

**Interfaces:**
- Consumes: the `gap:` entries in `mutation-baseline.json["triage"]`.
- Produces: tests that kill those entries; each entry is removed from `triage` (or re-marked `killed`) as it falls.

- [ ] **Step 1: pick one survivor class and write the fixture.** Start with the `while i < bytes.len() && <cond>` short-circuits in `graphql_consumer_sensor.rs` — 24 mutants, one shape. The fixture must drive the loop to **end-of-buffer while the second condition is still true**, which is what makes `||` overrun.

```rust
#[test]
fn t9_short_circuit_selection_loop_stops_at_end_of_buffer() {
    // `&&` -> `||` on the selection-set loop lets the parser run past
    // EOF. A body that ends mid-selection is the input that matters.
    let fields = top_level_fields("query { orders { id ");
    assert!(fields.iter().any(|(n, _)| n == "orders"));
}
```

- [ ] **Step 2: run and confirm the mutant is killed.** Apply the `&&` → `||` at the relevant line by hand, run `cargo test --test graphql_resolution <name>`, expect FAIL (panic or wrong output). **Revert the line by hand.** Confirm the test passes.

- [ ] **Step 3: repeat per class**, in this order (highest mutant-count per fixture first):
  1. `while i < bytes.len() && …` short-circuits — `graphql_consumer_sensor.rs` (24 mutants)
  2. `bytes[i] == b'X'` false-branch — `graphql_consumer_sensor.rs` (28) and `payload_schema.rs` (10): a fixture per byte `(`, `)`, `{`, `}`, `$`, `\n`, `;`, `]`, `_`
  3. Avro/JSON-Schema false branches — `payload_schema.rs` (`:67` record gate, `:149` union nullability, `:156`/`:159` nested-type, `:230-235` discriminators)
  4. `scan_fragment_spread` end-of-buffer — `graphql_consumer_sensor.rs` (~9)
  5. Line-number assertions — `graphql_consumer_sensor.rs` (`:230`, `:246`, `:398`): today no test asserts an exact line number, so the newline-counting mutations cannot die

- [ ] **Step 4: update `mutation-baseline.json["triage"]`** — delete each entry as its killer lands.

- [ ] **Step 5: verify.** `cargo test --test parsers_adversarial --test graphql_resolution` all pass; a full mutation run shows the two parser targets' survivor counts down by the class sizes above.

- [ ] **Step 6: commit.**

```bash
git add tests/parsers_adversarial.rs tests/graphql_resolution.rs scripts/mutation-baseline.json
git commit -m "test: kill parser byte-boundary mutant classes"
```

---

### Task 6: Joiner boundary class

`consumer_protocol.rs` (10 survivors) and `field_join.rs` (14). These are decision-boundary mutants: the tests only ever drive the *true* branch.

**Files:**
- Modify: `src/server/federation/contracts/joiner_tests.rs`
- Modify: `src/server/federation/contracts/field_join.rs` (`#[cfg(test)]` only)

**Interfaces:**
- Consumes: `triage` entries for those two files.
- Produces: boundary tests; `field_join::tests::is_suffix_*` extended so Task 2's new `field_join::tests` filter actually kills its mutants.

- [ ] **Step 1: write the failing fixtures.** The gap list names them precisely — for each, the fixture must reach the *false* branch:

```rust
#[test]
fn ws_consumer_with_a_non_post_method_is_not_a_graphql_route() {
    // `is_graphql_route` requires `POST | Unknown`. A GET provider on
    // "/graphql" must not be treated as the GraphQL route owner.
    // Kills the `&& matches!(method, …)` mutation at consumer_protocol.rs:232.
    let provider = graphql_provider_node("orders", "/graphql", HttpMethod::Get);
    let consumer = ws_consumer_node("billing", "orders.internal", "/graphql", 20);
    let cfg = ws_config("orders", "orders.internal");
    let out = ContractJoiner::run(&[provider, consumer.clone()], &[], &cfg);
    let cid = GlobalId::parse(&consumer.id).expect("global id parse");
    let res = out.index.consumers.get(&cid).expect("consumer resolution");
    assert!(
        !matches!(res.target, Some(ConsumerTarget::Binds { .. })),
        "a GET provider must not satisfy the GraphQL route-owner rule: {:?}",
        res.target
    );
}

#[test]
fn field_join_unique_suffix_guard_refuses_two_candidates() {
    // `suffix_matches.len() == 1` — two ResponseFields sharing a path
    // suffix must not silently bind to the first. Kills the `==` -> `!=`
    // at field_join.rs:449.
    let mut fields = BTreeMap::new();
    fields.insert(path(&["a", "total"]), field(TypeDesc::Number, true, false));
    fields.insert(path(&["b", "total"]), field(TypeDesc::Number, true, false));
    let reads = vec![read_of("total")];
    let bound = join_reads_to_fields(&reads, &fields);
    assert!(
        bound.iter().all(|b| b.is_unknown()),
        "an ambiguous suffix must stay unbound, not pick the first: {bound:?}"
    );
}

#[test]
fn is_suffix_rejects_a_partial_byte_match() {
    // `fp[fp.len()-read.len()..] == read[..]` is a byte-suffix check;
    // a path that is a prefix but not a suffix must not match.
    // Kills field_join.rs:652 — these live in `field_join::tests`,
    // which Task 2 made reachable.
    assert!(is_suffix("orders.customer_id", "customer_id"));
    assert!(
        !is_suffix("customer_id_orders", "customer_id"),
        "a prefix match must not be accepted as a suffix match"
    );
    assert!(!is_suffix("cust", "customer_id"), "a shorter fp cannot suffix-match");
}
```

- [ ] **Step 2–4:** for each, apply the mutation by hand → watch FAIL → revert by hand → watch PASS. Update `triage`.

- [ ] **Step 5: verify.** `cargo test --lib joiner_tests --lib field_join` green; mutation run shows both targets down.

- [ ] **Step 6: commit.**

```bash
git add src/server/federation/contracts/joiner_tests.rs src/server/federation/contracts/field_join.rs scripts/mutation-baseline.json
git commit -m "test: kill joiner and field_join decision-boundary mutants"
```

---

### Task 7: BFS depth-gate class

Four `>=` → `>` mutants, one per BFS variant in `graph/mod.rs` (`traverse`, `subgraph_around`, `bfs_from`, and the inner `next_depth` check). A surviving mutant means "one extra hop", which for a blast-radius tool is a wrong blast radius.

**Files:**
- Modify: `src/server/graph/mod.rs` (`#[cfg(test)]` only)

**Interfaces:**
- Consumes: Task 2's corrected `graph` lib filter.
- Produces: one fixture per BFS variant, each asserting the exact node set for a chain of length 3.

- [ ] **Step 1: write the failing fixture** (a chain `a → b → c → d`, depth window `[1,2]` must return exactly `{b, c}`):

```rust
#[test]
fn traverse_depth_window_returns_exactly_the_window() {
    // `if current_depth >= max_depth { continue; }` mutated to `>`
    // walks one extra hop and returns `d` too.
    let g = chain_of(4);            // a -> b -> c -> d
    let got = g.traverse(&["a"], /*min*/1, /*max*/2, …);
    assert_eq!(names(&got), vec!["b", "c"]);
}
```

- [ ] **Step 2: confirm it kills each of the four mutants** — apply each `>=` → `>` in turn, expect FAIL, revert by hand.

- [ ] **Step 3: verify + commit.**

```bash
cargo test --lib graph
git add src/server/graph/mod.rs scripts/mutation-baseline.json
git commit -m "test: pin BFS depth windows so an off-by-one is caught"
```

---

## Phase 3 — Make it a gate

### Task 8: Floor check in CI

**Files:**
- Modify: `scripts/mutation-check.py` (`--check-floor`)
- Modify: `scripts/mutation-baseline.json` (record `floor`)
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: write the failing check.**

```python
# --check-floor: read mutation-baseline.json, fail if the gap rate rose
def check_floor():
    doc = json.load(open(os.path.join(ROOT, "scripts", "mutation-baseline.json")))
    floor = doc.get("floor", {"global_killed_pct": 70, "per_target": {
        "src/server/sensors/graphql_consumer_sensor.rs": 85,
        "src/server/sensors/payload_schema.rs": 85,
        "src/server/federation/contracts/joiner/consumer_protocol.rs": 85,
        "src/server/federation/contracts/field_join.rs": 85,
    }})
    bad = []
    for t in doc["targets"]:
        pct = 100.0 * t["killed"] / max(t["mutants"], 1)
        want = floor["per_target"].get(t["path"])
        if want is not None and pct < want:
            bad.append(f"{t['path']}: {pct:.0f}% < required {want}%")
    total = 100.0 * sum(t["killed"] for t in doc["targets"]) / max(sum(t["mutants"] for t in doc["targets"]), 1)
    if total < floor["global_killed_pct"]:
        bad.append(f"global {total:.0f}% < required {floor['global_killed_pct']}%")
    return bad
```

- [ ] **Step 2: confirm it FAILS** against the current baseline (the parser targets are well under 85%).

Run: `python3 scripts/mutation-check.py --check-floor`
Expected: FAIL listing the four per-target violations.

- [ ] **Step 3: make it pass** by re-running Tasks 5–7 until the floors are met, or — if a target genuinely cannot reach its floor — lowering that target's floor **in `mutation-baseline.json` with a written reason next to it**. A silent floor reduction is the failure mode this gate exists to prevent.

- [ ] **Step 4: wire CI.** In the lane that already runs `check-mod-resolution.sh`:

```yaml
      - name: Mutation floors
        shell: bash
        run: python3 scripts/mutation-check.py --check-floor
```

Note in the workflow comment that this checks the **recorded** baseline; the full mutation run stays a periodic/manual job because of its wall-clock cost, and the baseline is refreshed when the suite changes materially.

- [ ] **Step 5: verify + commit.**

```bash
python3 scripts/mutation-check.py --check-floor   # PASS
git add scripts/mutation-check.py scripts/mutation-baseline.json .github/workflows/ci.yml
git commit -m "ci: mutation-score floors as a gate"
```

---

## Targets

Stated up front so the last task has a definition of done:

| target | required killed | why |
|---|---|---|
| `graphql_consumer_sensor.rs` | **85%** | only path from `.ts`/`.js` to a `ContractFact` |
| `payload_schema.rs` | **85%** | only path from proto/Avro/JSON-Schema to schema fields |
| `consumer_protocol.rs` | **85%** | decides `Binds` vs `Unresolved` — invented bindings are the worst failure |
| `field_join.rs` | **85%** | decides which schema field a read binds to |
| `graph/mod.rs` | 60% | 3669 lines, mixed concerns; 60% is honest for breadth code |
| global | **70%** | |

Every survivor above those floors must be classified in `mutation-baseline.json["triage"]` with a reason. **An unclassified survivor blocks the task.**

## Out of scope

- New mutation operators beyond the existing four (`>=`,`<=`,`&&`,`==`). Statement-deletion operators would help but change what the score *means*, and the number must stay comparable to the recorded baseline. Revisit only after the floors hold.
- Rewriting the parsers. The plan closes verification gaps, not design ones.
- `tests/real_federation/*`, which is red for unrelated reasons (see the 2026-10-07 plan).
