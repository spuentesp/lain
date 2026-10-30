#!/usr/bin/env python3
"""Measure rust-analyzer's peak RSS under Lain's two LSP profiles.

Run: python3 scripts/lsp-memory-probe.py [workspace] [seconds]

Lain starts rust-analyzer with one of two sets of `initializationOptions`:

  fidelity (default)  checkOnSave + rust-analyzer diagnostics off.
                       Every symbol rust-analyzer resolves is the symbol
                       the user builds.

  lean (LAIN_LSP_LEAN=1)
                       additionally: proc-macro expansion off, build
                       scripts off, the workspace member's own `default`
                       feature off, `target`/`.git` excluded from
                       indexing.

This script starts a real rust-analyzer under each profile, lets it
index the workspace, samples the peak RSS of the process tree (rust-analyzer
plus its proc-macro server), and prints the difference.

The lean profile trades symbol fidelity for memory: derive-generated
impls, `env!("OUT_DIR")` code and default-feature code stop being
visible to `textDocument/references`. Read the delta as "what the memory
costs you in symbols", not as a free win.
"""
import json
import os
import subprocess
import sys
import time

FIDELITY = {
    "checkOnSave": False,
    "diagnostics": {"enable": False},
}
LEAN = {
    "checkOnSave": False,
    "diagnostics": {"enable": False},
    "procMacro": {"enable": False},
    "cargo": {"buildScripts": {"enable": False}, "noDefaultFeatures": True},
    "files": {"exclude": ["target", ".git"]},
}


def rss_kb(pid):
    try:
        with open(f"/proc/{pid}/statm") as f:
            return int(f.read().split()[1]) * (os.sysconf("SC_PAGE_SIZE") // 1024)
    except OSError:
        return 0


def descendants(pid):
    out = []
    try:
        with open(f"/proc/{pid}/task/{pid}/children") as f:
            out = [int(x) for x in f.read().split()]
    except OSError:
        pass
    for child in list(out):
        out.extend(descendants(child))
    return out


def tree_rss_kb(pid):
    return rss_kb(pid) + sum(rss_kb(c) for c in descendants(pid))


def frame(obj):
    body = json.dumps(obj).encode()
    return b"Content-Length: %d\r\n\r\n%s" % (len(body), body)


def run(label, init_opts, workspace, seconds):
    proc = subprocess.Popen(
        ["rust-analyzer"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        cwd=workspace,
    )
    proc.stdin.write(
        frame(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": os.getpid(),
                    "rootUri": "file://" + workspace,
                    "workspaceFolders": [
                        {"uri": "file://" + workspace, "name": "workspace"}
                    ],
                    "capabilities": {
                        "workspace": {
                            "configuration": True,
                            "didChangeConfiguration": {"dynamicRegistration": True},
                            "symbol": {"dynamicRegistration": True},
                        },
                        "textDocument": {
                            "synchronization": {"dynamicRegistration": True},
                            "documentSymbol": {"dynamicRegistration": True},
                            "references": {"dynamicRegistration": True},
                        },
                    },
                    "initializationOptions": init_opts,
                },
            }
        )
    )
    proc.stdin.flush()
    proc.stdin.write(frame({"jsonrpc": "2.0", "method": "initialized", "params": {}}))
    proc.stdin.flush()

    peak = 0
    deadline = time.time() + seconds
    while time.time() < deadline:
        peak = max(peak, tree_rss_kb(proc.pid))
        time.sleep(0.5)

    proc.kill()
    proc.wait()
    print(f"{label:10s} peak RSS {peak / 1024:7.0f} MiB")
    return peak


def main():
    workspace = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.getcwd())
    seconds = int(sys.argv[2]) if len(sys.argv) > 2 else 90

    if not shutil_which("rust-analyzer"):
        print("rust-analyzer is not on PATH; install it with "
              "`rustup component add rust-analyzer`")
        return 1

    print(f"workspace: {workspace}")
    print(f"indexing window: {seconds}s per profile\n")
    fidelity = run("fidelity", FIDELITY, workspace, seconds)
    lean = run("lean", LEAN, workspace, seconds)

    saved = fidelity - lean
    pct = 100.0 * saved / fidelity if fidelity else 0.0
    print(f"\nlean saves {saved / 1024:.0f} MiB of peak RSS ({pct:.1f}%)")
    print("Remember: the lean profile hides derive-generated impls, "
          "build-script output and default-feature code.")
    return 0


def shutil_which(binary):
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = os.path.join(directory, binary)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


if __name__ == "__main__":
    sys.exit(main())
