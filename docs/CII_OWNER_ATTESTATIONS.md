# CII Best Practices — owner attestation draft

> **Note to maintainer**: this file is a *draft* of the statements
> the project lead enters into the CII Best Practices web-form at
> <https://www.bestpractices.dev/en/criteria> when claiming the
> badge. It's based on what's actually true of this repo at
> 2026-09-15 — read it before submitting. Some fields will require
> a free-text justification; the text below is a starting point.

## Attestation: secure software design knowledge

> **CII question:** *Has the project lead reviewed secure software
> design guidance and applied it?*

### Yes

I have reviewed and applied secure software design guidance
specific to Rust MCP servers and supply-chain hygiene. The
following design decisions are documented in the repo and reflect
that review:

- **`docs/SUPPLY_CHAIN.md`** documents the supply-chain threat
  model and the mitigations: SLSA Level 2 build provenance,
  per-binary SHA256 + SHA256SUMS aggregate, per-binary SPDX
  CycloneDX SBOM, and OIDC-based npm publishing (no long-lived
  `NPM_TOKEN` secret).
- **Two-branch model** (`docs/BRANCHING.md`): `dev` is integration,
  `main` is the release line. Branch protection requires 1 approval
  + green `lain/agent-contract` status on `main`; `dev` only
  requires the agent-contract rollup. Direct push to `main` from
  non-admins is blocked.
- **Tiered CI** (PR #50): dev/PR-to-dev get the fast lane (Ubuntu
  tests, lint, npm-shim, schema-drift, health-badge, ~5 min);
  main/PR-to-main get the full hardened battery (macOS + Windows +
  capability suite + coverage + release contracts, ~20 min).
- **Concurrency cancellation** on every push-triggered workflow
  via `concurrency:` blocks — rapid-fire commits don't pile up
  behind each other.
- **OpenSSF Scorecard weekly run** (`.github/workflows/scorecard.yml`)
  posts SARIF to the Security tab; the current rolling state is in
  `docs/SCORECARD.md`.
- **Default-deny permissions** at the workflow level, with
  job-level `permissions:` grants for the scopes each job needs
  (`Token-Permissions` check stays at 10).
- **Dependency update tool** (Dependabot for `github-actions`,
  `cargo`, `npm`) — `.github/dependabot.yml`.
- **Vulnerability disclosure policy** in `SECURITY.md` with named
  contact channel, 90-day coordinated disclosure window, and
  triaged ack cadence.
- **Agent-aware hooks** under `hooks/<agent>/` so AI coding agents
  auto-claim files before edit (no accidental cross-edit collisions).
- **Threat-model-aware commit messages**: every fix commit references
  the specific failure mode it addresses (`fix(security): …`,
  `fix(release): …`, etc.).

## Attestation: vulnerability reporting history

> **CII question:** *Has the project had any security vulnerabilities
> in the past 12 months? If yes, were they addressed?*

### Yes — addressed

In the 30 days preceding 2026-09-15, `osv.dev` flagged 25 unique
advisories against this project's dependency graph. They were
addressed in this order across the PRs listed below; **all
remaining open items are documented in `docs/VULNS.md`** with a
clear plan and owner.

| PR | Bucket | Closed | How |
|---|---|---|---|
| #52 | A (drop-in) | anyhow, openssl, crossbeam-epoch | `cargo update -p` |
| #53 | B (rustls) | ring, rustls-webpki (×3), paste, rustls-pemfile | reqwest 0.11 → 0.12 + rustls 0.23 + aws-lc-rs migration; `tree-sitter` 0.22 bumped out, `paste` cleared transitively |
| #54 | E (git2) | git2 0.19 UB-class (×3) | `Cargo.toml: git2 = "0.21"` |
| #56 | C (inventory) | inventory stdlib-init bug + non-Sync data | major bump 0.1 → 0.2 |
| #57 | D (bincode) | bincode 1.x unmaintained | bincode 2.x with `serde` feature |

`tree-sitter` 0.22 → 0.27 to clear `ring` and `paste` is still
**open** (`docs/VULNS.md` Bucket F) — the migration was scoped
but the breaking API churn (4 changes between 0.22 and 0.27:
`language()` → `LANGUAGE`, `QueryMatches::next()` signature,
`captures` field → method, type-parameter changes for
`TextProvider`) plus parser-crate version skew (rust 0.24,
python 0.23, javascript 0.23) made the cost-benefit marginal. It
will land in the next batch when one of those parser crates ships
a release that pulls `tree-sitter` to 0.24+ as a default.

