# Badge rollout plan

Companion to `docs/AGENT_UX_ROADMAP.md`. Each step is independently shippable.
Steps are ordered by the two waves proposed for README impact: the cheap
"trust + activity" row first, then the deeper supply-chain / LAIN-specific
signals second.

**Scope guard.** No step in this document touches `Cargo.toml` `version`,
the npm-shim `package.json`, the release workflow's tag-input, or any other
version-bearing artifact. Items that would (Latest Release, crates.io,
npm, MCP Registry) are intentionally **deferred** to a future "version
work" plan and are listed only at the bottom for visibility.

**Effort / value recap** (from the originating proposal):

| #  | Badge / signal                            | Effort          | Value         | Status before this plan | Action |
|----|-------------------------------------------|-----------------|---------------|-------------------------|--------|
| 1  | GitHub Actions / CI Passing               | ~10 min         | very high     | missing                 | add    |
| 2  | OpenSSF Scorecard                         | 30–60 min       | very high     | already wired           | verify |
| 3  | SafeSkill 88/100                          | already done    | high          | already wired           | verify |
| 4  | License: MIT                              | already done    | medium        | already wired           | verify |
| 5  | Supported Platforms                       | ~10 min         | medium-high   | missing                 | add    |
| 6  | Rust MSRV                                 | ~10 min         | medium        | missing                 | add    |
| 7  | Codecov / Coverage                        | 1–2 h           | high if good  | missing                 | add    |
| 8  | SBOM Available                            | 1–2 h           | high          | artifact exists         | add    |
| 9  | Verified Build Provenance                 | 2–4 h           | very high     | artifact exists         | add    |
| 10 | MCP Tools \| <count>                      | ~1 h            | LAIN-specific | missing                 | add    |
| 11 | Agent Contract \| Passing                 | ~1 h            | LAIN-specific | partly wired            | add    |
| 12 | Latest Release (DEFERRED — version)       | ~5 min          | high          | n/a                     | skip   |
| 13 | crates.io (DEFERRED — publish)            | 30–90 min       | very high     | n/a                     | skip   |
| 14 | npm Version/Downloads (DEFERRED — publish)| 5–30 min        | high          | n/a                     | skip   |
| 15 | MCP Registry (DEFERRED — publish)         | 30–90 min       | very high     | n/a                     | skip   |

## Target README shape after both waves

```text
[![CI](https://github.com/spuentesp/lain/actions/workflows/ci.yml/badge.svg)](…)
[![SafeSkill 88/100](…)](…)
[![OpenSSF Scorecard](…)](…)
[![License: MIT](…)](…)
[![Platforms: Linux | macOS | Windows](…)](…)
[![Rust 1.75 or newer](…)](…)
[![Codecov](…)](…)
[![SBOM](…)](…)
[![Build Provenance](…)](…)
[![MCP Tools | <count>](…)](…)
[![Agent Contract | Passing](…)](…)
```

---

## Wave 1 — Cheap, high-impact trust row

### Step 1 · CI Passing badge

**Why:** immediate proof the project builds and tests on every supported
platform. Free, comes from the existing `ci.yml` matrix.

**Files**

- `README.md` — insert the badge in the existing badge cluster above the
  hero description. Place it first so the eye lands on the green check.

**Implementation**

```markdown
[![CI](https://github.com/spuentesp/lain/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/spuentesp/lain/actions/workflows/ci.yml?query=branch%3Amain)
```

**Acceptance**

- Badge renders green against the current `main` branch.
- The link resolves to the `ci.yml` workflow run history filtered to `main`.

**Effort:** ~5 min.

---

### Step 2 · OpenSSF Scorecard (verify)

**Why:** independent security/repository-quality signal. Already in
README (`README.md:4`); re-verify the badge target and the workflow.

**Files**

- `README.md:4` — confirm URL is exactly
  `https://img.shields.io/ossf-scorecard/github.com/spuentesp/lain`
  pointing at `https://scorecard.dev/viewer/?uri=github.com/spuentesp/lain`.
- `.github/workflows/scorecard.yml` — confirm weekly cron at `17 5 * * 1`
  is still in place, and `publish_results: true` so the scorecard.dev
  listing updates.

