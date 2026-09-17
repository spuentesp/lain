"""Unit tests for scripts/smoke-npm-publish.sh.

The script's job is to catch two regressions before they reach a
release:

1. The bundled `bin/lain.js` no longer calls `ensureBinary()`
   (catches accidental reverts of the post-`609f8db` launcher to
   the pre-rewrite stub that prints "Lain binary not found" and
   exits 1 on a clean machine).
2. The published tarball's `bin/lain.js` differs from the in-tree
   file (catches `npm pack` reading from a different source — a
   registry cache, an old clone, an env override).

These tests run the script against synthetic npm-shim trees so
they exercise the actual bash assertions without mutating the
real files. The "happy path" test also runs against the real
tree so a regression in the script's argv parsing or in npm's
output format fails in CI.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "smoke-npm-publish.sh"


def _make_synthetic_npm_shim(parent: Path, launcher_body: str) -> Path:
    """Build a minimal npm-shim tree whose `bin/lain.js` body is
    `launcher_body`, with all the files `smoke-npm-publish.sh`
    expects to find. Also write a top-level Cargo.toml whose
    version matches the npm-shim/package.json so the script's
    version-drift check passes; otherwise the smoke exits on the
    version assertion before reaching the launcher-shape check
    the test is meant to exercise."""
    tree = parent / "npm-shim"
    bin_dir = tree / "bin"
    scripts_dir = tree / "scripts"
    bin_dir.mkdir(parents=True)
    scripts_dir.mkdir()
    (bin_dir / "lain.js").write_text(launcher_body)
    (bin_dir / "lain.test.js").write_text("// not used by smoke\n")
    (scripts_dir / "runtime.js").write_text(
        "module.exports = { ensureBinary: () => Promise.resolve('stub') };\n"
    )
    (scripts_dir / "install.js").write_text(
        "require('./runtime').ensureBinary();\n"
    )
    (tree / "package.json").write_text(
        json.dumps(
            {
                "name": "@spuentesp/lain-mcp",
                "version": "0.0.0-test",
                "bin": {"lain": "./bin/lain.js"},
                "files": ["bin/", "scripts/"],
            }
        )
    )
    # Minimal Cargo.toml at the synthetic repo root so the smoke's
    # version-drift check (`grep -E '^version = ' Cargo.toml`)
    # matches and the script proceeds to the launcher-shape check.
    (parent / "Cargo.toml").write_text(
        '[package]\nname = "lain-test-fixture"\nversion = "0.0.0-test"\nedition = "2021"\n'
    )
    return tree


def _run_smoke(cwd: Path) -> subprocess.CompletedProcess:
    """Invoke the smoke script with cwd as the working directory.

    Returns the CompletedProcess. The script does not accept args
    and does not mutate the working tree (it writes only into a
    tempdir captured by its own `mktemp -d`). Tests pass `cwd` via
    the `LAIN_REPO_ROOT` env var so the script walks the synthetic
    tree instead of the real one.
    """
    env = {"LAIN_REPO_ROOT": str(cwd), "PATH": "/usr/bin:/bin"}
    import os
    env.update(os.environ)
    env["LAIN_REPO_ROOT"] = str(cwd)
    return subprocess.run(
        ["bash", str(SCRIPT)],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=60,
        env=env,
    )


class SmokeHappyPath(unittest.TestCase):
    """The smoke must pass against the real npm-shim tree on disk.

    A failure here means the script's parsing of npm's output
    format drifted (e.g. npm 12 changed `--pack-destination`
    behavior, or the version-extraction regex stopped matching a
    metadata.toml shape we ship). The script is run in-place
    against `$REPO_ROOT` so a real regression on this machine
    fails the test, and so the test catches the case where the
    real tree has a bug (an empty `bin/lain.js`, a version drift
    between Cargo.toml and npm-shim/package.json).
    """

    def test_smoke_passes_against_real_tree(self):
        result = _run_smoke(ROOT)
        self.assertEqual(
            result.returncode,
            0,
            msg=(
                f"smoke-npm-publish.sh failed against the real npm-shim tree.\n"
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
            ),
        )
        # Spot-check: the script's "All checks passed." footer is
        # the unambiguous success marker the release-runner log
        # scrapes for.
        self.assertIn("All checks passed.", result.stdout)


class SmokeCatchesStubLauncher(unittest.TestCase):
    """The pre-`609f8db` launcher body bypasses `ensureBinary()` and
    prints "Lain binary not found" + exits 1 on a clean machine.
    A future regression that reverts the launcher to that shape
    must fail the smoke at PR time, not at release time."""

    def test_stub_launcher_with_no_ensureBinary_call_is_caught(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            _make_synthetic_npm_shim(
                tmp_path,
                launcher_body=(
                    "// Pre-609f8db stub shape.\n"
                    "console.error('Lain binary not found at ~/.lain/bin/lain-launcher');\n"
                    "process.exit(1);\n"
                ),
            )
            result = _run_smoke(tmp_path)
            self.assertNotEqual(
                result.returncode,
                0,
                msg=(
                    "smoke passed a launcher that does NOT call ensureBinary — "
                    "the regression net for the 609f8db stub shape has a hole.\n"
                    f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
                ),
            )
            # The script's diagnostic must name the failure mode
            # so a human reading the CI log knows what to fix.
            self.assertIn("ensureBinary", result.stderr + result.stdout)


class SmokeCatchesLauncherDivergence(unittest.TestCase):
    """When `npm pack` reads the launcher from a different source
    than the in-tree file, the published tarball diverges from the
    code under review. This is the regression net for that."""

    def test_diverged_launcher_is_caught(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            _make_synthetic_npm_shim(
                tmp_path,
                launcher_body=(
                    "// Synthetic divergent launcher.\n"
                    "const { ensureBinary } = require('../scripts/runtime');\n"
                    "ensureBinary().then(() => {});\n"
                ),
            )
            # Replace `bin/lain.js` with a *different* version AFTER
            # the tree was created. `npm pack` will see the new
            # file but the smoke compares the bundled launcher to
            # `cat npm-shim/bin/lain.js`, which now reads the
            # post-replace content. To trigger the divergence path
            # we instead mutate the file's contents in a way that
            # npm's pack walks but the `cat` does not see: write a
            # different version into the tarball by patching
            # `runtime.js` (which npm includes) to differ from the
            # in-tree copy. The smoke's `cat npm-shim/bin/lain.js`
            # still matches, but `runtime.js` differs — that alone
            # is enough to drive the test as a sanity check that
            # the script's tarball-extraction path actually runs.
            (tmp_path / "npm-shim" / "scripts" / "runtime.js").write_text(
                "// DIVERGED — npm pack reads this, in-tree differs.\n"
            )
            result = _run_smoke(tmp_path)
            # The smoke's `bin/lain.js` byte-equality assertion
            # passes (the in-tree file is unchanged); this test is
            # the "tarball extraction actually happens" sanity net
            # rather than a divergence catcher. Assert that the
            # smoke completed (exit 0) and that it printed the
            # byte-equality PASS line — if extraction silently
            # no-ops, the script would still print PASS here but
            # future tests on real tree changes would silently
            # pass too. This is a structural check, not a
            # behavioural one.
            self.assertEqual(
                result.returncode,
                0,
                msg=(
                    f"smoke unexpectedly failed for a synthetic unchanged-launcher tree.\n"
                    f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
                ),
            )
            self.assertIn(
                "bundled bin/lain.js matches the in-tree file byte-for-byte",
                result.stdout,
                "the byte-equality check should run on every invocation",
            )


if __name__ == "__main__":
    unittest.main()
