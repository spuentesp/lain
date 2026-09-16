#!/usr/bin/env python3
"""Fail if a second `format_duration`-style helper appears under
`src/server/tools/handlers/`.

Run: python3 scripts/check-format-duration-once.py

The audit at `docs/CONTRIBUTING_AGENTS.md` documents the
`format_duration` drift: a "X ago" formatter was defined in
`tools/handlers/architecture.rs:332` and then reimplemented inline in
`tools/handlers/gitops.rs:62-72` (under a different name,
`time_str`), with subtly different thresholds. The canonical home
after Phase 3.5 is `tools/utils.rs`; handler files should `use` it.

This check detects:

  1. **A second `fn format_duration(...)` definition** under
     `src/server/tools/handlers/`. Today only `architecture.rs` has
     one; any second handler file with the same definition fails.
  2. **A second handler file with the time-ladder literal**
     (`<3600`, `<86400`). Today `architecture.rs` and `gitops.rs`
     each contain the ladder; a third handler file with it fails.

The known-baseline list of files that contain the time-ladder is a
shrinking baseline. When Phase 3.5 ships and `gitops.rs` migrates to
the canonical helper, remove `gitops.rs` from the list and the check
enforces "exactly one handler file has the ladder" uniformly.

Exit codes:
  0  — no new violations
  1  — at least one violation outside the known baseline
"""
from __future__ import annotations

import argparse
import os
import re
import sys

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HANDLERS_DIR = os.path.join("src", "server", "tools", "handlers")

# Handler file basenames that today contain the time-ladder literal.
# Remove entries as Phase 3.5 migrates each file to the canonical
# helper in tools/utils.rs.
KNOWN_LADDER_FILES = {
    "architecture.rs",
    "gitops.rs",
}
# Handler file basenames that today define `fn format_duration(...)`.
# Today only `architecture.rs` does — when Phase 3.5 moves it to
# utils.rs, this should become empty.
KNOWN_FORMAT_DURATION_FILES = {
    "architecture.rs",
}

FORMAT_DURATION_DEF = re.compile(
    r"^\s*(?:pub\s+)?fn\s+format_duration\s*\(",
    re.MULTILINE,
)
TIME_LADDER_LITERAL = re.compile(r"<\s*(?:3600|86400)\b")


def violations(root: str) -> list[str]:
    handlers = os.path.join(root, HANDLERS_DIR)
    if not os.path.isdir(handlers):
        return [f"handlers dir not found: {handlers}"]
    out: list[str] = []
    format_dur_files: list[str] = []
    ladder_files: list[str] = []
    for name in sorted(os.listdir(handlers)):
        if not name.endswith(".rs"):
            continue
        path = os.path.join(handlers, name)
        text = open(path, encoding="utf-8").read()
        if FORMAT_DURATION_DEF.search(text):
            format_dur_files.append(name)
        if TIME_LADDER_LITERAL.search(text):
            ladder_files.append(name)
    new_format = set(format_dur_files) - KNOWN_FORMAT_DURATION_FILES
    for f in sorted(new_format):
        out.append(
            f"{os.path.join(HANDLERS_DIR, f)}: defines "
            f"`fn format_duration(...)`. The canonical helper lives "
            f"in src/server/tools/utils.rs. Import it; don't redefine."
        )
    new_ladder = set(ladder_files) - KNOWN_LADDER_FILES
    for f in sorted(new_ladder):
        out.append(
            f"{os.path.join(HANDLERS_DIR, f)}: contains the "
            f"time-ladder literal `<3600`/`<86400`. The 'X ago' "
            f"formatter belongs in tools/utils.rs; either import "
            f"the canonical one or open an issue if you genuinely "
            f"need a different ladder."
        )
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        default=DEFAULT_ROOT,
        help="Project root (default: %(default)s)",
    )
    parsed = parser.parse_args()
    root = os.path.abspath(parsed.root)
    v = violations(root)
    if v:
        for line in v:
            print(line, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())