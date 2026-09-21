# Vulnerabilities — triage and remediation log

Rolling inventory of OSV advisories against the dependency graph in
`Cargo.lock`.

**Snapshot date:** 2026-09-20

**Source:** `https://api.osv.dev/v1/querybatch` queried for every crates.io
package in `Cargo.lock`

**Current result:** three advisory ids across three crates

## Active advisories

### `ring 0.17.9`

- `RUSTSEC-2025-0009` / `GHSA-4p46-pwfr-66x6`
- Fixed in `ring 0.17.12` or newer.
- A direct lockfile update is currently blocked: newer `ring` requires
  `cc >=1.2.8`, while `tree-sitter-javascript 0.21.4` constrains `cc` to
  `~1.0.90`.
- **Action:** update the JavaScript tree-sitter dependency far enough to remove
  the old `cc` constraint, then update `ring` and run the full parser, TLS, and
  cross-platform test matrix. Alternatively, evaluate a rustls provider setup
  that does not pull `ring`.

### `paste 1.0.15`

- `RUSTSEC-2024-0436`: crate is unmaintained.
- It is transitive through `tokenizers 0.21.4`; Lain does not call it directly.
- **Action:** track a `tokenizers` release that removes `paste`, or patch the
  dependency to a maintained compatible implementation after verifying the
  tokenizer and ONNX paths on every supported platform.

### `bincode 2.0.1`

- `RUSTSEC-2025-0141`: crate is unmaintained. This is a maintenance advisory,
  not a reported memory-safety vulnerability.
- Lain uses the 2.x serde compatibility API with legacy encoding for persisted
  graph compatibility.
- **Action:** evaluate a maintained serialization format and write an explicit
  on-disk migration plan before replacing it. Until then, retain compatibility
  tests and treat persisted graph files as local, untrusted input.

## Recently cleared dependency work

- `git2` is now `0.21.0`; the previously tracked 0.19/0.20 advisories no
  longer appear in the OSV result.
- `inventory` is now `0.2.3`; the old 0.1 advisories are gone.
- `rustls-webpki` is now `0.103.15`; its previously tracked advisory is gone.
- `h2` is no longer present in the current dependency tree.
- `rustls` is now `0.23.45`; the remaining TLS advisory is the independently
  pinned `ring` version described above.

## Refresh procedure

Run this from the repository root and replace the snapshot above with the
result:

```bash
python3 <<'PY'
import json, pathlib, tomllib, urllib.request

lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text())
packages = [{"name": p["name"], "version": p["version"]}
            for p in lock["package"]]
body = json.dumps({"queries": [
    {"package": {"name": p["name"], "ecosystem": "crates.io"},
     "version": p["version"]}
    for p in packages
]}).encode()
request = urllib.request.Request(
    "https://api.osv.dev/v1/querybatch",
    data=body,
    headers={"Content-Type": "application/json"},
)
with urllib.request.urlopen(request, timeout=60) as response:
    results = json.loads(response.read())["results"]
for package, result in zip(packages, results):
    ids = [vulnerability["id"] for vulnerability in result.get("vulns", [])]
    if ids:
        print(f'{package["name"]}@{package["version"]}: {", ".join(ids)}')
PY
```

Also run `cargo tree -i <crate>` for every result before proposing a fix; OSV
identifies the affected package, not the dependency path or upgrade blocker.
