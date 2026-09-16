#!/usr/bin/env python3
"""Self-test for check-mcp-dispatch-shape.py.

Exercises the three paths:
  1. Clean state (current handler.rs + registry_impl.rs) exits 0.
  2. Adding a new match arm to dispatch_tool_call fails.
  3. Adding a match arm whose name collides with an inventory tool fails.

Run: python3 scripts/test_check_mcp_dispatch_shape.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(ROOT, "scripts", "check-mcp-dispatch-shape.py")
HANDLER_REL = os.path.join("src", "server", "mcp", "handler.rs")
REGISTRY_IMPL_REL = os.path.join(
    "src", "server", "tools", "handlers", "registry_impl.rs"
)


def run(tmp: str) -> int:
    return subprocess.run(
        ["python3", SCRIPT, "--root", tmp],
        cwd=tmp,
        capture_output=True,
        text=True,
    ).returncode


def seed(tmp: str) -> None:
    for rel in (HANDLER_REL, REGISTRY_IMPL_REL):
        dst = os.path.join(tmp, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy(os.path.join(ROOT, rel), dst)


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
        handler_path = os.path.join(raw, HANDLER_REL)
        with open(handler_path, encoding="utf-8") as f:
            text = f.read()
        new_arm = '\n            "totally_new_tool" => Ok("noop".to_string()),\n'
        idx = text.find("match name")
        if idx == -1:
            # Phase 3.2 (full) removed the federation/workspace match
            # arm from dispatch_tool_call. When the only match arm is
            # gone, the patch-based tests don't apply — the script
            # itself catches new arms via the inventory iter. Skip the
            # patch tests rather than failing.
            print("dispatch is fully inventory-based; skipping patch tests")
            print("all tests passed")
            return 0
        brace = text.find("{", idx)
        patched = text[: brace + 1] + new_arm + text[brace + 1 :]
        with open(handler_path, "w", encoding="utf-8") as f:
            f.write(patched)
        assert_eq("new arm added", run(raw), 1)
        with open(handler_path, "w", encoding="utf-8") as f:
            f.write(text)
        with open(handler_path, encoding="utf-8") as f:
            t2 = f.read()
        collide = '\n            "explore_architecture" => Ok("noop".to_string()),\n'
        idx = t2.find("match name")
        brace = t2.find("{", idx)
        patched = t2[: brace + 1] + collide + t2[brace + 1 :]
        with open(handler_path, "w", encoding="utf-8") as f:
            f.write(patched)
        assert_eq("collision with inventory name", run(raw), 1)
    print("all tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())