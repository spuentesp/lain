#!/usr/bin/env python3
"""Cross-repo contract task acceptance (test 3 of the program).

A contract change between a service and its consumer: `orders` renames the
response field `customer_id` -> `customerId`, `billing` must follow. The
test checks the four outcomes already established for tests 1-2 (installs /
answers correctly / stays updated / helps real work) plus the one that makes
this test different:

    5. evidence classes — the federation (and the agent) DISTINGUISH
       relationships that are proven (static Binds/ReadsField, impact
       Verified) from ones that are flagged but unproven
       (NeedsInvestigation) and from information that is simply not in the
       graph (coverage.complete=false, external hosts). Presenting inferred
       or absent evidence as verified is the serious failure.

The oracle lives in tests/fixtures/contract_task/ground_truth.json —
declared BEFORE any query. Empty or stale answers are failures, never
"no risk". The fixture base is the deterministic 4-repo
scripts/contracts-fixture.sh (offline), which already carries the
provider-side scenario as the tag `s11-rename-field`.

--fixture selects which experiment the runner operates on:
    contract (default)  the two-repo contract change above
    scale               tests/fixtures/scale_task — the six-repo
                        homonym/near-miss/unresolvable-consumer
                        experiment (the program's decisive A/B); its
                        oracle is scale_task/ground_truth.json

Modes (same contract as scripts/acceptance/onboarding.py):
    --mode graph   "does the federation work?" — scripted queries against
                   the pre-declared truth, scripted change, diff verdict.
                   For --fixture scale this is the structural lane:
                   fixture builds, oracle red -> green, and the
                   ground-truth decoys stay out of blast-radius answers.
    --mode agent   "does it help an agent?" — a real coding agent gets the
                   task; --arm lain|no-lain is the exploratory A/B.
"""
import argparse
import json
import os
import platform
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import onboarding as ob  # noqa: E402  (clean room, install, agent runner, report)

FIXTURE_DIRS = {
    "contract": os.path.join(REPO_ROOT, "tests", "fixtures", "contract_task"),
    "scale": os.path.join(REPO_ROOT, "tests", "fixtures", "scale_task"),
}


def fixture_paths(fixture):
    """{ground_truth, task_gen, task, task_no_lain} for --fixture."""
    d = FIXTURE_DIRS[fixture]
    return {
        "dir": d,
        "ground_truth": os.path.join(d, "ground_truth.json"),
        "task_gen": os.path.join(d, "make_task.py"),
        "task": os.path.join(d, "task.md"),
        "task_no_lain": os.path.join(d, "task-no-lain.md"),
    }



# ── federation server + contract tools over HTTP ────────────────────────────

def mcp_call(url, tool, args):
    req = urllib.request.Request(
        url + "/mcp",
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": tool, "arguments": args}}).encode(),
        headers={"Content-Type": "application/json"})
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120))
    except Exception as e:  # noqa: BLE001  (transport errors are evidence)
        return f"TRANSPORT ERROR: {e}"
    if "error" in r:
        return "RPC ERROR: " + json.dumps(r["error"])
    return "".join(c.get("text", "") for c in r.get("result", {}).get("content", []))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_server(lain, fx, env, ev, log_path):
    port = free_port()
    log = open(log_path, "w")
    proc = subprocess.Popen(
        [lain, "server", "--config", os.path.join(fx, "repos.yaml"),
         "--transport", "http", "--port", str(port)],
        stdout=log, stderr=subprocess.STDOUT, env=env, cwd=fx)
    url = f"http://127.0.0.1:{port}"
    deadline = time.time() + 120
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"lain server exited early; see {log_path}")
        if mcp_call(url, "get_health", {}).find("Error") < 0:
            return proc, url
        time.sleep(2)
    raise RuntimeError(f"lain server never answered on {url}; see {log_path}")


def wait_federation_ready(url, repos, timeout=600):
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = mcp_call(url, "get_federation_health", {})
        if f'"ready":{repos}' in last.replace(" ", ""):
            return last
        time.sleep(3)
    raise RuntimeError(f"federation never reached ready:{repos}; last: {last[:300]}")


