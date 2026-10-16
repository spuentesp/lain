# Continuous integration

The repository enforces its build, test, schema, packaging, and architecture
contracts through `.github/workflows/ci.yml`. A workflow-level concurrency
group cancels superseded runs for the same ref.

## Fast and full lanes

| Job | `dev` / PR to `dev` | `main` / PR to `main` |
|---|:---:|:---:|
| Ubuntu build and `cargo test --workspace` | yes | yes |
| JavaScript UI tests | yes | yes |
| macOS and Windows Cargo tests | no | yes |
| format, Clippy, and doc tests | yes | yes |
| npm-shim tests | yes | yes |
| architecture guardrails | yes | yes |
| generated tool-schema drift | yes for code changes | yes for code changes |
| incremental mutation gate (`scripts/mutation-gate.sh`) | PRs for code changes | no |
| architecture health report | PRs | PRs |
| capability suite (`scripts/demo.sh --quick`) | yes for code changes | yes for code changes |
| coverage | no | yes for code changes |
| release/action contracts | no | yes |
| release-version drift | no | push to `main` |

Draft PRs and positively identified documentation-only changes skip selected
expensive jobs. Changes to `ci.yml` itself always run the full applicable
battery; changes to `docs/tool-schema.json` are treated as code so schema drift
cannot be bypassed.

The mutation gate runs only on PRs to `dev`, never on `main`: it is
incremental (it mutates only the lines the PR diff touched), and a PR to
`main` is a release PR whose diff is the entire release. Every commit in it
was already gated when its own PR landed on `dev`.

## Required checks

Branch protection is described in [`BRANCHING.md`](BRANCHING.md). The live
required contexts as of 2026-09-20 are:

- `dev`: `lain/agent-contract`, `Lint + format + doc tests`, and
  `Cargo Test (ubuntu-latest)`.
- `main`: the same contexts plus `Cargo Test (macos-latest)` and
  `Cargo Test (windows-latest)`.

The `lain/agent-contract` commit status is published by the `agent-contract`
job. It aggregates capability, schema-drift, npm-shim, and mutation-gate
results; a skipped heavy capability job counts as satisfied on the fast lane.

## Local verification

Run checks in proportion to the change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- \
  -D warnings -A clippy::style -A clippy::complexity \
  -A clippy::perf -A clippy::pedantic -A unused -A dead_code
cargo test --workspace
```

The coordination battery (`shared_presence`, `multi_agent_concurrency`,
`coordination_*`, `attribution`, `presence_e2e`, `audit_integration`)
runs against real
zoanthrope arbitration when a `zoanthrope` binary is on `PATH` (or
`$ZOANTHROPE_BIN`) and skips with a message when it is not. CI installs
`@spuentesp/zoanthrope@0.1.6` and sets `CI_REQUIRE_ZOANTHROPE=1`, which
turns that skip into a hard failure — do the same locally
(`npm install -g @spuentesp/zoanthrope@0.1.6`) to reproduce a CI
coordination failure.

For npm launcher changes:

```bash
cd npm-shim
npm ci
npm test
```

For architecture-sensitive changes, run the guard scripts listed in
`arch-guardrails` in `.github/workflows/ci.yml`.

## Schema drift

`docs/tool-schema.json` is generated from the live full tool registry. When a
tool definition changes:

```bash
make schema
git diff --exit-code docs/tool-schema.json
```

CI independently builds the release binary, writes a fresh schema to a
temporary file, and compares it with the committed snapshot. The test
`tests/schema_dump_smoke.rs::live_tools_list_byte_matches_on_disk_schema_dump`
pins the same contract during `cargo test`.

## Mutation gate

`scripts/mutation-gate.sh` is the anti-weak-assertion layer. For every line
of library code a PR diff touches, `cargo mutants --in-diff` generates the
mutants that line admits (operator swaps, `true`/`false` returns, deleted
`!`, deleted match arms, …) and runs the unit-test suite against each one.
A mutant that survives means no test observes that behavior: hardcoded
branches, dead code, or an assertion that asserts less than it claims.

Only the diff is mutated, so cost scales with the PR, not with the tree.
The unmutated tree is assumed green (`--baseline skip` — the `test` job
proves it), and each mutant is judged by `cargo test --lib` (~2500 unit
tests, ~35s), which keeps per-mutant cost around a minute and leaves the
integration tests in `tests/` unrun — a mutant observed only by an
integration test is reported `MISSED` even though the full suite would
catch it. See the script header for the full blind-spot list and the
manual re-check command. The gate is wired into the `lain/agent-contract`
rollup, so a surviving mutant fails the PR.

Run it locally against your branch's diff since `dev`:

```bash
TMPDIR=/data/agents/tmp-lain-tests scripts/mutation-gate.sh          # this dev machine
TMPDIR="$TMPDIR" scripts/mutation-gate.sh <base-ref> <path-filter>   # narrower
```

The `TMPDIR` is a quirk of the current dev machine (its `/tmp` is
quota-exhausted, and a `TMPDIR` inside the repo breaks 9 git walk-up
tests); the script itself is host-agnostic and honors whatever `TMPDIR`
it is given. CI has neither problem. `cargo-mutants` must be installed
(`cargo install cargo-mutants --version 27.1.0`; CI pins the same
version).

Output is one line per mutant (`CAUGHT` / `MISSED` / `TIMEOUT` /
`UNVIABLE`) plus a summary. Exit 0 means every mutant was caught (or the
diff contains no mutable product code); exit 1 means at least one
survivor — treat each `MISSED` name as a finding:

1. Re-check it against the full suite before blaming the tests
   (`cargo test --workspace` with the mutant re-applied): the `--lib`
   blind spot can produce false survivors.
2. If the full suite also misses it, the behavior is genuinely unpinned —
   add the test that catches it.
3. Never convert a survivor into green by widening `.cargo/mutants.toml`
   exclusions; those are only for triaged equivalent mutants, each with a
   comment saying why the mutation cannot change observable behavior.

## Distribution acceptance

`.github/workflows/distribution-acceptance.yml` is separate from the ordinary
branch CI. It exercises the published npm package on Linux, macOS, and Windows
in both user and forced-install automation modes. A release-triggered gate also
tests downloaded release assets directly. See
[`DISTRIBUTION.md`](DISTRIBUTION.md) for the release checklist and current
known issue.
