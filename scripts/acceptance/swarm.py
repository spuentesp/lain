#!/usr/bin/env python3
"""3-agent swarm deep-chain experiment runner.

Runs ONE ARM per invocation; the experiment runs this script twice:

    python3 scripts/acceptance/swarm.py --arm lain   --work /tmp/swarm-lain
    python3 scripts/acceptance/swarm.py --arm no-lain --work /tmp/swarm-nolain

Phases (briefs in tests/fixtures/swarm_deepchain/briefs/):
  1  ISOLATION — alpha/beta/gamma each get a FRESH fixture build in
     <work>/p1/<agent>/fx and analyse independently (00-scenario + 01-analysis).
  2  SHARED    — all three work concurrently in <work>/shared/fx
     (00-scenario + 02-consensus; the {claims_instructions} placeholder is
     filled per arm so the claims protocol differs between arms).
  3  SHARED    — cross-review round-robin in the same dir, post-phase-2 state
     (00-scenario + 03-review: alpha→beta, beta→gamma, gamma→alpha).

Within a wave the three agents launch CONCURRENTLY (prompt on stdin, same
run_agent machinery as onboarding.py); the next wave starts only after all
three finish. Every invocation records rc, wall duration, transcript path
and a launch-failure flag — a launch failure is printed loudly and makes
the run exit non-zero; it can never silently pass.

LAIN arm (once per run, mirroring onboarding agent_mode's stage-b-alone
path): install_dist(dist) + install_shim(), then configure_client()
(`lain setup` -> .mcp.json) per workspace copy — the `lain` prefix is
shared across copies, PATH puts it first and is scrubbed of any
pre-existing lain by onboarding.clean_env. no-lain arm: no install.

--dry-run: skips install_dist/install_shim/configure_client and replaces
--agent-cmd with a generated stub (<work>/stub-agent.py) that performs the
scripted phase actions, so the FULL flow — waves, transcripts,
report.json, metrics — runs end to end without a real coding agent.

Output: <work>/report.json
  {arm, dist, agent_cmd, started_at, finished_at, fixture_sha, repo_sha,
   host, dry_run, invocations[phase,agent,rc,duration_s,transcript,
   launch_failed] x9, metrics}
Exit codes: 0 clean; 1 a launch failure was recorded; 2 harness error;
3 metrics failed.
"""
import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import threading
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

import onboarding  # noqa: E402
import swarm_metrics  # noqa: E402

FIXTURE_DIR = os.path.join(REPO_ROOT, "tests", "fixtures", "swarm_deepchain")
MAKE_TASK = os.path.join(FIXTURE_DIR, "make_task.py")
BRIEFS = os.path.join(FIXTURE_DIR, "briefs")

AGENTS = ("alpha", "beta", "gamma")
REVIEWEE = {"alpha": "beta", "beta": "gamma", "gamma": "alpha"}

DEFAULT_AGENT_CMD = ("claude -p --verbose --dangerously-skip-permissions "
                     "--output-format stream-json")

CLAIMS_INSTRUCTIONS = {
    "lain": ("Lain is configured here: `.mcp.json` launches "
             "`lain server --config ./repos.yaml` (the federation server), so "
             "the Lain MCP tools (trace_impact, get_contract, claim_files, ...) "
             "are live. Use `lain impact --format claims <symbol>` or the MCP "
             "impact tools for evidence, and register the files you are about "
             "to edit with the Lain `claim_files` MCP tool so the other agents "
             "can see the claim."),
    "no-lain": ("No code-graph tooling exists in this arm. Use grep, git and "
                "code reading. Write the files you are about to edit to "
                "`consensus/claims-{agent}.md` before editing."),
}

TOOLING_NOTE = (
    "\n\nTooling note (this arm): lain is installed and configured in this "
    "workspace via `.mcp.json` — it launches `lain server --config "
    "./repos.yaml`, the federation server over all repos in `repos.yaml`. "
    "Use the lain MCP tools (trace_impact, get_contract, claim_files, ...) "
    "and `lain impact --format claims <symbol>` for impact evidence, and "
    "register file claims with the `claim_files` MCP tool so your claim is "
    "visible to the other agents.\n")


# ── dry-run stub ────────────────────────────────────────────────────────────