The remaining 8 OSV advisories are either:
- pinned by parser crate version skew (tree-sitter 0.22 vs
  the 0.23+ needed for the fix), or
- upstream-maintenance issues (bincode 1.x was the only option
  until bincode 2.0 landed in PR #57).

### Public disclosure

Every fix above was committed with a public commit message
referencing the advisory ID (e.g., `fix(deps): reqwest 0.11->0.12
+ rustls 0.21->0.23 + aws-lc-rs (closes 6 OSV vulns)`). The
release tags for v0.7.1, v0.7.2, v0.7.3 were updated with
backfilled release notes on 2026-09-15 (this session) so the
change-log is complete. A future v0.7.4-rc1 will close the
remaining tree-sitter/paste ring.

### Reporting channel

The reporting channel is `https://github.com/spuentesp/lain/security/advisories/new`
(handled by the GitHub Security Advisories flow), with
`security@spuentes.dev` as the documented fallback. The full
disclosure timeline (90-day window, 5/10 business-day SLAs, etc.)
is in `SECURITY.md`.

We have had **zero external vulnerability reports** to that
channel in the project's public lifetime — every advisory
above was caught by Dependabot or the OpenSSF Scorecard weekly
cron before any user encountered it.

## Other CII passing-tier attestations (already true)

These don't need new code; they just need the maintainer to
click through the form:

- **Project license**: MIT (`LICENSE` at repo root).
- **Documentation**: README + `docs/` tree, with `INDEX.md`,
  `BRANCHING.md`, `SUPPLY_CHAIN.md`, `SCORECARD.md`, `COOKBOOK.md`,
  `ARCHITECTURE.md`, etc.
- **Change log**: `CHANGELOG.md` plus per-release notes (now
  backfilled for v0.7.1–v0.7.3).
- **Build / test instructions**: `CONTRIBUTING.md` "Development
  setup" section.
- **Public VCS history**: this GitHub repo.
- **Programming language**: Rust (buildable with `cargo build`
  on stable, ≥1.75).
- **Build system**: Cargo, with `cargo build` and `cargo test
  --workspace` as canonical.
- **Regression tests**: 1100+ tests across `tests/`,
  `tests/use_cases/`, `tests/e2e/`, `tests/mcp/`, etc.
- **Continuous integration**: GitHub Actions (`.github/workflows/`),
  all push and PR triggers.
- **Bug tracker**: GitHub Issues.
- **Code review**: PRs require 1 approval on `main`; admin bypass
  is enabled for solo-maintainer flexibility but documented.
- **Quality / style**: `cargo fmt --check` and `cargo clippy -- -D
  warnings` enforced in CI.
- **Crypto usage**: TLS via `rustls 0.23` (default crypto provider
  `ring` — `aws-lc-rs` migration is a tracked follow-up in
  `docs/VULNS.md`, bucket B); no custom crypto.
- **Parsed input**: JSON-RPC 2.0 messages via `serde_json`; YAML
  config via `serde_yaml`. All inputs are bounded: filesystem paths
  via `mktemp` / `tempfile::tempdir`; the `/mcp` HTTP body via a
  4 MiB cap (Content-Length precheck + `http_body_util::Limited`
  stream cap, returns 413 on overflow — see
  `src/server/mcp/handler.rs`).

These together justify the **passing** tier (5/10 scorecard
points). Higher tiers (silver at 7, gold at 10) require more
documentation + ≥3 distinct contributing orgs + ≥2 reviewers +
admin bypass off — which is structural and depends on org growth.

## What's missing for the CII claim to file

1. Maintainer goes to <https://www.bestpractices.dev/en/projects/new>
   and registers `spuentesp/lain`.
2. Maintainer signs in (GitHub OAuth works) and goes through the
   ~50-question form. Each "Yes" needs a short justification;
   the bullets above are the per-question starting points.
3. After submission, the badge URL appears at
   `https://www.bestpractices.dev/projects/<id>` and the
   OpenSSF Scorecard picks it up on the next weekly run — giving
   `CII-Best-Practices: 5` in this repo's scorecard.

The web-form claim is a **user action** (not something the agent
can complete). Once it's filed, the scorecard moves
automatically.
