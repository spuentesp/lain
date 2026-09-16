"""Unit tests for assert_cached_binary.py's record/verify-unchanged pair."""
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "assert_cached_binary", ROOT / "scripts/assert_cached_binary.py"
)
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)


class AssertCachedBinaryTests(unittest.TestCase):
    def test_record_then_verify_unchanged_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "cache", "0.7.0", "x86_64")
            os.makedirs(cache_dir)
            binary = os.path.join(cache_dir, "lain")
            Path(binary).write_bytes(b"fake binary")
            snapshot = os.path.join(tmp, "snapshot.json")

            tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "record"])
            # Must not raise: the binary is untouched.
            tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "verify-unchanged"])

    def test_verify_unchanged_fails_when_binary_was_rewritten(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "cache")
            os.makedirs(cache_dir)
            binary = os.path.join(cache_dir, "lain")
            Path(binary).write_bytes(b"first download")
            snapshot = os.path.join(tmp, "snapshot.json")
            tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "record"])

            # Simulate a re-download: new content, and force the mtime
            # forward so this is deterministic even on filesystems with
            # coarse mtime resolution.
            recorded_mtime = os.path.getmtime(binary)
            Path(binary).write_bytes(b"second download, different bytes")
            os.utime(binary, (recorded_mtime + 5, recorded_mtime + 5))

            with self.assertRaises(SystemExit):
                tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "verify-unchanged"])

    def test_missing_binary_fails_clearly(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "empty-cache")
            os.makedirs(cache_dir)
            snapshot = os.path.join(tmp, "snapshot.json")
            with self.assertRaises(SystemExit):
                tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "record"])

    def test_multiple_binaries_fails_clearly(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "cache")
            os.makedirs(os.path.join(cache_dir, "0.7.0", "a"))
            os.makedirs(os.path.join(cache_dir, "0.6.0", "b"))
            Path(os.path.join(cache_dir, "0.7.0", "a", "lain")).write_bytes(b"x")
            Path(os.path.join(cache_dir, "0.6.0", "b", "lain")).write_bytes(b"y")
            snapshot = os.path.join(tmp, "snapshot.json")
            with self.assertRaises(SystemExit):
                tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "record"])

    def test_windows_exe_name_is_recognized(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "cache")
            os.makedirs(cache_dir)
            Path(os.path.join(cache_dir, "lain.exe")).write_bytes(b"x")
            snapshot = os.path.join(tmp, "snapshot.json")
            tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "record"])
            tool.main(["--cache-dir", cache_dir, "--snapshot", snapshot, "verify-unchanged"])


if __name__ == "__main__":
    unittest.main()