STUB = r'''#!/usr/bin/env python3
"""Dry-run stub agent for scripts/acceptance/swarm.py.

Reads the brief prompt on stdin, infers the agent from the first
'agent **X**' marker and the phase from the '# Phase N' header, then
performs the minimal scripted actions swarm_metrics expects:
  phase 1  consensus/private-<agent>.md  (canned AFFECTED lines + DONE)
  phase 2  consensus/positions-<agent>.md; consensus/consensus.md written
           exactly once (O_CREAT|O_EXCL, so only one stub wins); one source
           edit in the assigned repo (alpha->core, beta->api, gamma->billing)
           committed with git commit --author "<agent> <agent>@swarm.local"
  phase 3  consensus/review-<agent>.md    (verdict lines + DONE)
Prints one compact stream-json-like event so token/turn metrics are real.
"""
import json
import os
import re
import subprocess
import sys

prompt = sys.stdin.read()
m = re.search(r"agent \*\*(\w+)\*\*", prompt)
agent = m.group(1) if m else "unknown"
ph = re.findall(r"# Phase (\d)", prompt) or re.findall(r"Phase (\d)", prompt)
phase = int(ph[0]) if ph else 0

CANNED = (
    "AFFECTED: api:src/forward.ts:forward_order EVIDENCE: verified"
    "  reason: forwards core's customer_id field on the wire\n"
    "AFFECTED: cli:src/show.ts:show_order EVIDENCE: verified"
    "  reason: reads resp.customer_id from core's response\n"
    "AFFECTED: billing:src/main.py:build_invoice EVIDENCE: verified"
    "  reason: re-emits the field read from api's response\n"
    "AFFECTED: notify:src/notify.py:send_notice EVIDENCE: needs-investigation"
    "  reason: dynamic key resp.get('customer_' + suffix); contract chain\n"
    "AFFECTED: external:partner-portal EVIDENCE: missing"
    "  reason: declared in core's openapi.yaml, code outside the workspace\n"
    "AFFECTED: reporting:src/report_invoice.py:build_invoice EVIDENCE: verified"
    "  reason: homonym in the reporting repo — not affected by the rename\n"
)
EDITS = {"alpha": ("core", "src/models.rs", "// "),
         "beta": ("api", "src/forward.ts", "// "),
         "gamma": ("billing", "src/main.py", "# ")}

if agent == "unknown" or phase not in (1, 2, 3):
    print(f"stub: cannot infer agent/phase (agent={agent} phase={phase})",
          file=sys.stderr)
    sys.exit(3)

os.makedirs("consensus", exist_ok=True)
if phase == 1:
    with open("consensus/private-%s.md" % agent, "w") as f:
        f.write("# private analysis of %s\n\n" % agent + CANNED + "DONE\n")
elif phase == 2:
    with open("consensus/positions-%s.md" % agent, "w") as f:
        f.write("# positions of %s\n\n" % agent + CANNED)
    try:
        fd = os.open("consensus/consensus.md",
                     os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
    except FileExistsError:
        fd = None
    if fd is not None:
        with os.fdopen(fd, "w") as f:
            f.write("# consensus (first writer: %s)\n\n" % agent + CANNED +
                    "\nINDEX-CHECK: re-read the committed diff (stub)\n\n"
                    "## disagreements:\n"
                    "- notify evidence: alpha says verified, "
                    "beta says needs-investigation (stub)\n"
                    "## done\nDONE\n")
    repo, rel, comment = EDITS[agent]
    with open(os.path.join(repo, rel), "a") as f:
        f.write("%sstub edit by %s (swarm dry-run)\n" % (comment, agent))
    env = dict(os.environ)
    env.update(GIT_AUTHOR_NAME=agent, GIT_AUTHOR_EMAIL="%s@swarm.local" % agent,
               GIT_COMMITTER_NAME=agent,
               GIT_COMMITTER_EMAIL="%s@swarm.local" % agent)
    subprocess.run(["git", "-C", repo, "add", rel], check=True, env=env)
    subprocess.run(["git", "-C", repo, "-c", "commit.gpgsign=false",
                    "commit", "--author", "%s <%s@swarm.local>" % (agent, agent),
                    "-m", "%s: stub dry-run edit" % agent],
                   check=True, env=env)
else:  # phase 3
    m2 = re.search(r"Review the work of agent \*\*(\w+)\*\*", prompt)
    reviewee = m2.group(1) if m2 else "?"
    with open("consensus/review-%s.md" % agent, "w") as f:
        f.write("# review of %s by %s\n\n" % (reviewee, agent) +
                "VERDICT: verified — api:src/forward.ts:forward_order"
                " (call site named)\n"
                "VERDICT: refuted — reporting:src/report_invoice.py:"
                "build_invoice (homonym trap)\n"
                "VERDICT: unevidenced — catalog:src/catalog.py:summarize"
                " (no check performed)\n"
                "MISSED: external:partner-portal (never listed by the"
                " reviewee)\n"
                "OUT-OF-SCOPE: tests/test_forward_contract.py edited by the"
                " reviewee\n"
                "DONE\n")

print(json.dumps(
    {"type": "assistant",
     "message": {"content": [{"type": "text",
                              "text": "stub phase %d agent %s" % (phase, agent)}]},
     "usage": {"input_tokens": 12, "output_tokens": 6,
               "cache_read_input_tokens": 3},
     "duration_ms": 250}, separators=(",", ":")))
print("STUB-DONE phase=%d agent=%s" % (phase, agent))
'''


