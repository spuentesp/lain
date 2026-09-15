#!/usr/bin/env python3
"""Milestone 9 (docs/AGENT_UX_ROADMAP.md): drive a real `lain mcp` process
through the exact protocol sequence the roadmap specifies for "distribution
acceptance" and fail loudly on any deviation.

Deliberately generic over how `lain` is invoked -- the same script backs
the public `npx @spuentesp/lain-mcp mcp` lane, the forced-install
automation lane, and the release-gate lane (an extracted release tarball
binary), so all three exercise identical protocol assertions and only
differ in how the process gets started.

Sequence (roadmap steps 7-12): initialize -> tools/list -> get_capabilities
-> poll (respecting the envelope's own retry_after_ms, never faster) until
the structural capability is ready or the index budget expires -> one
structural query that must return a normal, non-loading answer. Every
line read from stdout must parse as JSON-RPC; anything else is a protocol
violation, not a warning.
"""
import argparse
import json
import queue
import subprocess
import sys
import threading
import time


class ProtocolError(RuntimeError):
    """The server said something that violates the MCP/lain contract."""


def _send(proc, message):
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()


def _start_reader(proc):
    """Background thread that reads stdout lines, parses each as JSON,
    and pushes `(kind, payload)` tuples onto a queue.

    A thread + `Queue.get(timeout=...)` is the only reliable
    cross-platform way to put a deadline on reading a subprocess pipe:
    `readline()` itself takes no timeout, and `select()` on pipes isn't
    dependable on Windows -- and this script runs on all three of
    Milestone 9's target platforms. A silent server (or one that never
    starts at all) must raise `TimeoutError`, never hang the check.
    """
    q = queue.Queue()

    def pump():
        try:
            for line in proc.stdout:
                line = line.rstrip("\n")
                if not line:
                    continue
                try:
                    q.put(("msg", json.loads(line)))
                except json.JSONDecodeError:
                    q.put(("protocol_error", f"non-JSON line on stdout: {line!r}"))
                    return
            q.put(("eof", None))
        except Exception as e:  # defensive: never let the reader die silently
            q.put(("protocol_error", f"reader thread failed: {e}"))

    thread = threading.Thread(target=pump, daemon=True)
    thread.start()
    return q, thread


def _recv_response(q, want_id, timeout):
    """Read messages until one matches `want_id`. A message with no `id`
    is a server-initiated notification (e.g. capabilities_changed) and is
    expected traffic, not skipped-over noise to complain about."""
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"no response for id={want_id} within the timeout")
        try:
            kind, payload = q.get(timeout=remaining)
        except queue.Empty:
            raise TimeoutError(f"no response for id={want_id} within the timeout")
        if kind == "eof":
            raise ProtocolError("server closed stdout (EOF) before responding")
        if kind == "protocol_error":
            raise ProtocolError(payload)
        if payload.get("id") == want_id:
            return payload


def _tool_text(response, tool_name):
    if "error" in response:
        raise ProtocolError(f"{tool_name} returned a JSON-RPC error: {response['error']}")
    content = response.get("result", {}).get("content")
    if not content:
        raise ProtocolError(f"{tool_name} response has no content: {response}")
    return content[0].get("text", "")


def run_check(command, index_budget_seconds, request_timeout):
    proc = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    reader_queue, reader_thread = _start_reader(proc)
    next_id = [1]

    def call(method, params):
        this_id = next_id[0]
        next_id[0] += 1
        _send(proc, {"jsonrpc": "2.0", "id": this_id, "method": method, "params": params})
        return _recv_response(reader_queue, this_id, request_timeout)

    try:
        init = call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "lain-clean-room-check", "version": "1"},
            },
        )
        if "result" not in init:
            raise ProtocolError(f"initialize did not return a result: {init}")
        print("initialize: ok")

        listed = call("tools/list", {})
        tools = listed.get("result", {}).get("tools")
        if not tools:
            raise ProtocolError(f"tools/list returned no tools: {listed}")
        print(f"tools/list: ok ({len(tools)} tools)")

        def get_capabilities():
            resp = call("tools/call", {"name": "get_capabilities", "arguments": {}})
            text = _tool_text(resp, "get_capabilities")
            try:
                return json.loads(text)
            except json.JSONDecodeError as e:
                raise ProtocolError(
                    f"get_capabilities did not return JSON (published binary may predate "
                    f"this tool): {text!r}"
                ) from e

        caps = get_capabilities()
        deadline = time.monotonic() + index_budget_seconds
        while caps.get("indexing", {}).get("state") == "warming_up":
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"indexing never reached ready within {index_budget_seconds}s; "
                    f"last capabilities: {caps}"
                )
            retry_after_ms = caps.get("indexing", {}).get("retry_after_ms") or 1000
            time.sleep(retry_after_ms / 1000.0)
            caps = get_capabilities()
        state = caps.get("indexing", {}).get("state")
        if state != "ready":
            raise ProtocolError(f"indexing ended in {state!r}, not ready: {caps}")
        print("get_capabilities: ok (indexing ready)")

        query = call("tools/call", {"name": "find_anchors", "arguments": {"limit": 5}})
        text = _tool_text(query, "find_anchors")
        if not text.strip():
            raise ProtocolError("find_anchors returned an empty answer")
        try:
            envelope = json.loads(text)
        except json.JSONDecodeError:
            envelope = None  # find_anchors' normal answer is Markdown prose, not JSON.
        if isinstance(envelope, dict) and envelope.get("state") == "warming_up":
            raise ProtocolError(
                f"find_anchors still reports warming_up after capabilities said ready: {text}"
            )
        print("find_anchors: ok (non-loading answer)")

        print("CLEAN ROOM CHECK: PASSED")
        return 0
    finally:
        try:
            proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
        # The process is dead, so its stdout has hit EOF; give the
        # reader thread a moment to drain and exit before closing the
        # pipes out from under it.
        reader_thread.join(timeout=5)
        if proc.stderr is not None:
            stderr_tail = proc.stderr.read()
            if stderr_tail and stderr_tail.strip():
                print(f"--- server stderr ---\n{stderr_tail}", file=sys.stderr)
        for pipe in (proc.stdin, proc.stdout, proc.stderr):
            try:
                if pipe is not None:
                    pipe.close()
            except OSError:
                pass


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workspace", required=True, help="Fixture repository path passed as --workspace"
    )
    parser.add_argument(
        "--index-budget-seconds",
        type=float,
        default=60.0,
        help="Upper bound on waiting for indexing to reach ready",
    )
    parser.add_argument(
        "--request-timeout",
        type=float,
        default=30.0,
        help="Upper bound on waiting for any single JSON-RPC response",
    )
    parser.add_argument(
        "command",
        nargs="+",
        help="Command that launches `lain mcp`, e.g. npx @spuentesp/lain-mcp mcp",
    )
    args = parser.parse_args(argv)

    full_command = [*args.command, "--workspace", args.workspace]
    try:
        return run_check(full_command, args.index_budget_seconds, args.request_timeout)
    except (ProtocolError, TimeoutError) as e:
        print(f"CLEAN ROOM CHECK: FAILED -- {e}", file=sys.stderr)
        return 1
    except OSError as e:
        print(f"CLEAN ROOM CHECK: FAILED -- could not launch {full_command!r}: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
