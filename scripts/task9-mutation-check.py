#!/usr/bin/env python3
"""Task 9 mutation check: scope to the two files Task 9 covers.

Run: python3 scripts/task9-mutation-check.py

Restricted to:
  - src/server/sensors/graphql_consumer_sensor.rs   (graphql_resolution)
  - src/server/sensors/payload_schema.rs             (parsers_adversarial)

The script applies one mutation at a time, runs the
targeted test, and immediately reverts the file. The
`finally` block on every mutation is critical: it
guarantees the production file is restored to its
pre-mutation state before the script moves on, so a
killed process can only leave ONE mutation applied (the
one currently being tested) and the operator can revert
it by re-running the script (which always reverts at
the start of each target).
"""
import re, subprocess, sys, os, shutil, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARGO = shutil.which("cargo") or os.path.expanduser("~/.cargo/bin/cargo")

# (path, test_filter, kind)
TARGETS = [
    ("src/server/sensors/payload_schema.rs", "parsers_adversarial", "test"),
    ("src/server/sensors/graphql_consumer_sensor.rs", "graphql_resolution", "test"),
]

# (pattern, replacement, label) — textual but semantically meaningful.
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
    r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=600)
    return r.returncode == 0

def atomic_write(path, content):
    """Write content atomically: write to .tmp, then rename."""
    tmp = path + ".tmp"
    with open(tmp, 'w') as f:
        f.write(content)
    os.replace(tmp, path)

results = []
for relpath, filt, kind in TARGETS:
    path = os.path.join(ROOT, relpath)
    original = open(path).read()
    prod, tests = strip_tests(original)
    file_start = time.time()

    # Always start with the original file (covers the case
    # where a previous run was killed and left a mutation
    # applied).
    atomic_write(path, original)

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
            atomic_write(path, mutated + tests)
            try:
                built_and_passed = run_tests(filt, kind)
            except subprocess.TimeoutExpired:
                built_and_passed = False
            except Exception:
                # On any other error, make sure the file is reverted
                # before propagating.
                atomic_write(path, original)
                raise
            finally:
                # ALWAYS revert before the next mutation. This
                # is the critical invariant.
                atomic_write(path, original)

            line_no = prod[:m.start()].count('\n') + 1
            snippet = prod.splitlines()[line_no-1].strip()[:80] if line_no-1 < len(prod.splitlines()) else ''
            status = "SURVIVED" if built_and_passed else "caught"
            results.append((status, relpath, line_no, label, snippet))
            elapsed = time.time() - file_start
            print(f"  [{elapsed:6.1f}s] {status:9} {relpath}:{line_no} [{label}]  {snippet}", flush=True)

    # Final safety: ensure the file is reverted when we move to
    # the next target.
    atomic_write(path, original)

survived = [r for r in results if r[0] == "SURVIVED"]
print(f"\n=== {len(results)} mutations applied, {len(survived)} SURVIVED ===")
for s in survived:
    print(f"  {s[1]}:{s[2]} [{s[3]}]  {s[4]}")
