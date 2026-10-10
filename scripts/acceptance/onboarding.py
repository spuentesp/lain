#!/usr/bin/env python3
"""Onboarding acceptance: install Lain in a clean room and finish with a
real task — separating four very different outcomes:

    1. it installs        (clean npm/dev install, docs-only)
    2. it answers correctly (pre-declared ground truth, incl. false-positive trap)
    3. it stays updated   (re-query after the edit; stale index = FAIL)
    4. it helps real work (agent lane only; exploratory)

Two evaluations, deliberately separate:

    --mode graph   "does Lain work?"   — scripted queries asserted against
                   tests/fixtures/onboarding/ground_truth.json, which is
                   declared BEFORE any query. Empty or stale answers are
                   failures, never "no risk".
    --mode agent   "does Lain help an agent?" — a real coding agent gets a
                   task in the same clean room (with or without Lain, via
                   --arm lain|no-lain). One comparison is exploratory: it
                   finds problems, it does not prove improvement.

Honesty rules (enforced here):
  * clean disposable environment: fresh HOME/XDG/npm prefix, PATH scrubbed
    of any pre-existing `lain`;
  * evidence: report.json records dist, installed version, OS and every
    command executed;
  * no rescue: only publicly documented commands (`--help`, README) are
    used; anything else is recorded in report.doc_gaps as an onboarding
    problem;
  * verifiable answers: every assertion cites file/symbol pairs from the
    ground truth, never "whatever the tool said".

Usage examples:
    python3 scripts/acceptance/onboarding.py --dist dev --mode graph
    python3 scripts/acceptance/onboarding.py --dist npm --mode graph
    python3 scripts/acceptance/onboarding.py --dist npm --mode agent \
        --agent-cmd 'claude -p --permission-mode acceptEdits'
"""
import argparse
import glob
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import run  # noqa: E402  (Mcp, callers_of — the shared stdio client)

FIXTURES = os.path.join(REPO_ROOT, "tests", "fixtures", "onboarding")
GROUND_TRUTH = os.path.join(FIXTURES, "ground_truth.json")
FIXTURE_GEN = os.path.join(FIXTURES, "make_fixture.py")
TASK = os.path.join(FIXTURES, "task.md")
TASK_NO_LAIN = os.path.join(FIXTURES, "task-no-lain.md")
INSTALL_SH = os.path.join(REPO_ROOT, "install.sh")

NPM_PACKAGE = "@spuentesp/lain-mcp"


# ── evidence ────────────────────────────────────────────────────────────────

class Evidence:
    def __init__(self):
        self.commands = []
        self.doc_gaps = []
        self.notes = []

    def cmd(self, argv, **kw):
        """Run a command, recording it. Returns CompletedProcess."""
        rec = {"argv": [str(a) for a in argv]}
        t0 = time.time()
        p = subprocess.run(argv, capture_output=True, text=True, **kw)
        rec.update(returncode=p.returncode, seconds=round(time.time() - t0, 1),
                   stdout_tail=p.stdout[-400:], stderr_tail=p.stderr[-400:])
        self.commands.append(rec)
        return p

    def doc_gap(self, what, detail):
        self.doc_gaps.append({"needed": what, "detail": detail})


def record_only(ev, what):
    ev.notes.append(what)


# ── clean room ──────────────────────────────────────────────────────────────

def scrub_path(ev):
    """Drop PATH entries that already carry a `lain` binary so a previous
    install can never be mistaken for the one under test."""
    kept, dropped = [], []
    for entry in os.environ.get("PATH", "").split(os.pathsep):
        if entry and os.path.isfile(os.path.join(entry, "lain")):
            dropped.append(entry)
        else:
            kept.append(entry)
    if dropped:
        record_only(ev, f"PATH scrubbed of pre-existing lain in: {dropped}")
    return os.pathsep.join(kept)


