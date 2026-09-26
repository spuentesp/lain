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
| architecture health report | PRs | PRs |
| capability suite (`scripts/demo.sh --quick`) | yes for code changes | yes for code changes |
| coverage | no | yes for code changes |
| release/action contracts | no | yes |
| release-version drift | no | push to `main` |

Draft PRs and positively identified documentation-only changes skip selected
expensive jobs. Changes to `ci.yml` itself always run the full applicable
battery; changes to `docs/tool-schema.json` are treated as code so schema drift
cannot be bypassed.

## Required checks

Branch protection is described in [`BRANCHING.md`](BRANCHING.md). The live
required contexts as of 2026-09-20 are:

- `dev`: `lain/agent-contract`, `Lint + format + doc tests`, and
  `Cargo Test (ubuntu-latest)`.
- `main`: the same contexts plus `Cargo Test (macos-latest)` and
  `Cargo Test (windows-latest)`.

The `lain/agent-contract` commit status is published by the `agent-contract`
job. It aggregates capability, schema-drift, and npm-shim results; a skipped
heavy capability job counts as satisfied on the fast lane.

## Local verification

Run checks in proportion to the change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- \
  -D warnings -A clippy::style -A clippy::complexity \
  -A clippy::perf -A clippy::pedantic -A unused -A dead_code
cargo test --workspace
```

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

## Distribution acceptance

`.github/workflows/distribution-acceptance.yml` is separate from the ordinary
branch CI. It exercises the published npm package on Linux, macOS, and Windows
in both user and forced-install automation modes. A release-triggered gate also
tests downloaded release assets directly. See
[`DISTRIBUTION.md`](DISTRIBUTION.md) for the release checklist and current
known issue.