def repos_map(fx, ref):
    """{repo_id: ref} for prepare_snapshot, read from the fixture's repos.yaml."""
    text = open(os.path.join(fx, "repos.yaml")).read()
    return {rid: ref for rid in re.findall(r"- id: (\S+)", text)}


def prepare_snapshot(url, ev, repos, **args):
    args.setdefault("wait_ms", 60000)
    args["repos"] = repos
    raw = ""
    for _ in range(30):
        raw = mcp_call(url, "prepare_snapshot", args)
        try:
            data = json.loads(raw).get("data", {})
        except ValueError:
            data = {}
        state, snap = data.get("state"), data.get("snapshot")
        if state == "ready" and snap:
            return snap, raw
        if state not in ("pending", "indexing", None):
            return None, raw
        time.sleep(3)
    return None, raw


# ── graph mode: does the federation work? ───────────────────────────────────

def head_commits(fx, repos=("orders", "billing")):
    out = {}
    for repo in repos:
        p = subprocess.run(["git", "-C", os.path.join(fx, repo), "rev-parse", "HEAD"],
                           capture_output=True, text=True)
        if p.returncode == 0:
            out[repo] = p.stdout.strip()
    return out


def indexed_commits(url):
    """{repo_id: last_indexed_commit} from get_capabilities."""
    text = mcp_call(url, "get_capabilities", {})
    return dict(re.findall(
        r'"id":"(\w+)".{0,400}?"last_indexed_commit":"(\w+)"', text))


