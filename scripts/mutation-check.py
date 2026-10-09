#!/usr/bin/env python3
"""Targeted mutation testing: does the suite actually catch behaviour changes?

Run: python3 scripts/mutation-check.py

The guards in `tests/` check *shape* — that a module is declared, that a
test attribute is attached, that a documented knob is read. None of them
can see a function that is called and does the wrong thing. This measures
that directly: it changes behaviour and asks whether the suite notices.

The first run over the original five files scored 6/17 — eleven behaviours
with no test pinning them, including the BFS depth gate underneath
`get_blast_radius` and `get_call_chain`. Fixing what that exposed turned
up a real bug: `{"depth":{"min":2,...}}` returned nothing at all, because
the walk-expansion gate reused the collection range and stopped before it
could reach `min`. It also caught a test *this harness prompted* that
passed for the wrong reason — its fixture tripped an earlier suppression,
so the filter under test never ran.

That batch now scores 13/17. The four survivors in `resolve.rs` are
equivalent mutants: the `max_edges` budget is enforced by four separate
guards, so flipping any one leaves the other three holding the cap.

The command-center contract batch (Tasks 2–6) brought five new files
under the harness. Adding the `==`→`!=` mutation operator to bite the
byte-comparisons the parsers depend on produced **291 mutations
total, 159 SURVIVED, 132 caught** across the ten TARGETS. The
`max_edges`-style pattern from `resolve.rs` (a guard duplicated four
times) is not the only equivalent-mutant class here — the depth gates
under `get_blast_radius` / `get_call_chain` are similar — but most of
the new survivors are **real coverage gaps**, not equivalent
mutants. The detailed per-survivor classification is the body of the
most recent run's `=== N mutations applied ===` summary; the
recommended follow-ups for Task 9 are the ones that pin the
behaviour back.

The second batch's tests (`tests/sensor_coexistence.rs`,
`tests/parsers_adversarial.rs`, the amended `tests/graphql_resolution.rs`,
and the joiner additions under
`src/server/federation/contracts/joiner_tests.rs`) each watched its own
fix fail first — but that only proves the test catches the bug it was
written for. Mutation testing measures whether the surrounding
behaviour is pinned. `scripts/mutation-check.py` re-runs are
idempotent: the production file is rewritten between mutations and
restored in a `finally:` block, so an interrupted run cannot leave the
tree in a half-mutated state.

Applies one small semantic mutation at a time to a production file, runs a
scoped test subset, and records whether the suite noticed. A mutation that
SURVIVES means that behaviour is not pinned by any test.
"""
import json, re, subprocess, sys, os, shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARGO = shutil.which("cargo") or os.path.expanduser("~/.cargo/bin/cargo")

# (path, test_filter, kind)
#   kind "lib"  → cargo test --lib  <test_filter>     (unit / cfg(test) tests in src/)
#   kind "test" → cargo test --test <test_filter>     (integration tests in tests/)
TARGETS = [
    # ── original batch ────────────────────────────────────────────────
    ("src/server/ingest/resolve.rs", "resolve", "lib"),
    ("src/server/tools/handlers/metrics.rs", "metrics", "lib"),
    ("src/server/audit.rs", "audit", "lib"),
    ("src/server/query/executor.rs", "executor", "lib"),
    ("src/server/sensors/http_sensor.rs", "http_sensor", "lib"),
    # ── command-center contract batch (Tasks 2–6) ─────────────────────
    # Task 5: proto/avro/json-schema payload parsers (integration suite)
    ("src/server/sensors/payload_schema.rs", "parsers_adversarial", "test"),
    # Task 3: WebSocket consumer joiner (host evidence + ambiguity refusal)
    ("src/server/federation/contracts/joiner/consumer_protocol.rs", "joiner_tests", "lib"),
    # Task 4: field_join schema keying must match build_endpoints.
    # The filter MUST be `field_join::tests`, not `joiner_tests`: the
    # tests that pin `is_suffix` and the decl-loop branches live in
    # `field_join::tests`, which `joiner_tests` never runs. Under the
    # old filter 13 of 19 killed mutants read as survivors.
    ("src/server/federation/contracts/field_join.rs", "field_join::tests", "lib"),
    # Task 2: sensor_owner_of — coexistence with other sensors.
    # `graph/mod.rs` is 3.6k lines; `sensor_coexistence` only reaches
    # `sensor_owner_of`, so every BFS/entry-point mutant survived and
    # the 5% read as "no coverage" when it meant "wrong filter".
    ("src/server/graph/mod.rs", "graph", "lib"),
    # Task 6: GraphQL fragment / directive / deep-selection parsing
    ("src/server/sensors/graphql_consumer_sensor.rs", "graphql_resolution", "test"),
]

