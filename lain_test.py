#!/usr/bin/env python3
"""
Lain MCP Server - End-to-End Test & Evaluation Script

Usage:
    python3 lain_test.py [--workspace PATH] [--binary PATH]

Defaults to testing against the lain repo itself using target/release/lain.
Runs through all major tools and scores quality of results.
"""

import subprocess
import json
import sys
import time
import argparse
import os
from typing import Optional

BINARY = "./target/release/lain"
WORKSPACE = "."

# ── colours ──────────────────────────────────────────────────────────────────
GRN = "\033[92m"
YEL = "\033[93m"
RED = "\033[91m"
BLU = "\033[94m"
CYN = "\033[96m"
DIM = "\033[2m"
RST = "\033[0m"
BOL = "\033[1m"

def ok(msg):  print(f"  {GRN}✓{RST} {msg}")
def warn(msg):print(f"  {YEL}⚠{RST} {msg}")
def fail(msg):print(f"  {RED}✗{RST} {msg}")
def hdr(msg): print(f"\n{BOL}{BLU}{'─'*60}{RST}\n{BOL}{msg}{RST}")
def sub(msg): print(f"\n{CYN}▶ {msg}{RST}")

# ── MCP client ────────────────────────────────────────────────────────────────
class LainClient:
    def __init__(self, binary: str, workspace: str):
        self.proc = subprocess.Popen(
            [binary, "--workspace", workspace],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._id = 0

    def _next_id(self):
        self._id += 1
        return self._id

    def send(self, method: str, params: dict = None) -> dict:
        msg = {"jsonrpc": "2.0", "id": self._next_id(), "method": method, "params": params or {}}
        line = json.dumps(msg) + "\n"
        self.proc.stdin.write(line)
        self.proc.stdin.flush()
        raw = self.proc.stdout.readline()
        if not raw:
            raise RuntimeError("Server closed stdout")
        return json.loads(raw)

    def call(self, tool: str, args: dict = None) -> str:
        resp = self.send("tools/call", {"name": tool, "arguments": args or {}})
        if "error" in resp:
            return f"[ERROR] {resp['error']}"
        content = resp.get("result", {}).get("content", [])
        if content:
            return content[0].get("text", "")
        return ""

    def initialize(self):
        return self.send("initialize", {
            "protocolVersion": "2024-11-05",
            "clientInfo": {"name": "lain-test", "version": "1.0"},
            "capabilities": {},
        })

    def list_tools(self) -> list:
        resp = self.send("tools/list", {})
        return resp.get("result", {}).get("tools", [])

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=3)
        except Exception:
            self.proc.kill()


# ── scoring helpers ────────────────────────────────────────────────────────────
def score_result(text: str, expect_keywords: list, label: str) -> int:
    """Returns 0-3: 0=empty, 1=minimal, 2=good, 3=rich"""
    if not text or len(text) < 20:
        fail(f"{label}: empty response")
        return 0
    hits = sum(1 for kw in expect_keywords if kw.lower() in text.lower())
    if hits == 0:
        warn(f"{label}: responded but no expected keywords {expect_keywords}")
        return 1
    if hits < len(expect_keywords) // 2 + 1:
        ok(f"{label}: partial match ({hits}/{len(expect_keywords)} keywords)")
        return 2
    ok(f"{label}: rich response ({hits}/{len(expect_keywords)} keywords)")
    return 3

def preview(text: str, lines: int = 6) -> str:
    rows = text.strip().splitlines()[:lines]
    return "\n".join(f"    {DIM}{r}{RST}" for r in rows)


