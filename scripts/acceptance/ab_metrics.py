#!/usr/bin/env python3
"""Extract A/B comparison metrics from acceptance run directories.

Reads report.json + transcript-b.txt (claude --output-format stream-json)
+ the fixture repos' git state. Metrics per run:
  tokens (input/output/cache), wall-clock, agent duration, turns,
  tool calls, files touched, scope violations (tests/ or trap repos),
  committed?, AFFECTED claims, sync activity (index verification calls).

Claims are split into what the AGENT asserted (assistant text and tool
inputs) vs what a TOOL printed (tool results — raw `lain impact` output
is LAIN's claim, not the agent's). Agent claims are then scored against
the predeclared oracle in tests/fixtures/scale_task/ground_truth.json:
omissions (must_find absent), serious false positives (must_not_claim
claimed affected/verified) and misclassifications (wrong evidence class).

Usage: ab_metrics.py <run-dir> [<run-dir> ...]
"""
import json, os, re, subprocess, sys

CLAIM_RE = re.compile(r'(?<!NOT )AFFECTED:\s+(.+?)\s+EVIDENCE:\s+([\w-]+)([^\n]*)')
NEG_RE = re.compile(r"not affected|not edited|no orders call|never calls|-> not", re.I)
GT_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "..", "..", "tests", "fixtures", "scale_task",
                       "ground_truth.json")


def git(dir_, *a):
    return subprocess.run(["git", "-C", dir_, *a], capture_output=True, text=True).stdout


INCIDENTAL_RE = re.compile(r"target/|__pycache__|Cargo.lock|\.pyc$")
TESTS_RE = re.compile(r"(^|/)tests/")


def tests_scope_edit(path):
    """True when a changed file is a tests/ scope violation.

    ANY edit under a `tests/` path counts — including the protected
    oracle tests (`tests/test_*.py`), which the old rule
    (`startswith("tests/") and "test_" not in f`) excluded exactly,
    letting an agent edit the oracles to make the task pass while the
    A/B metrics reported no violation. The incidental-artifact
    exclusion (target/, __pycache__, Cargo.lock, *.pyc) is what the old
    exemption was reaching for and stays: a built artifact is not an
    agent edit.
    """
    return bool(TESTS_RE.search(path)) and not INCIDENTAL_RE.search(path)


def _strings(obj, out):
    if isinstance(obj, str):
        out.append(obj)
    elif isinstance(obj, dict):
        for v in obj.values():
            _strings(v, out)
    elif isinstance(obj, list):
        for v in obj:
            _strings(v, out)


def extract_claims(tr):
    """Split AFFECTED claims into agent-asserted vs tool-printed. A claim
    echoed in both (agent heredoc echoed by its own tool_result) keeps only
    the agent attribution; tool_only is therefore pure tool output."""
    agent, tool = [], []
    for ln in tr.splitlines():
        ln = ln.strip()
        if not ln.startswith("{"):
            continue
        try:
            r = json.loads(ln)
        except json.JSONDecodeError:
            continue
        typ = r.get("type")
        if typ not in ("assistant", "user"):
            continue
        strs = []
        _strings(r.get("message", {}).get("content", []), strs)
        found = [(a, e, q.strip()) for a, e, q in CLAIM_RE.findall("\n".join(strs))
                 if "<" not in a and "<" not in e]
        if not found:
            continue
        (agent if typ == "assistant" else tool).extend(found)
    agent_u = sorted(set(agent))
    tool_only = sorted(set(tool) - set(agent))
    return agent_u, tool_only


def _fmt(claims):
    out = []
    for a, e, q in claims:
        out.append({"symbol": a, "evidence": e, "qualifier": q or None,
                    "asserts_affected": not bool(NEG_RE.search(q))})
    return out


# Per-key acceptable-evidence overrides for must_find entries: the default
# table applies when the caller passes overrides=None. Callers scoring a
# different oracle pass both `gt=` and their own `overrides=` (e.g. the
# swarm fixture wants notify:send_notice restricted to needs-investigation).
ACCEPTABLE_OVERRIDES = {"analytics:report": ["missing", "needs-investigation"]}


