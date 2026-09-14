# Supply-chain security for LAIN

This document explains the choices behind the supply-chain artifacts
that ship with each `lain` release. It complements
[`docs/VERIFICATION.md`](VERIFICATION.md), which has the copy-pasteable
consumer commands. The artifact decisions are part of the LAIN Trust &
Visibility umbrella; PR #2 is the first PR to land them.

## Threat model

The consumer's question is: **"Is this `lain` binary what the
maintainer actually built, with the dependencies they say it has?"**

The supply-chain artifacts answer three sub-questions:

| Question | Answer |
|----------|--------|
| Has the binary been tampered with on its way to me? | SHA256SUMS |
| Did GitHub Actions actually build this binary from the public source? | Build-provenance attestation (SLSA Level 2) |
| What dependencies are linked into this binary? | CycloneDX SBOM |

A consumer who runs all three checks has a chain-of-custody proof
that:

1. The bytes they downloaded are exactly what the release workflow
   produced (`sha256sum -c`).
2. The release workflow that produced them is the workflow defined
   in *this* repository at *this* commit (`gh attestation verify`).
3. The dependency surface inside the binary matches what the SBOM
   claims (`cyclonedx-cli validate` or equivalent).

## What we publish per release

### 1. SHA256SUMS

Each `lain-<ver>-<target>.tar.gz` is hashed at build time. Two
artifacts attach to the release:

- A sidecar `<tarball>.sha256` per binary, computed at build time and
  uploaded with the binary itself.
- An aggregate `SHA256SUMS` file produced by the `sha256sums` job,
  which downloads the tarballs from the release and hashes them.

Both formats match GNU `sha256sum -c` expectations, so consumers can
verify either way. The aggregate file is computed against the exact
bytes consumers will receive — there is no separate attacker-controlled
surface.

### 2. Build provenance attestation

`actions/attest-build-provenance@v2` produces a SLSA-style provenance
attestation for each tarball. The attestation is signed by GitHub's
OIDC token at build time and discoverable via `gh attestation verify`.

This binds the binary to:

- The exact commit hash built.
- The exact workflow file used.
- The exact runner image.

A consumer can verify the tarball they're holding was produced by
*this* repository's release workflow from *this* commit, not by a fork
with a tampered binary.

### 3. CycloneDX SBOM

`anchore/sbom-action@v0.24.2` (with syft) scans each tarball and
produces a CycloneDX JSON file. CycloneDX is the de-facto SBOM
standard with broad tooling support (Grype, Dependency-Track, etc.).

Why syft over cargo-cyclonedx:

- **Syft scans the binary**, which captures *actual* linked
  dependencies — including C libraries and platform-specific
  bindings shipped by the Rust toolchain.
- **cargo-cyclonedx** captures the *source* dependency graph, which
  may differ from what's actually linked into the released binary
  (e.g. dev-dependencies stripped at build time, conditional
  features, vendored C code).
- For a server-side MCP binary, the runtime composition matters more
  than the source tree.

### 4. Reproducible-build environment variables

Each `cargo build` runs with:

```yaml
SOURCE_DATE_EPOCH: ${{ github.event.repository.updated_at }}
CARGO_INCREMENTAL: '0'
```

`SOURCE_DATE_EPOCH` normalizes timestamps embedded by the build
(helpful for byte-identical reproduction across runs).
`CARGO_INCREMENTAL=0` disables incremental compilation artifacts that
would otherwise embed absolute paths.

These are **not** full reproducible builds — they don't pin the
toolchain (`rust-toolchain.toml`) or freeze `RUSTFLAGS`. The next
iteration of this workflow will add those. For now, the provenance
attestation guarantees binary↔commit binding even when the bytes
aren't identical across runs.

## What does NOT ship in the release (and why)

- **Cosign signatures.** `actions/attest-build-provenance` covers
  the same ground with OIDC, doesn't require the maintainer to manage
  a separate signing key, and is verifiable via the GitHub-native
  `gh attestation verify`. Adding cosign on top would double the
  signature surface without doubling the security value.
- **SLSA Level 3 hardened builders.** Level 2 (what we ship) requires
  provenance from a hosted build platform — which GitHub Actions is.
  Level 3 also requires an isolated, ephemeral build environment;
  that's GitHub-hosted runners with no extra config, but the
  hermetic-build requirements add complexity not justified for a
  single-maintainer project.
- **A separate signing key.** OIDC + GitHub's attestation API means
  the workflow itself is the trust root. If the workflow is
  compromised, the attestation is broken too — but if the workflow
  is compromised, the binary is also broken, so no separate key
  would help.

## How to add a new supply-chain artifact

1. Generate the artifact in the relevant build job (per-binary) or
   aggregate job (cross-binary).
2. Attach it via `softprops/action-gh-release` so it ships in the
   same release.
3. Document the verification command in `docs/VERIFICATION.md`.
4. Add a test in `scripts/test_*.py` if the generation script is new.

## Where these decisions came from

The choice matrix (Syft vs CycloneDX-via-cargo, sha256sum vs cosign,
attest-build-provenance vs sigstore) was originally surveyed in PR #2
of the LAIN Trust & Visibility umbrella. PR #1 (security baseline)
and PR #2 (this work) are the first two PRs in that sequence; the
remaining PRs add npm provenance, MCP registry submission, and the
canonical metadata source.