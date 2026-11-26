#!/usr/bin/env python3
"""Measure what PR analyses cost a running `lain server` in memory.

Phase 0 of docs/plans/layered-snapshots.md. Starts `lain server` over
HTTP with `LAIN_MEM_PROBE=1`, waits until it answers `list_services`
(the readiness check the Parallax Fly entrypoint uses), then runs each
PR the way Parallax's assessment worker does:

  prepare_snapshot(base) / prepare_snapshot(head)  (long-polled to ready)
  diff_contracts(base, head)
  get_coverage(head)
  trace_impact(...) for up to 8 changed provider endpoints
                    (EndpointRemoved is traced on base)
  get_service(...) for each traced service, on head

Between steps it records the server's footprint, RSS + swap (sampled every
200 ms from /proc/<pid>/status) and the peak inside the step. After the last PR it
idles for --rest-secs and records RSS at rest. The server's own
`mem_probe` log lines (hydrate / project / join per repo, one per tool
call) are collected from its log and printed as a per-phase table.

Output: a human table on stdout and, with --json, the raw numbers.

  scripts/measure_pr_memory.py --lain target/release/lain \\
      --config target/bench-fixture/repos-bench.yaml \\
      --pr 'OpenShell#4995:OpenShell=44ef98…:3ca79f…' \\
      --pr 'OpenShell#6003:OpenShell=b8b6d6…:e8b92f…' \\
      --rest-secs 150 --json target/bench-fixture/run.json

A --pr spec is `label:repo=base_sha:head_sha`; every other repo of the
config is pinned at its checkout's HEAD in both snapshots (what
Parallax does for repos a PR does not touch). Repos that only exist in
the config are read from `source.path` (workspace_dir sources only).

The server runs with the Fly image's settings (no language servers,
`LAIN_DISABLE_COMMIT_SYNC=1`, `LAIN_DISABLE_FILE_WATCHER=1`); pass
--host-env to use this host's instead. With a language server on PATH,
a live re-index runs rust-analyzer inside the measured process tree and
dwarfs everything a snapshot costs.

Any change under src/ changes `analyzer_version` (it carries the source
fingerprint) and the build id, so the first run after a rebuild
re-indexes the live repos and every snapshot commit. --warmup runs the
same PRs once, unmeasured, in a separate server process first.

Run it in a scope without swap so pages cannot hide from RSS, e.g.
  systemd-run --user --scope -p MemorySwapMax=0 -p MemoryMax=12G scripts/measure_pr_memory.py …

Linux only (reads /proc).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
import urllib.request

MAX_TRACE_TARGETS = 8  # Parallax assessment-worker.ts
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def rss_kb(pid: int) -> int:
    """Footprint: VmRSS + VmSwap, in KiB.

    RSS alone undercounts on a host that swaps (pages pushed out leave
    RSS); the Parallax podman recipe measures memory + swap for the
    same reason.
    """
    total = 0
    try:
        with open(f"/proc/{pid}/status") as fh:
            for line in fh:
                if line.startswith(("VmRSS:", "VmSwap:")):
                    total += int(line.split()[1])
    except OSError:
        pass
    return total


class Sampler(threading.Thread):
    """Samples one pid's RSS; `window()` returns the peak since the last call."""

    def __init__(self, pid: int, every: float = 0.2):
        super().__init__(daemon=True)
        self.pid = pid
        self.every = every
        self.lock = threading.Lock()
        self.peak = 0
        self.overall = 0
        self.stop = threading.Event()

    def run(self):
        while not self.stop.is_set():
            v = rss_kb(self.pid)
            with self.lock:
                self.peak = max(self.peak, v)
                self.overall = max(self.overall, v)
            time.sleep(self.every)

    def window(self) -> int:
        v = rss_kb(self.pid)
        with self.lock:
            peak = max(self.peak, v)
            self.peak = v
            self.overall = max(self.overall, v)
        return peak