**Acceptance**

- scorecard.dev viewer for `spuentesp/lain` returns a non-zero score and
  the badge reflects it.

**Effort:** ~5 min.

---

### Step 3 · SafeSkill 88/100 (verify)

**Why:** independent MCP/security signal. Already in README
(`README.md:3`).

**Files**

- `README.md:3` — confirm URL still resolves to
  <https://safeskill.dev/scan/spuentesp-lain>.
- `.github/workflows/safeskill.yml` — confirm the public rescan job on
  `main` is intact so the public listing stays current.
- `docs/SAFESKILL.md` — already explains the score boundary; no edit
  needed.

**Acceptance**

- The hosted report page is reachable and the badge colour matches the
  current score band (`>= 80 = yellow/green`, `40–79 = orange`,
  `< 40 = red`).

**Effort:** ~5 min.

---

### Step 4 · License: MIT (verify)

**Why:** standard OSS credibility signal. The file is correct; the
badge just needs to exist in the README cluster.

**Files**

- `README.md` — add a Shields static badge so the visual claim matches
  the file:

```markdown
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
```

- `LICENSE` — no edit (already MIT, year 2026, spuentesp).

**Acceptance**

- Badge clickable from README resolves to the `LICENSE` file at the repo
  root, file header matches the badge label.

**Effort:** ~2 min.

---

### Step 5 · Supported Platforms badge

**Why:** the project advertises Linux / macOS / Windows in CI; the badge
should advertise the same on the README so a reader doesn't have to
click through.

**Files**

- `README.md` — add a Shields static badge in the cluster:

```markdown
[![Platforms: Linux | macOS | Windows](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue)](…)
```

Link it to the CI matrix definition (`.github/workflows/ci.yml`) so the
claim is self-auditing.

**Caveat to document**

- The badge claims what the CI matrix covers. If the matrix drops or
  adds a platform, the badge text must change in the same PR. Pin this
  in `docs/CI.md` with a one-line note.

**Acceptance**

- README badge text matches the literal `os:` entries in
  `.github/workflows/ci.yml`.

**Effort:** ~10 min.

---

### Step 6 · Rust MSRV badge

**Why:** a small but meaningful technical-maturity signal. The README
already lists "Rust (build only) | 1.75 or newer" (`README.md:279`).

**Files**

- `README.md` — add a Shields dynamic badge tied to the `dtolnay/rust-toolchain`
  pin used by CI. We can use a Shields "for the badge" pattern that
  reads from a tiny JSON file we own (avoids the network-flap of
  third-party MSRV feeds):

```markdown
[![Rust 1.75 or newer](https://img.shields.io/badge/rust-1.75%20or%20newer-orange)](…)
```

- New file `docs/badges/msrv.json` (or inline Shields query) so the
  badge resolves. Cheapest path: ship a single Shields endpoint
  `https://img.shields.io/badge/rust-1.75%20or%20newer-orange` with the link
  pointing at `Cargo.toml`.

**Caveat to document**

- When the MSRV bumps in `rust-toolchain.toml` / CI, the badge text and
  the `Rust (build only)` README line must change together. Add a
  cross-reference in `docs/CI.md`.

**Acceptance**

- README text `1.75 or newer` matches the badge text `1.75 or newer`.

**Effort:** ~10 min.

---

## Wave 2 — Supply-chain and LAIN-specific signals

### Step 7 · Codecov / Coverage

**Why:** high value if the percentage is respectable. LAIN's Rust test
suite already runs in CI, so the only new piece is the uploader and the
badge.

**Files**

- `.github/workflows/ci.yml` — add a coverage job *only on `push` to
  `main`* (PRs would otherwise flake on cross-OS coverage):
  - `dtolnay/rust-toolchain` (already pinned).
  - `Swatinem/rust-cache` (already pinned).
  - `cargo llvm-cov --workspace --all-targets --lcov --output-path lcov.info`.
  - `codecov/codecov-action@v5` upload with `fail_ci_if_error: false`
    until the first green run establishes the baseline.
- `codecov.yml` at the repo root — set `target: 70%`, `threshold: 1%`
  and `range: 70…85` so coverage gates don't fail closed before the
  first deliberate baseline PR.
