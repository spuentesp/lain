"""Unit tests for fill-formula-sha256.py.

v0.14.0 shipped a `Formula/lain.rb` whose sha256 values did not match
the published artifacts: the post-publish fill step (AGENTS.md step 6)
was skipped, so `brew install lain` failed checksum verification. These
tests pin the fill step's behaviour on copies of the real Formula, so
the step cannot be skipped silently again.
"""
from __future__ import annotations

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def _load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_fill = _load_module(ROOT / "scripts" / "fill-formula-sha256.py", "fill_formula_under_test")

LINUX = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-msvc"
LINUX_SHA = "1f3129e3f5a3a0075416291429da1f7c5015fa50e97dd5aee2a860dcc3c5923d"
WINDOWS_SHA = "692cda2e43b4c567115556bbfcf7cd90b1dd8ef578e25fb53e350c05558493f5"


class FillFormulaTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        formula_dir = self.root / "Formula"
        formula_dir.mkdir()
        self.formula = formula_dir / "lain.rb"
        shutil.copyfile(ROOT / "Formula" / "lain.rb", self.formula)
        self.original_formula = _fill.FORMULA
        _fill.FORMULA = self.formula
        self.addCleanup(setattr, _fill, "FORMULA", self.original_formula)
        self.checksums = {
            f"lain-0.15.0-{LINUX}.tar.gz": LINUX_SHA,
            f"lain-0.15.0-{WINDOWS}.tar.gz": WINDOWS_SHA,
        }

    def test_fill_writes_the_published_checksums(self):
        self.assertTrue(_fill.fill("v0.15.0", self.checksums))
        text = self.formula.read_text()
        self.assertIn(LINUX_SHA, text)
        self.assertIn(WINDOWS_SHA, text)

    def test_fill_is_idempotent(self):
        _fill.fill("v0.15.0", self.checksums)
        once = self.formula.read_text()
        _fill.fill("v0.15.0", self.checksums)
        self.assertEqual(once, self.formula.read_text())

    def test_fill_reports_a_missing_checksum_entry(self):
        self.assertFalse(_fill.fill("v0.15.0", {}))

    def test_check_passes_after_fill_and_fails_before(self):
        self.assertFalse(_fill.check("v0.15.0", self.checksums))
        _fill.fill("v0.15.0", self.checksums)
        self.assertTrue(_fill.check("v0.15.0", self.checksums))

    def test_formula_has_no_macos_stanza(self):
        # release.yml builds Linux and Windows only (the macOS ARM64
        # target was dropped in v0.14.0), so a macOS stanza would point
        # at an asset that is never published.
        self.assertNotIn("aarch64-apple-darwin", self.formula.read_text())

    def test_formula_version_matches_the_release(self):
        import re

        text = self.formula.read_text()
        version = re.search(r'^\s*version "([^"]+)"', text, re.M)
        self.assertIsNotNone(version)
        self.assertEqual(version.group(1), "0.15.0")


if __name__ == "__main__":
    unittest.main()
