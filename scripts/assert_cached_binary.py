#!/usr/bin/env python3
"""CI helper for Milestone 9's clean-room lanes (docs/AGENT_UX_ROADMAP.md):
"require an actual downloaded binary, and then verify offline cache
reuse." Records the cached `lain` binary's path and mtime after an
install (`record`), then proves a later invocation left it untouched
rather than silently re-downloading (`verify-unchanged`).

An mtime comparison is the provable signal here, not a time-since-now
heuristic: a fast re-download could also look "recent," but it cannot
reproduce the exact same mtime the first install already wrote.
"""
import argparse
import glob
import json
import os
import sys


def find_binary(cache_dir: str) -> str:
    candidates = [
        p
        for p in (
            glob.glob(os.path.join(cache_dir, "**", "lain"), recursive=True)
            + glob.glob(os.path.join(cache_dir, "**", "lain.exe"), recursive=True)
        )
        if os.path.isfile(p)
    ]
    if not candidates:
        raise SystemExit(f"::error::no cached lain binary found under {cache_dir}")
    if len(candidates) > 1:
        raise SystemExit(
            f"::error::expected exactly one cached binary under {cache_dir}, found {candidates}"
        )
    return candidates[0]


def cmd_record(args) -> None:
    path = find_binary(args.cache_dir)
    snapshot = {"path": path, "mtime": os.path.getmtime(path)}
    with open(args.snapshot, "w") as f:
        json.dump(snapshot, f)
    print(f"recorded: {snapshot}")


def cmd_verify_unchanged(args) -> None:
    with open(args.snapshot) as f:
        snapshot = json.load(f)
    path = find_binary(args.cache_dir)
    if path != snapshot["path"]:
        raise SystemExit(
            f"::error::cached binary path changed between invocations: "
            f"{snapshot['path']!r} -> {path!r}"
        )
    mtime = os.path.getmtime(path)
    if mtime != snapshot["mtime"]:
        raise SystemExit(
            f"::error::{path} mtime changed ({snapshot['mtime']} -> {mtime}) -- "
            f"a second invocation re-downloaded instead of reusing the cache"
        )
    print(f"verified unchanged: {path} (mtime {mtime})")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", required=True)
    parser.add_argument(
        "--snapshot", required=True, help="State file shared between record and verify-unchanged"
    )
    sub = parser.add_subparsers(dest="mode", required=True)
    sub.add_parser("record")
    sub.add_parser("verify-unchanged")
    args = parser.parse_args(argv)
    # `SystemExit(message)` from the cmd_* helpers propagates as-is:
    # Python prints the message to stderr and exits 1, exactly what a CI
    # step failure needs, with no extra plumbing here.
    (cmd_record if args.mode == "record" else cmd_verify_unchanged)(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