# ── helpers ─────────────────────────────────────────────────────────────────

def _brief(name):
    with open(os.path.join(BRIEFS, name)) as f:
        return f.read()


def build_env(work, ev):
    """clean_env + the agent-mode PATH/HOME from onboarding.main: scrub any
    pre-existing `lain` from PATH, put the scratch prefix first, real HOME
    for the agent CLI (LAIN's own config/state stay in the XDG clean room)."""
    env = onboarding.clean_env(work, ev)
    prefix = os.path.join(work, "prefix")
    os.makedirs(os.path.join(prefix, "bin"), exist_ok=True)
    env["npm_config_prefix"] = prefix
    env["PATH"] = (os.path.join(prefix, "bin") + os.pathsep + prefix
                   + os.pathsep + env["PATH"])
    env["HOME"] = os.environ.get("HOME", env["HOME"])
    return env


def write_stub(work):
    path = os.path.join(work, "stub-agent.py")
    with open(path, "w") as f:
        f.write(STUB)
    os.chmod(path, 0o755)
    return f"{sys.executable} {path}"


def _build_fixture(ev, dest):
    if os.path.isdir(dest) and os.listdir(dest):
        onboarding.record_only(ev, f"re-run: removed previous build at {dest}")
        shutil.rmtree(dest)
    p = ev.cmd([sys.executable, MAKE_TASK, "build", dest])
    if p.returncode != 0:
        raise RuntimeError(
            f"fixture build failed for {dest}: {p.stdout[-500:]} {p.stderr[-500:]}")


def _configure(lain, fixture, env, ev, label):
    res = onboarding.configure_client(lain, fixture, env, ev)
    onboarding.record_only(ev, f"configure_client({label}): {res}")
    if not res.get("config_written"):
        # Without .mcp.json the LAIN arm silently degrades to a control arm
        # (observed 2026-10-11: agents then report 'CONNECTION_CLOSED' and
        # fall back to grep). That is an INVALID run, not a warning.
        raise RuntimeError(
            f"INVALID RUN: .mcp.json not written for {label}: {res}")
    return res


def fixture_sha():
    """Content hash of the fixture sources (the fixture is untracked in git,
    so there is no commit sha to record — a content hash is the honest id)."""
    h = hashlib.sha256()
    for rel in ("make_task.py", "ground_truth.json",
                os.path.join("briefs", "00-scenario.md"),
                os.path.join("briefs", "01-analysis.md"),
                os.path.join("briefs", "02-consensus.md"),
                os.path.join("briefs", "03-review.md")):
        h.update(rel.encode())
        h.update(b"\0")
        with open(os.path.join(FIXTURE_DIR, rel), "rb") as f:
            h.update(f.read())
        h.update(b"\0")
    return h.hexdigest()


def repo_sha():
    p = subprocess.run(["git", "-C", REPO_ROOT, "rev-parse", "HEAD"],
                       capture_output=True, text=True)
    return p.stdout.strip() if p.returncode == 0 else None


# ── prompts ─────────────────────────────────────────────────────────────────

def prompt_p1(arm, agent):
    text = _brief("00-scenario.md") + "\n\n" + _brief("01-analysis.md")
    text = text.replace("{agent}", agent)
    if arm == "lain":
        text += TOOLING_NOTE
    return text


def prompt_p2(arm, agent):
    text = _brief("00-scenario.md") + "\n\n" + _brief("02-consensus.md")
    # claims first: the no-lain text itself contains {agent}
    return (text.replace("{claims_instructions}", CLAIMS_INSTRUCTIONS[arm])
                .replace("{agent}", agent))


def prompt_p3(arm, agent):
    text = _brief("00-scenario.md") + "\n\n" + _brief("03-review.md")
    return (text.replace("{agent}", agent)
                .replace("{reviewee}", REVIEWEE[agent]))


# ── waves ───────────────────────────────────────────────────────────────────

