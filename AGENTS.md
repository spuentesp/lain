# AGENTS.md — notes for AI agents working in this Lain checkout

## Background

The upstream `v0.7.0` release tarball shipped a `lain` binary whose
`--version` reported `lain 0.6.1`. Root cause: `Cargo.toml`'s
`version` field was never bumped from `0.6.1` when the `v0.7.0`
git tag was cut, so any binary built from that tree carried the
old version string. Functionally the binary behaved as v0.7.0;
only the version string was wrong.

## Status (2026-09-02 → 2026-09-03)

1. **Local fix (commits `93d6344` + `ae2a527`):** bumped
   `Cargo.toml` to `0.7.0` on `fix/v0.7.0-version-bump`, merged
   to `main`, and rebuilt `~/.local/lain/lain` so `--version`
   correctly reports `lain 0.7.0`. Original tarball binary is
   kept as `~/.local/lain/lain.bak.0.6.1`.
2. **Upstream fix (tag `v0.7.1`):** bumped `Cargo.toml` to `0.7.1`
   and pushed the tag so the release workflow publishes corrected
   binaries to GitHub Releases. `server.json`'s top-level + nested
   `version` fields get updated automatically by the release
   workflow; `Formula/lain.rb` and `npm-shim/package.json` are out
   of scope for this fix.
3. **Installer fix (tag `v0.7.2`):** `install.sh` had a
   function-call-ordering bug — it invoked
   `apply_noninteractive_defaults` at the top of the file before
   defining the function further down. With `set -e`, that killed
   the script with `command not found` before any work happened,
   which is why this environment always installed Lain manually.
   `apply_noninteractive_defaults` is now defined above its call
   site, so `curl … | bash` and direct invocation both work.

After the `v0.7.1` workflow completed, the official tarballs at
<https://github.com/spuentesp/lain/releases/tag/v0.7.1> report
the correct version string. After the `v0.7.2` workflow
completes, fresh installs no longer hit the silent-exit bug.

## Re-installing the official tarball

`install.sh` from upstream will now install `lain 0.7.2` (or
newer). Fresh installs work end-to-end via `curl … | bash` or
direct invocation — no more manual install dance.

## CI badge

The repo runs its own `lain-health-badge` action on every pull
request — see `.github/actions/lain-health-badge/`. The action
is the artifact of `8931bd1`; this note is the documentation of
that fact for future agents landing changes.

## If upstream `Cargo.toml` on `main` is regressed to `0.6.1`

That would re-introduce the original packaging bug. The fix on
this branch (or its descendant commits) bumps `Cargo.toml` to
match each release tag. Verify with `git log --oneline --
Cargo.toml` before cutting a new tag.
