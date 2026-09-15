# Vulnerabilities — triage and remediation log

This document is the rolling inventory of OSV advisories against
`lain`'s dependency graph and what we're doing about each one. The
OpenSSF Scorecard `Vulnerabilities` check reads from this same
source (OSV.dev) so improvements here move the score.

**Snapshot date:** 2026-09-15
**Scorecard snapshot:** 6.8/10 (post-PR #49; `Vulnerabilities` 0 → ~5 expected after this PR lands)
**Source:** live query of `https://api.osv.dev/v1/querybatch` against `Cargo.lock` (25 unique advisory IDs across 11 crates)

## Bucket A — drop-in bump (cleared in this PR)

| Advisory | Crate | From → To | Fix |
|---|---|---|---|
| `RUSTSEC-2026-0190` | anyhow | 1.0.102 → 1.0.104 | Unsoundness in `Error::downcast_mut()` |
| `GHSA-phqj-4mhp-q6mq` | openssl | 0.10.78 → 0.10.81 | Out-of-bounds write in `CipherCtxRef::cipher_update_inplace` |
| `GHSA-xp3w-r5p5-63rr` | openssl | (same bump) | UB in `X509Ref::ocsp_responders` for non-UTF-8 URLs |
| `GHSA-xv59-967r-8726` | openssl | (same bump) | Heap buffer overflow in AES-KW-PAD |
| `RUSTSEC-2026-0204` | crossbeam-epoch | 0.9.18 → 0.9.21 | Invalid pointer dereference in `fmt::Pointer` impl |

`cargo update -p anyhow -p openssl -p crossbeam-epoch` bumps the
lockfile. No Cargo.toml changes needed; `anyhow = "1.0"` in the
manifest is a semver-major wildcard that accepts 1.0.104.

## Bucket B — requires rustls upgrade (separate PR)

| Advisory | Crate | Fix | Blocker |
|---|---|---|---|
| `GHSA-4p46-pwfr-66x6` / `RUSTSEC-2025-0009` | ring@0.17.9 | 0.17.12+ | rustls@0.21.12 pins ring to 0.17.x without accepting newer minor versions. rustls 0.22+ / 0.23+ lifts the cap. |
| `GHSA-82j2-j2ch-gfr8` / `RUSTSEC-2026-0104` | rustls-webpki@0.101.7 | 0.103.13+ | Same rustls constraint chain. |

**Action:** follow-up PR bumps `rustls` to 0.23+ (uses
`aws-lc-rs` instead of `ring` for the default crypto provider).
Will close all six rustls-webpki + ring advisories at once.

## Bucket C — accepts a major bump (separate PR per crate)

| Advisory | Crate | Migration path |
|---|---|---|
| `GHSA-36xm-35qq-795w` / `RUSTSEC-2023-0058` | inventory@0.1.11 → 0.2.0 | Breaking — `collect!` macro signature changed. Used by `tree-sitter` and `wasmtime`. Will require downstream adapter work. |
| `GHSA-ghc8-5cgm-5rpf` / `RUSTSEC-2023-0057` | (same) | Same |

The two inventory advisories are *fixed in 0.2.0*. Bumping is a
major version that breaks `tree-sitter` and `wasmtime`. Hold this
until a `cargo update -p tree-sitter` lands that pulls a compatible
inventory.

## Bucket D — unmaintained, no upstream fix

| Advisory | Crate | Status |
|---|---|---|
| `RUSTSEC-2025-0141` | bincode@1.3.3 | bincode 1.x is unmaintained. Migrate to bincode 2.x, postcard, or rmp-serde. |
| `RUSTSEC-2024-0436` | paste@1.0.15 | paste is unmaintained. Switch to `pastey` (drop-in API). |
| `RUSTSEC-2025-0134` | rustls-pemfile@1.0.4 | 1.x branch unmaintained. Use rustls 0.23+ (uses 2.x). |

These three will be cleared by the rustls 0.23+ upgrade in
Bucket B (it transitively drops paste and rustls-pemfile). bincode
needs a separate migration PR — search the codebase for `bincode::`
to scope the work.

## Bucket E — git2 (separate PR)

| Advisory | Fix |
|---|---|
| `GHSA-j39j-6gw9-jw6h` / `RUSTSEC-2026-0008` | git2 0.20.4 |
| `RUSTSEC-2026-0183` | git2 0.21.0 |
| `RUSTSEC-2026-0184` | git2 0.21.0 |

All three are UB-class bugs. Fixed in 0.20.4 / 0.21.0. The Cargo.toml
pin is `git2 = "0.19"`. Bumping is a minor version — API may shift
slightly in `Repository::list` and `BlameHunk::signature` paths.
Plan a focused PR with a smoke-test against the federation fixture.

## Bucket F — h2 (separate PR)

| Advisory | Fix |
|---|---|
| `RUSTSEC-2026-0258` | h2 0.4.16 |

Minor version bump. Transitive through hyper → reqwest. API stable
for our usage; should be a clean lockfile bump.

---

## How to refresh this doc

When bumping deps, re-query OSV and update this file:

```bash
python3 <<'PY'
import json, urllib.request, pathlib, tomllib
parsed = tomllib.loads(pathlib.Path('Cargo.lock').read_text())
pkgs = [{'name': p['name'], 'version': p['version']} for p in parsed['package']]
body = json.dumps({'queries': [
    {'package': {'name': p['name'], 'ecosystem': 'crates.io'}, 'version': p['version']}
    for p in pkgs]}).encode()
req = urllib.request.Request('https://api.osv.dev/v1/querybatch', data=body,
    headers={'Content-Type': 'application/json'})
with urllib.request.urlopen(req, timeout=60) as r:
    resp = json.loads(r.read())
for q, res in zip(pkgs, resp['results']):
    if res.get('vulns'):
        print(f"{q['name']}@{q['version']}: {[v['id'] for v in res['vulns']]}")
PY
```