# ── test suite ─────────────────────────────────────────────────────────────────
def run_tests(client: LainClient):
    scores = {}

    # ── 1. Initialize ──────────────────────────────────────────────────────────
    hdr("1. MCP Handshake")
    init = client.initialize()
    server_info = init.get("result", {}).get("serverInfo", {})
    print(f"  Server: {server_info.get('name')} v{server_info.get('version')}")
    print(f"  Protocol: {init.get('result', {}).get('protocolVersion')}")
    if "lain" in server_info.get("name", "").lower():
        ok("Initialize succeeded")
        scores["init"] = 3
    else:
        fail("Unexpected server name")
        scores["init"] = 0

    # ── 2. Tools list ─────────────────────────────────────────────────────────
    hdr("2. Tool Registry")
    tools = client.list_tools()
    tool_names = [t["name"] for t in tools]
    expected = ["find_anchors", "get_blast_radius", "semantic_search",
                "get_call_chain", "explore_architecture", "get_health", "run_enrichment"]
    present = [t for t in expected if t in tool_names]
    print(f"  Total tools: {len(tools)}")
    print(f"  Expected: {len(present)}/{len(expected)} present")
    for name in tool_names:
        print(f"    {DIM}• {name}{RST}")
    scores["tools"] = 3 if len(present) == len(expected) else 1

    # ── 3. Health check ───────────────────────────────────────────────────────
    hdr("3. get_health")
    sub("Checking server health and graph stats")
    health = client.call("get_health")
    print(preview(health, 12))
    scores["health"] = score_result(health,
        ["Static Nodes", "Static Edges", "Operational"],
        "get_health")

    # ── 4. Master map (staleness) ─────────────────────────────────────────────
    hdr("4. get_master_map")
    sub("Staleness report — when was each module last indexed?")
    mmap = client.call("get_master_map")
    print(preview(mmap, 10))
    scores["master_map"] = score_result(mmap,
        ["src", "Fresh", "Stale"],
        "get_master_map")

    # ── 5. Architecture exploration ───────────────────────────────────────────
    hdr("5. explore_architecture")
    sub("High-level module tree (depth 2)")
    arch = client.call("explore_architecture", {"max_depth": 2})
    print(preview(arch, 10))
    scores["arch"] = score_result(arch,
        ["src", "anchor", "file"],
        "explore_architecture")

    # ── 6. Entry points ───────────────────────────────────────────────────────
    hdr("6. list_entry_points")
    sub("Find main() and architectural roots")
    ep = client.call("list_entry_points")
    print(preview(ep, 8))
    scores["entry_points"] = score_result(ep,
        ["main", "entry"],
        "list_entry_points")

    # ── 7. Find anchors ───────────────────────────────────────────────────────
    hdr("7. find_anchors")
    sub("Most central/foundational symbols by fan_in/fan_out ratio")
    anchors = client.call("find_anchors", {"limit": 10})
    print(preview(anchors, 12))
    scores["anchors"] = score_result(anchors,
        ["anchor", "score", "fan"],
        "find_anchors")

    # ── 8. Semantic search ────────────────────────────────────────────────────
    hdr("8. semantic_search")
    queries = [
        ("error handling", ["error", "LainError", "Result"]),
        ("graph storage and persistence", ["graph", "GraphDatabase", "bin"]),
        ("LSP language server", ["lsp", "symbol", "language"]),
    ]
    search_score = 0
    for query, keywords in queries:
        sub(f"Query: '{query}'")
        result = client.call("semantic_search", {"query": query, "limit": 5})
        print(preview(result, 6))
        s = score_result(result, keywords, f"semantic_search('{query}')")
        search_score += s
    scores["semantic_search"] = search_score // len(queries)

    # ── 9. Blast radius ───────────────────────────────────────────────────────
    hdr("9. get_blast_radius")
    blast_symbols = ["LainError", "GraphDatabase", "ToolExecutor"]
    blast_score = 0
    for sym in blast_symbols:
        sub(f"Symbol: '{sym}' — what breaks if this changes?")
        result = client.call("get_blast_radius", {"symbol": sym, "include_coupling": True})
        print(preview(result, 6))
        s = score_result(result, [sym, "blast", "affected"],
            f"get_blast_radius({sym})")
        blast_score += s
    scores["blast_radius"] = blast_score // len(blast_symbols)

    # ── 10. Trace dependency ──────────────────────────────────────────────────
    hdr("10. trace_dependency")
    sub("What does 'ToolExecutor' depend on?")
    dep = client.call("trace_dependency", {"symbol": "ToolExecutor"})
    print(preview(dep, 8))
    scores["trace_dep"] = score_result(dep,
        ["ToolExecutor", "depend", "edge"],
        "trace_dependency")

    # ── 11. Call chain ────────────────────────────────────────────────────────
    hdr("11. get_call_chain")
    sub("Call path from 'main' to 'get_health'")
    chain = client.call("get_call_chain", {"from": "main", "to": "get_health"})
    print(preview(chain, 8))
    scores["call_chain"] = score_result(chain,
        ["main", "chain", "path", "call"],
        "get_call_chain")

    # ── 12. Navigate to anchor ────────────────────────────────────────────────
    hdr("12. navigate_to_anchor")
    sub("From leaf 'embed' — trace back to foundational anchor")
    nav = client.call("navigate_to_anchor", {"symbol": "embed"})
    print(preview(nav, 8))
    scores["navigate"] = score_result(nav,
        ["anchor", "score", "navigate"],
        "navigate_to_anchor")

    # ── 13. Explain symbol ────────────────────────────────────────────────────
    hdr("13. explain_symbol")
    sub("Full architectural summary for 'ToolExecutor'")
    explain = client.call("explain_symbol", {"symbol": "ToolExecutor"})
    print(preview(explain, 12))
    scores["explain"] = score_result(explain,
        ["ToolExecutor", "depth", "anchor"],
        "explain_symbol")

    # ── 14. Suggest refactor ──────────────────────────────────────────────────
    hdr("14. suggest_refactor_targets")
    sub("Find high-debt / high-coupling code")
    refactor = client.call("suggest_refactor_targets", {"limit": 5})
    print(preview(refactor, 10))
    scores["refactor"] = score_result(refactor,
        ["refactor", "debt", "fan-out"],
        "suggest_refactor_targets")

    # ── 15. Agent strategy ────────────────────────────────────────────────────
    hdr("15. get_agent_strategy")
    sub("The operational manual for AI agents")
    strategy = client.call("get_agent_strategy")
    print(preview(strategy, 8))
    scores["strategy"] = score_result(strategy,
        ["agent", "strategy", "tool", "map"],
        "get_agent_strategy")

    # ── 16. run_enrichment + wait + re-test anchors ───────────────────────────
    hdr("16. run_enrichment → re-test anchors")
    sub("Running enrichment (co-change + anchor scores + depths)…")
    enrich = client.call("run_enrichment")
    print(f"    {DIM}{enrich}{RST}")
    print("  Waiting 4s for background job…")
    time.sleep(4)
    sub("Re-checking anchors after enrichment")
    anchors2 = client.call("find_anchors", {"limit": 5})
    print(preview(anchors2, 8))
    scores["post_enrich"] = score_result(anchors2,
        ["anchor", "score"],
        "find_anchors (post-enrichment)")

    # ── Summary ───────────────────────────────────────────────────────────────
    hdr("SCORE SUMMARY")
    total = sum(scores.values())
    max_total = len(scores) * 3
    pct = int(total / max_total * 100)

    grade = GRN + "EXCELLENT" if pct >= 80 else (YEL + "GOOD" if pct >= 60 else (YEL + "PARTIAL" if pct >= 40 else RED + "POOR"))

    for name, s in scores.items():
        bar = "█" * s + "░" * (3 - s)
        colour = GRN if s == 3 else (YEL if s >= 1 else RED)
        print(f"  {colour}{bar}{RST}  {name}")

    print(f"\n  {BOL}Total: {total}/{max_total} ({pct}%)  →  {grade}{RST}\n")

    return scores