- `README.md` — add the Shields Codecov badge:

```markdown
[![Codecov](https://codecov.io/gh/spuentesp/lain/graph/badge.svg?token=…)](https://codecov.io/gh/spuentesp/lain)
```

**Out of scope for this plan**

- Choosing the baseline percentage is a separate decision (do it in
  the PR that turns on coverage). The badge value should not be
  published to README until the baseline is established.

**Acceptance**

- A real coverage report lands on codecov.io for the `ci.yml` job.
- README badge present, but only after the baseline PR.

**Effort:** 1–2 h (most of it is the codecov.io account setup the first
time).

---

### Step 8 · SBOM Available

**Why:** supply-chain artefact is already produced by `release.yml`
(`docs/SUPPLY_CHAIN.md:67-82`). The badge is the public-facing
existence claim.

**Files**

- `README.md` — add a Shields dynamic badge pointing at the latest
  release's SBOM asset:

```markdown
[![SBOM](https://img.shields.io/badge/SBOM-CycloneDX-blueviolet)](https://github.com/spuentesp/lain/releases/latest)
```

- `docs/VERIFICATION.md` — already documents the consumer commands
  (`cyclonedx-cli validate`, etc.). No edit.

**Caveat to document**

- The badge text "CycloneDX" matches what `anchore/sbom-action` emits.
  If we ever switch SBOM generators, the badge text and
  `docs/SUPPLY_CHAIN.md` move together.

**Acceptance**

- Clicking the badge lands on the latest GitHub release page where
  `*.cdx.json` (or current naming) is downloadable.

**Effort:** ~5 min.

---

### Step 9 · Verified Build Provenance

**Why:** strongest supply-chain claim in the current artefact set
(`docs/SUPPLY_CHAIN.md:49-63`). Pair the badge with a one-line
verification command.

**Files**

- `README.md` — add a Shields static badge:

```markdown
[![Build Provenance](https://img.shields.io/badge/Provenance-SLSA_L2-success)](docs/VERIFICATION.md)
```

- `docs/VERIFICATION.md` — already lists the `gh attestation verify`
  command; no edit.

**Acceptance**

- A consumer can copy the verification command from
  `docs/VERIFICATION.md`, point it at any release tarball, and pass.

**Effort:** ~5 min (badge wiring only; attestation is already live).

---

### Step 10 · MCP Tools | <count> (LAIN-specific)

**Why:** more marketing value than half the generic badges — it proves
LAIN is not a toy MCP server. Hard-coding the number drifts every time
a tool is added; a tiny CI job is the right shape.

**Design**

- New workflow `.github/workflows/mcp-tools-count.yml`:
  - Trigger: push to `main`, weekly `cron: '23 4 * * 1'`, manual dispatch.
  - Build `target/release/lain` on ubuntu.
  - Run `./target/release/lain schema dump --out /tmp/schema.json`.
  - `jq '. | length' /tmp/schema.json` → integer `N`.
  - Push the integer to a Shields endpoint backed by a tiny repo
    file: `.github/badges/mcp-tools.json`:

```json
{ "schemaVersion": 1, "label": "MCP tools", "message": "67", "color": "blue" }
```

  - Use `shieldsio/endpoint-badge` or commit the JSON to the repo and
    point the badge at `https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/spuentesp/lain/main/.github/badges/mcp-tools.json`.

**Files**

- `.github/workflows/mcp-tools-count.yml` — new.
- `.github/badges/mcp-tools.json` — new (committed, updated by the
  workflow via a follow-up PR or auto-commit on `main`).
- `README.md` — add the badge:

```markdown
[![MCP tools](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/spuentesp/lain/main/.github/badges/mcp-tools.json)](https://github.com/spuentesp/lain/blob/main/docs/tool-schema.json)
```

**Permissions**

- Job needs `contents: write` only on the *follow-up commit* step
  (separate `GITHUB_TOKEN` or `peter-evans/create-pull-request` if we
  want a PR instead of a direct push). Default to a PR for review.

**Acceptance**

- README badge value matches the current `docs/tool-schema.json`
  element count.
- Workflow fails if `lain schema dump` cannot produce a JSON file or
  if `jq` cannot parse it.
