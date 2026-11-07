#!/usr/bin/env python3
"""Benchmark real NVIDIA federations; Python stdlib only, Linux RSS sampling."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

GROUPS = {
    "openshell": [("openshell", "OpenShell"), ("nemo-fabric", "NeMo-Fabric"), ("nemoclaw", "NemoClaw")],
    "nat": [("nat", "NeMo-Agent-Toolkit"), ("nat-ui", "NeMo-Agent-Toolkit-UI"),
            ("nat-examples", "NeMo-Agent-Toolkit-Examples")],
}


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def readiness(body, expected):
    """Fail closed: exact roster, every repo ready; degraded is never success."""
    repos = body.get("repos", [])
    return (len(repos) == len(expected)
            and {r.get("id") for r in repos} == set(expected)
            and all(r.get("health", "").lower() == "ready" for r in repos))


def request(url, payload=None, timeout=1):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def rss_kib(pid):
    try:
        fields = Path(f"/proc/{pid}/status").read_text().splitlines()
        return next(int(line.split()[1]) for line in fields if line.startswith("VmRSS:"))
    except (OSError, StopIteration):
        return 0


def stop(proc):
    # Each invocation owns a new process group, including sidecars/LSP children.
    try:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        # Reap children even when the parent exited first.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()
    except ProcessLookupError:
        proc.wait()


def startup_timings(lines):
    """Preserve repeated per-repo spans; parent spans include their children."""
    timings = {}
    for line in lines:
        match = re.search(r"federation startup timing: ([a-z0-9_]+) ([0-9]+(?:\.[0-9]+)?)ms(?:\s|$)", line)
        if match:
            timings.setdefault(match.group(1), []).append(float(match.group(2)))
    return timings


def measure(binary, config, expected, work, timeout, lsp, phase, run, digest=False, timings=False):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = os.environ.copy()
    # Explicitly control the mode, independent of the invoking shell.
    for key in ("LAIN_TEST_NO_LSP", "LAIN_DISABLE_LSP", "LAIN_LSP_LEAN", "LAIN_LSP_PREWARM",
                "LAIN_API_KEYS", "LAIN_BIND_ADDR"):
        env.pop(key, None)
    if not lsp:
        env["LAIN_DISABLE_LSP"] = "1"
    env["LAIN_GIT_SENSOR"] = "sidecar"
    env["LAIN_GIT_SIDECAR_BIN"] = str(binary.with_name("lain-git-sidecar"))
    env["LAIN_INDEX_TIMEOUT"] = str(int(timeout))
    env["LAIN_STARTUP_DIGEST"] = "1" if digest else "0"
    env["LAIN_STARTUP_TIMING"] = "1" if timings else "0"
    log = work / f"{run}-{phase}.log"
    result = {"run": run, "phase": phase, "ready_s": None, "http_s": None,
              "tools_list_s": None, "peak_parent_rss_mib": 0, "status": "timeout",
              "host_loadavg_start": os.getloadavg()}
    start = time.monotonic()
    with log.open("w") as output:
        proc = subprocess.Popen([str(binary), "server", "--config", str(config), "--transport", "http",
                                 "--bind", "127.0.0.1", "--port", str(port), "--workspace", ""],
                                cwd=work, env=env, stdout=output, stderr=subprocess.STDOUT,
                                start_new_session=True)
        try:
            while time.monotonic() - start < timeout:
                result["peak_parent_rss_mib"] = max(result["peak_parent_rss_mib"], rss_kib(proc.pid) / 1024)
                if proc.poll() is not None:
                    result["status"] = f"exited:{proc.returncode}"
                    break
                try:
                    status, body = request(base + "/health/index-status")
                    now = time.monotonic() - start
                    if result["http_s"] is None:
                        result["http_s"] = now
                    result["last_index_status"] = body
                    if result["tools_list_s"] is None:
                        code, tools = request(base + "/mcp", {
                            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
                        if code == 200 and tools.get("result", {}).get("tools"):
                            result["tools_list_s"] = time.monotonic() - start
                    if status == 200 and readiness(body, expected):
                        result["ready_s"] = now
                        code, health = request(base + "/health", timeout=5)
                        result["health"] = health
                        # Nonempty federation graph is additional correctness evidence.
                        federation = health.get("federation") or {}
                        valid = (code == 200 and health.get("status") == "ok"
                                 and readiness(federation, expected)
                                 and federation.get("total_nodes", 0) > 0
                                 and federation.get("total_edges", 0) > 0
                                 and result["tools_list_s"] is not None)
                        result["status"] = "ready" if valid else "invalid_health"
                        break
                    if status == 503:
                        result["status"] = "unavailable"
                        break
                except (OSError, ValueError, urllib.error.URLError):
                    pass
                time.sleep(0.1)
        finally:
            result["peak_parent_rss_mib"] = max(result["peak_parent_rss_mib"], rss_kib(proc.pid) / 1024)
            stop(proc)
    result["elapsed_s"] = time.monotonic() - start
    result["host_loadavg_end"] = os.getloadavg()
    lines = log.read_text(errors="replace").splitlines()
    result["cache_hit"] = any("federation startup cache hit:" in line for line in lines)
    if timings:
        result["startup_timings_ms"] = startup_timings(lines)
        if result["status"] == "ready" and not result["startup_timings_ms"].get("loader_total"):
            result["status"] = "missing_timings"
    result["timing_log"] = [line for line in lines if "fully indexed in" in line
                            or "sensor finished (slow)" in line or ": projected " in line
                            or "cross-repo edge(s)" in line or "anchor miner:" in line]
    if digest:
        matches = [m.group(1) for line in lines if (m := re.search(r"federation startup digest: ([0-9a-f]{64})", line))]
        result["graph_digest"] = matches[-1] if matches else None
        if result["status"] == "ready" and not matches:
            result["status"] = "missing_digest"
    if result["status"] != "ready":
        result["log_tail"] = log.read_text(errors="replace").splitlines()[-30:]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workspace", type=Path, help="directory containing the six NVIDIA clones")
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[2] / "target/release/lain")
    parser.add_argument("--group", choices=["all", *GROUPS], default="all")
    parser.add_argument("--timings", action="store_true", help="capture nested startup phase timings; requires an instrumented binary")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=900)
    parser.add_argument("--concurrency", type=int, default=1, help="concurrent repo indexers (default: 1)")
    parser.add_argument("--digest", action="store_true", help="verify full graph content agrees across all runs; includes fingerprinting in measured startup")
    parser.add_argument("--lsp", action="store_true", help="enable normal LSP; default measures static readiness")
    parser.add_argument("--output", type=Path, required=True, help="JSON evidence file; temporary graphs/logs are removed")
    args = parser.parse_args()
    def interrupted(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    if args.runs < 1 or args.timeout <= 0 or args.concurrency < 1:
        parser.error("runs, timeout and concurrency must be positive")
    binary = args.binary.resolve(strict=True)
    sidecar = binary.with_name("lain-git-sidecar").resolve(strict=True)
    workspace = args.workspace.resolve(strict=True)
    groups = GROUPS if args.group == "all" else {args.group: GROUPS[args.group]}
    inputs = {}
    for repos in groups.values():
        for repo_id, name in repos:
            root = workspace / name
            inputs[repo_id] = {"path": str(root), "commit": git(root, "rev-parse", "HEAD"),
                               "dirty": bool(git(root, "status", "--porcelain", "--untracked-files=no")),
                               "tracked_files": len(git(root, "ls-files").splitlines())}
    identity = json.loads(subprocess.run(
        [str(binary), "doctor", "--json"], cwd=Path(__file__).resolve().parents[2],
        text=True, capture_output=True, timeout=30, check=False).stdout)
    report = {"schema_version": 1, "binary_build_commit": identity.get("build_commit"),
              "binary_version": identity.get("server_version"), "mode": "default-lsp" if args.lsp else "static-no-lsp",
              "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "sidecar_binary": str(sidecar), "sidecar_sha256": hashlib.sha256(sidecar.read_bytes()).hexdigest(),
              "git_sensor": "sidecar", "platform": platform.platform(), "cpu_count": os.cpu_count(), "inputs": inputs,
              "concurrency": args.concurrency, "requested_runs": args.runs, "timeout_s": args.timeout, "digest_enabled": args.digest, "timings_enabled": args.timings, "graph_digest_algorithm": "blake3-tagged-record-multiset-v1", "runs": [], "summary": {},
              "notes": ["cold = fresh Lain data directory; OS page cache is not flushed",
                        "warm = restart using the preceding cold run's data directory",
                        "RSS samples cover the server parent only, at roughly 100ms plus probe latency",
                        "Readiness requires the exact repo roster ready after cross-repo linking"],
              "source_commit": git(Path(__file__).resolve().parents[2], "rev-parse", "HEAD"),
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    def save():
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    save()
    try:
        for group, repos in groups.items():
            for run in range(1, args.runs + 1):
                with tempfile.TemporaryDirectory(prefix="lain-nvidia-startup-") as temp:
                    work = Path(temp)
                    config = work / "repos.yaml"
                    # JSON is a YAML subset; avoids a third-party YAML dependency.
                    config.write_text(json.dumps({"data_dir": str(work / "data"),
                        "max_concurrent_indexers": args.concurrency,
                        "repos": [{"id": rid, "source": {"type": "workspace_dir", "path": inputs[rid]["path"]}}
                                  for rid, _ in repos]}))
                    for phase in ("cold", "warm"):
                        result = measure(binary, config, [rid for rid, _ in repos], work,
                                         args.timeout, args.lsp, phase, run, digest=args.digest, timings=args.timings)
                        result["group"] = group
                        report["runs"].append(result)
                        save()
                        print(f"{group} {phase} #{run}: {result['status']} ready={result['ready_s']}s "
                              f"HTTP={result['http_s']}s RSS={result['peak_parent_rss_mib']:.1f}MiB", flush=True)
                        if result["status"] != "ready":
                            break
    finally:
        for group in groups:
            for phase in ("cold", "warm"):
                values = [r["ready_s"] for r in report["runs"] if r["group"] == group
                          and r["phase"] == phase and r["status"] == "ready"]
                if values:
                    report["summary"][f"{group}/{phase}"] = {"n": len(values),
                        "median_s": statistics.median(values), "min_s": min(values), "max_s": max(values)}
        save()
    if args.digest:
        report["graph_digests_stable"] = {
            group: len({r.get("graph_digest") for r in report["runs"] if r["group"] == group}) == 1
            and all(r.get("graph_digest") for r in report["runs"] if r["group"] == group)
            for group in groups}
        save()
        for group, valid in report["graph_digests_stable"].items():
            if not valid:
                print(f"FAIL: graph content differs across {group} startup runs", flush=True)
    stable = not args.digest or all(report["graph_digests_stable"].values())
    return 0 if stable and len(report["runs"]) == len(groups) * args.runs * 2 and all(
        r["status"] == "ready" for r in report["runs"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