def graph_mode(lain, fx, env, ev, truth, task_gen):
    verdicts = {"answers_correctly": None, "stays_updated": None,
                "distinguishes_evidence": None}
    checks = []

    def check(name, ok, got, expected):
        checks.append({"check": name, "ok": bool(ok), "expected": expected,
                       "got": (got or "")[:400]})

    server, url = start_server(lain, fx, env, ev, os.path.join(ev.work, "server.log"))
    try:
        wait_federation_ready(url, 4)
        # Ref-pinned snapshots: `base` is the immutable pre-change contract,
        # `main` moves when the change is committed. workspace_dir sources
        # honor the ref (verified empirically), so both snapshots coexist on
        # one server and diff_contracts can compare them.
        snap, raw = prepare_snapshot(url, ev, repos=repos_map(fx, "base"))
        check("contract snapshot reaches ready", bool(snap), raw, "state=ready with a snapshot id")

        pre = truth["pre_edit"]
        contract = mcp_call(url, "get_contract",
                            {"snapshot": snap, "service": "orders", "repo_id": "orders",
                             "key": pre["contract_state"]["key"]})
        check(f"contract shows response field {pre['contract_state']['response_field']}",
              pre["contract_state"]["response_field"] in contract, contract,
              pre["contract_state"]["response_field"])

        svc = mcp_call(url, "get_service", {"snapshot": snap, "service": "orders", "depth": 2})
        # Consumers surface as HttpClientCall nodes (the call site), the
        # bind is what must be proven: static kind, from billing's code.
        check("orders' consumers include billing (verified static bind)",
              "billing" in svc and "src/main.py" in svc and "static" in svc, svc,
              "billing consumer with a static binding from src/main.py")
        for trap in truth["pre_edit"]["false_positive_traps"]:
            if trap["repo"] == "reports" or trap["repo"] == "platform":
                check(f"trap {trap['symbol']} is not a consumer of orders",
                      trap["symbol"] not in svc, svc, f"absent: {trap['symbol']}")

        cov = mcp_call(url, "get_coverage", {"snapshot": snap})
        m = re.search(r'"complete"\s*:\s*(true|false)', cov)
        check("coverage admits incompleteness (missing info stays visible)",
              m and m.group(1) == "false", cov, '"complete": false')

        unres = mcp_call(url, "list_unresolved", {"snapshot": snap, "limit": 20})
        check("external stripe host is never listed as unresolved",
              "stripe" not in unres.lower(), unres, "no stripe entries")

        verdicts["answers_correctly"] = all(c["ok"] for c in checks)

        # ── scripted change (graph mode): provider scenario + consumer fix ──
        ev.cmd([sys.executable, task_gen, "apply-edit", fx])
        tests_ok = True
        for repo, argv in truth["fixture"]["test_commands"].items():
            t = ev.cmd(argv, cwd=os.path.join(fx, repo))
            check(f"tests pass in {repo} after the change", t.returncode == 0,
                  t.stdout + t.stderr, "unittest OK")
            tests_ok = tests_ok and t.returncode == 0

        # Commit both — the change record. Then two federation questions,
        # which have different answers (verified experimentally):
        #  (a) does the contract layer deliver the cross-repo verdict for
        #      this change? — proven over the fixture's own immutable
        #      scenario refs (orders@s11-rename-field vs @base), the exact
        #      setup of tests/fixtures/contracts/ground_truth.yaml #11;
        #  (b) does the federation see NEW commits in workspace_dir
        #      sources? — this is where the product falls short today
        #      (finding recorded below).
        for repo in ("orders", "billing"):
            ev.cmd(["git", "add", "-A"], cwd=os.path.join(fx, repo))
            ev.cmd(["git", "-c", "user.email=contract-task@lain.local",
                    "-c", "user.name=contract-task", "commit", "-q", "-m",
                    "rename response field customer_id -> customerId"], cwd=os.path.join(fx, repo))

        # (a) cross-repo verdict over the fixture's scenario-11 refs.
        head_refs = repos_map(fx, "base")
        head_refs["orders"] = "s11-rename-field"
        snap_b, _ = prepare_snapshot(url, ev, repos=repos_map(fx, "base"))
        snap_h, _ = prepare_snapshot(url, ev, repos=head_refs)
        diff = mcp_call(url, "diff_contracts", {"base": snap_b, "head": snap_h})
        d = truth["post_edit"]["diff_contract"]
        diff_ok = all(s in diff for s in d["must_include"]) and d["impact_class"] in diff \
            and d["affected_service"] in diff
        check("diff_contracts reports FieldRenamed with Verified impact on billing",
              diff_ok, diff,
              f"{d['must_include']} + {d['impact_class']} + {d['affected_service']}")
        verdicts["distinguishes_evidence"] = diff_ok

        # (b) "the index reflects the change" has two layers, with
        # different realities (each verified):
        #  (b1) symbol/graph layer — follows commits; a new indexing pass
        #       (fresh server / lain reindex) lands on the change's HEADs;
        #  (b2) contract snapshot layer — frozen for workspace_dir sources
        #       (the product finding recorded above).
        heads = head_commits(fx)
        server.kill()
        server.wait(timeout=30)
        server, url = start_server(lain, fx, env, ev, os.path.join(ev.work, "server2.log"))
        wait_federation_ready(url, 4)
        indexed = indexed_commits(url)
        b1 = bool(heads) and all(indexed.get(r) == h for r, h in heads.items())
        check("symbol layer follows the committed change (index at the change HEADs)",
              b1, f"indexed={indexed} heads={heads}",
              "last_indexed_commit == HEAD per changed repo")

        snap2, raw2 = prepare_snapshot(url, ev, repos=repos_map(fx, "main"))
        post = truth["post_edit"]["tool_reflects"]
        contract2 = (mcp_call(url, "get_contract",
                              {"snapshot": snap2, "service": "orders", "repo_id": "orders",
                               "key": post["key"]}) if snap2
                     else f"SNAPSHOT UNAVAILABLE: {raw2[:150]}")
        paths = re.findall(r'"json_path":\s*"([^"]+)"', contract2)
        b2 = post["response_field"] in paths and post["response_field_absent"] not in paths
        check("contract layer follows the committed change (workspace_dir sources)",
              b2, f"json_paths: {paths} raw: {contract2[:150]}",
              f"{post['response_field']} present, {post['response_field_absent']} absent")
        if not b2:
            ob.record_only(ev, "STALE CONTRACT LAYER (product finding): "
                           + truth["post_edit"]["stale_would_look_like"])
        verdicts["stays_updated"] = b1 and b2
    finally:
        server.kill()
        try:
            server.wait(timeout=30)
        except Exception:
            pass
    return verdicts, checks