def _ok_classes(mf, ov):
    """Evidence classes that score this must_find entry correct.

    Default is exactly the entry's `evidence`; the caller's overrides table
    (ov) replaces it per key; a fixture-declared `acceptable` list (the
    enterprise_mesh oracle's inline tolerance) wins over both.
    """
    ok = [mf["evidence"]]
    key = mf["repo"] + ":" + mf["symbol"]
    if key in ov:
        ok = list(ov[key])
    if mf.get("acceptable") is not None:
        ok = list(mf["acceptable"])
    return ok


def _recall_by_class(gt, by, ov):
    """Per-coupling-class recall over gt's must_find entries.

    Only called when the oracle declares `class` on its entries; entries
    without one land in an `unclassified` bucket. found = claimed at all,
    correct_class = at least one claim carries a class in ok_cls, omitted =
    never claimed, misclassified = claimed but never with an ok class.
    """
    recall = {}
    for mf in gt["affected"]["must_find"]:
        cls = mf.get("class") or "unclassified"
        b = recall.setdefault(cls, {"found": 0, "total": 0,
                                    "correct_class": 0, "omitted": [],
                                    "misclassified": []})
        b["total"] += 1
        key = mf["repo"] + ":" + mf["symbol"]
        hits = by.get(("must", key), [])
        if not hits:
            b["omitted"].append(key)
            continue
        b["found"] += 1
        if any(c["evidence"] in _ok_classes(mf, ov) for c in hits):
            b["correct_class"] += 1
        else:
            evs = "/".join(sorted({c["evidence"] for c in hits}))
            b["misclassified"].append(f"{key} -> {evs}")
    return recall


