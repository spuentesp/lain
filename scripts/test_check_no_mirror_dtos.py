#!/usr/bin/env python3
"""Self-test for check-no-mirror-dtos.py.

Exercises the three paths:
  1. Clean state (current dto.rs + schema.rs) exits 0.
  2. Adding a NEW mirror struct to dto.rs (with overlap >= 3 with a
     schema struct) exits 1.
  3. Adding a NON-mirror struct to dto.rs (overlap < 3) still exits 0.

Run: python3 scripts/test_check_no_mirror_dtos.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(ROOT, "scripts", "check-no-mirror-dtos.py")
DTO_REL = os.path.join("src", "server", "mcp", "federation_tools", "dto.rs")
SCHEMA_REL = os.path.join("src", "server", "schema.rs")


def run(tmp: str) -> int:
    return subprocess.run(
        ["python3", SCRIPT, "--root", tmp],
        cwd=tmp,
        capture_output=True,
        text=True,
    ).returncode


def seed(tmp: str) -> None:
    for rel in (DTO_REL, SCHEMA_REL):
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
        dto_path = os.path.join(raw, DTO_REL)
        with open(dto_path, encoding="utf-8") as f:
            text = f.read()
        new_mirror = (
            "\n#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]\n"
            "pub struct NewGraphMirror {\n"
            "    pub id: String,\n"
            "    pub name: String,\n"
            "    pub path: String,\n"
            "    pub signature: String,\n"
            "    pub docstring: String,\n"
            "    pub extra_field: String,\n"
            "}\n"
        )
        with open(dto_path, "w", encoding="utf-8") as f:
            f.write(text + new_mirror)
        assert_eq("new mirror struct", run(raw), 1)
        with open(dto_path, "w", encoding="utf-8") as f:
            f.write(text)
        new_clean = (
            "\n#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]\n"
            "pub struct TotallyUnrelatedProjection {\n"
            "    pub alpha: String,\n"
            "    pub beta: usize,\n"
            "    pub gamma: f32,\n"
            "}\n"
        )
        with open(dto_path, "w", encoding="utf-8") as f:
            f.write(text + new_clean)
        assert_eq("non-mirror struct", run(raw), 0)
    print("all tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())