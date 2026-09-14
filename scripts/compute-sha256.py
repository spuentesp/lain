#!/usr/bin/env python3
"""Compute SHA-256 of one or more files and emit a GNU-format sidecar each.

Output format (one line per file):

    <hex>  <filename>

This is exactly what `sha256sum -c` consumes, so the sidecars are
drop-in compatible with the standard tooling.

Why a Python script and not `sha256sum` directly:

    - Cross-platform. Windows runners do ship `sha256sum` via Git for
      Windows, but the PATH ordering and the way `tar` invokes it have
      produced flaky logs in CI. Python's `hashlib` is the same code
      path on Linux, macOS, and Windows.
    - One less tool to pin. The release workflow already requires
      `actions/setup-python@v5` for `check-release-version.py`; this
      script runs on that interpreter.

Usage:

    python scripts/compute-sha256.py release/lain-0.7.3-x86_64-unknown-linux-gnu.tar.gz

Emits `release/lain-0.7.3-x86_64-unknown-linux-gnu.tar.gz.sha256` and
prints `<hash>  <name>` to stdout.

Pass multiple files for batch mode.
"""
from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

CHUNK_SIZE = 1 << 16  # 64 KiB — matches GNU coreutils' default buffer


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(CHUNK_SIZE), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="+", type=Path, help="Files to hash")
    args = parser.parse_args()

    rc = 0
    for path in args.files:
        if not path.is_file():
            print(f"error: not a file: {path}", file=sys.stderr)
            rc = 1
            continue
        digest = sha256_of(path)
        sidecar = path.with_name(path.name + ".sha256")
        sidecar.write_text(f"{digest}  {path.name}\n")
        print(f"{digest}  {path.name}")
    return rc


if __name__ == "__main__":
    sys.exit(main())