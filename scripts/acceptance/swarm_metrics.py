#!/usr/bin/env python3
"""Phase + cost metrics for the 3-agent swarm deep-chain experiment.

Reads the layout produced by swarm.py:

    <work>/p1/<agent>/fx/consensus/private-<agent>.md     phase 1 (isolated)
    <work>/shared/fx/consensus/positions-<agent>.md       phase 2 positions
    <work>/shared/fx/consensus/consensus.md               phase 2 consensus
    <work>/shared/fx/consensus/review-<agent>.md          phase 3 reviews
    <work>/shared/fx/<repo>/...                           git attribution
    <work>/report.json                                    invocations (durations)

Prints one JSON object:

  p1_scores        per agent: AFFECTED claims from private-<agent>.md,
                   scored against the fixture's ground_truth.json (default
                   swarm_deepchain) + recall_by_class when the oracle
                   declares per-entry coupling classes
  p2_scores        consensus.md claims scored the same way + disagreement
                   count + whether the three positions claim sets match
                   + recall_by_class over the consensus list
  clobbering       per-repo files-by-agent, overlapping (edited by >1 agent),
                   unattributed commits, trap/tests edits, uncommitted work
  p3_reviews       verdict-marker counts, misses, scope violations per review
  cost             per transcript (9 = 3 phases x 3 agents): tokens/turns
                   (same stream-json regexes as ab_metrics) + wall duration
                   from report.json; aggregated per phase and per arm

Claim extraction and ground-truth scoring REUSE ab_metrics (CLAIM_RE, _fmt,
score_ground_truth) so this oracle and the scale_task A/B score share one
implementation. swarm scoring passes the selected fixture's ground truth
(swarm_deepchain's evidence_rules overrides apply only to that fixture);
an oracle whose must_find entries declare `class`/`acceptable` also gets
the per-class recall block — entries without those fields score exactly
as before (swarm_deepchain output is unchanged).

Usage: swarm_metrics.py <work-dir> [--fixture NAME] [--gt PATH]
  --fixture NAME   score against tests/fixtures/<NAME>/ground_truth.json
                   (default swarm_deepchain). report.json does NOT record
                   the fixture, so re-scoring an enterprise_mesh run after
                   the fact needs this flag (swarm.py passes fixture= at
                   run time).
  --gt PATH        explicit ground_truth.json path (wins over --fixture
                   for content; alone it implies no evidence overrides)
  (or as a library:
   swarm_metrics.collect(work, invocations=..., arm=..., fixture=...))
"""
import argparse
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

import ab_metrics  # noqa: E402

SWARM_GT_PATH = os.path.join(REPO_ROOT, "tests", "fixtures", "swarm_deepchain",
                             "ground_truth.json")
FIXTURES_DIR = os.path.join(REPO_ROOT, "tests", "fixtures")
# evidence_rules of swarm_deepchain/ground_truth.json: notify must be
# exactly needs-investigation (missing understates, verified overstates);
# the external consumer must be exactly missing AND present. These keys
# belong to that fixture; other fixtures score with no overrides table
# (their oracle declares `acceptable` inline where a class is tolerated).
SWARM_OVERRIDES = {
    "notify:send_notice": ["needs-investigation"],
    "external:partner-portal": ["missing"],
}
AGENTS = ("alpha", "beta", "gamma")
TRAP_REPOS = ("reporting", "catalog")

git = ab_metrics.git  # git(dir, *args) -> stdout


def _oracle(fixture=None, gt=None):
    """(ground_truth dict, evidence overrides) for the selected fixture.

    Both None defaults to swarm_deepchain — identical to the original
    behavior. fixture=<name> loads tests/fixtures/<name>/ground_truth.json;
    a preloaded gt= (e.g. from --gt) skips the load, and a custom oracle
    gets no overrides table unless fixture= names swarm_deepchain.
    """
    if fixture is None and gt is None:
        fixture = "swarm_deepchain"
    if gt is None:
        path = (SWARM_GT_PATH if fixture == "swarm_deepchain"
                else os.path.join(FIXTURES_DIR, fixture, "ground_truth.json"))
        with open(path) as f:
            gt = json.load(f)
    return gt, (SWARM_OVERRIDES if fixture == "swarm_deepchain" else {})


