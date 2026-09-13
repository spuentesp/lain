#!/usr/bin/env python3
"""Reject drift between package metadata, an optional release tag, and a binary."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tomllib


def check(root, tag=None, binary=None):
    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    versions = {"Cargo.toml": version}
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    versions["Cargo.lock"] = next(p["version"] for p in lock["package"] if p["name"] == "lain")

    def collect(value, path):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == "version":
                    versions[f"{path}.{key}"] = child
                else:
                    collect(child, f"{path}.{key}")
        elif isinstance(value, list):
            for i, child in enumerate(value):
                collect(child, f"{path}[{i}]")

    for name in ["server.json", "npm-shim/package.json"]:
        collect(json.loads((root / name).read_text()), name)
    formula = (root / "Formula/lain.rb").read_text()
    versions["Formula/lain.rb"] = re.search(r'^\s*version "([^"]+)"', formula, re.M)[1]
    # The badge pins a published binary, which can lag release-preparation metadata.
    if tag is not None:
        if tag != f"v{version}":
            raise ValueError(f"tag {tag!r} does not match Cargo version v{version}")
    mismatches = [f"{name}: {value!r}" for name, value in versions.items() if value != version]
    if mismatches:
        raise ValueError(f"expected {version}; " + "; ".join(mismatches))
    if binary:
        result = subprocess.run([str(Path(binary).resolve()), "--version"], check=True,
                                capture_output=True, text=True)
        if result.stdout.strip() != f"lain {version}":
            raise ValueError(f"binary version mismatch: {result.stdout.strip()!r}")
    print(f"OK: release metadata matches {version}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag")
    parser.add_argument("--binary")
    args = parser.parse_args()
    check(Path(__file__).resolve().parents[1], args.tag, args.binary)