def run_wave(phase, agent_cmd, jobs, env, ev, invocations):
    """Launch the wave's 3 agents CONCURRENTLY, then record every result."""
    print(f"--- wave (phase {phase}): launching "
          f"{', '.join(j[0] for j in jobs)} concurrently", flush=True)
    results = {}

    def one(agent, prompt, cwd, transcript):
        t0 = time.time()
        try:
            p = onboarding.run_agent(agent_cmd, prompt, cwd, env, ev, transcript)
        except subprocess.TimeoutExpired:
            p = subprocess.CompletedProcess(
                [agent_cmd], 124,
                "HARNESS: agent invocation timed out after 1800s\n", "")
        except Exception as e:  # harness-side failure: never silent
            p = subprocess.CompletedProcess([agent_cmd], 125, "",
                                            f"harness error: {e}")
        if not os.path.isfile(transcript):
            with open(transcript, "w") as f:
                f.write(p.stdout or "")
                f.write("\n--- stderr ---\n")
                f.write(p.stderr or "")
        results[agent] = (p, round(time.time() - t0, 1))

    threads = [threading.Thread(target=one, args=job) for job in jobs]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    for agent, prompt, cwd, transcript in jobs:
        p, dur = results[agent]
        failed = onboarding.agent_launch_failed(p)
        invocations.append({"phase": phase, "agent": agent, "rc": p.returncode,
                            "duration_s": dur, "transcript": transcript,
                            "launch_failed": failed})
        status = ("LAUNCH-FAILURE" if failed
                  else "ok" if p.returncode == 0 else f"rc={p.returncode}")
        print(f"    phase {phase} {agent}: {status} in {dur}s -> {transcript}",
              flush=True)
        if failed:
            print(f"!!! INVALID RUN: agent CLI failed to launch "
                  f"(rc={p.returncode}) phase {phase} agent {agent}: "
                  f"{(p.stderr or '')[-300:]}", file=sys.stderr, flush=True)


# ── report ──────────────────────────────────────────────────────────────────

def print_summary(report):
    print(f"\n=== swarm arm={report['arm']} dry_run={report['dry_run']} "
          f"dist={report['dist']} ===")
    for i in report["invocations"]:
        mark = "LAUNCH-FAILURE" if i["launch_failed"] else f"rc={i['rc']}"
        print(f"  phase {i['phase']} {i['agent']:<6} {mark} "
              f"{i['duration_s']}s")
    m = report.get("metrics") or {}
    for agent in AGENTS:
        rec = (m.get("p1_scores") or {}).get(agent) or {}
        harm = ((rec.get("ground_truth_score") or {}).get("harm")) if rec.get("exists") else None
        print(f"  p1 {agent}: exists={rec.get('exists')} "
              f"claims={rec.get('claim_count', 0)} harm={harm}")
    p2 = m.get("p2_scores") or {}
    gt2 = p2.get("ground_truth_score") or {}
    pa = p2.get("positions_agreement") or {}
    print(f"  p2 consensus: exists={p2.get('exists')} "
          f"claims={p2.get('claim_count')} harm={gt2.get('harm')} "
          f"disagreements={p2.get('disagreements_count')} "
          f"positions_agree={pa.get('agree')}")
    c = m.get("clobbering") or {}
    print(f"  clobbering files_by_agent: {c.get('files_by_agent')}")
    print(f"  clobbering: overlapping={c.get('overlapping_files')} "
          f"trap/tests={c.get('trap_or_tests_edits')} "
          f"unattributed={c.get('unattributed_commits')} "
          f"uncommitted={c.get('repos_with_uncommitted_work')}")
    for agent in AGENTS:
        rec = (m.get("p3_reviews") or {}).get(agent) or {}
        if rec.get("exists"):
            print(f"  p3 {agent}: verdicts={rec.get('verdicts')} "
                  f"misses={rec.get('misses_reported')} "
                  f"scope={rec.get('scope_violations_reported')}")
    print(f"  cost per_phase: {(m.get('cost') or {}).get('per_phase')}")
    print(f"  cost per_arm: {(m.get('cost') or {}).get('per_arm')}")