def _resolve(gt, overrides):
    """Defaults for callers (p1_scores/p2_scores) that pass nothing."""
    if gt is None:
        gt, d_ov = _oracle()
        if overrides is None:
            overrides = d_ov
    elif overrides is None:
        overrides = {}
    return gt, overrides


def _score(claims_fmt, gt, overrides):
    """score_ground_truth against the selected fixture oracle."""
    return ab_metrics.score_ground_truth(claims_fmt, gt=gt,
                                         overrides=overrides)


def _claim_triples(text):
    """AFFECTED claims from plain text — same regex and '<' template filter
    as ab_metrics.extract_claims, but on a file body instead of a stream."""
    return [(a, e, q.strip()) for a, e, q in ab_metrics.CLAIM_RE.findall(text)
            if "<" not in a and "<" not in e]


def _read(path):
    with open(path, errors="replace") as f:
        return f.read()


# ── phase 1: independent analyses ───────────────────────────────────────────

def p1_scores(work, gt=None, overrides=None):
    gt, overrides = _resolve(gt, overrides)
    out = {}
    for agent in AGENTS:
        path = os.path.join(work, "p1", agent, "fx", "consensus",
                            f"private-{agent}.md")
        rec = {"file": os.path.relpath(path, work), "exists": os.path.isfile(path)}
        if rec["exists"]:
            claims = ab_metrics._fmt(sorted(set(_claim_triples(_read(path)))))
            rec["claim_count"] = len(claims)
            rec["claims"] = claims
            rec["ground_truth_score"] = _score(claims, gt, overrides)
            rbc = (rec["ground_truth_score"] or {}).get("recall_by_class")
            if rbc is not None:
                rec["recall_by_class"] = rbc
        out[agent] = rec
    return out


# ── phase 2: consensus ──────────────────────────────────────────────────────

def _disagreements_count(text):
    """Dispute entries under a `disagreements:` marker (heading or bullet).
    A declaration of 'none ...' closes immediately; list items count as one
    dispute each; indented continuation lines don't."""
    in_dis, count = False, 0
    for line in text.splitlines():
        m = re.match(r"^\s*#*\s*disagreements\b\s*:?\s*(.*)$", line, re.I)
        if m:
            rest = m.group(1).strip()
            if rest and re.match(r"^(none|no\b)", rest, re.I):
                in_dis = False
            elif rest:
                count += 1
                in_dis = False
            else:
                in_dis = True
            continue
        if in_dis and line.startswith("#"):
            in_dis = False
            continue
        if in_dis:
            s = line.strip()
            if s and re.match(r"^[-*]\s", s):
                count += 1
    return count


def p2_scores(shared_fx, gt=None, overrides=None):
    gt, overrides = _resolve(gt, overrides)
    cons = os.path.join(shared_fx, "consensus", "consensus.md")
    out = {"exists": os.path.isfile(cons), "claim_count": 0, "claims": [],
           "ground_truth_score": None, "disagreements_count": 0,
           "positions_agreement": {"agree": None, "missing": list(AGENTS),
                                   "claims": {}}}
    if out["exists"]:
        text = _read(cons)
        claims = ab_metrics._fmt(sorted(set(_claim_triples(text))))
        out["claim_count"] = len(claims)
        out["claims"] = claims
        out["ground_truth_score"] = _score(claims, gt, overrides)
        rbc = (out["ground_truth_score"] or {}).get("recall_by_class")
        if rbc is not None:
            out["recall_by_class"] = rbc
        out["disagreements_count"] = _disagreements_count(text)

    # Do the three positions files carry the SAME claim set (symbol+evidence)?
    sets, missing = {}, []
    for agent in AGENTS:
        path = os.path.join(shared_fx, "consensus", f"positions-{agent}.md")
        if not os.path.isfile(path):
            missing.append(agent)
            sets[agent] = None
            continue
        triples = sorted(set((a, e) for a, e, _ in _claim_triples(_read(path))))
        sets[agent] = triples
    present = [sets[a] for a in AGENTS if sets[a] is not None]
    agree = (not missing) and all(s == present[0] for s in present)
    out["positions_agreement"] = {
        "agree": agree,
        "missing": missing,
        "claims": {a: ([f"{sym} [{ev}]" for sym, ev in sets[a]]
                       if sets[a] is not None else None) for a in AGENTS},
    }
    return out