- Drift from committed `docs/tool-schema.json` is already enforced by
  the `schema-drift` job (`.github/workflows/ci.yml:217-252`); the
  count badge is downstream of that and must agree.

**Effort:** ~1 h.

---

### Step 11 · Agent Contract | Passing (LAIN-specific)

**Why:** the user-facing "this agent contract still holds" signal,
driven by the `tests/use_cases/battery_*` suite that the README already
leads with. We already have `capability` (demo.sh ground truth) and
`schema-drift` jobs; we need a single badge that says "all of the above
are green on this commit".

**Design**

Compose the existing jobs into a single commit status. Two viable shapes:

1. **Composite check (preferred).** Use a `gh action` workflow job that
   depends on `capability`, `tool-schema`, and `use-cases-battery`
   outcomes, with `needs:` and a final `if: success()` step that
   creates a single `lain/agent-contract` commit status. No new tests
   are added; existing job outcomes become the badge source.

2. **Dedicated smoke job.** Add a new job that runs
   `bash scripts/demo.sh --quick` *and* asserts the schema dump equals
   the committed snapshot *and* runs the subset of `tests/use_cases`
   that already exercise the agent-facing MCP surface. Single source of
   truth, more compute, harder to keep green.

Prefer (1). Add a new `.github/workflows/agent-contract.yml`:

```yaml
name: Agent contract
on:
  workflow_run:
    workflows: [CI]
    types: [completed]
permissions: { statuses: write }
jobs:
  rollup:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@… # pinned
      - name: Compute rollup
        run: |
          # Map the workflow_run conclusion for each child job to PASS/FAIL.
          # Emit a single commit status on the originating SHA.
          …
```

**Files**

- `.github/workflows/agent-contract.yml` — new.
- `README.md` — add the badge:

```markdown
[![Agent contract](https://github.com/spuentesp/lain/actions/workflows/agent-contract.yml/badge.svg?branch=main)](…)
```

**Acceptance**

- The badge turns green only when all the underlying jobs are green on
  the same SHA.
- The badge turns red the moment any underlying job fails on the same
  SHA.
- The contract does *not* re-run tests; it is a pure rollup. If a
  downstream test gets added later, the contract job is the place that
  has to learn about it.

**Effort:** ~1 h for the rollup workflow; longer if we choose shape
(2).

---

## Wave 3 — Deferred (version / publish work)

Documented here for visibility; **not in this plan's scope**.

| #  | Step                              | Why deferred |
|----|-----------------------------------|--------------|
| 12 | Latest Release badge              | Requires a release tag; touches the version-drift machinery in `ci.yml:128-174` |
| 13 | crates.io Version / Downloads     | Requires `cargo publish` and a token, plus a `Cargo.toml` version bump |
| 14 | npm Version / Downloads           | Requires publishing `@spuentesp/lain-mcp` with `npm publish` and a real `1.x.y` |
| 15 | MCP Registry listing              | Requires a published server entry with a real version reference |

These four will live in a separate `docs/release_publish_plan.md` once
the version-number machinery is settled.

---

## Rollout checklist

Each step is a single PR. Land in order; do not skip ahead.

- [ ] Step 1 — CI badge
- [ ] Step 2 — verify Scorecard
- [ ] Step 3 — verify SafeSkill
- [ ] Step 4 — MIT badge
- [ ] Step 5 — Platforms badge
- [ ] Step 6 — MSRV badge
- [ ] Step 7 — Codecov (gated on baseline)
- [ ] Step 8 — SBOM badge
- [ ] Step 9 — Provenance badge
- [ ] Step 10 — MCP tools count badge workflow
- [ ] Step 11 — Agent contract rollup workflow

## Definition of done (overall)

- All ten active steps land green on `main`.
- README renders the full badge row with no broken images / 404s.
- No step in this document edits `Cargo.toml` `version`,
  `npm-shim/package.json` `version`, the release tag input, or any other
  version-bearing artefact. That stays a property of the plan itself.
- `docs/AGENT_UX_ROADMAP.md` is updated to cross-reference this document
  in the "Trust and visibility" sub-section if it exists, otherwise at
  the top of `Suggested implementation order`.