def clean_env(work, ev):
    home = os.path.join(work, "home")
    for d in (home, os.path.join(work, "xdg-config"),
              os.path.join(work, "xdg-state"), os.path.join(work, "xdg-cache")):
        os.makedirs(d, exist_ok=True)
    env = dict(os.environ)
    env.update(
        HOME=home,
        XDG_CONFIG_HOME=os.path.join(work, "xdg-config"),
        XDG_STATE_HOME=os.path.join(work, "xdg-state"),
        XDG_CACHE_HOME=os.path.join(work, "xdg-cache"),
        GIT_TERMINAL_PROMPT="0",
    )
    env["PATH"] = scrub_path(ev)
    return env


def find_lain(work, path):
    """A working `lain` wherever the install landed: PATH first, then any
    `*/bin/lain` or cached platform binary under the clean room (the agent
    may choose its own npm prefix — that is its right, not a failure)."""
    hit = shutil.which("lain", path=path)
    if hit:
        return hit
    return next(iter(find_all_lains(work, path)), None)


def find_all_lains(work, path):
    out = []

    def add(c):
        if c and c not in out and os.path.exists(c):
            out.append(c)

    add(shutil.which("lain", path=path))
    patterns = (os.path.join(work, "*", "bin", "lain"),
                os.path.join(work, "xdg-cache", "lain", "*", "*", "lain"),
                os.path.join(work, "*", "lib", "node_modules", "*", "bin", "lain"))
    for pattern in patterns:
        for c in sorted(glob.glob(pattern)):
            add(c)
    root = os.path.realpath(work)
    for c in list(out):
        real = os.path.realpath(c)
        if real.startswith(root + os.sep):
            add(real)  # symlink targets inside the clean room get wrapped too
    return out


# ── install (the "it installs" outcome) ─────────────────────────────────────

def package_dev(work, ev):
    """Build and package the working tree exactly like release.yml does:
    a flat tarball with `lain` + `lain-git-sidecar`, plus GNU SHA256SUMS."""
    bins = {}
    for name in ("lain", "lain-git-sidecar"):
        path = os.path.join(REPO_ROOT, "target", "release", name)
        bins[name] = path
    if not all(os.path.isfile(p) for p in bins.values()) or \
            os.environ.get("LAIN_REBUILD") == "1":
        p = ev.cmd(["cargo", "build", "--release", "--bin", "lain",
                    "--bin", "lain-git-sidecar"], cwd=REPO_ROOT)
        if p.returncode != 0:
            raise RuntimeError(f"cargo build failed:\n{p.stderr[-2000:]}")

    version = package_version()
    platform_triple = detect_platform(ev)
    dist = os.path.join(work, "dist")
    os.makedirs(dist, exist_ok=True)
    for name, src in bins.items():
        shutil.copy2(src, os.path.join(dist, name))

    asset = f"lain-{version}-{platform_triple}.tar.gz"
    ev.cmd(["tar", "czf", os.path.join(dist, asset), "-C", dist,
            "lain", "lain-git-sidecar"])
    import hashlib
    digest = hashlib.sha256(open(os.path.join(dist, asset), "rb").read()).hexdigest()
    with open(os.path.join(dist, "SHA256SUMS"), "w") as f:
        f.write(f"{digest}  {asset}\n")
    return dist, version


def package_version():
    import tomllib
    with open(os.path.join(REPO_ROOT, "Cargo.toml"), "rb") as f:
        return tomllib.load(f)["package"]["version"]


def detect_platform(ev):
    # Reuse the installer's own detection: the tarball name must match the
    # one install.sh builds internally.
    p = ev.cmd(["bash", "-c", f"source '{INSTALL_SH}' && detect_platform"])
    triple = p.stdout.strip().splitlines()[-1] if p.stdout.strip() else ""
    if p.returncode != 0 or not triple or triple == "unsupported":
        raise RuntimeError(f"platform detection failed: {p.stdout} {p.stderr}")
    return triple


def serve_dir(path, ev):
    """Local HTTP server standing in for GitHub Releases."""
    import http.server
    import functools
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=path)
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    record_only(ev, f"serving {path} on http://127.0.0.1:{port}")
    return httpd, port


