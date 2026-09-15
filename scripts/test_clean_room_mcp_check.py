"""Unit tests for clean_room_mcp_check.py against a fake MCP server.

Drives the real `run_check` protocol logic against a small, deterministic
stand-in process instead of a live `lain mcp` (network-dependent, and the
currently *published* binary predates the `get_capabilities` tool this
script requires -- see docs/AGENT_UX_ROADMAP.md's Milestone 9 status).
These tests pin the script's own logic: response matching, the
warming-up poll loop, and protocol-violation detection.
"""
import importlib.util
import sys
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "clean_room_mcp_check", ROOT / "scripts/clean_room_mcp_check.py"
)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def fake_server_command(body: str) -> list:
    """A `command` list that runs `body` as a tiny stdin-driven JSON-RPC
    server via `python3 -c`, so tests need no extra fixture file and no
    real `lain` binary."""
    program = textwrap.dedent(
        """
        import json, sys
        {body}
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            req = json.loads(line)
            resp = handle(req)
            if resp is not None:
                sys.stdout.write(json.dumps(resp) + "\\n")
                sys.stdout.flush()
        """
    ).format(body=textwrap.indent(body, ""))
    return [sys.executable, "-c", program]


def capability_envelope(state, retry_after_ms=None):
    indexing = {"state": state}
    if retry_after_ms is not None:
        indexing["retry_after_ms"] = retry_after_ms
    return {"indexing": indexing}


class CleanRoomCheckTests(unittest.TestCase):
    def test_happy_path_passes(self):
        import json as _json

        ready_caps = _json.dumps(capability_envelope("ready"))
        body = textwrap.dedent(
            f"""
            def handle(req):
                m = req['method']
                if m == 'initialize':
                    return {{'jsonrpc': '2.0', 'id': req['id'], 'result': {{'ok': True}}}}
                if m == 'tools/list':
                    return {{'jsonrpc': '2.0', 'id': req['id'], 'result': {{'tools': [{{'name': 'x'}}]}}}}
                if m == 'tools/call':
                    name = req['params']['name']
                    if name == 'get_capabilities':
                        text = {ready_caps!r}
                    else:
                        text = 'Anchors: found 1'
                    return {{'jsonrpc': '2.0', 'id': req['id'],
                             'result': {{'content': [{{'type': 'text', 'text': text}}]}}}}
                return None
            """
        )
        command = fake_server_command(body)
        rc = check.run_check(command, index_budget_seconds=5, request_timeout=5)
        self.assertEqual(rc, 0)

    def test_polls_through_warming_up_before_ready(self):
        body = textwrap.dedent(
            """
            import json
            calls = {'n': 0}
            def handle(req):
                m = req['method']
                if m == 'initialize':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'ok': True}}
                if m == 'tools/list':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'tools': [{'name': 'x'}]}}
                if m == 'tools/call':
                    name = req['params']['name']
                    if name == 'get_capabilities':
                        calls['n'] += 1
                        state = 'warming_up' if calls['n'] < 3 else 'ready'
                        text = json.dumps({'indexing': {'state': state, 'retry_after_ms': 10}})
                    else:
                        text = 'Anchors: found 1'
                    return {'jsonrpc': '2.0', 'id': req['id'],
                            'result': {'content': [{'type': 'text', 'text': text}]}}
                return None
            """
        )
        command = fake_server_command(body)
        rc = check.run_check(command, index_budget_seconds=5, request_timeout=5)
        self.assertEqual(rc, 0)

    def test_index_budget_exceeded_raises_timeout(self):
        body = textwrap.dedent(
            """
            import json
            def handle(req):
                m = req['method']
                if m == 'initialize':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'ok': True}}
                if m == 'tools/list':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'tools': [{'name': 'x'}]}}
                if m == 'tools/call':
                    text = json.dumps({'indexing': {'state': 'warming_up', 'retry_after_ms': 50}})
                    return {'jsonrpc': '2.0', 'id': req['id'],
                            'result': {'content': [{'type': 'text', 'text': text}]}}
                return None
            """
        )
        command = fake_server_command(body)
        with self.assertRaises(TimeoutError):
            check.run_check(command, index_budget_seconds=0.3, request_timeout=5)

    def test_unknown_tool_response_is_a_clear_protocol_error(self):
        # Mirrors a real pre-Milestone-4 binary: get_capabilities isn't a
        # registered tool, so its "content" is a plain-text error, not JSON.
        body = textwrap.dedent(
            """
            def handle(req):
                m = req['method']
                if m == 'initialize':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'ok': True}}
                if m == 'tools/list':
                    return {'jsonrpc': '2.0', 'id': req['id'], 'result': {'tools': [{'name': 'x'}]}}
                if m == 'tools/call':
                    return {'jsonrpc': '2.0', 'id': req['id'],
                            'result': {'content': [{'type': 'text', 'text': 'Unknown tool: get_capabilities'}],
                                       'isError': True}}
                return None
            """
        )
        command = fake_server_command(body)
        with self.assertRaisesRegex(check.ProtocolError, "did not return JSON"):
            check.run_check(command, index_budget_seconds=5, request_timeout=5)

    def test_server_exiting_before_responding_is_a_protocol_error(self):
        body = textwrap.dedent(
            """
            def handle(req):
                import sys
                sys.exit(0)
            """
        )
        command = fake_server_command(body)
        with self.assertRaisesRegex(check.ProtocolError, "EOF"):
            check.run_check(command, index_budget_seconds=5, request_timeout=5)

    def test_silent_server_raises_timeout_not_hang(self):
        body = "def handle(req):\n    return None\n"
        command = fake_server_command(body)
        with self.assertRaises(TimeoutError):
            check.run_check(command, index_budget_seconds=5, request_timeout=0.3)

    def test_find_anchors_still_warming_up_after_ready_is_a_protocol_error(self):
        # A defensive check for a theoretical race: capabilities said
        # ready, but the structural query itself still came back gated.
        import json as _json

        gated = _json.dumps({"state": "warming_up"})
        body = textwrap.dedent(
            f"""
            def handle(req):
                m = req['method']
                if m == 'initialize':
                    return {{'jsonrpc': '2.0', 'id': req['id'], 'result': {{'ok': True}}}}
                if m == 'tools/list':
                    return {{'jsonrpc': '2.0', 'id': req['id'], 'result': {{'tools': [{{'name': 'x'}}]}}}}
                if m == 'tools/call':
                    name = req['params']['name']
                    if name == 'get_capabilities':
                        text = '{{"indexing": {{"state": "ready"}}}}'
                    else:
                        text = {gated!r}
                    return {{'jsonrpc': '2.0', 'id': req['id'],
                             'result': {{'content': [{{'type': 'text', 'text': text}}]}}}}
                return None
            """
        )
        command = fake_server_command(body)
        with self.assertRaisesRegex(check.ProtocolError, "still reports warming_up"):
            check.run_check(command, index_budget_seconds=5, request_timeout=5)

    def test_main_reports_failure_cleanly_for_missing_binary(self):
        rc = check.main(["--workspace", "/tmp", "--request-timeout", "2", "--", "/no/such/lain-binary"])
        self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
