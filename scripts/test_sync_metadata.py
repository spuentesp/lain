"""Unit tests for sync-server-json.py and the METADATA.toml invariant.

The "METADATA.toml is canonical" rule is enforced two ways:

    1. scripts/sync-server-json.py refreshes server.json from it on
       every release.
    2. scripts/check-release-version.py already rejects version drift
       across server.json and npm-shim/package.json; PR #2 extends the
       same idea to the description / homepage / keywords surface.

These tests run on copies of the real files (the same pattern
test_release_version.py uses) so they exercise the actual
implementation without mutating the working tree.
"""
from __future__ import annotations

import importlib.util
import json
import shutil
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def _load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_sync = _load_module(ROOT / "scripts" / "sync-server-json.py", "sync_server_json_under_test")
_sha = _load_module(ROOT / "scripts" / "compute-sha256.py", "compute_sha256_under_test")


def _seed(tmp: Path) -> tuple[Path, Path]:
    """Copy METADATA.toml and server.json into a temp tree, return their paths."""
    docs = tmp / "docs"
    docs.mkdir()
    metadata_src = ROOT / "docs" / "METADATA.toml"
    server_src = ROOT / "server.json"
    metadata_dst = docs / "METADATA.toml"
    server_dst = tmp / "server.json"
    shutil.copyfile(metadata_src, metadata_dst)
    shutil.copyfile(server_src, server_dst)
    return metadata_dst, server_dst


class SyncServerJsonTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        _seed(self.root)

    def test_updates_top_level_version(self) -> None:
        rc = _sync.sync(self.root, "9.9.9")
        self.assertEqual(rc, 0)
        data = json.loads((self.root / "server.json").read_text())
        self.assertEqual(data["version"], "9.9.9")

    def test_updates_nested_package_version(self) -> None:
        # This is the bug PR #2 fixes: the old sed only matched the
        # first occurrence, so packages[].version drifted from the top.
        rc = _sync.sync(self.root, "9.9.9")
        self.assertEqual(rc, 0)
        data = json.loads((self.root / "server.json").read_text())
        for pkg in data["packages"]:
            self.assertEqual(pkg["version"], "9.9.9")

    def test_refreshes_description_from_metadata(self) -> None:
        server_path = self.root / "server.json"
        data = json.loads(server_path.read_text())
        data["description"] = "stale description from a previous edit"
        server_path.write_text(json.dumps(data))

        _sync.sync(self.root, "0.7.3")

        refreshed = json.loads(server_path.read_text())
        metadata = tomllib.loads((self.root / "docs" / "METADATA.toml").read_text())
        self.assertEqual(refreshed["description"], metadata["project"]["description"])

    def test_refreshes_keywords_from_metadata(self) -> None:
        server_path = self.root / "server.json"
        data = json.loads(server_path.read_text())
        data["keywords"] = ["stale", "keywords"]
        server_path.write_text(json.dumps(data))

        _sync.sync(self.root, "0.7.3")

        refreshed = json.loads(server_path.read_text())
        metadata = tomllib.loads((self.root / "docs" / "METADATA.toml").read_text())
        self.assertEqual(refreshed["keywords"], metadata["project"]["keywords"])

    def test_refreshes_homepage_from_metadata(self) -> None:
        server_path = self.root / "server.json"
        data = json.loads(server_path.read_text())
        data["homepage"] = "https://example.com/wrong"
        server_path.write_text(json.dumps(data))

        _sync.sync(self.root, "0.7.3")

        refreshed = json.loads(server_path.read_text())
        metadata = tomllib.loads((self.root / "docs" / "METADATA.toml").read_text())
        self.assertEqual(refreshed["homepage"], metadata["project"]["homepage"])

    def test_refreshes_repository_from_metadata(self) -> None:
        server_path = self.root / "server.json"
        data = json.loads(server_path.read_text())
        data["repository"]["url"] = "https://example.com/wrong"
        server_path.write_text(json.dumps(data))

        _sync.sync(self.root, "0.7.3")

        refreshed = json.loads(server_path.read_text())
        metadata = tomllib.loads((self.root / "docs" / "METADATA.toml").read_text())
        self.assertEqual(refreshed["repository"]["url"], metadata["repository"]["url"])
        self.assertEqual(refreshed["repository"]["source"], metadata["repository"]["source"])

    def test_is_idempotent_on_aligned_inputs(self) -> None:
        # First sync: brings the file into alignment.
        _sync.sync(self.root, "0.7.3")
        first = (self.root / "server.json").read_text()

        # Second sync with same input: byte-identical.
        _sync.sync(self.root, "0.7.3")
        second = (self.root / "server.json").read_text()

        self.assertEqual(first, second)

    def test_preserves_unrelated_fields(self) -> None:
        server_path = self.root / "server.json"
        original = json.loads(server_path.read_text())

        _sync.sync(self.root, "9.9.9")

        refreshed = json.loads(server_path.read_text())
        # These fields are hand-authored; sync must not touch them.
        self.assertEqual(refreshed["name"], original["name"])
        self.assertEqual(refreshed["$schema"], original["$schema"])
        self.assertEqual(refreshed["installation"], original["installation"])
        self.assertEqual(refreshed["packages"][0]["identifier"], original["packages"][0]["identifier"])

    def test_current_metadata_passes(self) -> None:
        # The real METADATA.toml + the real server.json must be
        # already-aligned (otherwise the release gate would already be
        # red). If this fails, one of them drifted out of sync.
        _sync.sync(self.root, "0.7.3")
        server_data = json.loads((self.root / "server.json").read_text())
        metadata = tomllib.loads((self.root / "docs" / "METADATA.toml").read_text())

        self.assertEqual(server_data["description"], metadata["project"]["description"])
        self.assertEqual(server_data["homepage"], metadata["project"]["homepage"])
        self.assertEqual(server_data["keywords"], metadata["project"]["keywords"])


