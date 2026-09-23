#!/usr/bin/env python3
"""Acceptance run: does Lain do what the README promises, on real projects?

Unit tests prove the code does what its tests say. This checks the claims a
user reads — every promised language, setup, the first query, search, the
multi-repo flow, coordination — end to end against a built binary, the way
an agent or a person uses it, and compares each answer with ground truth
verified by hand (see languages.json and README.md here).

    scripts/acceptance/run.py --lain target/release/lain [--work DIR] [--only NAME]

Needs git and network access (projects are cloned at pinned commits). Exits
non-zero if any check fails, and prints PASS/FAIL with the evidence.
"""
import argparse, json, os, re, shutil, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = []


def record(name, ok, detail):
    RESULTS.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}\n      {detail}", flush=True)


class in_dir:
    """Run from `path`: Lain reads its parent's cwd, and we are the parent."""

    def __init__(self, path):
        self.path = path

    def __enter__(self):
        self.old = os.getcwd()
        os.chdir(self.path)

    def __exit__(self, *exc):
        os.chdir(self.old)


# ── MCP over stdio, as an agent host runs it ────────────────────────────────

class Mcp:
    def __init__(self, lain, cwd, env):
        # `lain mcp` finds its repository from the *agent host's* working
        # directory (its parent process), so start it from inside the
        # project, as a host running in that project would.
        with in_dir(cwd):
            self.p = subprocess.Popen([lain, "mcp"], cwd=cwd, env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.id = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "acceptance", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params):
        self.id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("lain mcp exited")
            msg = json.loads(line)
            if msg.get("id") == self.id:
                return msg

    def call(self, tool, args):
        r = self.rpc("tools/call", {"name": tool, "arguments": args})
        if "error" in r:
            return "RPC ERROR: " + json.dumps(r["error"])
        return "".join(c.get("text", "") for c in r["result"].get("content", []))

    def wait_ready(self, timeout=600):
        """Until symbols and the call graph both report ready."""
        t0 = time.time()
        while time.time() - t0 < timeout:
            try:
                caps = json.loads(self.call("get_capabilities", {}))["capabilities"]
                if all(caps[k]["state"] == "ready" for k in ("symbols", "call_graph")):
                    return time.time() - t0
            except (ValueError, KeyError):
                pass
            time.sleep(1)
        raise RuntimeError("index never became ready")

    def close(self):
        self.p.stdin.close()
        self.p.terminate()
        self.p.wait(timeout=30)


def callers_of(text):
    """Caller names from get_call_sites output."""
    return {m.group(1) for m in re.finditer(r"^- \*\*(.+?)\*\* \(", text, re.M)}


# ── Projects ────────────────────────────────────────────────────────────────