class Lain:
    def __init__(self, url: str, timeout: float = 900):
        self.url = url
        self.timeout = timeout
        self.next_id = 0

    def call(self, tool: str, args: dict) -> dict:
        self.next_id += 1
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": self.next_id,
                "method": "tools/call",
                "params": {"name": tool, "arguments": args},
            }
        ).encode()
        req = urllib.request.Request(
            self.url,
            data=body,
            headers={
                "content-type": "application/json",
                "accept": "application/json, text/event-stream",
            },
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as res:
            text = res.read().decode()
        data_line = next((l for l in text.splitlines() if l.startswith("data: ")), None)
        message = json.loads(data_line[6:] if data_line else text)
        if "error" in message:
            raise RuntimeError(f"{tool}: JSON-RPC error {message['error']}")
        result = message.get("result") or {}
        payload = result.get("structuredContent")
        if payload is None:
            content = result.get("content") or [{}]
            raw = content[0].get("text", "null")
            try:
                payload = json.loads(raw)
            except json.JSONDecodeError:
                payload = {"text": raw}
        if isinstance(payload, dict) and "data" in payload and isinstance(payload["data"], dict):
            merged = dict(payload["data"])
            for k in ("ok", "error", "truncated", "truncation_reason"):
                if k in payload and k not in merged:
                    merged[k] = payload[k]
            return merged
        return payload if isinstance(payload, dict) else {"value": payload}


def parse_pr(spec: str):
    label, rest = spec.split(":", 1)
    repo, shas = rest.split("=", 1)
    base, head = shas.split(":", 1)
    return {"label": label, "repo": repo, "base": base, "head": head}


def config_repos(config_path: str) -> dict:
    """repo id -> workspace_dir path, from a repos.yaml (no yaml dep)."""
    text = open(config_path).read()
    out = {}
    for m in re.finditer(r"-\s*id:\s*(\S+)\s*\n(?:\s+\w.*\n)*?\s+source:\s*\{[^}]*path:\s*([^},]+)\}", text):
        out[m.group(1)] = m.group(2).strip()
    return out


def git_head(path: str) -> str:
    return subprocess.check_output(["git", "-C", path, "rev-parse", "HEAD"], text=True).strip()


def wait_ready(lain: Lain, proc: subprocess.Popen, limit_s: int) -> float:
    started = time.time()
    while time.time() - started < limit_s:
        if proc.poll() is not None:
            raise RuntimeError(f"lain exited with {proc.returncode} before it was ready")
        try:
            lain.call("list_services", {"snapshot": "live"})
            return time.time() - started
        except Exception:
            time.sleep(1)
    raise RuntimeError(f"lain not ready after {limit_s}s")


def await_snapshot(lain: Lain, repos: dict, limit_s: int) -> str:
    snap = lain.call("prepare_snapshot", {"repos": repos, "wait_ms": 60000})
    started = time.time()
    while snap.get("state") not in ("ready", "failed"):
        if time.time() - started > limit_s:
            raise RuntimeError(f"snapshot {snap.get('snapshot')} not ready after {limit_s}s")
        snap = lain.call("get_snapshot", {"snapshot": snap["snapshot"], "wait_ms": 60000})
    if snap.get("state") != "ready":
        raise RuntimeError(f"snapshot {snap.get('snapshot')} failed: {snap.get('error')}")
    return snap["snapshot"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lain", required=True)
    ap.add_argument("--config", required=True)
    ap.add_argument("--port", type=int, default=9989)
    ap.add_argument("--pr", action="append", required=True)
    ap.add_argument("--rest-secs", type=int, default=150)
    ap.add_argument("--ready-secs", type=int, default=3600)
    ap.add_argument("--snapshot-secs", type=int, default=3600)
    ap.add_argument("--log", default=None, help="server log path (default: next to --json or ./lain-measure.log)")
    ap.add_argument("--json", default=None)
    ap.add_argument("--env", action="append", default=[], help="extra KEY=VALUE for the server")
    ap.add_argument("--host-env", action="store_true", help="do not apply the Fly image's settings")
    ap.add_argument("--warmup", action="store_true", help="run the PRs once, unmeasured, first")
    args = ap.parse_args()

    if args.warmup:
        argv = [a for a in sys.argv[1:] if a != "--warmup"]
        if "--json" in argv:
            i = argv.index("--json")
            argv[i + 1] = os.path.splitext(argv[i + 1])[0] + ".warmup.json"
        argv += ["--rest-secs", "10"]
        print("warm-up pass (unmeasured)", flush=True)
        subprocess.run([sys.executable, os.path.abspath(__file__)] + argv, check=False)
        print("measured pass", flush=True)

    log_path = args.log or (os.path.splitext(args.json)[0] + ".log" if args.json else "lain-measure.log")
    env = dict(os.environ)
    env["LAIN_MEM_PROBE"] = "1"
    env["NO_COLOR"] = "1"
    if not args.host_env:
        # deploy/fly in Parallax: the image ships no language servers.
        env.update({
            "LAIN_DISABLE_LSP": "1",
            "LAIN_DISABLE_COMMIT_SYNC": "1",
            "LAIN_DISABLE_FILE_WATCHER": "1",
        })
    env.setdefault("RUST_LOG", "info")
    for kv in args.env:
        k, v = kv.split("=", 1)
        env[k] = v

    repos_paths = config_repos(args.config)
    pinned = {rid: git_head(path) for rid, path in repos_paths.items()}

    log = open(log_path, "w")
    proc = subprocess.Popen(
        [args.lain, "server", "--config", args.config, "--transport", "http", "--port", str(args.port)],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
    )
    sampler = Sampler(proc.pid)
    sampler.start()
    lain = Lain(f"http://127.0.0.1:{args.port}/mcp")
    steps = []

    def step(name: str, fn):
        t0 = time.time()
        err = None
        out = None
        try:
            out = fn()
        except Exception as e:  # recorded, never hidden
            err = str(e)
        peak = sampler.window()
        row = {
            "step": name,
            "seconds": round(time.time() - t0, 1),
            "peak_mb": round(peak / 1024),
            "rss_after_mb": round(rss_kb(proc.pid) / 1024),
            "error": err,
        }
        steps.append(row)
        print(f"  {name:<48} {row['seconds']:>7}s  peak {row['peak_mb']:>6} MB  after {row['rss_after_mb']:>6} MB" + (f"  ERROR {err}" if err else ""), flush=True)
        return out

    results = {"pinned": pinned, "prs": []}
    try:
        print(f"lain pid {proc.pid}, log {log_path}", flush=True)
        step("startup (until list_services answers)", lambda: wait_ready(lain, proc, args.ready_secs))
        for spec in args.pr:
            pr = parse_pr(spec)
            base_repos = dict(pinned, **{pr["repo"]: pr["base"]})
            head_repos = dict(pinned, **{pr["repo"]: pr["head"]})
            print(f"PR {pr['label']}", flush=True)
            base = step(f"{pr['label']} snapshot base", lambda: await_snapshot(lain, base_repos, args.snapshot_secs))
            head = step(f"{pr['label']} snapshot head", lambda: await_snapshot(lain, head_repos, args.snapshot_secs))
            if not base or not head:
                results["prs"].append({"label": pr["label"], "error": "snapshot failed"})
                continue
            diff = step(f"{pr['label']} diff_contracts", lambda: lain.call("diff_contracts", {"base": base, "head": head})) or {}
            step(f"{pr['label']} get_coverage", lambda: lain.call("get_coverage", {"snapshot": head}))
            changes = diff.get("changes") or []
            traceable = [
                c for c in changes
                if isinstance(c.get("endpoint"), dict)
                and c["endpoint"].get("key") is not None
                and c["endpoint"].get("service") is not None
                and c.get("side") != "consumer"
            ][:MAX_TRACE_TARGETS]
            traces = []
            for i, c in enumerate(traceable):
                ep = c["endpoint"]
                view = base if c.get("kind") == "EndpointRemoved" else head
                r = step(
                    f"{pr['label']} trace_impact #{i}",
                    lambda: lain.call("trace_impact", {"snapshot": view, "from": {"endpoint": {"service": ep["service"], "key": ep["key"]}}}),
                )
                traces.append({"endpoint": ep, "kind": c.get("kind"), "truncated": (r or {}).get("truncated")})
            for svc in sorted({c["endpoint"]["service"] for c in traceable}):
                step(f"{pr['label']} get_service {svc}", lambda: lain.call("get_service", {"snapshot": head, "service": svc}))
            results["prs"].append({
                "label": pr["label"], "base": base, "head": head,
                "changes": len(changes), "traced": len(traceable), "traces": traces,
                "diff_truncated": diff.get("truncated"),
            })
        def rest():
            # Sample at rest every 10 s so the curve is visible.
            curve = []
            for _ in range(max(1, args.rest_secs // 10)):
                time.sleep(10)
                curve.append(round(rss_kb(proc.pid) / 1024))
            return curve
        curve = step(f"rest {args.rest_secs}s", rest)
        results["rest_curve_mb"] = curve
    finally:
        sampler.stop.set()
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()

    probes = []
    edges = []
    pat = re.compile(r"mem_probe.*?phase=\"?([\w:]+)\"?.*?detail=\"?([^\"]*?)\"? rss_kb=(\d+)(?: swap_kb=(\d+))? peak_kb=(\d+)")
    with open(log_path, errors="replace") as fh:
        for line in fh:
            if "mem_probe" not in line:
                continue
            line = ANSI.sub("", line)
            if "mem_probe_edges" in line:
                e = re.search(r"snapshot=\"?(\S+?)\"? total_edges=(\d+) cross_repo=(\{.*?\})", line)
                if e:
                    edges.append({"snapshot": e.group(1), "total_edges": int(e.group(2)), "cross_repo": e.group(3)})
                continue
            m = pat.search(line)
            if m:
                probes.append({
                    "phase": m.group(1), "detail": m.group(2),
                    "rss_mb": int(m.group(3)) // 1024,
                    "swap_mb": int(m.group(4) or 0) // 1024,
                    "peak_mb": int(m.group(5)) // 1024,
                })
    print("\nserver mem_probe phases (peak = resident peak since previous probe; excludes swap)")
    for p in probes:
        print(f"  {p['phase']:<22} {p['detail']:<66} peak {p['peak_mb']:>6} MB  rss {p['rss_mb']:>6} MB  swap {p['swap_mb']:>5} MB")
    if edges:
        print("\nsnapshot edges (cross-repo by type)")
        for e in edges:
            print(f"  {e['snapshot']}  total {e['total_edges']}  cross {e['cross_repo']}")
    print(f"\noverall sampled peak: {round(sampler.overall / 1024)} MB")
    results.update({"steps": steps, "probes": probes, "edges": edges, "overall_peak_mb": round(sampler.overall / 1024)})
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(results, fh, indent=2)
    return 1 if any(s["error"] for s in steps) else 0


if __name__ == "__main__":
    sys.exit(main())