# ── scale fixture graph mode: structural checks for the experiment ──────────

SCALE_REPOS = ("orders", "gateway", "billing", "reporting", "catalog", "analytics")
# Repos whose oracle tests must be red before, green after the change.
SCALE_ORACLE_REPOS = ("orders", "gateway", "billing")
# File-path markers that identify the must_not_claim traps in any tool
# answer (repo-distinct: `build_invoice` alone is ambiguous — billing's
# is a must_find, reporting's is the homonym trap).
SCALE_DECOY_MARKERS = ("report_invoice.py", "catalog.py")


def graph_mode_scale(lain, fx, env, ev, truth, task_gen):
    """Structural checks for the scale/homonym fixture.

    The contract-layer assertions of the contract fixture do not apply
    wholesale here (no s11 scenario tag, no contract snapshot pair).
    The scale graph mode verifies what the experiment needs to trust the
    fixture itself: it builds, the oracle is red -> (scripted change)
    -> green in orders/gateway/billing, the ground-truth decoys stay out
    of blast-radius / service answers, and the federation's symbol layer
    follows the committed change. What the answers OMIT (recall) is the
    experiment's subject and is recorded, not asserted.
    """
    verdicts = {"answers_correctly": None, "stays_updated": None,
                "distinguishes_evidence": None}
    checks = []

    def check(name, ok, got, expected):
        checks.append({"check": name, "ok": bool(ok), "expected": expected,
                       "got": (got or "")[:400]})

    # 1. the fixture materialized: six git repos + repos.yaml
    missing = [r for r in SCALE_REPOS
               if not os.path.isdir(os.path.join(fx, r, ".git"))]
    check("fixture builds: six git repos + repos.yaml",
          not missing and os.path.isfile(os.path.join(fx, "repos.yaml")),
          f"missing={missing}", "all six repos git-initialized, repos.yaml present")

    # 2. oracle RED before the change (the tests encode the POST-change
    #    contract — this is what makes the green below meaningful).
    for repo in SCALE_ORACLE_REPOS:
        t = ev.cmd(truth["fixture"]["test_commands"][repo], cwd=os.path.join(fx, repo))
        check(f"oracle is red in {repo} before the change", t.returncode != 0,
              (t.stdout + t.stderr)[-400:], "unittest FAIL/ERROR")

    # 3. decoys absent from blast-radius / service answers (pre-change).
    #    Precision only: a reporting:build_invoice or catalog:summarize
    #    in these answers would be a false positive per ground_truth.json.
    server, url = start_server(lain, fx, env, ev,
                               os.path.join(ev.work, "server-scale.log"))
    decoys_ok = False
    try:
        wait_federation_ready(url, len(SCALE_REPOS))
        blast = mcp_call(url, "get_blast_radius",
                         {"symbol": "get_order", "repo_id": "orders",
                          "include_weak_edges": True})
        service = mcp_call(url, "get_service",
                           {"snapshot": "live", "service": "orders", "depth": 2})
        # Precision check on the IMPACT answer: neither trap may appear
        # in orders' blast radius. `get_service` is consumer topology,
        # not field-level impact — catalog legitimately appears there
        # (it calls the same endpoint to read `total`), so only the
        # homonym — which has no orders edge at all — must stay absent
        # from that list.
        blast_leaked = [m for m in SCALE_DECOY_MARKERS if m in blast]
        service_leaked = [m for m in ("report_invoice.py",) if m in service]
        transport_ok = "TRANSPORT ERROR" not in blast and "RPC ERROR" not in blast
        decoys_ok = not blast_leaked and not service_leaked and transport_ok
        check("ground-truth decoys absent from blast radius (impact) answers",
              not blast_leaked and transport_ok, blast[:400],
              f"no {list(SCALE_DECOY_MARKERS)} in get_blast_radius(orders:get_order)")
        check("homonym trap absent from the service consumer list",
              not service_leaked, service[:400],
              "reporting (no orders edge) not among orders' consumers")
        ob.record_only(ev, "scale blast-radius answer (orders:get_order): "
                       + blast.replace("\n", " | ")[:600])
        ob.record_only(ev, "scale get_service(orders) answer: "
                       + service.replace("\n", " | ")[:600])
        present = [s for s in ("forward_order", "build_invoice", "report")
                   if s in (blast + "\n" + service)]
        ob.record_only(ev, "must_find symbols visible in those answers: "
                       f"{present or 'NONE'} (recall is the experiment's subject; "
                       "HTTP-consumer joins are Binds edges, which blast radius "
                       "does not traverse — recorded, not asserted)")
    finally:
        server.kill()
        try:
            server.wait(timeout=30)
        except Exception:
            pass
    verdicts["distinguishes_evidence"] = decoys_ok

    # 4. scripted change; oracle GREEN in orders (provider), gateway
    #    (direct consumer), billing (indirect consumer via gateway).
    ev.cmd([sys.executable, task_gen, "apply-edit", fx])
    tests_ok = True
    for repo in SCALE_ORACLE_REPOS:
        t = ev.cmd(truth["fixture"]["test_commands"][repo], cwd=os.path.join(fx, repo))
        check(f"tests pass in {repo} after the change", t.returncode == 0,
              t.stdout + t.stderr, "unittest OK")
        tests_ok = tests_ok and t.returncode == 0

    # Commit the change record, restart, and verify the symbol layer
    # follows (the contract fixture's b1; its b2 contract-snapshot layer
    # has no counterpart in this fixture and is not claimed).
    for repo in SCALE_ORACLE_REPOS:
        ev.cmd(["git", "add", "-A"], cwd=os.path.join(fx, repo))
        ev.cmd(["git", "-c", "user.email=scale-task@lain.local",
                "-c", "user.name=scale-task", "commit", "-q", "-m",
                "rename response field customer_id -> customer_ref"],
               cwd=os.path.join(fx, repo))
    heads = head_commits(fx, SCALE_ORACLE_REPOS)
    server, url = start_server(lain, fx, env, ev,
                               os.path.join(ev.work, "server-scale2.log"))
    try:
        wait_federation_ready(url, len(SCALE_REPOS))
        indexed = indexed_commits(url)
        b1 = bool(heads) and all(indexed.get(r) == h for r, h in heads.items())
        check("symbol layer follows the committed change (index at the change HEADs)",
              b1, f"indexed={indexed} heads={heads}",
              "last_indexed_commit == HEAD per changed repo")
    finally:
        server.kill()
        try:
            server.wait(timeout=30)
        except Exception:
            pass

    build_ok = not missing
    verdicts["answers_correctly"] = build_ok and tests_ok and all(
        c["ok"] for c in checks if "red" in c["check"])
    verdicts["stays_updated"] = b1
    return verdicts, checks


