#!/usr/bin/env python3
"""Targeted mutation testing: does the suite actually catch behaviour changes?

Run: python3 scripts/mutation-check.py

The guards in `tests/` check *shape* — that a module is declared, that a
test attribute is attached, that a documented knob is read. None of them
can see a function that is called and does the wrong thing. This measures
that directly: it changes behaviour and asks whether the suite notices.

The first run over these five files scored 6/17 — eleven behaviours with
no test pinning them, including the BFS depth gate underneath
`get_blast_radius` and `get_call_chain`. Fixing what that exposed turned
up a real bug: `{"depth":{"min":2,...}}` returned nothing at all, because
the walk-expansion gate reused the collection range and stopped before it
could reach `min`. It also caught a test *this harness prompted* that
passed for the wrong reason — its fixture tripped an earlier suppression,
so the filter under test never ran.

The score is now 13/17. The four survivors in `resolve.rs` are equivalent
mutants: the `max_edges` budget is enforced by four separate guards, so
flipping any one leaves the other three holding the cap.

Applies one small semantic mutation at a time to a production file, runs a
scoped test subset, and records whether the suite noticed. A mutation that
SURVIVES means that behaviour is not pinned by any test.
"""
import re, subprocess, sys, os, shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARGO = shutil.which("cargo") or os.path.expanduser("~/.cargo/bin/cargo")

TARGETS = [
    ("src/server/ingest/resolve.rs", "resolve"),
    ("src/server/tools/handlers/metrics.rs", "metrics"),
    ("src/server/audit.rs", "audit"),
    ("src/server/query/executor.rs", "executor"),
    ("src/server/sensors/http_sensor.rs", "http_sensor"),
]

# (pattern, replacement, label) — textual but semantically meaningful
MUTATIONS = [
    (r'(?<![<>=!])>=(?!=)', '>',  'ge->gt'),
    (r'(?<![<>=!])<=(?!=)', '<',  'le->lt'),
    (r'\s&&\s', ' || ',           'and->or'),
]

def strip_tests(src):
    """Only mutate production code, not the tests themselves."""
    i = src.find('#[cfg(test)]')
    return (src[:i], src[i:]) if i != -1 else (src, '')

def run_tests(filt):
    r = subprocess.run([CARGO, "test", "--lib", filt],
                       cwd=ROOT, capture_output=True, text=True, timeout=900)
    return r.returncode == 0

results = []
for relpath, filt in TARGETS:
    path = os.path.join(ROOT, relpath)
    original = open(path).read()
    prod, tests = strip_tests(original)

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
                built_and_passed = run_tests(filt)
            except subprocess.TimeoutExpired:
                built_and_passed = False
            open(path, 'w').write(original)

            line_no = prod[:m.start()].count('\n') + 1
            snippet = prod.splitlines()[line_no-1].strip()[:80] if line_no-1 < len(prod.splitlines()) else ''
            status = "SURVIVED" if built_and_passed else "caught"
            results.append((status, relpath, line_no, label, snippet))
            print(f"  {status:9} {relpath}:{line_no} [{label}]  {snippet}", flush=True)

open(path, 'w').write(original) if TARGETS else None
survived = [r for r in results if r[0] == "SURVIVED"]
print(f"\n=== {len(results)} mutations applied, {len(survived)} SURVIVED ===")
for s in survived:
    print(f"  {s[1]}:{s[2]} [{s[3]}]  {s[4]}")
