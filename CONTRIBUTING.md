# Contributing to LAIN

Thanks for considering a contribution. This file is the canonical entry
point for the change-management process; the details live in
[`docs/BRANCHING.md`](docs/BRANCHING.md) and
[`docs/SUPPLY_CHAIN.md`](docs/SUPPLY_CHAIN.md).

## Development setup

You need a recent stable Rust toolchain (1.85 or newer; check
`rust-toolchain.toml` if present, otherwise the toolchain that
last touched `Cargo.lock` is fine) and Node 18+
(the npm-shim postinstall runs under Node).

```bash
git clone https://github.com/spuentesp/lain
cd lain
cargo build                    # debug build
cargo test                     # unit + integration + use-case suite
cargo clippy -- -D warnings    # CI gates on this
cargo fmt --check              # CI gates on this
node npm-shim/scripts/install.test.js && \
  node npm-shim/bin/lain.test.js     # npm-shim test suite
```

For end-to-end coverage, run [`scripts/demo.sh`](scripts/demo.sh). It
exercises the agent contract (tool schema, capability suite, npm-shim)
against a sandbox repo and is what the
[`lain/agent-contract`](.github/workflows/agent-contract.yml) rollup
status check is built on.

A quick sanity check at any time: `cargo run -- doctor`. It validates
the local install against the binary's expectations and reports any
drift between the npm-shim and the underlying `lain` binary.

## Pull request flow

The two-branch model is described in detail in
[`docs/BRANCHING.md`](docs/BRANCHING.md). Short version:

1. Branch off `dev` — never off `main`.
   ```bash
   git switch dev
   git pull --ff-only
   git switch -c feature/<short-kebab-name>
   ```
2. Push the branch and open a PR against `dev`.
   ```bash
   git push -u origin HEAD
   gh pr create --base dev --head "$(git branch --show-current)"
   ```
3. CI must be green. The rollup check that matters is
   `lain/agent-contract`; lint, fmt, and clippy are enforced on the
   same run but not on the rollup.
4. One approving review from a maintainer is required before merge
   (`dev` itself doesn't require reviews; `main` does, see step 6).
5. After merging to `dev`, your change is in the next release.
6. Releases are cut by a maintainer opening a `release/vX.Y.Z` PR
   from `dev` into `main`. That PR needs one approval + a green
   `lain/agent-contract` rollup to land.

### Hotfix path

For something that can't wait for the next release:

```bash
git switch main
git switch -c hotfix/<short-kebab-name>
# … fix, push, open PR against main …
gh pr create --base main --head hotfix/<short-kebab-name>
```

After merge, fast-forward `dev` to catch up:

```bash
git switch dev
git merge --ff-only main
```

## Issue triage

- **`bug`** and **`feature request`** labels go to the GitHub issue
  queue and are triaged within **5 business days**.
- **`security`** issues are *never* filed publicly. Use the channels
  in [`SECURITY.md`](SECURITY.md) — GitHub Security Advisories is
  preferred; the `security@spuentes.dev` fallback is for emergencies.
- For `good first issue` / `help wanted` items, comment on the issue
  before opening a PR so a maintainer can confirm the scope matches
  what you'd like to work on.

## Coding conventions

- **`cargo fmt` and `cargo clippy -- -D warnings`** must pass on every
  PR. CI fails otherwise.
- **Public API changes** require a use-case test under
  `tests/use_cases/` covering the new surface. The `battery_*` tests
  in that directory are the template.
- **Server.json / metadata** changes flow through
  `docs/METADATA.toml`. Do not hand-edit `server.json`; the release
  workflow regenerates it from METADATA via
  `scripts/sync-server-json.py`.
- **Dependency additions** should be discussed in an issue first.
  The Scorecard `Vulnerabilities` check is sensitive to new transitive
  deps with known advisories, so we'd rather catch that in design
  than in the next weekly scorecard run.

## Commit and PR hygiene

- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
  style: `feat: …`, `fix: …`, `docs: …`, `chore: …`, `refactor: …`,
  `test: …`, `deps: …`, `release: …`. The PR title uses the same
  prefix.
- Keep PRs focused. A single change set per PR is much easier to
  review and to bisect on later. A 200-line PR is preferred over a
  2,000-line one, even if the work could be one branch.
- Squash-merge and rebase-merge are both allowed on protected
  branches. The merge button respects whatever the PR author picked
  in the GitHub UI.

## Release cadence

Tags follow [semver](https://semver.org/):

- **patch** (`v0.7.4`) for bug fixes and dependency bumps.
- **minor** (`v0.8.0`) for additive features and any change to the
  MCP tool surface (`docs/tool-schema.json`).
- **major** (`v1.0.0`) is reserved for breaking changes; the project
  hasn't shipped one yet.

A release PR goes from `dev` to `main`, gets one approval + green
`lain/agent-contract`, and on merge the tag is cut and
[`.github/workflows/release.yml`](.github/workflows/release.yml) takes
over: it builds the three platform binaries, attaches SBOMs + SLSA
provenance, uploads a `SHA256SUMS` aggregate, refreshes `server.json`,
and publishes the npm shim. See [`docs/VERIFICATION.md`](docs/VERIFICATION.md)
for what consumers do with those artifacts.

## Release-signing key

The release workflow uses GitHub's OIDC-based build provenance
attestation (`actions/attest-build-provenance@v2`). We do not
maintain a separate PGP / Sigstore key for the project; the workflow
itself is the trust root. If you ever need a signing key for something
specific (e.g., a third-party package repo that doesn't accept OIDC
provenance), open an issue and we'll add it.

## Where to ask questions

- **Bugs and feature requests**: GitHub Issues.
- **Security**: [`SECURITY.md`](SECURITY.md) (private channels only).
- **Design / scope discussion**: open an issue with the `discussion`
  label, or comment on an existing one.
- **Anything else**: the maintainer's GitHub profile lists current
  contact preferences.

## License

By contributing, you agree that your contributions are licensed under
the project's [MIT license](LICENSE).