# ── main ───────────────────────────────────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(description="Lain MCP end-to-end test")
    parser.add_argument("--workspace", default=WORKSPACE, help="Workspace path to index")
    parser.add_argument("--binary", default=BINARY, help="Path to lain binary")
    args = parser.parse_args()

    binary = args.binary
    workspace = os.path.abspath(args.workspace)

    if not os.path.exists(binary):
        print(f"{RED}Binary not found: {binary}{RST}")
        print("Build with: cargo build --release")
        sys.exit(1)

    if not os.path.isdir(workspace):
        print(f"{RED}Workspace not found: {workspace}{RST}")
        sys.exit(1)

    print(f"\n{BOL}Lain MCP Server — End-to-End Test{RST}")
    print(f"  Binary:    {binary}")
    print(f"  Workspace: {workspace}")

    client = LainClient(binary, workspace)
    print(f"\n  {DIM}Starting server and waiting for background indexer…{RST}")

    # Poll get_health until node count stabilizes (indexer runs in background)
    prev_nodes = -1
    stable_rounds = 0
    for attempt in range(30):
        time.sleep(2)
        try:
            init_resp = client.initialize() if attempt == 0 else None
            health = client.call("get_health") if attempt > 0 else ""
            if not health:
                time.sleep(1)
                continue
            import re
            m = re.search(r"Static Nodes:\*\* (\d+)", health)
            if m:
                nodes = int(m.group(1))
                print(f"  {DIM}[{attempt*2}s] Static nodes: {nodes}{RST}", end="\r")
                if nodes == prev_nodes:
                    stable_rounds += 1
                    if stable_rounds >= 3 and nodes > 0:
                        break
                else:
                    stable_rounds = 0
                prev_nodes = nodes
        except Exception:
            pass
    print(f"\n  Indexer settled at {prev_nodes} static nodes.")

    try:
        run_tests(client)
    except KeyboardInterrupt:
        print("\nInterrupted.")
    except Exception as e:
        print(f"\n{RED}Test failed with exception: {e}{RST}")
        import traceback; traceback.print_exc()
    finally:
        client.close()


if __name__ == "__main__":
    main()
