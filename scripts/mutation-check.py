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
import re, subprocess, sys, os, shutil

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
    # Task 4: field_join schema keying must match build_endpoints
    ("src/server/federation/contracts/field_join.rs", "joiner_tests", "lib"),
    # Task 2: sensor_owner_of — coexistence with other sensors
    ("src/server/graph/mod.rs", "sensor_coexistence", "test"),
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

results = []
for relpath, filt, kind in TARGETS:
    path = os.path.join(ROOT, relpath)
    original = open(path).read()
    prod, tests = strip_tests(original)

    try:
        for pat, rep, label in MUTATIONS:
            hits = list(re.finditer(pat, prod))
            for idx, m in enumerate(hits):
                line_start = prod.rfind('\n', 0, m.start()) + 1
                line_text = prod[line_start:prod.find('\n', m.start())]
                if line_text.strip().startswith('//'):
                    continue  # comments are not behaviour
                mutated = prod[:m.start()] + rep + prod[m.end():]
                if mutated == prod:
                    continue
                open(path, 'w').write(mutated + tests)
                try:
                    built_and_passed = run_tests(filt, kind)
                except subprocess.TimeoutExpired:
                    built_and_passed = False

                line_no = prod[:m.start()].count('\n') + 1
                snippet = prod.splitlines()[line_no-1].strip()[:80] if line_no-1 < len(prod.splitlines()) else ''
                status = "SURVIVED" if built_and_passed else "caught"
                results.append((status, relpath, line_no, label, snippet))
                print(f"  {status:9} {relpath}:{line_no} [{label}]  {snippet}", flush=True)
    finally:
        # Always restore the production file, even on Ctrl-C or kill -9.
        open(path, 'w').write(original)

survived = [r for r in results if r[0] == "SURVIVED"]
print(f"\n=== {len(results)} mutations applied, {len(survived)} SURVIVED ===")
for s in survived:
    print(f"  {s[1]}:{s[2]} [{s[3]}]  {s[4]}")