# ── main ────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arm", choices=["lain", "no-lain"], required=True,
                    help="one arm per run; the experiment runs this script twice")
    ap.add_argument("--work", required=True,
                    help="work directory (report.json + all workspace copies)")
    ap.add_argument("--dist", default="dev",
                    help="lain dist to install (lain arm, non-dry-run only)")
    ap.add_argument("--agent-cmd", default=DEFAULT_AGENT_CMD,
                    help="command that runs the coding agent; prompt is piped "
                         "to stdin (overridden by the stub in --dry-run)")
    ap.add_argument("--dry-run", action="store_true",
                    help="skip install/shim/configure; use a generated stub "
                         "agent and exercise the full flow end to end")
    ap.add_argument("--phases", default="1,2,3",
                    help="comma-separated subset of waves to run (default 1,2,3)")
    args = ap.parse_args()

    try:
        phases = {int(x) for x in args.phases.split(",") if x.strip()}
    except ValueError:
        ap.error(f"bad --phases: {args.phases}")
    if not phases or not phases <= {1, 2, 3}:
        ap.error("--phases must be a non-empty subset of 1,2,3")

    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    started = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    ev = onboarding.Evidence()
    env = build_env(work, ev)
    agent_cmd = args.agent_cmd
    if args.dry_run:
        agent_cmd = write_stub(work)

    print(f"swarm: arm={args.arm} work={work} dist={args.dist} "
          f"phases={sorted(phases)} dry_run={args.dry_run}", flush=True)

    invocations = []
    exit_code = 0
    try:
        # LAIN arm only, once per run: install + traffic shim (the
        # stage-b-alone path of onboarding.agent_mode). Shim BEFORE any
        # `lain setup` so .mcp.json never points at a not-yet-wrapped binary.
        lain = None
        if args.arm == "lain" and not args.dry_run:
            lain, version, desc = onboarding.install_dist(
                args.dist, work=work, env=env, ev=ev)
            onboarding.record_only(ev, f"harness-installed {desc} "
                                       f"(reported version {version})")
            env = onboarding.install_shim(work, env, ev)
        elif args.arm == "lain":
            onboarding.record_only(ev, "dry-run: install_dist/install_shim "
                                       "skipped; no .mcp.json configured")

        # Workspaces: fresh fixture build per phase-1 agent (isolation),
        # one shared build for phases 2/3.
        if 1 in phases:
            for agent in AGENTS:
                fx = os.path.join(work, "p1", agent, "fx")
                _build_fixture(ev, fx)
                if lain:
                    _configure(lain, fx, env, ev, f"p1/{agent}")
        if phases & {2, 3}:
            shared = os.path.join(work, "shared", "fx")
            _build_fixture(ev, shared)
            if lain:
                _configure(lain, shared, env, ev, "shared")

        # Waves — concurrent inside, sequential across.
        if 1 in phases:
            run_wave(1, agent_cmd, [
                (a, prompt_p1(args.arm, a),
                 os.path.join(work, "p1", a, "fx"),
                 os.path.join(work, "p1", a, "transcript.txt"))
                for a in AGENTS], env, ev, invocations)
        if 2 in phases:
            shared = os.path.join(work, "shared", "fx")
            run_wave(2, agent_cmd, [
                (a, prompt_p2(args.arm, a), shared,
                 os.path.join(work, "shared", f"transcript-p2-{a}.txt"))
                for a in AGENTS], env, ev, invocations)
        if 3 in phases:
            shared = os.path.join(work, "shared", "fx")
            run_wave(3, agent_cmd, [
                (a, prompt_p3(args.arm, a), shared,
                 os.path.join(work, "shared", f"transcript-p3-{a}.txt"))
                for a in AGENTS], env, ev, invocations)
    except Exception:
        traceback.print_exc()
        print("SWARM RUN FAILED (harness error) — report written with the "
              "partial run below", file=sys.stderr, flush=True)
        exit_code = 2

    # Metrics (File 2), merged into report.json under "metrics".
    metrics = {}
    try:
        metrics = swarm_metrics.collect(work, invocations=invocations,
                                        arm=args.arm)
    except Exception:
        traceback.print_exc()
        print("SWARM METRICS FAILED", file=sys.stderr, flush=True)
        exit_code = exit_code or 3

    report = {
        "arm": args.arm,
        "dist": args.dist,
        "agent_cmd": agent_cmd,
        "started_at": started,
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "fixture_sha": fixture_sha(),
        "repo_sha": repo_sha(),
        "host": platform.node(),
        "dry_run": args.dry_run,
        "invocations": invocations,
        "metrics": metrics,
    }
    with open(os.path.join(work, "report.json"), "w") as f:
        json.dump(report, f, indent=2)

    print_summary(report)
    failed = [i for i in invocations if i["launch_failed"]]
    if failed:
        print(f"\n!!! {len(failed)} LAUNCH FAILURE(S) recorded in "
              f"{os.path.join(work, 'report.json')}: "
              f"{[(i['phase'], i['agent'], i['rc']) for i in failed]}",
              file=sys.stderr, flush=True)
        exit_code = exit_code or 1
    print(f"\nwork dir (evidence): {work}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
