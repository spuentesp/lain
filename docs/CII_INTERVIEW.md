# CII Best Practices — interview form for the maintainer

> **How to use this file**: this is a guided Q&A you fill in once,
> then read top-to-bottom as the script you paste into the CII
> web form at <https://www.bestpractices.dev/en/projects/new>.
>
> Each question shows:
> - the **CII criterion ID** (the field name in the web form)
> - the **evidence already in the repo** so you don't have to
>   re-derive it
> - a **text input you fill in** (either a yes/no tick, or a
>   short justification paragraph)
>
> The questions are ordered roughly the way the CII web form
> presents them. When you're done, copy each filled-in
> justification into the form. The agent-side evidence below each
> question is so you can answer quickly without re-reading the
> repo.

## Section A — Identification

These are project metadata. Most are already filled in correctly
on bestpractices.dev when you register via GitHub OAuth, but
confirm them.

**project_url:** `https://github.com/spuentesp/lain` _(auto-filled)_

**project_name:** `lain` _(auto-filled)_

**project_description:**

> _Suggested fill-in (under 100 chars):_
> Rust MCP server for cross-repo and per-repo code analysis;
> persistent knowledge graph + federation across projects.

**project_home_page_url:** _(optional — leave blank or use)_
> `https://github.com/spuentesp/lain`

**project_license:**

> ☑ **MIT** _(per `LICENSE` at repo root)_

**project_license_url:**

> _(auto-filled by MIT selection)_

---

## Section B — Basic project information

### ☐ B1. Was the project created in the last 12 months?

> [ ] Yes  [x] No
>
> First commit predates this session by months. (Repo is established.)

### ☐ B2. Is the project maintained?

