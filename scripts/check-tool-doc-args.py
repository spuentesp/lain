#!/usr/bin/env python3
"""Fail if a JSON example in `docs/quickstart-tools.md` uses argument
keys that aren't declared in the tool's input schema.

Run: python3 scripts/check-tool-doc-args.py

The audit at `DOGFOODING_REPORT.md` (2026-10-04, finding B9) is the
motivation: a user reading the docs for `get_coupling_radar` saw
"Files that co-change with this one" and tried
`{"path": "src/..."}` (the section's heading suggested `path`); the
input schema actually requires `symbol`, so the call failed with
"Missing required argument: symbol". This lint is the
machine-readable version of "every JSON example in the doc uses
keys that the tool actually accepts".

Today the lint is opt-in (called from a Makefile target, not a hard
CI gate) so existing drift doesn't block. New drift fails the
build.

Exit codes:
  0  — every example's keys are a subset of the schema's properties
  1  — at least one example uses a key the schema doesn't declare
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from typing import Any

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS_PATH = os.path.join("docs", "quickstart-tools.md")
REGISTRY_IMPL_PATH = os.path.join(
    "src", "server", "tools", "handlers", "registry_impl.rs"
)


def _slurp(path: str) -> str:
    with open(path, encoding="utf-8") as f:
        return f.read()


def registry_tool_schemas(registry_src: str) -> dict[str, dict[str, Any]]:
    """Parse `tool_meta!("<name>", ..., r#"<json-schema>"#, ...)` blocks."""
    out: dict[str, dict[str, Any]] = {}
    pattern = re.compile(
        r'tool_meta!\(\s*"(?P<name>[a-z_]+)"\s*,\s*'
        r'(?P<description>(?:"[^"]*"\s*)+)'
        r',\s*r#"(?P<schema>\{.*?\})"#',
        re.DOTALL,
    )
    for m in pattern.finditer(registry_src):
        name = m.group("name")
        raw = m.group("schema")
        try:
            schema = json.loads(raw)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"  schema parse error for {name}: {e}\n")
            continue
        out[name] = schema
    return out


def doc_examples(docs_src: str) -> list[tuple[str, dict[str, Any]]]:
    """Pull every `{ "name": "<tool>", "arguments": { ... } }` block."""
    out: list[tuple[str, dict[str, Any]]] = []
    pattern = re.compile(
        r'\{\s*"name"\s*:\s*"(?P<name>[a-z_]+)"\s*,\s*"arguments"\s*:\s*'
        r"(?P<args>\{[^{}]*\}|\[[^\[\]]*\])\s*\}",
        re.DOTALL,
    )
    for m in pattern.finditer(docs_src):
        name = m.group("name")
        args_raw = m.group("args")
        try:
            args = json.loads(args_raw)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"  doc parse error for {name}: {e}\n")
            continue
        if isinstance(args, dict):
            out.append((name, args))
    return out


def main() -> int:
    registry_src = _slurp(os.path.join(DEFAULT_ROOT, REGISTRY_IMPL_PATH))
    docs_src = _slurp(os.path.join(DEFAULT_ROOT, DOCS_PATH))

    schemas = registry_tool_schemas(registry_src)
    examples = doc_examples(docs_src)

    violations: list[str] = []
    for name, args in examples:
        schema = schemas.get(name)
        if schema is None:
            # The doc references a tool the registry doesn't define
            # (e.g. a federation-only or hidden tool). Skip — that's
            # not what this lint is for.
            continue
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            continue
        allowed = set(properties.keys())
        used = set(args.keys())
        bad = used - allowed
        if bad:
            violations.append(
                f"  {name}: doc uses keys {sorted(bad)} but schema "
                f"only declares {sorted(allowed)}"
            )

    if violations:
        sys.stderr.write(
            "doc/argument drift in docs/quickstart-tools.md:\n"
            + "\n".join(violations)
            + "\n"
            + "Fix the doc to match the tool's input schema, or fix "
            "the schema if the doc is right.\n"
        )
        return 1
    print(
        f"OK: {len(examples)} JSON examples in {DOCS_PATH} match "
        f"their tool's input schema."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
