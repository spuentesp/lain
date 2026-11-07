"""The real-repo smoke must not count fixture traffic as source coverage."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[2] / "scripts/eval/nat-http-contract-smoke.sh"
SOURCE = SCRIPT.read_text().split("<<'PY'\n", 1)[1].split("\nPY", 1)[0]


class ContractSmokeScopeTests(unittest.TestCase):
    def report(self, callers, strict="0", summary_count=None):
        count = len(callers) if summary_count is None else summary_count
        contracts = [{"endpoint": {"service": "nat", "key": "http:POST /chat/stream"},
                      "providers": [{"path": "src/routes.py"}], "bound_consumers": count}]
        response = {"result": {"content": [{"text": json.dumps({"data": {"items": [
            {"consumers": [{"site": {"path": path}} for path in callers]}
        ]}})}]}}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "contracts.json"
            path.write_text(json.dumps(contracts))
            output = io.StringIO()
            with patch("sys.argv", ["scope", "12345", str(path), strict]), \
                    patch("urllib.request.urlopen", return_value=io.StringIO(json.dumps(response))), \
                    contextlib.redirect_stdout(output):
                try:
                    exec(compile(SOURCE, str(SCRIPT), "exec"), {})
                except SystemExit as error:
                    return error.code, output.getvalue()
            return 0, output.getvalue()

    def test_strict_coverage_rejects_test_only_binding(self):
        status, output = self.report(["packages/core/tests/test_server.py"], strict="1")
        self.assertEqual(status, "FAIL: no non-test source HTTP consumer binding was demonstrated")
        self.assertIn("Non-test source HTTP bindings: 0", output)
        self.assertIn("'test': 1", output)

    def test_source_binding_is_separated_from_tests_examples_ci_and_unknown(self):
        status, output = self.report([
            "components/Chat.tsx", "__tests__/routes.test.ts", "examples/client.py",
            "ci/upload.py", "",
        ], strict="1")
        self.assertEqual(status, 0)
        self.assertIn("Non-test source HTTP bindings: 1", output)
        for scope in ["test", "example", "ci", "unknown"]:
            self.assertIn(f"'{scope}': 1", output)
        self.assertIn("Runtime production coverage: not established", output)

    def test_bound_consumer_summary_must_match_returned_details(self):
        status, _ = self.report(["components/Chat.tsx"], summary_count=2)
        self.assertEqual(status, "FAIL: bound-consumer detail count differs from its summary")


if __name__ == "__main__":
    unittest.main()
