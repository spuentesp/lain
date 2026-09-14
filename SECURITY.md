# Security Policy

LAIN treats the code-intelligence surface it gives to AI agents as
trust-sensitive infrastructure. A compromised binary or a tampered
graph would erode the only guarantee an agent has about the code it's
about to edit, so security reports are taken seriously and processed
on a tight timeline.

## Supported Versions

The latest released version receives security fixes. The previous minor
release receives fixes for at least 30 days after a new minor is cut;
anything older is best-effort.

| Version | Supported          |
|---------|--------------------|
| latest  | :white_check_mark: |
| prev.   | :white_check_mark: (30-day window) |
| older   | :x: (best-effort)   |

## Reporting a Vulnerability

**Primary channel:** GitHub Security Advisories — open a private report
via the repo's *Security* tab → *Report a vulnerability*. This routes
through GitHub's disclosure flow and gives us a private fork to develop
the fix in.

**Fallback:** email the maintainer at the address listed on the GitHub
profile if the Advisories flow is unavailable for any reason.

### Disclosure timeline

We follow a 90-day coordinated disclosure window from the date a
report is acknowledged.

| Day | Action |
|---|---|
| 0 | Vulnerability reported (private). |
| +5 business days | Acknowledgement sent, severity assigned. |
| +10 business days | Triage complete; fix plan proposed. |
| ≤ +90 days | Patch shipped and advisory published. |

A reporter can request a longer or shorter embargo; we negotiate that
during triage. We will not pursue legal action against researchers who
stay within this policy and act in good faith.

### What to expect

- **Acknowledgement** within 5 business days.
- **Triage** within 10 business days: we confirm the report, decide on
  severity, and propose a coordinated disclosure timeline.
- **Fix and release:** patches land on a private fork first, then ship
  with the next release. Releases include a build provenance attestation
  (see [`docs/VERIFICATION.md`](docs/VERIFICATION.md)) so downstream
  users can cryptographically verify what they downloaded.
- **CVE assignment** via GitHub Advisories for anything rated moderate
  or higher.
- **Recognition:** reporters who agree are credited in the advisory
  release notes.

## Ongoing Automated Checks

The repo runs the following checks on every push and pull request:

| Check | What it covers | Workflow |
|-------|----------------|----------|
| SafeSkill | npm-shim + agent-instruction surface | `.github/workflows/safeskill.yml` |
| OpenSSF Scorecard | supply-chain hygiene across the repo | `.github/workflows/scorecard.yml` |
| Dependency Review | new transitive deps with known CVEs | `.github/workflows/dependency-review.yml` |
| Cargo Test + clippy + fmt | Rust unit, integration, and use-case suites | `.github/workflows/ci.yml` |
| npm-shim tests | the JavaScript install script end-to-end | `.github/workflows/ci.yml` (npm-shim job) |

Reports from each check are visible on the repo's *Security* tab.

## Dependency Updates

GitHub Actions, Cargo crates, and npm packages are kept current by
Dependabot — see [`.github/dependabot.yml`](.github/dependabot.yml).
Dependabot PRs surface in the *Pull requests* tab; each one carries
a CI run before merge.

## Scope Notes

SafeSkill and OpenSSF Scorecard scan what they can fingerprint. The
Rust core is not currently covered by a dedicated Rust security audit
tool in the CI matrix; the Scorecard finding list tells us when to
add one (see [`docs/SUPPLY_CHAIN.md`](docs/SUPPLY_CHAIN.md) once
PR #2 lands).