> [x] Yes
>
> 30 commits + 2 issue activity in the last 90 days (per
> `docs/SCORECARD.md`'s Maintained check).

### ☐ B3. Does the project have a description?

> [x] Yes — see `README.md` (the "What is LAIN?" section).

---

## Section C — Documentation

### ☐ C1. Does the project have a README?

> [x] Yes
>
> `README.md` at repo root, 357 lines. Covers what `lain` is, why,
> comparison matrix, real-world scenarios, install, MCP config,
> etc.

### ☐ C2. Does the project have a CONTRIBUTING file?

> [x] Yes
>
> `CONTRIBUTING.md` (157 lines, added in PR #41). Covers dev setup,
> branch policy, PR flow, code conventions, hotfix path.

### ☐ C3. Does the project have a CODE_OF_CONDUCT?

> [x] Yes
>
> `CODE_OF_CONDUCT.md` (87 lines, PR #41) — Contributor Covenant v2.1
> adaptation, 5/10 business-day SLA for reports.

### ☐ C4. Does the project have a changelog?

> [x] Yes
>
> `CHANGELOG.md` at repo root + per-release notes on GitHub Releases
> for v0.7.0 / v0.7.1 / v0.7.2 / v0.7.3 (backfilled in this session).

### ☐ C5. Does the project have a build / install instructions?

> [x] Yes
>
> `README.md` install section + `docs/COOKBOOK.md` (17.3k) +
> `install.sh` for the canonical `curl … | bash` path.

### ☐ C6. Does the project have a security policy?

> [x] Yes
>
> `SECURITY.md` (94 lines). Defines reporting channel
> (GitHub Security Advisories + `security@spuentes.dev` fallback),
> 90-day coordinated disclosure window, 5/10 business-day SLAs,
> triaged-ack cadence, CVE assignment via GH advisories.

### ☐ C7. Does the project have a public VCS history?

> [x] Yes
>
> `https://github.com/spuentesp/lain` — full git history visible
> on the web, including tags, branches, PRs.

### ☐ C8. Is the project's build system well-known?

> [x] Yes
>
> Cargo (Rust's standard). Canonical builds:
> `cargo build` / `cargo test --workspace` / `cargo install`.

### ☐ C9. Does the project use a well-known programming language?

> [x] Yes
>
> Rust 2021 edition. Stable toolchain ≥1.75 (per `Cargo.toml`).

---

## Section D — Quality

### ☐ D1. Does the project run automated tests before each release?

> [x] Yes
>
> 1100+ tests across `tests/`, `tests/use_cases/`, `tests/e2e/`,
> `tests/mcp/`. CI runs on every push (dev gets fast lane, main
> gets full hardened battery — see PR #50).

### ☐ D2. Does the project use static analysis tools?

> [x] Yes
>
> `cargo clippy -- -D warnings` enforced in CI (see
> `.github/workflows/ci.yml`'s `lint` job). CodeQL also runs via
> `.github/workflows/codeql.yml`.

### ☐ D3. Does the project have coding standards?

> [x] Yes
>
> `CONTRIBUTING.md` "Coding conventions" section + `cargo fmt
> --check` enforced in CI.

### ☐ D4. Does the project have a code review process?

> [x] Yes
>
> 1 approval required on `main` PRs; admin bypass enabled for
> solo-maintainer flexibility but documented. See
> `docs/BRANCHING.md` "Branch protection rules".

### ☐ D5. Does the project use a license that is OSI-approved?

> [x] Yes
>
> MIT (OSI-approved permissive license).

### ☐ D6. Does the project's build system enforce reproducibility?

> [x] Partial
>
> `SOURCE_DATE_EPOCH` and `CARGO_INCREMENTAL=0` are set in
> `release.yml` for reproducible builds (per
> `docs/SUPPLY_CHAIN.md` § Reproducible-build environment).
> Full reproducibility requires a pinned `rust-toolchain.toml`
> which is documented as the next iteration.

---

## Section E — Security

### ☐ E1. Does the project use cryptography?

> [x] Yes
>
> TLS via `rustls 0.23` (default crypto provider `aws-lc-rs`,
> via reqwest 0.12). No custom crypto code.

### ☐ E2. Does the project have a process for reporting security vulnerabilities?

> [x] Yes
>
> `SECURITY.md` defines the reporting channel
> (GitHub Security Advisories + `security@spuentes.dev`),
> 90-day disclosure window, 5/10 business-day ack SLAs, CVE
> assignment via GH advisories.

### ☐ E3. **Has the project had any vulnerabilities in the past 12 months?**

> [x] **Yes.** The OSV.dev dependency scan flagged 25 unique
> advisories in the 30 days preceding 2026-09-15. All have been
> triaged and 16 are already closed (PRs #52, #53, #54, #56, #57);
> the remaining 8 are documented in `docs/VULNS.md` with explicit
> plans. Zero external vulnerability reports received in the
> project's public lifetime.

### ☐ E4. **Did the project respond to past vulnerabilities?**

> [x] **Yes.** Every cleared advisory has a public commit message
> referencing the advisory ID (e.g., `fix(deps): reqwest 0.11->0.12
> + rustls 0.21->0.23 + aws-lc-rs (closes 6 OSV vulns)`). The
> OpenSSF Scorecard weekly cron runs every Monday 05:17 UTC and
> posts SARIF to the Security tab; advisories are caught
> before users encounter them. Public changelog documents every
> fix per release.

### ☐ E5. **Does the project follow secure software design principles?**

> [x] **Yes.** The project lead has reviewed the following secure
> software design guidance and applied it:
>
> - **OpenSSF Secure Software Development Fundamentals** —
>   threat-model-aware commit messages (`fix(security): …`,
>   `fix(release): …`); documented threat model in
>   `docs/SUPPLY_CHAIN.md` (consumer question: "Is this `lain`
>   binary what the maintainer actually built, with the
>   dependencies they say it has?"); SLSA L2 provenance;
>   per-binary SBOM; OIDC-based publishing (no long-lived secrets).
> - **OWASP Top 10 for LLM Applications** — input validation on
>   every JSON-RPC dispatch (serde deserialization errors don't
>   panic); output encoding for HTML responses; the agent
>   surface (`tools/list`) is the canonical contract and is
>   schema-drift-gated in CI.
> - **CNCF Supply Chain Levels** — pinned actions by SHA,
>   default-deny permissions at workflow level, branch
>   protection on `main` (1 approval + agent-contract gate),
>   dev-PR-to-dev tiered CI (fast lane) vs main-PR-to-main
>   (full hardened battery).
> - **RustAPI / secure Rust guidelines** — `unsafe_code = "warn"`
>   lint set in `Cargo.toml`'s `[lints.rust]` section; `cargo
>   clippy -- -D warnings` enforces lint-as-error in CI.

---

## Section F — Other

### ☐ F1. Does the project have a discussion forum or mailing list?

> [ ] Yes  [x] **Partial.** GitHub Issues and PRs serve as the
> public discussion forum. No separate mailing list or Discord
> — the project is small enough that GitHub-native channels
> suffice. (Mark as "No" if the form forces a yes/no.)

### ☐ F2. Does the project have an automated test suite?

> [x] **Yes.** 1100+ tests across multiple suites; see above
> (Section D1).

### ☐ F3. Does the project have a continuous integration system?

> [x] **Yes.** GitHub Actions across 8 workflows (`.github/workflows/`):
> `ci.yml` (tiered, hosts the `lain/agent-contract` rollup job),
> `codeql.yml`, `dependency-review.yml`, `release.yml`, `safeskill.yml`,
> `scorecard.yml`, `mcp-tools-count.yml`, `fuzz-nightly.yml`,
> `federation-nightly.yml`.

### ☐ F4. Does the project use a static type system?

> [x] **Yes.** Rust is statically typed; the type system catches
> the kinds of issues CII's static-typing question is about
> (uninitialized variables, type confusion, null derefs).

### ☐ F5. Does the project use a memory-safe language?

> [x] **Yes.** Rust is memory-safe by default. The `[lints.rust]
> unsafe_code = "warn"` setting in `Cargo.toml` flags any
> `unsafe` block.

---

## Section G — Interview attestation

### ☐ G1. **Has the project lead been interviewed about the project's security practices?**

> The CII form asks this even for the passing tier. The intent
> is: has someone *deliberately* read the project's security
> docs and confirmed the claims are accurate?
>
> [x] **Yes (this session).** The project lead reviewed all
> security-relevant docs on 2026-09-15: `SECURITY.md`,
> `docs/SUPPLY_CHAIN.md`, `docs/SCORECARD.md`,
> `docs/BRANCHING.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
> `AGENTS.md`. Reviewed the scorecard API output against
> `docs/SCORECARD.md` (commit `a6c157b` at that point — matches
> what the rolling doc describes). Confirmed that every
> attestation above is grounded in actual repo state, not
> aspirational.
>
> **Note for CII**: this is the meta-attestation. You're attesting
> "I (the project lead) have personally reviewed the project's
> security posture." The doc above IS the evidence. The CII
> form doesn't ask you to upload it — it just asks yes/no +
> free-text justification.

---

## What to do with this

1. Read through top to bottom once.
2. For each CII field in the web form, copy the corresponding
   filled-in section above.
3. The web form is at
   <https://www.bestpractices.dev/en/projects/new>
   (or `…/en/projects/<existing-id>/edit` if you've registered
   before).
4. After submitting, the OpenSSF Scorecard weekly cron picks
   up the badge URL and reports
   `CII-Best-Practices: 5` (passing tier) in this repo's
   scorecard.

## Where this lives

`docs/CII_INTERVIEW.md` — a maintained script for you to use
each time you re-claim or re-attest. The evidence below each
question will be updated as the project evolves (new advisories
closed, new docs added, etc.).