# (pattern, replacement, label) — textual but semantically meaningful.
# Off-by-one guards and boolean short-circuits cover most of the suite;
# the byte-equality operator is the smallest mutation that exposes a
# silently-wrong parser (`payload_schema.rs` has 27 byte compares
# and `graphql_consumer_sensor.rs` has 52, vs only 2 `>=` there).
MUTATIONS = [
    (r'(?<![<>=!])>=(?!=)', '>',   'ge->gt'),
    (r'(?<![<>=!])<=(?!=)', '<',   'le->lt'),
    (r'\s&&\s',             ' || ', 'and->or'),
    (r'(?<!=)==(?!=)',      '!=',  'eq->ne'),
]

def strip_tests(src):
    """Only mutate production code, not the tests themselves."""
    i = src.find('#[cfg(test)]')
    return (src[:i], src[i:]) if i != -1 else (src, '')

def run_tests(filt, kind):
    if kind == "lib":
        args = [CARGO, "test", "--lib", filt]
    else:
        args = [CARGO, "test", "--test", filt]
    r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=900)
    return r.returncode == 0

BASELINE_PATH = os.path.join(ROOT, "scripts", "mutation-baseline.json")
TRIAGE_PATH = BASELINE_PATH  # triage lives inside the baseline document


def load_triage():
    """Previously-classified survivors. `equivalent: <reason>` entries are
    reported separately and do not count toward the gap rate; without
    this record every run re-opens the same questions and the achievable
    ceiling stays invisible."""
    try:
        with open(TRIAGE_PATH) as f:
            return json.load(f).get("triage", {})
    except (OSError, ValueError):
        return {}


def run_mutations(targets=None):
    """Apply one mutation at a time, run the scoped oracle, restore.

    `try/finally` restores the file even on Ctrl-C. A killed worker can
    still leave a mutation behind (SIGKILL skips finally), so callers
    must `git diff --stat src/` before committing.

    Two score-preserving speedups:
      * mutants already classified `equivalent` in the triage are not
        re-measured — the oracle cannot kill them by definition, and
        re-running them is pure wall-clock;
      * targets run concurrently (see `run_all`), each mutating its own
        file, so there is no shared-file contention.
    """
    triage = load_triage()
    results = []
    for relpath, filt, kind in (targets or TARGETS):
        path = os.path.join(ROOT, relpath)
        original = open(path).read()
        prod, tests = strip_tests(original)

        try:
            for pat, rep, label in MUTATIONS:
                hits = list(re.finditer(pat, prod))
                for m in hits:
                    line_start = prod.rfind('\n', 0, m.start()) + 1
                    line_end = prod.find('\n', m.start())
                    line_text = prod[line_start:line_end if line_end != -1 else len(prod)]
                    if line_text.strip().startswith('//'):
                        continue  # comments are not behaviour
                    mutated = prod[:m.start()] + rep + prod[m.end():]
                    if mutated == prod:
                        continue

                    line_no = prod[:m.start()].count('\n') + 1
                    key = f"{relpath}:{line_no}:{label}"
                    verdict = triage.get(key)
                    if (verdict or "").startswith("equivalent"):
                        # No oracle run: a mutant we have already judged
                        # unkillable. Recorded as SURVIVED+equivalent so
                        # the ceiling stays visible in the report.
                        results.append({
                            "status": "SURVIVED", "path": relpath, "line": line_no,
                            "op": label, "snippet": "", "verdict": verdict,
                            "skipped": True,
                        })
                        print(f"  SURVIVED  {key}  [equivalent, not re-measured]", flush=True)
                        continue

                    open(path, 'w').write(mutated + tests)
                    try:
                        built_and_passed = run_tests(filt, kind)
                    except subprocess.TimeoutExpired:
                        built_and_passed = False

                    lines = prod.splitlines()
                    snippet = lines[line_no - 1].strip()[:80] if line_no - 1 < len(lines) else ''
                    status = "SURVIVED" if built_and_passed else "caught"
                    results.append({
                        "status": status, "path": relpath, "line": line_no,
                        "op": label, "snippet": snippet, "verdict": verdict,
                    })
                    tag = "" if verdict is None else f"  [{verdict.split(':')[0]}]"
                    print(f"  {status:9} {key}{tag}  {snippet}", flush=True)
        finally:
            open(path, 'w').write(original)
    return results