class ComputeSha256Tests(unittest.TestCase):
    def test_known_hash_for_known_input(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        f = Path(self.tmp.name) / "fixture.bin"
        f.write_bytes(b"hello world")
        sys.argv = ["compute-sha256.py", str(f)]
        rc = _sha.main()
        self.assertEqual(rc, 0)
        self.assertEqual(
            (f.with_name(f.name + ".sha256")).read_text(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  fixture.bin\n",
        )

    def test_batch_mode_writes_one_sidecar_per_file(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        a = Path(self.tmp.name) / "a.bin"
        b = Path(self.tmp.name) / "b.bin"
        a.write_bytes(b"alpha")
        b.write_bytes(b"beta")
        sys.argv = ["compute-sha256.py", str(a), str(b)]
        rc = _sha.main()
        self.assertEqual(rc, 0)
        self.assertTrue(a.with_suffix(a.suffix + ".sha256").exists())
        self.assertTrue(b.with_suffix(b.suffix + ".sha256").exists())

    def test_missing_file_returns_nonzero(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        missing = Path(self.tmp.name) / "nope.bin"
        present = Path(self.tmp.name) / "present.bin"
        present.write_bytes(b"x")
        sys.argv = ["compute-sha256.py", str(missing), str(present)]
        rc = _sha.main()
        self.assertEqual(rc, 1)
        self.assertFalse(missing.with_suffix(missing.suffix + ".sha256").exists())
        self.assertTrue(present.with_suffix(present.suffix + ".sha256").exists())


class MetadataDriftGateTests(unittest.TestCase):
    """The umbrella's "every badge is a receipt" rule needs the canonical
    metadata source to actually be canonical — i.e. server.json's
    description / homepage / keywords match METADATA.toml at HEAD.

    This is the smoke test that catches future drift the same way
    test_release_version.py catches version drift.
    """

    def test_server_json_matches_metadata(self) -> None:
        server_data = json.loads((ROOT / "server.json").read_text())
        metadata = tomllib.loads((ROOT / "docs" / "METADATA.toml").read_text())

        self.assertEqual(server_data["description"], metadata["project"]["description"])
        self.assertEqual(server_data["homepage"], metadata["project"]["homepage"])
        self.assertEqual(server_data["keywords"], metadata["project"]["keywords"])
        self.assertEqual(
            server_data["repository"]["url"],
            metadata["repository"]["url"],
        )


if __name__ == "__main__":
    unittest.main()