def install_dist(dist, work, env, ev):
    """Install `dist` (=npm | npm@X | dev) into a scratch prefix the way a
    user would. Returns (lain_path, version_reported, dist_description)."""
    prefix = os.path.join(work, "prefix")
    os.makedirs(prefix, exist_ok=True)
    env = dict(env)

    if dist == "dev":
        dist_dir, version = package_dev(work, ev)
        httpd, port = serve_dir(dist_dir, ev)
        try:
            env.update(LAIN_INSTALL_DIR=prefix,
                       LAIN_VERSION=version,
                       LAIN_RELEASE_BASE_URL=f"http://127.0.0.1:{port}")
            p = ev.cmd(["bash", INSTALL_SH, "--yes"], env=env, cwd=work)
        finally:
            httpd.shutdown()
        if p.returncode != 0:
            raise RuntimeError(f"install.sh failed:\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}")
        lain = os.path.join(prefix, "lain")
        desc = f"dev build {version} installed via install.sh from a local release artifact"
    else:
        spec = dist if dist != "npm" else NPM_PACKAGE
        if spec == "npm":
            spec = NPM_PACKAGE
        elif spec.startswith("npm@"):
            spec = f"{NPM_PACKAGE}@{spec[4:]}"
        env["npm_config_prefix"] = prefix
        p = ev.cmd(["npm", "install", "-g", spec], env=env, cwd=work)
        if p.returncode != 0:
            raise RuntimeError(f"npm install failed:\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}")
        lain = os.path.join(prefix, "bin", "lain")
        desc = f"published npm package {spec} installed with npm -g"

    if not (os.path.isfile(lain) and os.access(lain, os.X_OK)):
        raise RuntimeError(f"no executable at {lain} after install")
    env["PATH"] = prefix + os.pathsep + os.path.join(prefix, "bin") + os.pathsep + env["PATH"]

    v = ev.cmd([lain, "--version"], env=env)
    return lain, v.stdout.strip(), desc


# ── configure the client (documented `lain setup`) ──────────────────────────

def configure_client(lain, fixture, env, ev):
    p = ev.cmd([lain, "setup", "--agent", "generic", "--json", "--no-model"],
               env=env, cwd=fixture)
    cfg = os.path.join(fixture, ".mcp.json")
    ok = p.returncode == 0 and os.path.isfile(cfg)
    shape_ok = False
    if ok:
        try:
            body = json.load(open(cfg))
            shape_ok = "mcpServers" in json.dumps(body) or "servers" in json.dumps(body)
        except ValueError:
            pass
    return {"ran": p.returncode == 0, "config_written": ok, "config_shape": shape_ok}


# ── graph mode: does Lain work? ─────────────────────────────────────────────