def score_ground_truth(claims_fmt, gt=None, overrides=None):
    """Score the agent's claim lines against the predeclared oracle.

    evidence_rules (ground_truth.json): must_find classes must match exactly
    (analytics may be missing|needs-investigation); a `verified` claim on a
    must_not_claim entry is the serious failure; a negated qualifier
    ('not affected') lists a candidate without asserting impact — right
    judgment, but misuse of the AFFECTED protocol.

    gt: a preloaded ground-truth dict (defaults to loading GT_PATH).
    overrides: {symbol: [acceptable classes]} replacing ACCEPTABLE_OVERRIDES
    for this call; None keeps the module default (scale_task behavior).
    A must_find entry may declare its own `acceptable` list (it wins over
    both `[evidence]` and the overrides table) and a coupling `class`
    (A/B/C/external); per-entry scoring is unchanged, but when any entry
    declares `class` the result additionally carries `recall_by_class`
    keyed by class. Oracles with neither field (scale_task,
    swarm_deepchain) score byte-identically to before.
    """
    if gt is None:
        try:
            gt = json.load(open(GT_PATH))
        except (OSError, json.JSONDecodeError):
            return None
    ov = ACCEPTABLE_OVERRIDES if overrides is None else overrides
    by = {}
    # Repo-level fallback: a claim may cite a field-access site instead of
    # the function name (e.g. `billing-svc:src/main.py:data["customer_id"]`
    # vs `...:build_invoice`) — both identify the same affected code. When a
    # repo hosts exactly one entry in a list, any claim in that repo counts.
    from collections import Counter
    mf_repos = Counter(e["repo"] for e in gt["affected"]["must_find"])
    mn_repos = Counter(e["repo"] for e in gt["affected"]["must_not_claim"])

    def _hit(c, entry, uniq):
        if c["symbol"].startswith(entry["repo"] + ":"):
            if c["symbol"].endswith(":" + entry["symbol"]):
                return True
            if uniq.get(entry["repo"], 0) == 1:
                return True
        return False

    for c in claims_fmt:
        for mf in gt["affected"]["must_find"]:
            key = mf["repo"] + ":" + mf["symbol"]
            hit = _hit(c, mf, mf_repos)
            if mf["repo"] == "external":
                # agents phrase out-of-workspace claims variously
                # (external:..., partner-portal:..., outside-workspace:...)
                hit = bool(re.search(r"partner-portal|outside|external",
                                     c["symbol"], re.I))
            if hit:
                by.setdefault(("must", key), []).append(c)
        for mn in gt["affected"]["must_not_claim"]:
            if _hit(c, mn, mn_repos):
                by.setdefault(("mustnot", mn["repo"] + ":" + mn["symbol"]), []).append(c)

    res = {"must_find": [], "must_not_claim": [], "omissions": [],
           "serious_false_positives": [], "misclassified": [],
           "protocol_misuse": []}
    for mf in gt["affected"]["must_find"]:
        key = mf["repo"] + ":" + mf["symbol"]
        ok_cls = _ok_classes(mf, ov)
        hits = by.get(("must", key), [])
        if not hits:
            res["omissions"].append(key)
            res["must_find"].append({"symbol": key, "status": "omitted"})
            continue
        st = "omitted"
        for c in hits:
            if not c["asserts_affected"]:
                st = "contradicted"
            elif c["evidence"] in ok_cls:
                st = "ok"
                break
            else:
                st = "misclassified:" + c["evidence"]
        if st.startswith("misclassified"):
            res["misclassified"].append(f"{key} -> {st.split(':', 1)[1]}")
        res["must_find"].append({"symbol": key, "status": st,
                                 "claimed": [c["evidence"] for c in hits]})
    for mn in gt["affected"]["must_not_claim"]:
        key = mn["repo"] + ":" + mn["symbol"]
        hits = by.get(("mustnot", key), [])
        if not hits:
            res["must_not_claim"].append({"symbol": key, "status": "not_claimed"})
            continue
        st = "not_claimed"
        for c in hits:
            if not c["asserts_affected"]:
                st = "listed_negated"
                res["protocol_misuse"].append(key)
            elif c["evidence"] == "verified":
                st = "SERIOUS-FP"
                res["serious_false_positives"].append(key)
            else:
                st = "fp:" + c["evidence"]
                res["serious_false_positives"].append(f"{key} [{c['evidence']}]")
        res["must_not_claim"].append({"symbol": key, "status": st,
                                      "claimed": [c["evidence"] for c in hits]})
    res["harm"] = {"omissions": len(res["omissions"]),
                   "serious_false_positives": len(res["serious_false_positives"]),
                   "misclassified": len(res["misclassified"])}
    # Noise policy (scale fixtures): any asserted claim in a repo outside
    # the declared set (seed / must_find / named traps) is a false positive
    # the decoy volume is designed to provoke.
    noise = []
    if str(gt["affected"].get("noise_policy", "")).startswith("unlisted_repos_are_noise"):
        allowed = {mf["repo"] for mf in gt["affected"]["must_find"]}
        allowed |= {mn["repo"] for mn in gt["affected"]["must_not_claim"]}
        allowed |= set(gt["affected"].get("seed_repos", []))
        for c in claims_fmt:
            if not c["asserts_affected"]:
                continue
            if re.search(r"partner-portal|outside|external", c["symbol"], re.I):
                continue
            if re.search(r"unknown|unseen|other consumers|beyond ", c["symbol"], re.I):
                continue  # free-text known-unknown declarations, not decoys
            repo = c["symbol"].split(":", 1)[0].strip()
            if repo and repo not in allowed:
                noise.append(f"{c['symbol']} [{c['evidence']}]")
    res["noise_false_positives"] = noise
    res["harm"]["noise_false_positives"] = len(noise)
    if any("class" in mf for mf in gt["affected"]["must_find"]):
        res["recall_by_class"] = _recall_by_class(gt, by, ov)
    return res