def run_all(workers=1):
    """Run every target. `workers > 1` runs targets concurrently.

    Parallelism is across TARGETS, never across mutants within one
    target — mutants of a target rewrite the same file and cannot be
    applied concurrently. Each worker gets its own `CARGO_TARGET_DIR`
    so `cargo`'s build lock does not serialise them; the score is
    identical either way, which `--self-test` asserts.
    """
    targets = TARGETS
    if workers <= 1:
        return run_mutations(targets)

    from concurrent.futures import ThreadPoolExecutor
    import tempfile

    def one(i_and_target):
        i, t = i_and_target
        td = tempfile.mkdtemp(prefix=f"mut-target-{i}-")
        old = os.environ.get("CARGO_TARGET_DIR")
        os.environ["CARGO_TARGET_DIR"] = td
        try:
            return run_mutations([t])
        finally:
            if old is None:
                os.environ.pop("CARGO_TARGET_DIR", None)
            else:
                os.environ["CARGO_TARGET_DIR"] = old
            shutil.rmtree(td, ignore_errors=True)

    with ThreadPoolExecutor(max_workers=workers) as ex:
        chunks = list(ex.map(one, enumerate(targets)))
    return [r for chunk in chunks for r in chunk]


def summarise(results):
    survived = [r for r in results if r["status"] == "SURVIVED"]
    equiv = [r for r in survived if (r.get("verdict") or "").startswith("equivalent")]
    gaps = [r for r in survived if not (r.get("verdict") or "").startswith("equivalent")]
    print(f"\n=== {len(results)} mutations applied, {len(survived)} SURVIVED "
          f"({len(equiv)} equivalent, {len(gaps)} gap) ===")
    for s in gaps:
        print(f"  gap       {s['path']}:{s['line']} [{s['op']}]  {s['snippet']}")
    for s in equiv:
        print(f"  equiv     {s['path']}:{s['line']} [{s['op']}]  {s.get('verdict')}")
    return survived, equiv, gaps


def write_baseline(results):
    import datetime
    per_target = {}
    for r in results:
        t = per_target.setdefault(r["path"], {"path": r["path"], "mutants": 0, "killed": 0, "survived": 0})
        t["mutants"] += 1
        if r["status"] == "SURVIVED":
            t["survived"] += 1
        else:
            t["killed"] += 1
    doc = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "operators": [label for _, _, label in MUTATIONS],
        "targets": sorted(per_target.values(), key=lambda t: t["path"]),
        "triage": load_triage(),
    }
    with open(BASELINE_PATH, "w") as f:
        json.dump(doc, f, indent=2, sort_keys=True)
    killed = sum(t["killed"] for t in doc["targets"])
    total = sum(t["mutants"] for t in doc["targets"])
    print(f"MUTATION_SUMMARY {killed}/{total} killed, {total - killed} survived")