def graph_mode(lain, fixture, env, ev, truth):
    verdicts = {"answers_correctly": None, "stays_updated": None}
    checks = []

    def check(name, ok, got, expected):
        checks.append({"check": name, "ok": bool(ok), "expected": expected,
                       "got": (got or "")[:400]})

    m = run.Mcp(lain, fixture, env)
    try:
        ready = m.wait_ready()
        record_only(ev, f"index ready in {ready:.1f}s")

        pre = truth["pre_edit"]
        blast = m.call("get_blast_radius", {"symbol": truth["fixture"]["target"]["symbol"]})
        for sym in pre["blast_radius"]["must_include"]:
            check(f"blast radius includes {sym}", sym in blast, blast, sym)
        for sym in pre["blast_radius"]["must_not_include"]:
            check(f"blast radius excludes decoy {sym}", sym not in blast, blast, f"absent: {sym}")
        direct = re.search(r"Direct dependents \((\d+)\)", blast)
        check("direct dependent count",
              direct and int(direct.group(1)) == pre["blast_radius"]["direct_dependents"],
              blast, pre["blast_radius"]["direct_dependents"])

        chain = m.call("get_call_chain", {"from": pre["call_chain"]["from"],
                                          "to": pre["call_chain"]["to"]})
        path = " → ".join(pre["call_chain"]["must_include_path"])
        check("call chain open_session -> normalize_token is the real path",
              path in chain, chain, path)

        sites = m.call("get_call_sites", {"symbol": truth["fixture"]["target"]["symbol"]})
        for c in pre["direct_callers"]:
            check(f"call sites name {c['symbol']} in {c['file']}",
                  c["symbol"] in sites and c["file"] in sites, sites,
                  f"{c['symbol']} ({c['file']})")

        # Symbol confusion trap: the decoy has callers of its own? None. A
        # blast radius that routes through it is a false positive.
        decoy = pre["decoys"][0]["symbol"]
        decoy_blast = m.call("get_blast_radius", {"symbol": decoy})
        check(f"decoy {decoy} has no dependents of its own",
              all(s not in decoy_blast for s in ("create_user", "load_user", "open_session")),
              decoy_blast, "no callers of normalize_token listed")

        answers_ok = all(c["ok"] for c in checks)
        verdicts["answers_correctly"] = answers_ok

        # ── the edit (scripted in graph mode) ──
        ev.cmd([sys.executable, FIXTURE_GEN, "apply-edit", fixture])
        tests = truth["fixture"]["test_command"]
        t = ev.cmd(tests, cwd=fixture)
        check("tests pass after the edit (cover direct + indirect consumers)",
              t.returncode == 0, t.stdout + t.stderr, "unittest OK")
        if t.returncode != 0:
            verdicts["answers_correctly"] = False

        # ── the staleness window: an answer missing recent work must say so.
        #    Silent empty = FAIL; the contract is "reflected, or visibly stale". ──
        window = truth["post_edit"]["staleness_window"]
        probe = m.call(window["tool"], window["args"])
        reflected = all(s in probe for s in truth["post_edit"]["index_updated"]["must_include"])
        explained = any(s in probe for s in window["warning_must_match_any"])
        check("uncommitted change: reflected or the gap explained — never a silent empty answer",
              reflected or explained, probe,
              f"caller found, or an explanation among {window['warning_must_match_any']}")
        if not (reflected or explained):
            verdicts["answers_correctly"] = False

        # ── commit: the unit the index follows (the tool's own error message
        #    says "committed and re-indexed") ──
        ev.cmd(["git", "add", "core.py"], cwd=fixture)
        ev.cmd(["git", "-c", "user.email=onboarding@lain.local",
                "-c", "user.name=onboarding", "commit", "-q", "-m",
                "normalize_token collapses internal whitespace"], cwd=fixture)

        # ── re-query: the update lands with the next indexing pass, i.e. a
        #    fresh session (a session that was already running does not
        #    re-index on new commits) ──
        upd = truth["post_edit"]["index_updated"]
        m.close()
        m = run.Mcp(lain, fixture, env)
        m.wait_ready()
        deadline = time.time() + 90
        seen = ""
        while time.time() < deadline:
            seen = m.call(upd["tool"], upd["args"])
            if all(s in seen for s in upd["must_include"]):
                break
            time.sleep(2)
        ok = all(s in seen for s in upd["must_include"])
        check("index reflects the edit after commit (collapse_ws called by normalize_token)",
              ok, seen, upd["must_include"])
        if not ok:
            record_only(ev, "STALE INDEX: " + upd["stale_would_look_like"])
        record_only(ev, "documentation finding: " + upd["live_session_note"])
        ev.doc_gap("README 'updates on change' qualification",
                   upd["live_session_note"])
        verdicts["stays_updated"] = ok
    finally:
        m.close()

    return verdicts, checks


# ── agent mode: does Lain help? (exploratory) ───────────────────────────────

WRAPPER = '''#!/usr/bin/env python3
# Traffic-logging shim: records which Lain calls the agent makes —
# both MCP JSON-RPC on stdio AND plain CLI invocations (oneshot/status/
# reindex run their MCP in-process, so stdio alone would miss them).
import json, os, subprocess, sys, threading
REAL = {real!r}
LOG = {log!r}
try:
    with open(LOG, "ab") as f:
        f.write(b"### ARGV " + repr(sys.argv[1:]).encode() + b"\\n")
except OSError:
    pass
proc = subprocess.Popen([REAL] + sys.argv[1:], stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE, stderr=None)
def pump(src, dst, tag):
    for line in iter(src.readline, b""):
        try:
            with open(LOG, "ab") as f:
                f.write(tag + b" " + line[:2000] + b"\\n")
        except OSError:
            pass
        dst.write(line); dst.flush()
    try:
        dst.close()
    except Exception:
        pass
threading.Thread(target=pump, args=(proc.stdout, sys.stdout.buffer, b">>>"), daemon=True).start()
threading.Thread(target=pump, args=(sys.stdin.buffer, proc.stdin, b"<<<"), daemon=True).start()
sys.exit(proc.wait())
'''


