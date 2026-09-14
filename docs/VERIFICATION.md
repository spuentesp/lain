# Verifying a LAIN release

This document explains how to verify a `lain` release artifact on a
fresh machine, with no prior context. Each step links to the artifact
that proves the previous one. See [`docs/SUPPLY_CHAIN.md`](SUPPLY_CHAIN.md)
for *why* these checks exist.

## What ships in a release

For each tagged release (e.g. `v0.7.3`) the workflow publishes:

| File | What it is |
|------|-----------|
| `lain-<ver>-<target>.tar.gz` | The compressed `lain` binary |
| `lain-<ver>-<target>.tar.gz.sha256` | `<hash>  <filename>` for that tarball |
| `lain-<ver>-<target>.tar.gz.cdx.json` | CycloneDX SBOM of that tarball |
| `SHA256SUMS` | `<hash>  <filename>` for all three tarballs |
| `server.json` | MCP registry manifest |

Plus, for each tarball, a build-provenance attestation signed by
GitHub's OIDC token during the build and discoverable via
`gh attestation verify`.

The supported `<target>` triples today are:

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

## Verification flow

The example below uses Linux x86_64 and tag `v0.7.3`. Replace the
`VER` and `TARGET` values for other releases and platforms.

### 1. Download the artifact and its checksums

```bash
VER=0.7.3
TARGET=x86_64-unknown-linux-gnu

mkdir lain-verify && cd lain-verify

# The tarball itself
gh release download "v${VER}" \
  --repo spuentesp/lain \
  --pattern "lain-${VER}-${TARGET}.tar.gz"

# Per-binary checksum
gh release download "v${VER}" \
  --repo spuentesp/lain \
  --pattern "lain-${VER}-${TARGET}.tar.gz.sha256"

# Aggregate checksum
gh release download "v${VER}" \
  --repo spuentesp/lain \
  --pattern SHA256SUMS
```

### 2. Verify the SHA256

Either check the per-binary sidecar:

```bash
sha256sum -c "lain-${VER}-${TARGET}.tar.gz.sha256"
# expected output:
# lain-0.7.3-x86_64-unknown-linux-gnu.tar.gz: OK
```

Or verify the same hash appears in the aggregate file:

```bash
grep "lain-${VER}-${TARGET}.tar.gz" SHA256SUMS \
  | sha256sum -c
```

### 3. Verify build provenance

```bash
gh attestation verify \
  "lain-${VER}-${TARGET}.tar.gz" \
  --repo spuentesp/lain
# expected output ends with:
# ✓ Verification succeeded!
```

The attestation is signed by GitHub Actions' OIDC token during the
build, binding the artifact to the specific commit and workflow run
that produced it. If the tarball was modified in transit, or if a
fork's release workflow claimed to produce it, this command fails.

### 4. Inspect the SBOM (optional)

```bash
gh release download "v${VER}" \
  --repo spuentesp/lain \
  --pattern "lain-${VER}-${TARGET}.tar.gz.cdx.json"

# Human-readable component summary
python -c "
import json, sys
sbom = json.load(open('lain-${VER}-${TARGET}.tar.gz.cdx.json'))
for comp in sbom.get('components', []):
    print(f\"{comp.get('type', '?'):>10}  {comp.get('name', '?')}@{comp.get('version', '?')}\")
" | sort
```

Or upload to a CycloneDX-aware tool (Dependency-Track, Grype, etc.)
for deeper analysis.

### 5. Extract and run

```bash
tar xzf "lain-${VER}-${TARGET}.tar.gz"
./lain --version
# expected output: lain 0.7.3
```

## What to do if a check fails

| Failure | Likely cause | Action |
|---------|--------------|--------|
| `sha256sum -c` says FAILED | Downloaded file corrupted, or wrong artifact | Re-download with `gh release download` |
| `gh attestation verify` fails | Wrong artifact, or workflow changed | Cross-check the artifact name matches the release tag |
| SBOM parser errors | File truncated | Re-download; report if reproducible |
| `lain --version` mismatch | Wrong binary for your platform | Re-check the `<target>` component |

If the SHA256 and provenance checks both pass but `lain --version`
mismatches the tag, stop and report — that's an artifact-vs-build
disagreement that should not be possible if the workflow ran cleanly.

## Continuous verification

`docs/SUPPLY_CHAIN.md` explains why these checks exist and what they
protect against. `scripts/check-release-version.py` is the in-repo
gate that ensures the metadata side of every release is consistent
across `Cargo.toml`, `Cargo.lock`, `server.json`,
`npm-shim/package.json`, and `Formula/lain.rb`. The
`scripts/test_sync_metadata.py` smoke test (added in PR #2) catches
the description / homepage / keywords drift that the version check
ignores.