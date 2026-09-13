"""Exercise production action scripts with offline download and MCP fixtures."""
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ACTION = Path(__file__).resolve().parents[1]


class ActionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        GITHUB_PATH=str(self.root / "github-path"),
                        GITHUB_OUTPUT=str(self.root / "outputs"),
                        GITHUB_WORKSPACE=str(self.root), GITHUB_EVENT_NAME="push",
                        LAIN_INSTALL_DIR=str(self.root / "installed"),
                        REVIEW_CALLS=str(self.root / "calls"))

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/usr/bin/env python3\n" + body)
        path.chmod(0o755)

    def run_script(self, name, success=True):
        result = subprocess.run(["bash", str(ACTION / name)], env=self.env,
                                text=True, capture_output=True, timeout=20)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def download_fixture(self, binary_version, platform="Linux", arch="x86_64", binary="lain"):
        archive = self.root / "fixture.tar.gz"
        content = f'#!/bin/sh\necho "lain {binary_version}"\n'.encode()
        with tarfile.open(archive, "w:gz") as tar:
            member = tarfile.TarInfo(binary)
            member.size = len(content)
            member.mode = 0o755
            tar.addfile(member, io.BytesIO(content))
        self.env["REVIEW_ARCHIVE"] = str(archive)
        self.stub("uname", f'import sys\nprint({platform!r} if sys.argv[1] == "-s" else {arch!r})\n')
        self.stub("curl", '''import os, sys, shutil
from pathlib import Path
args = sys.argv[1:]
url = next(a for a in args if a.startswith("https:"))
Path(os.environ["REVIEW_CALLS"]).write_text(url)
assert "/releases/latest" not in url
shutil.copyfile(os.environ["REVIEW_ARCHIVE"], args[args.index("-o")+1])
''')

    def test_requested_version_selects_archive_and_binary(self):
        self.download_fixture("0.7.2")
        self.env["LAIN_VERSION"] = "v0.7.2"
        self.run_script("install-binary.sh")
        self.assertIn("/v0.7.2/lain-0.7.2-", (self.root / "calls").read_text())
        self.assertTrue((self.root / "installed/lain").exists())
        self.assertEqual((self.root / "github-path").read_text().strip(), self.env["LAIN_INSTALL_DIR"])

    def test_other_platform_archives(self):
        for platform, arch, target, binary in [
            ("Darwin", "arm64", "aarch64-apple-darwin", "lain"),
            ("MINGW64_NT", "x86_64", "x86_64-pc-windows-msvc", "lain.exe"),
        ]:
            with self.subTest(platform=platform):
                self.download_fixture("0.7.3", platform, arch, binary)
                self.env["LAIN_VERSION"] = "v0.7.3"
                self.run_script("install-binary.sh")
                self.assertIn(target, (self.root / "calls").read_text())
                self.assertTrue((self.root / "installed" / binary).exists())

    def test_composite_forwards_health_inputs(self):
        action = (ACTION / "action.yml").read_text()
        step = action.split("- name: Compute architecture health", 1)[1].split("- name:", 1)[0]
        self.assertIn("INPUT_MIN_FAN_OUT: ${{ inputs.min-fan-out }}", step)
        self.assertIn("INPUT_LSP_LANGUAGES: ${{ inputs.lsp-languages }}", step)

    def test_wrong_binary_version_fails_before_install(self):
        self.download_fixture("0.7.3")
        self.env["LAIN_VERSION"] = "v0.7.2"
        self.run_script("install-binary.sh", success=False)
        self.assertFalse((self.root / "installed/lain").exists())
        self.assertFalse((self.root / "github-path").exists())

    def health_fixture(self):
        (self.root / "Cargo.toml").touch()
        self.stub("lain", 'import sys, time\nif sys.argv[1] == "init": print("repos: []")\nelse: time.sleep(15)\n')
        self.stub("nc", "pass\n")
        self.stub("curl", '''import json, os, sys
args = sys.argv[1:]
request = json.loads(args[args.index("-d")+1])
with open(os.environ["REVIEW_CALLS"], "a") as log: log.write(json.dumps(request)+"\\n")
response = json.dumps({"result":{"content":[{"text":"Operational"}]}})
if os.environ.get("REVIEW_MCP_ERROR"): response = json.dumps({"error":{"code":-32603,"message":"broken"}})
if "-o" in args:
    with open(args[args.index("-o")+1], "w") as output: output.write(response)
    print("200")
else: print(response)
''')

    def test_empty_languages_disable_install_and_threshold_reaches_mcp(self):
        self.health_fixture()
        self.env.update(INPUT_LSP_LANGUAGES="", INPUT_MIN_FAN_OUT="27")
        self.run_script("health.sh")
        calls = [json.loads(line)["params"] for line in (self.root / "calls").read_text().splitlines()]
        self.assertNotIn("install_language_server", [call["name"] for call in calls])
        arch = next(call for call in calls if call["name"] == "architectural_observations")
        self.assertEqual(arch["arguments"]["min_fan_out"], 27)

    def test_mcp_error_does_not_publish_success(self):
        self.health_fixture()
        self.env.update(INPUT_LSP_LANGUAGES="", REVIEW_MCP_ERROR="1")
        self.run_script("health.sh", success=False)
        self.assertIn("level=error", (self.root / "outputs").read_text())
        self.assertNotIn("level=success", (self.root / "outputs").read_text())

    def test_failed_language_install_does_not_publish_success(self):
        self.health_fixture()
        self.env.update(INPUT_LSP_LANGUAGES="python", REVIEW_MCP_ERROR="1")
        self.run_script("health.sh", success=False)
        self.assertIn("Language-server installation failed", (self.root / "outputs").read_text())
        self.assertNotIn("level=success", (self.root / "outputs").read_text())

    def test_comma_separated_languages_are_individual_requests(self):
        self.health_fixture()
        self.env["INPUT_LSP_LANGUAGES"] = "python,rust"
        self.run_script("health.sh")
        calls = [json.loads(line)["params"] for line in (self.root / "calls").read_text().splitlines()]
        langs = [c["arguments"]["language"] for c in calls if c["name"] == "install_language_server"]
        self.assertEqual(langs, ["python", "rust"])


if __name__ == "__main__":
    unittest.main()