def install_shim(work, env, ev):
    """Transparent traffic logging: wrap every installed `lain` binary in
    place — wherever the agent installed or registered it — so the MCP
    calls it makes are recorded. Instrumentation only: the wrapper relays
    to the real binary unchanged."""
    log = os.path.join(work, "lain-traffic.log")
    wrapped = []
    for target in find_all_lains(work, env.get("PATH")):
        if os.path.islink(target) or target.endswith(".realbin"):
            continue  # symlinks lead to a wrapped target; never double-wrap
        real = target + ".realbin"
        shutil.move(target, real)
        with open(target, "w") as f:
            f.write(WRAPPER.format(real=real, log=log))
        os.chmod(target, 0o755)
        wrapped.append(target)
    record_only(ev, f"traffic-logging wrappers installed in place of: {wrapped or 'NONE FOUND'}")
    return env


def run_agent(cmd, prompt, cwd, env, ev, transcript_path):
    argv = cmd.split() + []
    p = ev.cmd(argv, env=env, cwd=cwd, input=prompt, timeout=1800)
    with open(transcript_path, "w") as f:
        f.write(p.stdout)
        f.write("\n--- stderr ---\n")
        f.write(p.stderr)
    return p


def agent_launch_failed(p):
    """The agent CLI itself failed to run (bad flag, auth, crash). Its
    output cannot be graded — an empty transcript must never turn into a
    vacuous pass like 'index has no staleness warning'."""
    return p.returncode != 0 and not p.stdout.strip()


def parse_tagged(text, tag):
    # Un-escape first: `claude -p --output-format stream-json` embeds the
    # agent's messages as JSON strings, so a real `\n` may be the two
    # characters \n in the captured stream. Tags must then match anywhere
    # a line starts, not just at byte 0.
    text = text.replace("\\n", "\n").replace("\\t", "\t")
    return [m.group(1).strip() for m in re.finditer(rf"(?:^|\n){tag}:\s*(.+)", text)]