def metrics(path):
    m = {"run": os.path.basename(path)}
    rep = json.load(open(os.path.join(path, "report.json")))
    tr = open(os.path.join(path, "transcript-b.txt"), errors="replace").read()

    # tokens + durations from the stream-json result event and messages
    tok = {"input": 0, "output": 0, "cache_read": 0}
    for u in re.findall(r'"usage":\{([^}]*)\}', tr):
        g = dict(re.findall(r'"(\w+)":(\d+)', u))
        tok["input"] += int(g.get("input_tokens", 0))
        tok["output"] += int(g.get("output_tokens", 0))
        tok["cache_read"] += int(g.get("cache_read_input_tokens", 0))
    m["tokens"] = tok
    dur = re.findall(r'"duration_ms":(\d+)', tr)
    m["agent_duration_s"] = round(max(map(int, dur)) / 1000, 1) if dur else None
    ts = re.findall(r'"timestamp":"([^"]+)"', tr)
    m["first_to_last_s"] = None
    if len(ts) >= 2:
        from datetime import datetime
        f = lambda s: datetime.fromisoformat(s.replace("Z", "+00:00"))
        m["first_to_last_s"] = round((f(ts[-1]) - f(ts[0])).total_seconds(), 1)

    # activity counts
    m["turns"] = len(re.findall(r'"type":"assistant"', tr))
    m["tool_calls_total"] = len(re.findall(r'"name":"(Bash|Edit|Write|Read|Glob|Grep|MultiEdit)"', tr))
    m["bash_calls"] = len(re.findall(r'"name":"Bash"', tr))
    notes = " ".join(rep.get("notes", []))
    mcp = re.search(r"via MCP: \[([^\]]*)\]", notes)
    cli = re.search(r"via CLI: \[([^\]]*)\]", notes)
    m["lain_tools_mcp"] = [x.strip(" '\"") for x in (mcp.group(1).split(",") if mcp else []) if x.strip(" '\"")]
    m["lain_tools_cli"] = [x.strip(" '\"") for x in (cli.group(1).split(",") if cli else []) if x.strip(" '\"")]

    # sync activity: how much of the agent's bash work was index sync/verify
    sync_calls = len(re.findall(r'\b(reindex|status|doctor|oneshot)\b', tr))
    m["sync_mentions"] = sync_calls

    # claims: agent-asserted vs tool-printed, then oracle score
    agent_c, tool_c = extract_claims(tr)
    m["claims_agent"] = _fmt(agent_c)
    m["claims_tool_only"] = _fmt(tool_c)
    m["claim_count_agent"] = len(agent_c)
    m["claim_count_tool_only"] = len(tool_c)
    m["claims"] = [f"{a} [{e}]" for a, e, _ in agent_c]
    m["claim_count"] = len(agent_c)
    m["ground_truth_score"] = score_ground_truth(m["claims_agent"])

    # clobbering: fixture repo file scope
    fx = os.path.join(path, "fx")
    touched, violations, uncommitted = [], [], []
    repos = (sorted(d for d in os.listdir(fx) if os.path.isdir(os.path.join(fx, d, ".git")) if os.path.isdir(os.path.join(fx, d)))
             if os.path.isdir(fx) else [])
    for repo in repos:
        rdir = os.path.join(fx, repo)
        # files the AGENT changed = the diff from the fixture's final setup
        # commit to HEAD (the agent may modify files the fixture created),
        # plus anything still dirty. Artifact paths (target/, __pycache__...)
        # are ignored here — a built fixture repo now ships a .gitignore, so
        # they no longer appear in git status at all.
        fx_sha = git(rdir, "log", "--author=fixture@lain.local", "-1",
                     "--format=%H").strip()
        files = [f for f in git(rdir, "diff", "--name-only", fx_sha, "HEAD").splitlines()
                 if f.strip()] if fx_sha else []
        dirty = [l[3:] for l in git(rdir, "status", "--short").splitlines()
                 if l.strip() and "__pycache__" not in l and "target/" not in l
                 and not l.endswith("Cargo.lock")]
        for f in files + dirty:
            touched.append(f"{repo}:{f}")
            if tests_scope_edit(f):  # any tests/ edit — oracle tests included
                violations.append(f"{repo}:{f}")
            if repo in ("reporting", "catalog", "analytics"):
                violations.append(f"TRAP-REPO touched: {repo}:{f}")
        if dirty:
            uncommitted.append(repo)
    incidental = [f for f in touched if INCIDENTAL_RE.search(f)]
    m["files_touched"] = [f for f in touched if f not in incidental]
    m["incidental_files_committed"] = len(incidental)
    m["scope_violations"] = violations
    if os.path.isdir(fx):
        m["repos_with_uncommitted_work"] = uncommitted
        m["all_changes_committed"] = not uncommitted
    else:
        m["repos_with_uncommitted_work"] = None
        m["all_changes_committed"] = None  # fx/ gone: unknown, not clean
    return m


if __name__ == "__main__":
    out = [metrics(p) for p in sys.argv[1:]]
    print(json.dumps(out, indent=2))
