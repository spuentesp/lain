#!/usr/bin/env python3
"""Retrieve exact upstream excerpts; --check verifies without writing fixtures."""

import argparse
import hashlib
import json
from pathlib import Path
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parent
SOURCES = [
    (
        "openshell.proto", "OpenShell", "6648bd0c290efbc41ba131ee9831ee45cd431f94",
        "proto/openshell.proto", [(1, 6), (23, 23), (47, 54), (73, 80)], "}\n",
    ),
    (
        "nemo-cargo-lock.txt", "NeMo-Fabric", "4fdf65a4b4c96024327501ae28243f2d206e7ec7",
        "Cargo.lock", [(1600, 1603)], "",
    ),
    (
        "nemo.rs", "NeMo-Fabric", "4fdf65a4b4c96024327501ae28243f2d206e7ec7",
        "environment-providers/openshell/src/lib.rs",
        [(17, 21), (1967, 1969), (1971, 1972), (1987, 2052), (1, 2)],
        "}\n",
    ),
    (
        "openshell_client.rs", "OpenShell", "6648bd0c290efbc41ba131ee9831ee45cd431f94",
        "crates/openshell-sdk/src/client.rs",
        [(15, 15), (37, 46), (48, 48), (99, 104), (1, 2)],
        "}\n",
    ),
    (
        "openshell_raw.rs", "OpenShell", "6648bd0c290efbc41ba131ee9831ee45cd431f94",
        "crates/openshell-sdk/src/raw.rs",
        [(21, 22), (38, 44), (1, 2)],
        "",
    ),
]


def sha256(content):
    return hashlib.sha256(content).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    provenance = {"format": 1, "files": []}
    outputs = {}
    for filename, repo, revision, path, ranges, suffix in SOURCES:
        url = f"https://raw.githubusercontent.com/NVIDIA/{repo}/{revision}/{path}"
        source = urlopen(url, timeout=30).read()
        lines = source.decode("utf-8").splitlines(keepends=True)
        fixture = "// Exact upstream excerpts. See provenance.json; not a compilable crate.\n"
        mappings = []
        for start, end in ranges:
            fixture += f"\n// Upstream {path}:{start}-{end}\n"
            fixture_start = len(fixture.splitlines()) + 1
            fixture += "".join(lines[start - 1:end])
            mappings.append({"fixture_start": fixture_start, "source_start": start, "source_end": end})
        if suffix:
            kind = "service" if filename.endswith(".proto") else "impl"
            fixture += f"\n// Fixture scaffolding: close the excerpted {kind}.\n" + suffix
        output = fixture.encode("utf-8")
        outputs[filename] = output
        provenance["files"].append({
            "fixture": filename, "repository": f"https://github.com/NVIDIA/{repo}",
            "revision": revision, "source_path": path, "retrieval_url": url,
            "source_sha256": sha256(source), "fixture_sha256": sha256(output),
            "ranges": mappings,
        })
    outputs["provenance.json"] = (json.dumps(provenance, indent=2) + "\n").encode()
    for filename, output in outputs.items():
        destination = ROOT / filename
        if args.check:
            if destination.read_bytes() != output:
                raise SystemExit(f"Fixture differs: {filename}")
        else:
            destination.write_bytes(output)
    print("Exact-revision fixtures verified" if args.check else "Exact-revision fixtures written")


if __name__ == "__main__":
    main()
