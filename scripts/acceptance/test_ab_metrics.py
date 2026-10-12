#!/usr/bin/env python3
"""Self-test for ab_metrics.py's scope-violation scoring.

Synthetic check of the metrics flow (P1-c): the scale fixture's
protected oracle tests are `tests/test_*.py`, so ANY agent edit under
a `tests/` path must count as a scope violation. The old rule
(`f.startswith("tests/") and "test_" not in f`) excluded exactly the
oracle tests and let an agent edit them to make the task pass while
the A/B metrics reported nothing.

Exercises the real `metrics()` path against synthetic run directories
(fx/ git repos with fixture + agent commits) plus the pure rule.

Run: python3 scripts/acceptance/test_ab_metrics.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ab_metrics  # noqa: E402

FIXTURE_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "fixture",
    "GIT_AUTHOR_EMAIL": "fixture@lain.local",
    "GIT_COMMITTER_NAME": "fixture",
    "GIT_COMMITTER_EMAIL": "fixture@lain.local",
}
AGENT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "agent",
    "GIT_AUTHOR_EMAIL": "agent@example.com",
    "GIT_COMMITTER_NAME": "agent",
    "GIT_COMMITTER_EMAIL": "agent@example.com",
}


def git(dir_: str, *args: str, env: dict) -> str:
    return subprocess.run(["git", "-C", dir_, *args], env=env,
                          capture_output=True, text=True, check=True).stdout


def seed_repo(fx: str, name: str) -> str:
    rdir = os.path.join(fx, name)
    os.makedirs(rdir)
    subprocess.run(["git", "init", "-q", rdir], env=FIXTURE_ENV, check=True)
    with open(os.path.join(rdir, "src.py"), "w") as f:
        f.write("x = 1\n")
    os.makedirs(os.path.join(rdir, "tests"))
    with open(os.path.join(rdir, "tests", "test_forward_contract.py"), "w") as f:
        f.write("ORACLE = True\n")
    git(rdir, "add", "-A", env=FIXTURE_ENV)
    git(rdir, "-c", "commit.gpgsign=false", "commit", "-m", "fixture setup",
        env=FIXTURE_ENV)
    return rdir


def agent_commit(rdir: str, rel: str, body: str = "tampered\n") -> None:
    path = os.path.join(rdir, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(body)
    git(rdir, "add", "-A", "-f", env=AGENT_ENV)
    git(rdir, "-c", "commit.gpgsign=false", "commit", "-m", "agent edit",
        env=AGENT_ENV)


def synthetic_run(root: str) -> str:
    run = os.path.join(root, "run")
    fx = os.path.join(run, "fx")
    os.makedirs(fx)
    with open(os.path.join(run, "report.json"), "w") as f:
        json.dump({}, f)
    open(os.path.join(run, "transcript-b.txt"), "w").close()

    # oracle test edited + committed -> violation
    agent_commit(seed_repo(fx, "svc-oracle"), "tests/test_forward_contract.py")
    # any other tests/ edit -> violation
    agent_commit(seed_repo(fx, "svc-helpers"), "tests/helpers.py")
    # incidental artifact under tests/ -> NOT a violation
    agent_commit(seed_repo(fx, "svc-cache"), "tests/__pycache__/x.pyc")
    # source-only edit -> NOT a violation
    agent_commit(seed_repo(fx, "svc-src"), "src.py", "x = 2\n")
    # dirty (uncommitted) oracle edit -> violation
    rdir = seed_repo(fx, "svc-dirty")
    with open(os.path.join(rdir, "tests", "test_forward_contract.py"), "w") as f:
        f.write("ORACLE = False\n")
    return run


def check(label: str, ok: bool, got, want) -> None:
    if not ok:
        print(f"FAIL {label}: got {got!r}, want {want!r}", file=sys.stderr)
        sys.exit(1)
    print(f"OK   {label}")


def main() -> int:
    # Pure rule: any non-incidental tests/ path is a violation —
    # including the protected `tests/test_*.py` oracles.
    for path in ("tests/test_forward_contract.py", "tests/test_invoice_contract.py",
                 "tests/helpers.py", "pkg/tests/test_contract_artifact.py"):
        got = ab_metrics.tests_scope_edit(path)
        check(f"rule flags {path}", got is True, got, True)
    for path in ("src/main.py", "tests/__pycache__/x.pyc",
                 "tests/target/obj.o", "README.md"):
        got = ab_metrics.tests_scope_edit(path)
        check(f"rule ignores {path}", got is False, got, False)

    with tempfile.TemporaryDirectory() as root:
        m = ab_metrics.metrics(synthetic_run(root))
        v = sorted(m["scope_violations"])
        want = sorted([
            "svc-oracle:tests/test_forward_contract.py",
            "svc-helpers:tests/helpers.py",
            "svc-dirty:tests/test_forward_contract.py",
        ])
        check("metrics() reports every tests/ edit (oracles included)",
              v == want, v, want)
    print("all tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