def agent_mode(fixture, env, ev, truth, args):
    verdicts = {"installs": None, "answers_correctly": None,
                "stays_updated": None, "helps_real_work": None}
    checks = []
    arm = args.arm

    def check(name, ok, got, expected):
        checks.append({"check": name, "ok": bool(ok), "expected": expected,
                       "got": (got or "")[:400]})

    if arm == "lain":
        # Stage A — onboarding: the AGENT installs and configures, docs-only.
        # The harness only observes; pre-installing here would be rescue.
        if args.stage in ("a", "all"):
            prompt = (
                "You are in a fresh git repository. Install the code-graph tool "
                "`lain` from npm (package @spuentesp/lain-mcp) following ONLY its "
                "public documentation (`npm info`, `lain --help`, README), then "
                "configure this project's MCP client for it (`lain setup --help`). "
                "Verify the install with one real query about this repository. "
                "Do not ask any human for help — if the documentation is missing "
                "something, record it yourself. When finished print "
                "`SETUP-DONE: <how>` or `SETUP-FAILED: <reason>`.\n"
            )
            p = run_agent(args.agent_cmd, prompt, fixture, env, ev,
                          os.path.join(args.work, "transcript-a.txt"))
            if agent_launch_failed(p):
                ev.notes.append(f"INVALID RUN: agent CLI failed to launch "
                                f"(rc={p.returncode}): {p.stderr[-300:]}")
                return {"installs": None, "answers_correctly": None,
                        "stays_updated": None, "helps_real_work": None}, checks
            lain = find_lain(args.work, env.get("PATH"))
            version = ev.cmd([lain, "--version"], env=env) if lain else None
            cfg_file = os.path.join(fixture, ".mcp.json")
            cfg_ok = os.path.isfile(cfg_file) or bool(
                re.search(r"mcp list|connected|verified", p.stdout, re.I))
            done = "SETUP-DONE" in p.stdout
            works = bool(lain and version and version.returncode == 0)
            verdicts["installs"] = bool(works and cfg_ok and done)
            check("agent installed and configured the client from public docs",
                  verdicts["installs"], p.stdout[-800:],
                  "SETUP-DONE + a working lain installed in the clean room "
                  "+ client-config evidence (.mcp.json or the client's MCP registry)")
            notes_file = os.path.join(args.work, "ONBOARDING-NOTES.md")
            if os.path.isfile(notes_file):
                ev.doc_gap("agent-recorded onboarding notes (no rescue used)",
                           open(notes_file).read()[:2000])
            if not verdicts["installs"]:
                ev.notes.append("ONBOARDING PROBLEM: agent could not install/configure "
                                "from public docs alone (see transcript-a.txt)")
                # A failed onboarding means there is no tool to do the task
                # with — report and stop honestly instead of crashing in stage B.
                return verdicts, checks
        else:
            # Stage B alone: nobody installed anything yet — the harness
            # installs so the task has a tool (the agent's own onboarding is
            # NOT under test in this stage; run --stage a or all for that).
            lain, version, desc = install_dist(args.dist, work=args.work,
                                               env=env, ev=ev)
            record_only(ev, f"stage b alone: harness-installed {desc}")
    else:
        lain = None  # control arm: nothing to install

    # Stage B — the real task: change a function used from several files.
    if args.stage in ("b", "all"):
        if arm == "lain":
            if args.stage in ("a", "all"):
                lain = find_lain(args.work, env.get("PATH"))
                if not lain:
                    raise RuntimeError("stage A finished but no lain is on PATH")
            env = install_shim(args.work, env, ev)
            prompt = open(TASK).read()
        else:
            prompt = open(TASK_NO_LAIN).read()
        p = run_agent(args.agent_cmd, prompt, fixture, env, ev,
                      os.path.join(args.work, "transcript-b.txt"))
        out = p.stdout
        if agent_launch_failed(p):
            ev.notes.append(f"INVALID RUN: agent CLI failed to launch "
                            f"(rc={p.returncode}): {p.stderr[-300:]}")
            return {"installs": verdicts.get("installs"), "answers_correctly": None,
                    "stays_updated": None, "helps_real_work": None}, checks

        t = ev.cmd(truth["fixture"]["test_command"], cwd=fixture)
        check("tests pass after the agent's change", t.returncode == 0,
              t.stdout + t.stderr, "unittest OK")

        affected = parse_tagged(out, "AFFECTED")
        named = set()
        for line in affected:
            if ":" in line:
                named.add(line.split(":", 1)[1].strip())
            else:
                named.add(line.strip())
        expected = {c["symbol"] for c in truth["pre_edit"]["direct_callers"]} | \
                   {c["symbol"] for c in truth["pre_edit"]["indirect_callers"]}
        missed = sorted(expected - named)
        decoys = {d["symbol"] for d in truth["pre_edit"]["decoys"]}
        false_pos = sorted(decoys & named)
        if missed:
            corpus = out.replace("\\n", "\n")
            prose_hits = sorted(s for s in expected if s in corpus)
            if prose_hits:
                record_only(ev, "analysis present but not protocol-formatted: the stream "
                                f"names {prose_hits} in prose/thinking, yet no AFFECTED: "
                                "lines were emitted — the claims are not machine-checkable")
        check("agent enumerated the affected before editing (AFFECTED lines)",
              len(affected) > 0, out[-1500:], "one AFFECTED line per consumer")
        check("no consumer forgotten", not missed, ",".join(sorted(named)),
              f"all of {sorted(expected)}")
        check("no decoy claimed as affected", not false_pos, ",".join(sorted(named)),
              f"none of {sorted(decoys)}")

        index_check = parse_tagged(out, "INDEX-CHECK")
        check("agent re-queried the index after the edit", bool(index_check),
              out[-1500:], "an INDEX-CHECK line")
        if arm == "lain":
            traffic = os.path.join(args.work, "lain-traffic.log")
            tools, cli = set(), set()
            if os.path.isfile(traffic):
                for line in open(traffic, errors="replace"):
                    if line.startswith("### ARGV"):
                        import ast
                        try:
                            argv = ast.literal_eval(line.split(" ", 2)[2].strip())
                        except (ValueError, SyntaxError, IndexError):
                            argv = []
                        for a in argv:
                            if isinstance(a, str) and a in (
                                    "oneshot", "status", "reindex", "doctor", "query",
                                    "schema", "init", "setup", "repos", "workspaces",
                                    "capabilities"):
                                cli.add(a)
                    elif '"tools/call"' in line:
                        mm = re.search(r'"name"\s*:\s*"([a-z_]+)"', line)
                        if mm:
                            tools.add(mm.group(1))
            record_only(ev, f"Lain tools the agent called via MCP: {sorted(tools) or 'NONE'}; "
                            f"via CLI: {sorted(cli) or 'NONE'}")
            if not tools and not cli:
                ev.notes.append("AGENT MADE NO LAIN QUERIES — Lain contributed nothing "
                                "to this solution; treat as a finding, not a pass")

            # Harness post-check, solution-agnostic: a fresh indexing pass
            # must see the change with NO staleness warning ("... modified
            # after it was last indexed"). The index follows commits — the
            # harness never commits for the agent; leaving the change
            # uncommitted is a real 'stays_updated' failure.
            post = run.Mcp(lain, fixture, env)
            try:
                post.wait_ready()
                probes = [post.call("get_call_sites", {"symbol": "normalize_token"})]
                if "def collapse_ws" in open(os.path.join(fixture, "core.py")).read():
                    probes.append(post.call("get_call_sites", {"symbol": "collapse_ws"}))
            finally:
                post.close()
            seen = "\n".join(probes)
            stale_warn = "after it was last indexed" in seen
            updated = (not stale_warn) and "normalize_token" in seen
            check("index reflects the agent's change (fresh session, no staleness warning)",
                  updated, seen, "callers found and no 'after it was last indexed' warning")
            recognized = bool(re.search(r"stale|commit|re-?index|reconnect|modified",
                                        " ".join(index_check), re.I))
            if not updated and not recognized:
                ev.notes.append("CLAIMED SUCCESS ON STALE INDEX: the agent reported done "
                                "while the index did not reflect its change and never "
                                "flagged staleness")
            elif not updated:
                ev.notes.append("agent recognized the staleness window (the index follows "
                                "commits + a new indexing pass) but left the change "
                                "uncommitted")
        else:
            updated = None

        verdicts["answers_correctly"] = not missed and not false_pos and len(affected) > 0
        verdicts["stays_updated"] = updated
        # Exploratory by construction: one run finds problems, it does not
        # prove the agent is better with Lain. Compare against --arm no-lain.
        verdicts["helps_real_work"] = (p.returncode == 0 and t.returncode == 0
                                       and not missed and "DONE" in out)

    return verdicts, checks