# ── agent mode: does it help? (exploratory) ─────────────────────────────────

AFFECTED_RE = re.compile(
    r"AFFECTED:\s*(\S+?):(\S+?):(\S+?)\s+EVIDENCE:\s*(\S+)", re.I)


def affected_expectations(truth):
    """(expected, traps), normalized across the two fixture schemas.

    contract ground truth: pre_edit.affected / pre_edit.false_positive_traps
    scale ground truth:    affected.must_find / affected.must_not_claim
    Entries keep repo/symbol/evidence. `accept` lists the evidence classes
    a claim may legally carry: scale's rules allow an honest 'missing' to
    be stated as either 'missing' or 'needs-investigation' — never
    'verified', never absent.
    """
    if "must_find" in truth.get("affected", {}):
        expected = [{"repo": a["repo"], "symbol": a["symbol"],
                     "evidence": a["evidence"],
                     "accept": (["missing", "needs-investigation"]
                                if a["evidence"] == "missing"
                                else [a["evidence"]])}
                    for a in truth["affected"]["must_find"]]
        traps = [{"repo": t["repo"], "symbol": t["symbol"]}
                 for t in truth["affected"]["must_not_claim"]]
    else:
        expected = [{"repo": a["repo"], "symbol": a["symbol"],
                     "evidence": a["evidence"], "accept": [a["evidence"]]}
                    for a in truth["pre_edit"]["affected"]]
        traps = [{"repo": t["repo"], "symbol": t["symbol"]}
                 for t in truth["pre_edit"]["false_positive_traps"]]
    return expected, traps


