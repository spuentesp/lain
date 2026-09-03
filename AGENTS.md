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
2. **Upstream fix (commit `TBD`, tag `v0.7.1`):** bumped
   `Cargo.toml` to `0.7.1` and pushed the tag so the release
   workflow publishes corrected binaries to GitHub Releases.
   `server.json`'s top-level + nested `version` fields get
   updated automatically by the release workflow; `Formula/lain.rb`
   and `npm-shim/package.json` are out of scope for this fix.

After the `v0.7.1` workflow completes, the official tarballs at
<https://github.com/spuentesp/lain/releases/tag/v0.7.1> will
report the correct version string.

## Re-installing the official tarball

`install.sh` from upstream will now install `lain 0.7.1` (or newer),
so a fresh install no longer needs the local rebuild dance.

## If upstream `Cargo.toml` on `main` is regressed to `0.6.1`

That would re-introduce the original packaging bug. The fix on
this branch (or its descendant commits) bumps `Cargo.toml` to match
each release tag. Verify with `git log --oneline -- Cargo.toml`
before cutting a new tag.
