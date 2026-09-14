#!/usr/bin/env python3
"""Refresh server.json from docs/METADATA.toml + the current release version.

The canonical description / homepage / keywords live in
docs/METADATA.toml. server.json is regenerated from there plus the
release version being prepared.

The script is idempotent: re-running it with inputs that already match
the file produces no diff. The release workflow calls it once per
release to ensure server.json is consistent before attaching it to the
GitHub Release.

What it updates:

  - data["version"]              <- --version arg
  - data["packages"][*].version   <- --version arg  (all packages)
  - data["description"]           <- METADATA.project.description
  - data["homepage"]              <- METADATA.project.homepage
  - data["keywords"]              <- METADATA.project.keywords (order-sensitive)
  - data["repository"]["url"]     <- METADATA.repository.url
  - data["repository"]["source"]  <- METADATA.repository.source

What it does NOT touch:

  - data["name"], data["$schema"], data["installation"], data["packages"]
    structural keys. Those are release-stable and hand-authored.

Why this exists: the previous release.yml sed-edited only the first
"version" field it found, leaving packages[].version drifted from the
top-level version. tests/use_cases and the MCP registry both reject
that state. The release gate (scripts/test_release_version.py) catches
nested drift; this script fixes it.
"""
from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path


def sync(root: Path, version: str) -> int:
    metadata = tomllib.loads((root / "docs" / "METADATA.toml").read_text())
    project = metadata["project"]
    repository = metadata["repository"]

    canonical = {
        "version": version,
        "description": project["description"],
        "homepage": project["homepage"],
        "keywords": list(project["keywords"]),
        "repository": {
            "url": repository["url"],
            "source": repository["source"],
        },
    }

    server_path = root / "server.json"
    data = json.loads(server_path.read_text())

    changes: list[str] = []

    if data.get("version") != canonical["version"]:
        changes.append(f"version: {data.get('version')!r} -> {canonical['version']!r}")
        data["version"] = canonical["version"]

    for i, pkg in enumerate(data.get("packages", [])):
        if pkg.get("version") != canonical["version"]:
            changes.append(f"packages[{i}].version: {pkg.get('version')!r} -> {canonical['version']!r}")
            pkg["version"] = canonical["version"]

    if data.get("description") != canonical["description"]:
        changes.append(f"description: {data.get('description')!r} -> {canonical['description']!r}")
        data["description"] = canonical["description"]

    if data.get("homepage") != canonical["homepage"]:
        changes.append(f"homepage: {data.get('homepage')!r} -> {canonical['homepage']!r}")
        data["homepage"] = canonical["homepage"]

    if data.get("keywords") != canonical["keywords"]:
        changes.append(f"keywords: {data.get('keywords')!r} -> {canonical['keywords']!r}")
        data["keywords"] = canonical["keywords"]

    repo = data.get("repository")
    if isinstance(repo, dict):
        if repo.get("url") != canonical["repository"]["url"]:
            changes.append(f"repository.url: {repo.get('url')!r} -> {canonical['repository']['url']!r}")
            repo["url"] = canonical["repository"]["url"]
        if repo.get("source") != canonical["repository"]["source"]:
            changes.append(f"repository.source: {repo.get('source')!r} -> {canonical['repository']['source']!r}")
            repo["source"] = canonical["repository"]["source"]

    for line in changes:
        print(line, file=sys.stderr)

    server_path.write_text(json.dumps(data, indent=2) + "\n")

    if changes:
        print(f"server.json synced to version {version!r} ({len(changes)} field(s) updated)", file=sys.stderr)
    else:
        print(f"server.json already in sync (version {version!r}, no diff)", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True, help="Release version, e.g. 0.7.4")
    parser.add_argument("--root", type=Path, default=None,
                        help="Project root (default: parent of this script)")
    args = parser.parse_args()
    root = args.root or Path(__file__).resolve().parents[1]
    return sync(root, args.version)


if __name__ == "__main__":
    sys.exit(main())