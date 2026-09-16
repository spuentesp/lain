#!/usr/bin/env python3
"""Fail if a new struct in `mcp/federation_tools/dto.rs` mirrors a
canonical `schema::*` type instead of extending it.

Run: python3 scripts/check-no-mirror-dtos.py

The audit at `docs/CONTRIBUTING_AGENTS.md` documents why a federation
caller that needs extra fields (`repo_id`, `cross_repo`) should add
those fields to `schema::GraphNode` / `schema::GraphEdge` rather
than re-declare a parallel struct in
`src/server/mcp/federation_tools/dto.rs`. The mirror is a maintenance
trap: every fix to the schema type has to be re-applied to the dto,
and the wire format silently disagrees with the in-process format.

Today `dto.rs` holds two known mirrors awaiting Phase 3.3 (delete
`dto::GraphNode` / `dto::GraphEdge` after extending the schema
types). Any NEW struct added to `dto.rs` whose field set overlaps
substantially with a `schema::*` struct fails this check.

Exit codes:
  0  — no new mirror DTOs
  1  — at least one violation outside the known baseline
"""
from __future__ import annotations

import argparse
import os
import re
import sys

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DTO_PATH = os.path.join("src", "server", "mcp", "federation_tools", "dto.rs")
SCHEMA_PATH = os.path.join("src", "server", "schema.rs")

# Known mirrors awaiting Phase 3.3 deletion. When that lands, remove
# both entries and the check enforces the "no mirrors at all" invariant.
KNOWN_MIRRORS = {"GraphNode", "GraphEdge"}

# A dto struct is considered a "mirror" if at least this many of its
# field names (after stripping the `_id` suffix that `schema::*` adds
# on graph-edge fields) appear in the same schema struct. The bar
# is 3 so that common projection fields (`name`, `path`) don't trip
# the check on unrelated DTOs like `RepoInfo` or `SymbolMatch`.
MIRROR_FIELD_OVERLAP = 3

STRUCT_RE = re.compile(
    r"^\s*pub\s+struct\s+([A-Za-z_][A-Za-z_0-9]*)\s*\{",
    re.MULTILINE,
)
FIELD_RE = re.compile(
    r"^\s*pub\s+([A-Za-z_][A-Za-z_0-9]*)\s*:",
    re.MULTILINE,
)


def _slurp(path: str) -> str:
    with open(path, encoding="utf-8") as f:
        return f.read()


def parse_structs(text: str) -> dict[str, list[str]]:
    """Map struct name → ordered list of `pub` field names."""
    out: dict[str, list[str]] = {}
    for m in STRUCT_RE.finditer(text):
        name = m.group(1)
        start = m.end()
        depth = 1
        i = start
        while i < len(text) and depth > 0:
            c = text[i]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
            i += 1
        body = text[start : i - 1]
        out[name] = FIELD_RE.findall(body)
    return out


def normalize_field(name: str) -> str:
    """Strip the trailing `_id` that schema uses for graph-edge endpoints."""
    return name[:-3] if name.endswith("_id") else name


def violations(root: str) -> list[str]:
    dto_path = os.path.join(root, DTO_PATH)
    schema_path = os.path.join(root, SCHEMA_PATH)
    if not os.path.isfile(dto_path):
        return [f"{DTO_PATH}: not found"]
    if not os.path.isfile(schema_path):
        return [f"{SCHEMA_PATH}: not found"]
    dto_structs = parse_structs(_slurp(dto_path))
    schema_structs = parse_structs(_slurp(schema_path))
    schema_fields = {
        name: {normalize_field(f) for f in fields}
        for name, fields in schema_structs.items()
    }
    out: list[str] = []
    for dto_name, dto_fields in dto_structs.items():
        if dto_name in KNOWN_MIRRORS:
            continue
        normalized = {normalize_field(f) for f in dto_fields}
        if len(normalized) < MIRROR_FIELD_OVERLAP:
            continue
        best = max(
            schema_structs,
            key=lambda s: len(normalized & schema_fields[s]),
            default=None,
        )
        if best is None:
            continue
        overlap = len(normalized & schema_fields[best])
        if overlap >= MIRROR_FIELD_OVERLAP:
            out.append(
                f"{DTO_PATH}: struct `{dto_name}` has {overlap} field "
                f"name(s) overlapping with `schema::{best}`. This is a "
                f"mirror DTO — extend `schema::{best}` with the "
                f"extra field(s) instead and drop the mirror. See "
                f"docs/CONTRIBUTING_AGENTS.md#persistence-types."
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