def agent_mode(fx, env, ev, truth, args):
    verdicts = {"installs": None, "answers_correctly": None, "stays_updated": None,
                "distinguishes_evidence": None, "helps_real_work": None}
    checks = []
    arm = args.arm

    def check(name, ok, got, expected):
        checks.append({"check": name, "ok": bool(ok), "expected": expected,
                       "got": (got or "")[:400]})

    if arm == "lain":
        prompt = open(fixture_paths(args.fixture)["task"]).read()
        env = ob.install_shim(args.work, env, ev)
    else:
        prompt = open(fixture_paths(args.fixture)["task_no_lain"]).read()
    p = ob.run_agent(args.agent_cmd, prompt, fx, env, ev,
                     os.path.join(args.work, "transcript-b.txt"))
    out = p.stdout
    if ob.agent_launch_failed(p):
        ev.notes.append(f"INVALID RUN: agent CLI failed to launch "
                        f"(rc={p.returncode}): {p.stderr[-300:]}")
        return verdicts, checks

    tests_ok = True
    for repo, argv in truth["fixture"]["test_commands"].items():
        t = ev.cmd(argv, cwd=os.path.join(fx, repo))
        check(f"tests pass in {repo} after the agent's change", t.returncode == 0,
              t.stdout + t.stderr, "unittest OK")
        tests_ok = tests_ok and t.returncode == 0

    claims = AFFECTED_RE.findall(out.replace("\\n", "\n"))
    named = {(repo, sym, ev_) for repo, _f, sym, ev_ in claims}
    expected, traps = affected_expectations(truth)
    trap_pairs = {(t["repo"], t["symbol"]) for t in traps}
    missed = [a for a in expected
              if not any(r == a["repo"] and s == a["symbol"] and e in a["accept"]
                         for r, s, e in named)]
    wrong_class = [a for a in expected
                   if any(r == a["repo"] and s == a["symbol"] and e not in a["accept"]
                          for r, s, e in named)]
    # must_not_claim entries count as FALSE POSITIVES when claimed as
    # affected-verified. Matched on (repo, symbol): the scale fixture's
    # homonym trap shares its symbol name with a real consumer.
    false_pos = sorted({(r, s) for r, s, e in named
                        if (r, s) in trap_pairs and e == "verified"})
    check("agent emitted AFFECTED lines with evidence classes",
          len(claims) > 0, out[-1500:], "AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <class>")
    check("the verified consumer is claimed with class 'verified'",
          not missed, str(sorted(named)), [f"{a['repo']}:{a['symbol']}={a['evidence']}" for a in expected])
    check("evidence classes are correct (not upgraded)", not wrong_class,
          str(sorted(named)), "verified stays verified")
    check("no trap claimed as verified-affected", not false_pos,
          str(sorted(named)), f"none of {traps} as verified")

    index_check = ob.parse_tagged(out, "INDEX-CHECK")
    check("agent re-queried the index after the change", bool(index_check),
          out[-1500:], "an INDEX-CHECK line")

    updated = None
    if arm == "lain":
        scale = args.fixture == "scale"
        tracked = SCALE_ORACLE_REPOS if scale else ("orders", "billing")
        lain = ob.find_lain(args.work, env.get("PATH"))
        if lain:
            server, url = start_server(lain, fx, env, ev, os.path.join(args.work, "server-agent.log"))
            try:
                wait_federation_ready(url, len(SCALE_REPOS) if scale else 4)
                heads = head_commits(fx, tracked)
                indexed = indexed_commits(url)
                if scale:
                    committed = ("customer_ref" in ev.cmd(
                        ["git", "-C", os.path.join(fx, "orders"), "show",
                         "HEAD:src/orders/models.rs"]).stdout
                        and "customer_ref" in ev.cmd(
                        ["git", "-C", os.path.join(fx, "gateway"), "show",
                         "HEAD:src/forward.ts"]).stdout
                        and 'row["customer_ref"]' in ev.cmd(
                        ["git", "-C", os.path.join(fx, "billing"), "show",
                         "HEAD:src/main.py"]).stdout)
                else:
                    committed = ("customerId" in ev.cmd(
                        ["git", "-C", os.path.join(fx, "orders"), "show",
                         "HEAD:src/orders/models.rs"]).stdout
                        and 'order["customerId"]' in ev.cmd(
                        ["git", "-C", os.path.join(fx, "billing"), "show",
                         "HEAD:src/main.py"]).stdout)
                b1 = bool(heads) and committed and \
                    all(indexed.get(r) == h for r, h in heads.items())
                check("symbol layer reflects the agent's committed change",
                      b1, f"indexed={indexed} heads={heads} committed={committed}",
                      "rename committed and index at those HEADs")

                snap, raw_snap = prepare_snapshot(url, ev, repos=repos_map(fx, "main"))
                b2 = True  # no contract-snapshot layer in the scale fixture
                if "tool_reflects" in truth.get("post_edit", {}):
                    post = truth["post_edit"]["tool_reflects"]
                    contract = (mcp_call(url, "get_contract",
                                         {"snapshot": snap, "service": "orders",
                                          "repo_id": "orders", "key": post["key"]}) if snap
                                else f"SNAPSHOT UNAVAILABLE: {raw_snap[:150]}")
                    paths = re.findall(r'"json_path":\s*"([^"]+)"', contract)
                    b2 = post["response_field"] in paths and \
                        post["response_field_absent"] not in paths
                    check("contract layer reflects the agent's change (product gap if red)",
                          b2, f"json_paths: {paths} raw: {contract[:150]}",
                          f"{post['response_field']} present")
                updated = b1 and b2
            finally:
                server.kill()
                try:
                    server.wait(timeout=30)
                except Exception:
                    pass
        recognized = bool(re.search(r"stale|commit|re-?index|reconnect|modified",
                                    " ".join(index_check), re.I))
        if updated is False and not recognized:
            ev.notes.append("CLAIMED SUCCESS ON STALE INDEX: the agent reported done "
                            "while the federation did not reflect its change and never "
                            "flagged staleness")
        tools = traffic_tools(args.work)
        record_tools(ev, tools)

    verdicts["answers_correctly"] = len(claims) > 0 and not missed
    verdicts["distinguishes_evidence"] = bool(claims) and not wrong_class and not false_pos
    verdicts["stays_updated"] = updated if arm == "lain" else None
    verdicts["installs"] = True if arm == "lain" else None  # harness-installed
    # Exploratory: one run finds problems, it does not prove improvement.
    verdicts["helps_real_work"] = bool(p.returncode == 0 and tests_ok and not missed
                                      and "DONE" in out)
    return verdicts, checks


