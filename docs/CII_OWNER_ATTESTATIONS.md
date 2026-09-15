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
  per-binary SHA256 + SHA256SUMS aggregate, per-binary CycloneDX
  JSON SBOM, and OIDC-based npm publishing (no long-lived
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
- **Concurrency cancellation** on the core CI workflow
  (`.github/workflows/ci.yml`) via `concurrency:` blocks — rapid-fire
  commits cancel superseded runs.
- **OpenSSF Scorecard weekly run** (`.github/workflows/scorecard.yml`)
  posts SARIF to the Security tab; the current rolling state is in
  `docs/SCORECARD.md`.
- **Least-privilege permissions**: default-deny or read-only permissions
  at the workflow level, with job-level `permissions:` grants for the scopes
  each job needs (`Token-Permissions` scores 9/10 per `docs/SCORECARD.md`,
  reflecting minimal read grants in workflows like SafeSkill).
- **Dependency update tool** (Dependabot for `github-actions`,
  `cargo`, `npm`) — `.github/dependabot.yml`.
- **Vulnerability disclosure policy** in `SECURITY.md` with named
  contact channel, 90-day coordinated disclosure window, and
  triaged ack cadence.
- **Agent-aware hooks** under `hooks/<agent>/` providing automated
  claim scripts for active CLI agents (Agy, Codex, Claude Code, Kimi)
  and documented locking conventions for other tools.
- **Threat-model-aware commit messages**: every fix commit references
  the specific failure mode it addresses (`fix(security): …`,
  `fix(release): …`, etc.).

## Attestation: vulnerability reporting history

> **CII question:** *Has the project had any security vulnerabilities
> in the past 12 months? If yes, were they addressed?*

### Yes — remediation in progress (17 resolved, 8 remaining)

In recent audits, `osv.dev` flagged 25 unique advisories against this
project's dependency graph. 17 advisories were resolved across the PRs
listed below, with all remaining 8 transitive items tracked in
`docs/VULNS.md` with explicit remediation plans.

| PR | Bucket | Closed | How |
|---|---|---|---|
| #52 | A (drop-in) | anyhow, openssl, crossbeam-epoch (5) | `cargo update -p` |
| #53 | B (rustls) | rustls-webpki (×3), rustls-pemfile, and transitive TLS (6) | `reqwest 0.11 → 0.12` + `rustls 0.23` migration |
| #54 | E (git2) | git2 0.19 UB-class (3) | `Cargo.toml: git2 = "0.21"` |
| #56 | C (inventory) | inventory stdlib-init bug + non-Sync data (2) | major bump `inventory 0.1 → 0.2` |
| #57 | D (bincode) | bincode 1.x unmaintained (1) | `bincode 2.0.1` with `legacy` config and `serde` feature |

The remaining 8 open advisories are transitive dependencies tracked in
`docs/VULNS.md`:
- `ring` (0.17.9) pulled by `rustls` / `quinn-proto`
- `paste` (1.0.15) pulled transitively by `tokenizers`
- `h2` / parser crate skew (Bucket F)

These are tracked in `docs/VULNS.md` for upcoming dependency upgrade
batches as upstream crates release compatible versions.

### Public disclosure

Every fix above was committed with a public commit message
referencing the advisory ID (e.g., `fix(deps): reqwest 0.11->0.12
+ rustls 0.21->0.23 (closes 6 OSV vulns)`). The release notes
for v0.7.1 through v0.7.4-rc1 document resolved advisories and
user-facing changelogs.

### Reporting channel

The reporting channel is `https://github.com/spuentesp/lain/security/advisories/new`
(handled by the GitHub Security Advisories flow), with
`security@spuentes.dev` documented in `CONTRIBUTING.md` as direct
maintainer contact. The full disclosure timeline (90-day window,
5/10 business-day SLAs) is in `SECURITY.md`.

We have had **zero external vulnerability reports** received through
disclosure channels in the project's public lifetime; dependency
vulnerabilities are tracked proactively via Dependabot and weekly
OpenSSF Scorecard scans.

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
  on stable, ≥1.82; recommended ≥1.85 per `CONTRIBUTING.md`).
- **Build system**: Cargo, with `cargo build` and `cargo test
  --workspace` as canonical.
- **Regression tests**: 1100+ tests across `tests/`,
  `tests/use_cases/`, `tests/e2e/`, and Rust unit/doc tests in `src/`.
- **Continuous integration**: GitHub Actions (`.github/workflows/`),
  all push and PR triggers.
- **Bug tracker**: GitHub Issues.
- **Code review**: PRs require 1 approval on `main`; admin bypass
  is enabled for solo-maintainer flexibility but documented.
- **Quality / style**: `cargo fmt --all -- --check` and `cargo clippy
  --workspace --all-targets` enforced in CI.
- **Crypto usage**: TLS via `rustls 0.23` with the `ring`
  cryptographic provider; no custom crypto.
- **Parsed input**: JSON-RPC 2.0 messages parsed via `serde_json`;
  YAML config via `serde_yaml`. Deserialization errors return
  structured JSON-RPC error responses rather than panicking;
  test fixtures and scratch areas use isolated temporary directories
  via `tempfile`.

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
