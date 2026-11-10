#!/usr/bin/env python3
"""Fill `Formula/lain.rb`'s `sha256` values from a published release's SHA256SUMS.

The release artifacts are not bit-reproducible, so their hashes cannot be
known before `release.yml` uploads them. AGENTS.md step 6 therefore
splits the Formula work in two: the release PR bumps the version and
the URLs, and this script fills the hashes *after* the tag is published.

v0.14.0 shipped with the wrong hashes because that second step was
skipped, so `brew install lain` failed checksum verification. Run this
script right after publishing to close that loop.

Usage:
    python scripts/fill-formula-sha256.py --tag v0.15.0
    python scripts/fill-formula-sha256.py --tag v0.15.0 --checksum-file /tmp/SHA256SUMS
    python scripts/fill-formula-sha256.py --tag v0.15.0 --check

`--check` verifies the Formula against the published checksums without
writing, and exits non-zero on a mismatch. It is the mode CI should use.
"""
from __future__ import annotations

import argparse
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FORMULA = ROOT / "Formula" / "lain.rb"

# Target triple -> the `url` line's asset suffix. The macOS ARM64
# triple is intentionally absent: release.yml has no macOS build job
# (dropped in v0.14.0), so no macOS checksum is ever published.
TARGETS = ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc")

RELEASE_ASSETS_URL = "https://github.com/spuentesp/lain/releases/download/{tag}/SHA256SUMS"


def fetch_checksums(tag: str) -> dict[str, str]:
    """Return {asset filename: sha256} from the release's SHA256SUMS."""
    url = RELEASE_ASSETS_URL.format(tag=tag)
    with urllib.request.urlopen(url, timeout=60) as response:
        text = response.read().decode("utf-8")
    checksums: dict[str, str] = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and re.fullmatch(r"[0-9a-f]{64}", parts[0]):
            checksums[parts[1]] = parts[0]
    return checksums


def formula_targets(text: str) -> dict[str, str]:
    """Return {target triple: the sha256 currently in the formula}."""
    found: dict[str, str] = {}
    for target in TARGETS:
        pattern = re.compile(
            r"(url \"[^\"]*lain-[\d.]+-" + re.escape(target) + r"\.tar\.gz\"\s*\n\s*sha256 \")([0-9a-f]{64})(\")"
        )
        match = pattern.search(text)
        if match:
            found[target] = match.group(2)
    return found


def fill(tag: str, checksums: dict[str, str]) -> bool:
    text = FORMULA.read_text()
    changed = False
    for target in TARGETS:
        asset = f"lain-{tag.lstrip('v')}-{target}.tar.gz"
        expected = checksums.get(asset)
        if expected is None:
            print(f"ERROR: SHA256SUMS has no entry for {asset}", file=sys.stderr)
            return False
        pattern = re.compile(
            r"(url \"[^\"]*lain-[\d.]+-" + re.escape(target) + r"\.tar\.gz\"\s*\n\s*sha256 \")[0-9a-f]{64}(\")"
        )
        new_text, count = pattern.subn(r"\g<1>" + expected + r"\2", text)
        if count == 0:
            print(f"ERROR: no '{target}' stanza found in {FORMULA}", file=sys.stderr)
            return False
        if count == 1 and new_text != text:
            changed = True
        text = new_text
    if changed:
        FORMULA.write_text(text)
        print(f"filled {len(TARGETS)} sha256 values from {tag}")
    else:
        print(f"sha256 values already match {tag}")
    return True


def check(tag: str, checksums: dict[str, str]) -> bool:
    text = FORMULA.read_text()
    current = formula_targets(text)
    ok = True
    for target in TARGETS:
        asset = f"lain-{tag.lstrip('v')}-{target}.tar.gz"
        expected = checksums.get(asset)
        if expected is None:
            print(f"FAIL: SHA256SUMS has no entry for {asset}")
            ok = False
            continue
        actual = current.get(target)
        if actual is None:
            print(f"FAIL: no '{target}' stanza found in {FORMULA}")
            ok = False
        elif actual != expected:
            print(f"FAIL: {target} sha256 is {actual}, published {expected}")
            ok = False
        else:
            print(f"OK: {target} sha256 matches {tag}")
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="release tag, e.g. v0.15.0")
    parser.add_argument(
        "--checksum-file",
        help="path to a SHA256SUMS file to use instead of downloading it",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify only; do not write",
    )
    args = parser.parse_args()

    if args.checksum_file:
        checksums: dict[str, str] = {}
        for line in Path(args.checksum_file).read_text().splitlines():
            parts = line.split()
            if len(parts) == 2 and re.fullmatch(r"[0-9a-f]{64}", parts[0]):
                checksums[parts[1]] = parts[0]
    else:
        checksums = fetch_checksums(args.tag)

    if args.check:
        return 0 if check(args.tag, checksums) else 1
    return 0 if fill(args.tag, checksums) else 1


if __name__ == "__main__":
    sys.exit(main())
