#!/usr/bin/env python3
"""Fail if `mcp::handler::dispatch_tool_call` accumulates new match arms
without going through the inventory sub-registries.

Run: python3 scripts/check-mcp-dispatch-shape.py

The audit at `docs/CONTRIBUTING_AGENTS.md` documents why the
stringly-typed `match name` ladder in
`src/server/mcp/handler.rs::dispatch_tool_call` is being phased out:
each arm is a maintenance trap and a place a new agent can silently
add a tool that bypasses the inventory registration. This check
detects two specific drifts:

  1. **Inventory/double registration**: a tool name declared via
     `inventory::submit!(ToolHandlerEntry(&X))` in
     `src/server/tools/handlers/registry_impl.rs` also appears as a
     match arm in `dispatch_tool_call`. Such overlap means the same
     tool is reachable via two paths and the contract in
     `ToolRegistry::dispatch` is no longer the single source of
     truth.

  2. **Match-arm growth**: the number of distinct `match name` arms
     exceeds the documented baseline by more than 5. Today
     `dispatch_tool_call` has 22 known arms; once Phase 3.2
     (sub-registries for presence/federation/workspace) lands and
     every arm migrates, the count should drop. Until then, the
     check rejects new arms that weren't part of the original 22.

The known-baseline list and the match-arm count threshold are a
shrinking baseline. When Phase 3.2 ships, the list empties and the
count threshold drops to 0.

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
HANDLER_PATH = os.path.join("src", "server", "mcp", "handler.rs")
REGISTRY_IMPL_PATH = os.path.join(
    "src", "server", "tools", "handlers", "registry_impl.rs"
)

# Distinct match-arm names known to live in `dispatch_tool_call` today.
# Presence / audit / status / reload tools are already on the inventory
# sub-registry (Phase 3.2 partial); federation and workspace arms are
# the remaining migration target.
KNOWN_DISPATCH_ARMS = {
    "add_annotation",
    "claim_files",
    "detect_overlap",
    "get_active_workspace",
    "get_audit_log",
    "get_cross_repo_blast_radius",
    "get_cross_repo_blast_radius_for_repo",
    "get_federation_health",
    "get_pending_handoffs",
    "get_recent_activity",
    "get_reload_status",
    "get_repo_info",
    "get_server_status",
    "get_workspace",
    "get_workspace_graph",
    "get_world_state",
    "heartbeat",
    "leave_handoff_note",
    "list_active_agents",
    "list_annotations",
    "list_occupancy",
    "list_recent_projects",
    "list_repos",
    "list_subagents",
    "list_workspaces",
    "my_claims",
    "register_agent",
    "release_files",
    "request_reload",
    "resolve_annotation",
    "search_org",
    "who_am_i",
}
ARM_GROWTH_BUDGET = 0


def _slurp(path: str) -> str:
    with open(path, encoding="utf-8") as f:
        return f.read()


def dispatch_function_body(text: str) -> str | None:
    m = re.search(
        r"fn\s+dispatch_tool_call\s*\([^)]*\)\s*->\s*[^{]*\{",
        text,
    )
    if not m:
        return None
    start = m.end() - 1
    depth = 0
    for i in range(start, len(text)):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1 : i]
    return None


def dispatch_arm_names(body: str) -> set[str]:
    out: set[str] = set()
    for m in re.finditer(r'"([a-z][a-z_0-9]*)"\s*(?:=>|,|\s*$)', body):
        out.add(m.group(1))
    return out


def inventory_tool_names(text: str) -> set[str]:
    """Each `fn name(&self) -> &'static str { "<name>" }` in registry_impl.rs."""
    out: set[str] = set()
    for m in re.finditer(
        r'fn\s+name\s*\(\s*&self\s*\)\s*->\s*&\'static\s*str\s*\{\s*"([^"]+)"',
        text,
    ):
        out.add(m.group(1))
    return out


def violations(root: str) -> list[str]:
    handler = _slurp(os.path.join(root, HANDLER_PATH))
    body = dispatch_function_body(handler)
    if body is None:
        return [f"{HANDLER_PATH}: could not locate `fn dispatch_tool_call`"]
    arms = dispatch_arm_names(body)
    overlap = arms & inventory_tool_names(
        _slurp(os.path.join(root, REGISTRY_IMPL_PATH))
    )
    out: list[str] = []
    for name in sorted(overlap):
        out.append(
            f"{HANDLER_PATH}: dispatch_tool_call has arm "
            f"`\"{name}\" =>` that is ALSO registered via "
            f"inventory::submit!(ToolHandlerEntry(&…)) in "
            f"{REGISTRY_IMPL_PATH}. The inventory path is the "
            f"single source of truth; remove the match arm."
        )
    new_arms = arms - KNOWN_DISPATCH_ARMS
    if new_arms:
        out.append(
            f"{HANDLER_PATH}: dispatch_tool_call has {len(new_arms)} "
            f"match arm(s) not in the known baseline: "
            f"{', '.join(sorted(new_arms))}. Each new arm should go "
            f"through an inventory sub-registry instead — see "
            f"docs/CONTRIBUTING_AGENTS.md#inventory-pattern. If the "
            f"arm belongs to the migration plan (Phase 3.2), "
            f"document it here."
        )
    budget_excess = len(arms) - len(KNOWN_DISPATCH_ARMS) - ARM_GROWTH_BUDGET
    if budget_excess > 0 and not new_arms:
        out.append(
            f"{HANDLER_PATH}: dispatch_tool_call has {len(arms)} arms; "
            f"baseline {len(KNOWN_DISPATCH_ARMS)} + budget "
            f"{ARM_GROWTH_BUDGET} = {len(KNOWN_DISPATCH_ARMS) + ARM_GROWTH_BUDGET}. "
            f"Excess: {budget_excess}. Phase 3.2 should reduce this."
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
    handler_path = os.path.join(root, HANDLER_PATH)
    if not os.path.isfile(handler_path):
        print(f"handler not found: {handler_path}", file=sys.stderr)
        return 2
    v = violations(root)
    if v:
        for line in v:
            print(line, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())