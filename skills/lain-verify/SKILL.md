---
name: lain-verify
description: Use to verify a change through Lain — "run the tests", "does it build", "what is untested", "what changed", "which branch am I on". Covers the verify package (build/test/lint/coverage/git state).
---

# Lain verify — ship-it checks

The `verify` package is not in the default surface because it
**executes real processes** (slow, side effects on disk/caches).
Load it when you actually intend to run something.

## Load it

Call `load_package("verify")` — or set `LAIN_TOOL_PROFILE=verify`
at config time for agents that routinely verify. `load_package`
makes the tools appear in `tools/list`; tools are callable by
name even when hidden, but many MCP clients only permit tools
their cached list contains. If the tools do not appear after
loading, reconnect the client and refetch.

## The tools

- `run_build` / `run_tests` / `run_clippy` — the real thing: compile,
  run the suite (optionally filtered), lint (optionally auto-fix).
  Check `get_health` first so you are not testing a cold graph.
- `find_untested_functions` — where the call graph sees no test
  coverage. The right answer to "what should I test next".
- `get_coverage_summary` — structural estimate from connectivity.
  Fast, not a coverage report — say so when quoting it.
- `get_test_template` — a scaffold for one function or type.
- `get_file_diff` / `get_commit_history` / `get_branch_status` —
  git state before and after a change. For anything git-shaped
  beyond these, the shell is fine and often better.

## Rules of thumb

- Never `run_build`/`run_tests` just to answer a question — read
  first (comprehension skill), verify after changing.
- Long outputs: filter `run_tests` rather than pulling the whole
  suite.
- After a fix, the honest sequence is `assess_change` → edit →
  `run_tests` → `get_file_diff`.