# ── phase 2/3 workspace: clobbering ─────────────────────────────────────────

def _trap_or_tests(repo, path):
    if repo in TRAP_REPOS:
        return True
    return bool(re.search(r"(^|/)(%s)/" % "|".join(TRAP_REPOS), path)
                or re.search(r"(^|/)tests/", path))


def _dirty_files(rdir):
    # Same filtering as ab_metrics: artifact paths are not agent work.
    return [l[3:] for l in git(rdir, "status", "--short").splitlines()
            if l.strip() and "__pycache__" not in l and "target/" not in l
            and not l.endswith("Cargo.lock")]


def clobbering(shared_fx):
    out = {"files_by_agent": {}, "overlapping_files": [],
           "unattributed_commits": [], "trap_or_tests_edits": [],
           "repos_with_uncommitted_work": []}
    if not os.path.isdir(shared_fx):
        return out
    repos = sorted(d for d in os.listdir(shared_fx)
                   if os.path.isdir(os.path.join(shared_fx, d, ".git")))
    allowed_authors = {"fixture@lain.local"} | {
        f"{a}@swarm.local" for a in AGENTS}
    ownership = {}  # (repo, file) -> set of agents
    for repo in repos:
        rdir = os.path.join(shared_fx, repo)
        per = {}
        for agent in AGENTS:
            raw = git(rdir, "log", f"--author={agent}@swarm.local",
                      "--name-only", "--pretty=format:")
            files = sorted({l.strip() for l in raw.splitlines() if l.strip()})
            per[agent] = files
            for f in files:
                ownership.setdefault((repo, f), set()).add(agent)
        out["files_by_agent"][repo] = per

        for line in git(rdir, "log", "--pretty=format:%H %ae").splitlines():
            if not line.strip():
                continue
            sha, _, email = line.partition(" ")
            if email.strip() not in allowed_authors:
                out["unattributed_commits"].append(f"{repo}:{sha.strip()}")

        # Committed delta since the fixture's own last commit + dirty files:
        # anything added after fixture setup, whoever authored it.
        fx_sha = git(rdir, "log", "--author=fixture@lain.local", "-1",
                     "--format=%H").strip()
        committed = ([f for f in
                     git(rdir, "diff", "--name-only", fx_sha, "HEAD").splitlines()
                     if f.strip()] if fx_sha else [])
        dirty = _dirty_files(rdir)
        if dirty:
            out["repos_with_uncommitted_work"].append(repo)
        for f in sorted(set(committed) | set(dirty)):
            if _trap_or_tests(repo, f):
                out["trap_or_tests_edits"].append(f"{repo}:{f}")

    out["overlapping_files"] = sorted(
        f"{repo}:{f}" for (repo, f), ags in ownership.items() if len(ags) > 1)
    out["unattributed_commits"] = sorted(out["unattributed_commits"])
    out["trap_or_tests_edits"] = sorted(set(out["trap_or_tests_edits"]))
    out["repos_with_uncommitted_work"] = sorted(out["repos_with_uncommitted_work"])
    return out


# ── phase 3: cross-reviews ──────────────────────────────────────────────────

_VERDICTS = ("verified", "refuted", "unevidenced")
_CODEISH = re.compile(r"[A-Za-z0-9_.-]+[:/][A-Za-z0-9_./-]+")


def _entries(lines, keyword_re):
    """Lines matching `keyword` that also carry a code-ish token
    (repo:symbol / path) — the entries, not prose mentioning the keyword."""
    out = []
    for line in lines:
        if keyword_re.search(line) and _CODEISH.search(line):
            out.append(line.strip().lstrip("-*# ").strip())
    return out


def p3_reviews(shared_fx):
    out = {}
    for agent in AGENTS:
        path = os.path.join(shared_fx, "consensus", f"review-{agent}.md")
        rec = {"file": os.path.relpath(path, shared_fx) if shared_fx else None,
               "exists": os.path.isfile(path)}
        if rec["exists"]:
            lines = _read(path).splitlines()
            rec["verdicts"] = {
                v: sum(1 for l in lines if re.search(rf"\b{v}\b", l, re.I))
                for v in _VERDICTS}
            rec["misses_reported"] = _entries(
                lines, re.compile(r"\bmiss(es|ed|ing)?\b", re.I))
            rec["scope_violations_reported"] = _entries(
                lines, re.compile(r"scope", re.I))
        else:
            rec["verdicts"] = {v: 0 for v in _VERDICTS}
            rec["misses_reported"] = []
            rec["scope_violations_reported"] = []
        out[agent] = rec
    return out