# ── report ──────────────────────────────────────────────────────────────────

def write_report(path, verdicts, checks, ev, meta):
    report = {
        "meta": meta,
        "verdicts": verdicts,
        "checks": checks,
        "commands": ev.commands,
        "doc_gaps": ev.doc_gaps,
        "notes": ev.notes,
    }
    with open(path, "w") as f:
        json.dump(report, f, indent=2)
    return report


def print_summary(report):
    v = report["verdicts"]
    print("\n=== acceptance results ===")
    for name, val in v.items():
        mark = {True: "PASS", False: "FAIL", None: "n/a"}[val]
        print(f"  {mark:>4}  {name}")
    for c in report["checks"]:
        print(f"  {'PASS' if c['ok'] else 'FAIL'}  {c['check']}")
        if not c["ok"]:
            print(f"        expected: {c['expected']}")
            print(f"        got:      {c['got'][:200]}")
    for g in report["doc_gaps"]:
        print(f"  DOC GAP (onboarding problem): {g['needed']}: {g['detail']}")
    for n in report["notes"]:
        print(f"  note: {n}")


# ── main ────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dist", default="npm",
                    help="npm | npm@X.Y.Z | dev  (dev = build this working tree "
                         "and install it via install.sh from a local artifact)")
    ap.add_argument("--mode", choices=["graph", "agent"], default="graph")
    ap.add_argument("--arm", choices=["lain", "no-lain"], default="lain",
                    help="agent mode only: with Lain, or the control arm without it")
    ap.add_argument("--stage", choices=["a", "b", "all"], default="all",
                    help="agent mode: a=agent installs/configures, b=the task, all=both")
    ap.add_argument("--agent-cmd",
                    default="claude -p --verbose --dangerously-skip-permissions "
                            "--output-format stream-json",
                    help="command that runs the coding agent; prompt is piped to "
                         "stdin. The default captures the full message stream "
                         "(stream-json) so protocol lines printed mid-run are "
                         "recorded, not just the final summary")
    ap.add_argument("--work", default=None,
                    help="work directory (default: fresh mktemp; always kept as evidence)")
    args = ap.parse_args()

    if args.mode == "agent" and args.dist == "dev" and args.stage in ("a", "all"):
        ap.error("--mode agent's onboarding stage tests what an outsider can "
                 "install from public docs: use --dist npm or npm@X.Y.Z there "
                 "(--dist dev is fine for --stage b feature runs; the report "
                 "records that the onboarding path was not under test)")

    import tempfile
    work = args.work or tempfile.mkdtemp(prefix="lain-onboarding-")
    os.makedirs(work, exist_ok=True)
    args.work = work

    ev = Evidence()
    truth = json.load(open(GROUND_TRUTH))
    env = clean_env(work, ev)

    fixture = os.path.join(work, "fixture")
    ev.cmd([sys.executable, FIXTURE_GEN, "build", fixture])

    meta = {
        "dist": args.dist,
        "mode": args.mode,
        "arm": args.arm if args.mode == "agent" else None,
        "os": platform.platform(),
        "work": work,
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "exploratory": args.mode == "agent",
    }

    exit_code = 1
    try:
        if args.mode == "graph":
            lain, version, desc = install_dist(args.dist, work, env, ev)
            meta["installed_version"] = version
            meta["dist_description"] = desc
            meta["client_setup"] = configure_client(lain, fixture, env, ev)
            verdicts, checks = graph_mode(lain, fixture, env, ev, truth)
            verdicts["installs"] = True
        else:
            # Agent mode: stage A has the agent do its own onboarding — a
            # harness pre-install would be rescue. Give it a scratch npm
            # prefix on PATH so `npm install -g` lands in the clean room.
            prefix = os.path.join(work, "prefix")
            os.makedirs(os.path.join(prefix, "bin"), exist_ok=True)
            env["npm_config_prefix"] = prefix
            env["PATH"] = os.path.join(prefix, "bin") + os.pathsep + env["PATH"]
            # The agent CLI needs its own login (real HOME). LAIN's config and
            # state stay isolated: clean_env points XDG_CONFIG_HOME/XDG_STATE_HOME
            # at the scratch dirs, so no inherited LAIN state can leak in.
            env["HOME"] = os.environ.get("HOME", env["HOME"])
            meta["dist_description"] = (
                "control arm: no code-graph tool installed" if args.arm == "no-lain"
                else "the agent installs it (stage A); harness observes")
            verdicts, checks = agent_mode(fixture, env, ev, truth, args)
            meta["installed_version"] = None
            if args.arm == "lain":
                lain = find_lain(work, env["PATH"])
                if lain:
                    v = ev.cmd([lain, "--version"], env=env)
                    meta["installed_version"] = v.stdout.strip()
                if verdicts.get("installs") is None:  # stage b alone
                    verdicts["installs"] = bool(lain)

        report = write_report(os.path.join(work, "report.json"),
                              verdicts, checks, ev, meta)
        print_summary(report)
        if meta["exploratory"]:
            print("  (helps_real_work is EXPLORATORY: one run finds problems, "
                  "it does not prove improvement — compare with --arm no-lain)")
        required = [v for v in verdicts.values() if isinstance(v, bool)]
        exit_code = 0 if required and all(required) else 1
    finally:
        print(f"\nwork dir (evidence): {work}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