def check_filters():
    """A filter that runs no test touching the target file makes every
    mutant survive, which reads as 'no coverage' when it means 'wrong
    filter'. Fail loudly instead."""
    problems = []
    for relpath, filt, kind in TARGETS:
        args = [CARGO, "test", "--lib" if kind == "lib" else "--test", filt, "--", "--list"]
        try:
            r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=600)
        except subprocess.TimeoutExpired:
            problems.append(f"{relpath}: filter '{filt}' timed out listing tests")
            continue
        listed = [l for l in r.stdout.splitlines() if ": test" in l]
        if not listed:
            problems.append(f"{relpath}: filter '{filt}' lists zero tests")
            continue
        # For `lib` targets the filter selects by module path, so the
        # test names should mention the file's stem. For `test` targets
        # the filter names the test FILE itself — `parsers_adversarial`
        # is the suite for `payload_schema.rs` even though no test in it
        # carries that name — so a non-trivial count is the honest check.
        if kind == "test":
            if len(listed) < 3:
                problems.append(
                    f"{relpath}: filter '{filt}' lists only {len(listed)} test(s); "
                    f"too few to exercise a whole parser"
                )
            continue
        stem = os.path.basename(relpath).rsplit(".", 1)[0]
        if not any(stem in l or "tests::" in l for l in listed):
            problems.append(
                f"{relpath}: filter '{filt}' lists {len(listed)} tests but none "
                f"appear to target '{stem}' — mutants will all survive"
            )
    return problems


def check_floor():
    """Gate: the gap rate must not rise, and the risk-critical modules
    must clear their floor. Floors live in the baseline so a reduction
    is a reviewable diff, not a silent edit."""
    with open(BASELINE_PATH) as f:
        doc = json.load(f)
    floor = doc.get("floor", {
        "global_killed_pct": 70,
        "per_target": {
            "src/server/sensors/graphql_consumer_sensor.rs": 85,
            "src/server/sensors/payload_schema.rs": 85,
            "src/server/federation/contracts/joiner/consumer_protocol.rs": 85,
            "src/server/federation/contracts/field_join.rs": 85,
        },
    })
    bad = []
    if "targets" not in doc:
        return ["no measured targets in mutation-baseline.json — run a measurement first"]
    for t in doc["targets"]:
        pct = 100.0 * t["killed"] / max(t["mutants"], 1)
        want = floor["per_target"].get(t["path"])
        if want is not None and pct < want:
            bad.append(f"{t['path']}: {pct:.1f}% < required {want}%")
    total_killed = sum(t["killed"] for t in doc["targets"])
    total = sum(t["mutants"] for t in doc["targets"])
    pct = 100.0 * total_killed / max(total, 1)
    if pct < floor["global_killed_pct"]:
        bad.append(f"global {pct:.1f}% < required {floor['global_killed_pct']}%")
    return bad


if __name__ == "__main__":
    args = [a for a in sys.argv[1:]]
    mode = "--run"
    workers = 1
    if args and args[0].startswith("--"):
        mode = args[0]
        args = args[1:]
    for a in args:
        if a.startswith("--workers="):
            workers = int(a.split("=", 1)[1])

    if mode == "--check-filters":
        probs = check_filters()
        for p in probs:
            print("FAIL:", p)
        print("filter check:", "PASS" if not probs else f"{len(probs)} problem(s)")
        sys.exit(1 if probs else 0)

    if mode == "--check-floor":
        if not os.path.exists(BASELINE_PATH):
            print("FAIL: scripts/mutation-baseline.json missing — run a measurement first")
            sys.exit(1)
        bad = check_floor()
        for b in bad:
            print("FAIL:", b)
        print("floor check:", "PASS" if not bad else f"{len(bad)} violation(s)")
        sys.exit(1 if bad else 0)

    if mode == "--self-test":
        # Parallelism must not change which mutants die. Compare the
        # (path, line, op, status) tuples only — order differs because
        # chunks complete out of order.
        serial = sorted((r["path"], r["line"], r["op"], r["status"]) for r in run_mutations())
        par = sorted((r["path"], r["line"], r["op"], r["status"]) for r in run_all(workers=4))
        if serial == par:
            print(f"self-test: PASS — {len(serial)} mutants, identical verdicts serial vs parallel")
            sys.exit(0)
        only_s = [x for x in serial if x not in par]
        only_p = [x for x in par if x not in serial]
        print("self-test: FAIL — parallel run disagrees with serial")
        for x in only_s:
            print("  serial only:", x)
        for x in only_p:
            print("  parallel only:", x)
        sys.exit(1)

    # default: run the full measurement
    import time
    t0 = time.time()
    results = run_all(workers)
    summarise(results)
    write_baseline(results)
    print(f"MUTATION_WALLCLOCK {time.time() - t0:.0f}s (workers={workers})")
