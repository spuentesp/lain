"""Release gate regressions using copies of the actual project metadata."""
import importlib.util
from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release_check", ROOT / "scripts/check-release-version.py")
release_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_check)


class ReleaseGateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in ["Cargo.toml", "Cargo.lock", "server.json", "npm-shim/package.json",
                     "Formula/lain.rb", ".github/actions/lain-health-badge/action.yml"]:
            dest = self.root / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)

    def test_current_metadata_passes(self):
        release_check.check(self.root)

    def test_badge_can_pin_previous_published_release(self):
        path = self.root / ".github/actions/lain-health-badge/action.yml"
        path.write_text(path.read_text().replace("default: 'v0.7.3'", "default: 'v0.7.2'"))
        release_check.check(self.root)

    def test_binary_version_is_verified(self):
        import os
        if os.name == "nt":
            self.skipTest("shell executable fixture runs in Linux contract CI")
        version = tomllib.loads((self.root / "Cargo.toml").read_text())["package"]["version"]
        binary = self.root / "lain"
        binary.write_text(f'#!/bin/sh\necho "lain {version}"\n')
        binary.chmod(0o755)
        release_check.check(self.root, binary=binary)
        binary.write_text('#!/bin/sh\necho "lain 0.0.0"\n')
        with self.assertRaisesRegex(ValueError, "binary version mismatch"):
            release_check.check(self.root, binary=binary)

    def test_wrong_tag_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "tag"):
            release_check.check(self.root, tag="v0.0.0")

    def test_original_cargo_only_drift_is_rejected(self):
        path = self.root / "Cargo.toml"
        version = tomllib.loads(path.read_text())["package"]["version"]
        path.write_text(path.read_text().replace(f'version = "{version}"', 'version = "0.0.0"', 1))
        with self.assertRaisesRegex(ValueError, "expected"):
            release_check.check(self.root)

    def test_nested_package_drift_is_rejected(self):
        import json
        path = self.root / "server.json"
        data = json.loads(path.read_text())
        data["packages"][0]["version"] = "0.0.0"
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "packages"):
            release_check.check(self.root)


if __name__ == "__main__":
    unittest.main()
