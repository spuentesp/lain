#!/usr/bin/env python3
"""Self-test for check-format-duration-once.py.

Exercises the three paths:
  1. Clean state (current handlers/) exits 0.
  2. A second `fn format_duration` definition exits 1.
  3. A third handler file with the time-ladder literal exits 1.

Run: python3 scripts/test_check_format_duration_once.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(ROOT, "scripts", "check-format-duration-once.py")
HANDLERS_REL = os.path.join("src", "server", "tools", "handlers")


def run(tmp: str) -> int:
    return subprocess.run(
        ["python3", SCRIPT, "--root", tmp],
        cwd=tmp,
        capture_output=True,
        text=True,
    ).returncode


def seed(tmp: str) -> None:
    dst = os.path.join(tmp, HANDLERS_REL)
    os.makedirs(dst, exist_ok=True)
    for name in os.listdir(os.path.join(ROOT, HANDLERS_REL)):
        if name.endswith(".rs"):
            shutil.copy(
                os.path.join(ROOT, HANDLERS_REL, name),
                os.path.join(dst, name),
            )


def assert_eq(label: str, got: int, want: int) -> None:
    if got != want:
        print(f"FAIL {label}: exit {got}, want {want}", file=sys.stderr)
        sys.exit(1)
    print(f"OK   {label}")


def main() -> int:
    if not os.path.isfile(SCRIPT):
        print(f"missing script: {SCRIPT}", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory() as raw:
        seed(raw)
        assert_eq("clean state", run(raw), 0)
        handlers_dir = os.path.join(raw, HANDLERS_REL)
        dup = os.path.join(handlers_dir, "dup_duration.rs")
        with open(dup, "w", encoding="utf-8") as f:
            f.write("fn format_duration(seconds: i64) -> String { format!(\"{}s\", seconds) }\n")
        assert_eq("duplicate fn format_duration", run(raw), 1)
        os.remove(dup)
        ladder = os.path.join(handlers_dir, "search.rs")
        with open(ladder, "w", encoding="utf-8") as f:
            f.write("fn time_ladder(d: i64) -> String { if d < 3600 { \"m\".into() } else { \"h\".into() } }\n")
        assert_eq("third file with ladder", run(raw), 1)
        os.remove(ladder)
    print("all tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())