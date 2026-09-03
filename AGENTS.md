# AGENTS.md — notes for AI agents working in this Lain checkout

## Local rebuild of `~/.local/lain/lain` (2026-09-02)

The upstream `v0.7.0` release tarball ships a `lain` binary whose
`--version` reports `lain 0.6.1`. The root cause: `Cargo.toml`'s
`version` field was never bumped from `0.6.1` when the `v0.7.0`
git tag was cut, so any binary built from that tree carries the
old version string. Functionally the binary behaves as v0.7.0;
only the version string is wrong.

To fix the user-facing version string, a local rebuild was done
from branch `fix/v0.7.0-version-bump` (commit `93d6344`), which
bumps `Cargo.toml` to `0.7.0` and lets cargo regenerate `Cargo.lock`.

Build command used:

```
PATH="$HOME/.cargo/bin:$PATH" cargo build --release --bin lain
```

The installed binary at `~/.local/lain/lain` is the output of that
build. The original tarball binary was backed up to
`~/.local/lain/lain.bak.0.6.1` before the swap.

## If upstream `Cargo.toml` on `main` still says `0.6.1`

Expected. `fix/v0.7.0-version-bump` exists as a local fix, but the
underlying packaging bug (forgetting to bump `Cargo.toml` for the
tag) is still present on `main` until that branch is merged. To make
the fix permanent upstream, fast-forward or merge
`fix/v0.7.0-version-bump` into `main`. The same bump should
probably also land on `Formula/lain.rb`, `server.json`, and
`npm-shim/package.json` so the whole repo is consistent (out of
scope for this urgent fix — they don't affect `--version`).

## Re-installing the official tarball

If `install.sh` is re-run from upstream, the broken v0.7.0 binary
will overwrite the locally-rebuilt one and `--version` will go back
to reporting `0.6.1`. Re-apply the local rebuild steps above if
the version string matters again.
