#!/usr/bin/env python3
"""Tests for the Windows release archive assembly contract."""

from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "package-windows-release.sh"


class WindowsReleasePackageTests(unittest.TestCase):
    def fixture(self, root: Path, *, directml: bool = True) -> Path:
        build = root / "build"
        build.mkdir()
        (build / "lain.exe").write_bytes(b"lain")
        (build / "lain-git-sidecar.exe").write_bytes(b"sidecar")
        (build / "onnxruntime.dll").write_bytes(b"onnx")
        if directml:
            (build / "DirectML.dll").write_bytes(b"directml")
        return build

    def run_script(self, build: Path, archive: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPT), str(build), str(archive)],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_archive_contains_binaries_and_every_runtime_dll(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            archive = root / "out" / "lain-windows.tar.gz"
            result = self.run_script(self.fixture(root), archive)
            self.assertEqual(result.returncode, 0, result.stderr)
            with tarfile.open(archive, "r:gz") as bundle:
                self.assertEqual(
                    set(bundle.getnames()),
                    {
                        "lain.exe",
                        "lain-git-sidecar.exe",
                        "DirectML.dll",
                        "onnxruntime.dll",
                    },
                )

    def test_missing_directml_fails_before_creating_archive(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            archive = root / "lain-windows.tar.gz"
            result = self.run_script(self.fixture(root, directml=False), archive)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("DirectML.dll is missing", result.stderr)
            self.assertFalse(archive.exists())

    def test_missing_sidecar_fails_before_creating_archive(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            build = self.fixture(root)
            (build / "lain-git-sidecar.exe").unlink()
            archive = root / "lain-windows.tar.gz"
            result = self.run_script(build, archive)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("lain-git-sidecar.exe", result.stderr)
            self.assertFalse(archive.exists())


if __name__ == "__main__":
    unittest.main()