# ── cost: tokens / turns / wall ─────────────────────────────────────────────

def _transcript_stats(path):
    tok = {"input": 0, "output": 0, "cache_read": 0}
    if not path or not os.path.isfile(path):
        return {"exists": False, "tokens": tok, "turns": 0}
    tr = _read(path)
    for u in re.findall(r'"usage":\{([^}]*)\}', tr):
        g = dict(re.findall(r'"(\w+)":(\d+)', u))
        tok["input"] += int(g.get("input_tokens", 0))
        tok["output"] += int(g.get("output_tokens", 0))
        tok["cache_read"] += int(g.get("cache_read_input_tokens", 0))
    turns = len(re.findall(r'"type":"assistant"', tr))
    return {"exists": True, "tokens": tok, "turns": turns}


def _sum_tokens(stats_list):
    return {k: sum(s["tokens"][k] for s in stats_list)
            for k in ("input", "output", "cache_read")}


def cost(work, invocations, arm):
    by_key = {(i.get("phase"), i.get("agent")): i for i in invocations or []}
    per = {}
    for phase in (1, 2, 3):
        for agent in AGENTS:
            inv = by_key.get((phase, agent))
            tr = inv.get("transcript") if inv else None
            st = _transcript_stats(tr)
            st["rc"] = inv.get("rc") if inv else None
            st["duration_s"] = inv.get("duration_s") if inv else None
            st["transcript"] = tr
            per[f"p{phase}/{agent}"] = st

    per_phase = {}
    for phase in (1, 2, 3):
        entries = [per[f"p{phase}/{a}"] for a in AGENTS]
        per_phase[str(phase)] = {
            "transcripts": sum(1 for e in entries if e["exists"]),
            "tokens": _sum_tokens(entries),
            "turns": sum(e["turns"] for e in entries),
            "wall_s": round(sum(e["duration_s"] or 0 for e in entries), 1),
        }
    all_e = list(per.values())
    per_arm = {
        "arm": arm,
        "transcripts": sum(1 for e in all_e if e["exists"]),
        "tokens": _sum_tokens(all_e),
        "turns": sum(e["turns"] for e in all_e),
        "wall_s": round(sum(e["duration_s"] or 0 for e in all_e), 1),
    }
    return {"per_transcript": per, "per_phase": per_phase, "per_arm": per_arm}


# ── entry points ────────────────────────────────────────────────────────────

def collect(work, invocations=None, arm=None, fixture=None, gt=None):
    if invocations is None:
        rep = {}
        rp = os.path.join(work, "report.json")
        if os.path.isfile(rp):
            rep = json.load(open(rp))
        invocations = rep.get("invocations") or []
        arm = arm or rep.get("arm")
    gt_d, ov = _oracle(fixture, gt)
    shared = os.path.join(work, "shared", "fx")
    return {
        "p1_scores": p1_scores(work, gt=gt_d, overrides=ov),
        "p2_scores": p2_scores(shared, gt=gt_d, overrides=ov),
        "clobbering": clobbering(shared),
        "p3_reviews": p3_reviews(shared),
        "cost": cost(work, invocations, arm),
    }


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("work", help="work directory produced by swarm.py")
    ap.add_argument("--fixture", default=None,
                    help="fixture under tests/fixtures/ to score against "
                         "(default swarm_deepchain)")
    ap.add_argument("--gt", default=None,
                    help="explicit ground_truth.json path (wins over "
                         "--fixture for content; alone implies no "
                         "evidence overrides)")
    args = ap.parse_args()
    if args.fixture:
        p = os.path.join(FIXTURES_DIR, args.fixture, "ground_truth.json")
        if not os.path.isfile(p):
            ap.error(f"--fixture {args.fixture!r}: no ground_truth.json at {p}")
    gt = None
    if args.gt:
        with open(args.gt) as f:
            gt = json.load(f)
    print(json.dumps(collect(args.work, fixture=args.fixture, gt=gt),
                     indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