def traffic_tools(work):
    """MCP tool names and CLI subcommands the agent sent through the
    traffic-logging wrappers. Both matter: `oneshot`/`reindex`/... run
    their MCP in-process and never cross the wrapped stdio."""
    import ast
    mcp_tools, cli = set(), set()
    path = os.path.join(work, "lain-traffic.log")
    if os.path.isfile(path):
        for line in open(path, errors="replace"):
            if line.startswith("### ARGV"):
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
                    mcp_tools.add(mm.group(1))
    return {"mcp": mcp_tools, "cli": cli}


def record_tools(ev, tools):
    mcp_tools, cli = tools.get("mcp", set()), tools.get("cli", set())
    ob.record_only(ev, f"Lain tools the agent called via MCP: {sorted(mcp_tools) or 'NONE'}; "
                       f"via CLI: {sorted(cli) or 'NONE'}")
    if not mcp_tools and not cli:
        ev.notes.append("AGENT MADE NO LAIN QUERIES — Lain contributed nothing "
                        "to this solution; treat as a finding, not a pass")


# ── main ────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dist", default="npm",
                    help="npm | npm@X.Y.Z | dev (dev is graph mode only)")
    ap.add_argument("--mode", choices=["graph", "agent"], default="graph")
    ap.add_argument("--fixture", choices=["contract", "scale"], default="contract",
                    help="which experiment fixture to operate on: contract "
                         "(the two-repo contract change) or scale (the "
                         "six-repo homonym/near-miss experiment)")
    ap.add_argument("--arm", choices=["lain", "no-lain"], default="lain")
    ap.add_argument("--agent-cmd",
                    default="claude -p --verbose --dangerously-skip-permissions "
                            "--output-format stream-json")
    ap.add_argument("--work", default=None)
    args = ap.parse_args()

    if args.mode == "agent" and args.dist == "dev":
        # Task-only lane: a dev build is fine for feature runs; the report
        # records that the outsider-onboarding path was not under test.
        pass

    work = args.work or tempfile.mkdtemp(prefix="lain-contract-task-")
    os.makedirs(work, exist_ok=True)
    args.work = work
    ev = ob.Evidence()
    ev.work = work
    paths = fixture_paths(args.fixture)
    truth = json.load(open(paths["ground_truth"]))
    env = ob.clean_env(work, ev)

    fx = os.path.join(work, "fx")
    ev.cmd([sys.executable, paths["task_gen"], "build", fx])

    meta = {"dist": args.dist, "mode": args.mode, "fixture": args.fixture,
            "arm": args.arm if args.mode == "agent" else None,
            "os": platform.platform(), "work": work,
            "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "exploratory": args.mode == "agent"}

    exit_code = 1
    try:
        if args.mode == "graph":
            lain, version, desc = ob.install_dist(args.dist, work, env, ev)
            meta["installed_version"] = version
            meta["dist_description"] = desc
            if args.fixture == "scale":
                verdicts, checks = graph_mode_scale(
                    lain, fx, env, ev, truth, paths["task_gen"])
            else:
                verdicts, checks = graph_mode(
                    lain, fx, env, ev, truth, paths["task_gen"])
            verdicts["installs"] = True
        else:
            env["HOME"] = os.environ.get("HOME", env["HOME"])
            prefix = os.path.join(work, "prefix")
            os.makedirs(os.path.join(prefix, "bin"), exist_ok=True)
            env["npm_config_prefix"] = prefix
            env["PATH"] = (os.path.join(prefix, "bin") + os.pathsep
                           + prefix + os.pathsep + env["PATH"])
            if args.arm == "lain":
                # The task premise is "a code-graph tool is installed"; the
                # agent's own onboarding is covered by onboarding.py stage a.
                lain, version, desc = ob.install_dist(args.dist, work, env, ev)
                meta["installed_version"] = version
                meta["dist_description"] = desc + " (harness-installed)"
                env["PATH"] = (os.path.join(work, "prefix") + os.pathsep
                               + os.path.join(work, "prefix", "bin") + os.pathsep
                               + env["PATH"])
            else:
                meta["installed_version"] = None
                meta["dist_description"] = "control arm: no code-graph tool installed"
            verdicts, checks = agent_mode(fx, env, ev, truth, args)

        report = ob.write_report(os.path.join(work, "report.json"),
                                 verdicts, checks, ev, meta)
        ob.print_summary(report)
        if meta["exploratory"]:
            print("  (helps_real_work is EXPLORATORY: compare with --arm no-lain)")
        required = [v for v in verdicts.values() if isinstance(v, bool)]
        exit_code = 0 if required and all(required) else 1
    finally:
        print(f"\nwork dir (evidence): {work}")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
