# Security Policy

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

### What to expect

- **Acknowledgement** within 5 business days.
- **Triage** within 10 business days: we confirm the report, decide on
  severity, and propose a coordinated disclosure timeline.
- **Fix and release:** patches land on a private fork first, then ship
  with the next release. Releases include a build provenance attestation
  (see [`docs/VERIFICATION.md`](docs/VERIFICATION.md) once PR #2 of the
  Trust & Distribution track has merged) so downstream users can
  cryptographically verify what they downloaded.
- **CVE assignment** via GitHub Advisories for anything rated moderate
  or higher.

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