def checkout(work, repo, sha):
    path = os.path.join(work, repo.replace("/", "__"))
    if os.path.isdir(path):
        head = subprocess.run(["git", "-C", path, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
        if head == sha:
            shutil.rmtree(os.path.join(path, ".lain"), ignore_errors=True)
            return path
        shutil.rmtree(path)
    os.makedirs(path)
    run = lambda *a: subprocess.run(["git", "-C", path, *a], check=True, capture_output=True)
    run("init", "-q")
    run("fetch", "-q", "--depth", "50", f"https://github.com/{repo}", sha)
    run("checkout", "-q", "FETCH_HEAD")
    return path


SVELTE_FIXTURE = {
    "src/Cart.svelte": """<script>
  export let items = [];

  function formatPrice(cents) {
    return `$${(cents / 100).toFixed(2)}`;
  }

  function total() {
    return formatPrice(items.reduce((sum, i) => sum + i.cents, 0));
  }

  function label(item) {
    return `${item.name}: ${formatPrice(item.cents)}`;
  }
</script>

<ul>
  {#each items as item}<li>{label(item)}</li>{/each}
</ul>
<p>Total: {total()}</p>
""",
}


def fixture(work, name, files):
    path = os.path.join(work, "fixture__" + name)
    shutil.rmtree(path, ignore_errors=True)
    for rel, body in files.items():
        os.makedirs(os.path.dirname(os.path.join(path, rel)), exist_ok=True)
        open(os.path.join(path, rel), "w").write(body)
    for a in (["init", "-q"], ["add", "-A"], ["-c", "user.email=a@a", "-c", "user.name=a", "commit", "-qm", "fixture"]):
        subprocess.run(["git", "-C", path, *a], check=True, capture_output=True)
    return path


# ── Checks ──────────────────────────────────────────────────────────────────

def check_languages(args, env):
    spec = json.load(open(os.path.join(HERE, "languages.json")))
    for case in spec["cases"]:
        name = f"language: {case['lang']} — who calls {case['symbol']}"
        if args.only and args.only.lower() not in name.lower():
            continue
        try:
            path = (fixture(args.work, case["fixture"], SVELTE_FIXTURE) if "fixture" in case
                    else checkout(args.work, case["repo"], case["sha"]))
            mcp = Mcp(args.lain, path, env)
            secs = mcp.wait_ready()
            out = mcp.call("get_call_sites", {"symbol": case["symbol"]})
            mcp.close()
        except Exception as e:
            record(name, False, f"error: {e}")
            continue
        found = callers_of(out)
        want = set(case["callers"])
        missing = want - found
        extra = found - want - set(case.get("extra_ok", []))
        src = case.get("repo", "synthetic fixture") + (f"@{case['sha'][:7]}" if "sha" in case else "")
        record(name, not missing and not extra,
               f"{src}, indexed in {secs:.0f}s; expected {sorted(want)}; got {sorted(found)}"
               + (f"; MISSING {sorted(missing)}" if missing else "")
               + (f"; UNEXPECTED {sorted(extra)}" if extra else ""))


def check_single_repo_flow(args, env):
    """README: setup, first query, doctor, and the tool table, on psf/requests."""
    if args.only and args.only.lower() not in "single-repo flow":
        return
    repo = checkout(args.work, "psf/requests", "611c6162cbc4ac2020a2f91c7cfa4f3abf9bbb60")
    lain = args.lain
    with in_dir(repo):
        _single_repo_flow(args, env, repo, lain)


def _single_repo_flow(args, env, repo, lain):

    # `lain setup --agent generic`: configures and verifies a live connection;
    # installs no language server unasked (non-interactive).
    out = subprocess.run([lain, "setup", "--agent", "generic", "--json", "--no-model"],
                         cwd=repo, env=env, capture_output=True, text=True)
    try:
        rep = json.loads(out.stdout)
        ok = (rep["configuration"]["state"] == "configured" and rep["verification"]["healthy"]
              and "Python" in rep["languages"]
              and all(s["state"] in ("installed", "not_installed") for s in rep["language_servers"]))
        record("setup: configures an agent, verifies it, installs no language server unasked", ok,
               f"state={rep['configuration']['state']} verified={rep['verification']['healthy']} "
               f"tools={rep['verification'].get('tools_count')} languages={rep['languages']} "
               f"servers={[(s['server'], s['state']) for s in rep['language_servers']]}")
    except Exception as e:
        record("setup: configures an agent, verifies it, installs no language server unasked", False,
               f"exit {out.returncode}: {e}; {out.stdout[:300]} {out.stderr[-300:]}")

    # First query from a terminal.
    out = subprocess.run([lain, "oneshot", "get_blast_radius", "should_bypass_proxies"],
                         cwd=repo, env=env, capture_output=True, text=True, timeout=600)
    want = {"get_environ_proxies", "resolve_proxies"}
    got = set(re.findall(r"^\s+- (\w+) \(Function\)", out.stdout, re.M))
    record("first query: lain oneshot get_blast_radius", want <= got,
           f"direct+indirect callers include {sorted(want)}: {sorted(want & got)} (of {len(got)})")

    out = subprocess.run([lain, "doctor"], cwd=repo, env=env, capture_output=True, text=True)
    record("lain doctor: ready after indexing", out.returncode == 0 and "Agent-ready: YES" in out.stdout,
           f"exit {out.returncode}; {out.stdout.strip().splitlines()[-1] if out.stdout.strip() else out.stderr[-200:]}")

    mcp = Mcp(lain, repo, env)
    mcp.wait_ready()
    c = mcp.call
    t = c("find_symbol", {"name": "prepare_request"})
    record("find_symbol finds a method", "src/requests/sessions.py" in t, t.splitlines()[2] if len(t.splitlines()) > 2 else t[:200])
    t = c("search_code", {"query": "proxy bypass environment"})
    names = re.findall(r"^\d+\. \*\*(\w+)\*\*", t, re.M)
    record("search_code: search by name/meaning without a model",
           "should_bypass_proxies" in names[:5], f"top results {names[:5]}")
    t = c("assess_change", {"symbol": "merge_setting"})
    want = {"prepare_request", "merge_environment_settings", "merge_hooks"}
    got = callers_of(t)
    record("assess_change: callers before changing a symbol", want <= got, f"expected {sorted(want)} got {sorted(got)}")
    t = c("get_call_chain", {"from": "request", "to": "should_bypass_proxies"})
    record("get_call_chain: how two functions connect", "should_bypass_proxies" in t and "→" in t, t.strip().splitlines()[-1][:200])
    t = c("find_anchors", {"limit": 5})
    record("find_anchors: architectural entry points", bool(re.search(r"^1\. \w+", t, re.M)), t.strip().splitlines()[1][:120] if t.strip() else t)
    t = c("list_entry_points", {})
    record("list_entry_points answers", "error" not in t.lower()[:80] and len(t) > 20, t.strip()[:120].replace("\n", " | "))
    t = c("explain_dispatch", {"symbol": "dispatch_hook"})
    record("explain_dispatch: evidence, or insufficient_evidence",
           re.search(r"verdict: (static_only|static_and_heuristic|runtime|insufficient_evidence|\w+)", t) is not None,
           (re.search(r"verdict: \S+", t) or re.search(r".*", t)).group(0))
    t = c("get_coupling_radar", {"symbol": "src/requests/sessions.py"})
    record("co-change radar from git history", "co-change with" in t, t.strip().splitlines()[0][:160])
    t = c("understand_repository", {})
    record("understand_repository reports semantic search honestly",
           '"semantic_search"' in t and "unavailable_optional" in t.split('"semantic_search"')[1][:80], "no model loaded")

    # Coordination: two agents, one file.
    a = json.loads(c("register_agent", {"name": "alpha", "kind": "claude-code", "mode": "interactive"}))
    b = json.loads(c("register_agent", {"name": "beta", "kind": "codex", "mode": "interactive"}))
    g = c("claim_files", {"agent_id": a["agent_id"], "session_token": a["session_token"], "files": [{"path": "src/requests/api.py", "intent": "edit"}]})
    x = c("claim_files", {"agent_id": b["agent_id"], "session_token": b["session_token"], "files": [{"path": "src/requests/api.py", "intent": "edit"}]})
    record("advisory claims: a second editor sees the conflict", "granted" in g and "conflict" in x and "alpha" in x, "beta told alpha holds src/requests/api.py")
    i = c("lain_intent", {"agent_id": a["agent_id"], "session_token": a["session_token"], "goal": "acceptance: refactor api", "scopes": ["src/requests/api.py"]})
    record("intents: declare a goal, other agents see it", "intent_id" in i and "acceptance: refactor api" in c("list_active_intents", {}), "declared and listed")
    mcp.close()


def check_multi_repo_flow(args, env):
    """README 'Multiple repositories', verbatim, with tokio-rs/bytes + tokio."""
    if args.only and args.only.lower() not in "multi-repo flow":
        return
    lain, port = args.lain, 9987
    d = os.path.join(args.work, "multi")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    with in_dir(d):
        _multi_repo_flow(args, env, d, lain, port)


def _multi_repo_flow(args, env, d, lain, port):
    run = lambda *a: subprocess.run([lain, *a], cwd=d, env=env, capture_output=True, text=True, timeout=300)
    r1 = run("repos", "add", "bytes", "https://github.com/tokio-rs/bytes.git")
    r2 = run("repos", "add", "tokio", "https://github.com/tokio-rs/tokio.git")
    r3 = run("workspaces", "create", "tokio-stack", "--members", "bytes,tokio")
    repos = open(os.path.join(d, "repos.yaml")).read()
    record("repos add + workspaces create keep both repos", "id: bytes" in repos and "id: tokio" in repos
           and os.path.exists(os.path.join(d, "workspaces.yaml")), (r1.stdout + r2.stdout + r3.stdout).strip().replace("\n", " | "))
    srv = subprocess.Popen([lain, "server", "--config", "./repos.yaml", "--transport", "http", "--port", str(port)],
                           cwd=d, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        url = f"http://127.0.0.1:{port}"
        for _ in range(900):
            try:
                urllib.request.urlopen(url + "/health", timeout=2)
                break
            except Exception:
                time.sleep(1)

        def call(tool, a):
            req = urllib.request.Request(url + "/mcp", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                         "params": {"name": tool, "arguments": a}}).encode(), headers={"Content-Type": "application/json"})
            r = json.load(urllib.request.urlopen(req, timeout=300))
            return "".join(c.get("text", "") for c in r.get("result", {}).get("content", [])) or json.dumps(r)

        for _ in range(300):
            if '"ready":2' in call("get_federation_health", {}):
                break
            time.sleep(2)
        code = urllib.request.urlopen(url + "/", timeout=10).status
        record("Command Center serves", code == 200, f"GET / -> {code}")
        t = call("get_cross_repo_blast_radius_for_repo", {"repo_id": "bytes", "symbol": "put_slice", "depth": "1..2"})
        by = json.loads(t).get("by_repo", {}) if t.startswith("{") else {}
        record("cross-repo blast radius: tokio callers of bytes' put_slice", len(by.get("tokio", [])) > 0,
               f"by repo: { {k: len(v) for k, v in by.items()} }")
        t = call("search_org", {"query": "put_slice", "limit": 10})
        record("search_org finds symbols across repos", "bytes" in t and "tokio" in t, t[:160])
        t = call("get_health", {})
        record("get_health answers on a multi-repo server", "Repositories:** 2" in t, t.strip().splitlines()[0])
    finally:
        srv.terminate()
        srv.wait(timeout=30)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lain", required=True)
    ap.add_argument("--work", default=os.path.join(os.getcwd(), "target", "acceptance"))
    ap.add_argument("--only", default="")
    args = ap.parse_args()
    args.lain = os.path.abspath(args.lain)
    os.makedirs(args.work, exist_ok=True)
    home = os.path.join(args.work, "home")
    os.makedirs(home, exist_ok=True)
    # A clean user: no config, no installed language servers from the
    # caller's environment, no MCP client configs touched.
    env = {**os.environ, "HOME": home, "XDG_CONFIG_HOME": os.path.join(home, ".config"),
           "XDG_STATE_HOME": os.path.join(home, ".state"), "XDG_CACHE_HOME": os.path.join(home, ".cache"),
           "LAIN_TOOL_PROFILE": "full"}
    print(f"lain: {subprocess.run([args.lain, '--version'], capture_output=True, text=True).stdout.strip()}\n")
    check_languages(args, env)
    check_single_repo_flow(args, env)
    check_multi_repo_flow(args, env)
    failed = [r for r in RESULTS if not r[1]]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} claims hold.")
    for n, _, d in failed:
        print(f"  FAIL {n}: {d}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
