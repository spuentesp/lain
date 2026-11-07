"""Fail-closed readiness and process cleanup checks for the NVIDIA benchmark."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "bench", Path(__file__).resolve().parents[2] / "scripts/eval/nvidia-startup-bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class StartupBenchmarkTests(unittest.TestCase):
    def test_requires_exact_ready_roster(self):
        self.assertTrue(bench.readiness({"repos": [{"id": "a", "health": "ready"}]}, ["a"]))
        for repos in ([], [{"id": "a", "health": "degraded"}],
                      [{"id": "a", "health": "indexing"}], [{"id": "b", "health": "ready"}],
                      [{"id": "a", "health": "ready"}] * 2):
            self.assertFalse(bench.readiness({"repos": repos}, ["a"]))

    def run_measure(self, response, timeout=2, digest=False, timings=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "server"
            binary.write_text("#!/bin/sh\nexec sleep 60\n")
            binary.chmod(0o755)
            spawned = []
            real_popen = subprocess.Popen
            def spawn(*args, **kwargs):
                proc = real_popen(*args, **kwargs)
                spawned.append(proc)
                return proc
            with patch.object(bench, "request", side_effect=response), patch.object(bench.subprocess, "Popen", side_effect=spawn):
                result = bench.measure(binary, root / "config", ["a"], root, timeout, False, "cold", 1, digest=digest, timings=timings)
            self.assertIsNotNone(spawned[0].poll(), "benchmark must reap its server")
            return result

    def test_complete_run_and_cleanup(self):
        def response(url, *args, **kwargs):
            repos = [{"id": "a", "health": "ready"}]
            if url.endswith("index-status"):
                return 200, {"repos": repos}
            if url.endswith("/mcp"):
                return 200, {"result": {"tools": [{"name": "get_health"}]}}
            return 200, {"status": "ok", "federation": {"repos": repos, "total_nodes": 2, "total_edges": 1}}
        result = self.run_measure(response)
        self.assertEqual(result["status"], "ready")
        self.assertIsNotNone(result["ready_s"])

    def test_empty_graph_is_failure(self):
        def response(url, *args, **kwargs):
            repos = [{"id": "a", "health": "ready"}]
            if url.endswith("index-status"):
                return 200, {"repos": repos}
            if url.endswith("/mcp"):
                return 200, {"result": {"tools": [{"name": "get_health"}]}}
            return 200, {"status": "ok", "federation": {"repos": repos, "total_nodes": 0, "total_edges": 0}}
        self.assertEqual(self.run_measure(response)["status"], "invalid_health")

    def test_timings_preserve_repeated_spans_and_ignore_invalid_records(self):
        lines = [
            "INFO federation startup timing: metadata_service_refs 12.500ms",
            "INFO federation startup timing: metadata_service_refs 7.250ms",
            "INFO federation startup timing: metadata_restore 24.125ms",
            "INFO federation startup timing: invalid -1.0ms",
            "INFO federation startup timing: invalid NaNms",
            "INFO unrelated log 999ms",
        ]
        self.assertEqual(bench.startup_timings(lines), {
            "metadata_service_refs": [12.5, 7.25], "metadata_restore": [24.125],
        })

    def test_timings_mode_rejects_a_binary_without_phase_output(self):
        def response(url, *args, **kwargs):
            repos = [{"id": "a", "health": "ready"}]
            if url.endswith("index-status"):
                return 200, {"repos": repos}
            if url.endswith("/mcp"):
                return 200, {"result": {"tools": [{"name": "get_health"}]}}
            return 200, {"status": "ok", "federation": {"repos": repos, "total_nodes": 2, "total_edges": 1}}
        self.assertEqual(self.run_measure(response, timings=True)["status"], "missing_timings")

    def test_digest_mode_rejects_a_binary_without_fingerprint_output(self):
        def response(url, *args, **kwargs):
            repos = [{"id": "a", "health": "ready"}]
            if url.endswith("index-status"):
                return 200, {"repos": repos}
            if url.endswith("/mcp"):
                return 200, {"result": {"tools": [{"name": "get_health"}]}}
            return 200, {"status": "ok", "federation": {"repos": repos, "total_nodes": 2, "total_edges": 1}}
        self.assertEqual(self.run_measure(response, digest=True)["status"], "missing_digest")

    def test_doctor_unindexed_exit_does_not_abort_benchmark(self):
        # doctor exits 2 on an unindexed checkout but still emits valid identity JSON.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "lain"
            binary.write_text("""#!/bin/sh
echo '{"build_commit":"fixture","server_version":"test"}'
exit 2
""")
            binary.chmod(0o755)
            (root / "lain-git-sidecar").write_text("fixture")
            report = root / "report.json"
            argv = ["bench", str(root), "--binary", str(binary), "--group", "nat",
                    "--runs", "1", "--output", str(report)]
            result = {"status": "ready", "ready_s": 1, "http_s": 0.1, "peak_parent_rss_mib": 1}
            with patch("sys.argv", argv), patch.object(bench, "git", return_value="fixture"), \
                    patch.object(bench, "measure", side_effect=lambda *a, **kw: dict(result, phase=a[-2], run=a[-1])), patch("builtins.print"):
                self.assertEqual(bench.main(), 0)
            self.assertTrue(report.exists())

    def test_degraded_is_failure_and_cleans_up(self):
        result = self.run_measure(lambda *a, **kw: (503, {"repos": [{"id": "a", "health": "degraded"}]}))
        self.assertEqual(result["status"], "unavailable")
        self.assertIsNone(result["ready_s"])

    def test_unreachable_times_out_and_cleans_up(self):
        def unreachable(*args, **kwargs):
            raise OSError("not listening")
        result = self.run_measure(unreachable, timeout=0.15)
        self.assertEqual(result["status"], "timeout")
        self.assertIsNone(result["ready_s"])


if __name__ == "__main__":
    unittest.main()
