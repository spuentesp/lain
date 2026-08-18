# Multiplayer Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the gap between what `docs/wish-list.md` marks as addressed and what actually happens when a real, unconfigured agent edits a file. Wishlist items #1 (fail open), #2 (identity), and #5 (conflict shape) are marked done; measured against a live Claude Code session on 2026-08-17, all three still fail.

**Architecture:** Bash hardening (Tasks 1–2), a small `reqwest` client change (Task 2), name/timestamp plumbing in `presence.rs` + `presence_tools.rs` (Task 3), a new `--symbols` path through `lain hooks claim` (Task 4), and reaping + persistence fixes in `OccupancyMap` (Task 5). No new Cargo deps.

**Branch:** `fix/multiplayer-hardening` off `main`.

**Reference:** `docs/wish-list.md`, `docs/multiplayer.md`, `docs/multiplayer-e2e-report.md`.

---

## Why this plan exists

The primitives are correct and were verified working end-to-end: read-vs-edit filtering, symbol-level conflict scoping, and per-symbol occupancy all behave exactly as designed. `OccupancyMap::claim` is good code. The failures are all in the plumbing between that code and a real agent.

Measured on `main` @ `caa0427`:

```
# The shipped identity ladder, run inside a live Claude Code session:
→ FALLBACK: claude-code-1019114-sebastian-victus
  CLAUDE_AGENT_NAME  (unset)   MCP_CLIENT_NAME  (unset)   AGENT_NAME  (unset)

# lain hooks claim, server down:
Error: HTTP send
exit code: 1

# Two agents, same symbol, edit intent:
{"conflicts":[{"agent_id":"73f840d3-…","intent":"edit","symbols":[],
               "name":"<unknown>","last_touched_unix":0}],"granted":[]}
```

---

## Global Constraints

- **No new Cargo deps.** `reqwest`, `serde`, `uuid`, `blake3` are already in.
- **Backwards-compatible JSON.** Tasks 3–4 add and populate fields; no field is removed or retyped.
- **Fail-open is the invariant.** No change in this plan may introduce a path where a coordination failure blocks an edit. That includes hangs, which are currently unhandled.
- **All existing tests must pass:** 493 lib + 20 presence + persistence/presence/attribution e2e.

---

## File Structure

```
hooks/
├── claude-code/{pre,post}-edit.sh        (Task 1: identity; Task 2: timeout; Task 4: symbols)
├── kimi/pre-edit.sh                       (Tasks 1, 2)
├── agy/pre-edit.sh                        (Tasks 1, 2)
└── codex/pre-edit.sh                      (Tasks 1, 2)

src/cli/
├── hooks.rs                               (Task 2: exit 0 + client timeout; Task 4: --symbols;
│                                           Task 6: 0600 perms + name sanitize)
└── doctor.rs                              (Task 1: --identity subcommand)

src/server/
├── presence.rs                            (Task 3: file-level last_touched;
│                                           Task 5: reaping + persist intents/last_touched)
├── mcp/presence_tools.rs                  (Task 3: resolve agent name in conflicts)
└── config/mod.rs                          (Task 5: state key off repos.yaml, not temp workspace)

tests/
├── presence.rs                            (Tasks 3, 5)
└── e2e/multiplayer-identity.sh            (new — Task 7)
```

---

## Task 1: Identity that works without configuration

**Files:** Modify `hooks/claude-code/{pre,post}-edit.sh`, `hooks/{kimi,agy,codex}/pre-edit.sh`; modify `src/cli/doctor.rs`

**Goal:** Two Claude Code consoles on one machine get two stable, distinct identities with zero configuration — the actual ask in wishlist #2.

**Context:** Commit `a0b87f7` ("remove Orca-specific identity detection — use generic agent env vars") replaced `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_WORKTREE_ID` with `CLAUDE_AGENT_NAME` / `MCP_CLIENT_NAME` / `AGENT_NAME`. None of those three is set by Claude Code, so every rung falls through to the PID fallback. The commit removed a signal that was present and added three that are not.

- [ ] **Step 1: Rebuild the ladder on env vars that actually exist**

Verified present in a live Claude Code session: `CLAUDE_CODE_SESSION_ID` (a stable per-session UUID — one console, one value, distinct across windows, shared by subagents in the same process), `CLAUDE_PID`, `CLAUDECODE=1`, `AI_AGENT`. Also present when running under Orca: `ORCA_WORKTREE_ID`, `ORCA_WORKSPACE_ID`.

```bash
# Identity resolution order:
#   1. $LAIN_AGENT_NAME                  — explicit lain override
#   2. $CLAUDE_CODE_SESSION_ID (short)   — stable per-console UUID, set by Claude Code
#   3. $ORCA_WORKTREE_ID                 — one identity per Orca worktree
#   4. Generic vars other frameworks may set
#   5. $CLAUDE_PID, then $PPID           — last-resort process identity
if   [ -n "$LAIN_AGENT_NAME" ];        then AGENT_NAME="$LAIN_AGENT_NAME"
elif [ -n "$CLAUDE_CODE_SESSION_ID" ]; then AGENT_NAME="claude-code-${CLAUDE_CODE_SESSION_ID%%-*}"
elif [ -n "$ORCA_WORKTREE_ID" ];       then AGENT_NAME="claude-code-${ORCA_WORKTREE_ID%%::*}"
elif [ -n "$CLAUDE_AGENT_NAME" ];      then AGENT_NAME="claude-code-$CLAUDE_AGENT_NAME"
elif [ -n "$MCP_CLIENT_NAME" ];        then AGENT_NAME="$MCP_CLIENT_NAME"
elif [ -n "$LAIN_GENERIC_AGENT_NAME" ];then AGENT_NAME="$LAIN_GENERIC_AGENT_NAME"
else
    SHORT_HOST=$(hostname -s 2>/dev/null || echo host)
    AGENT_NAME="claude-code-${CLAUDE_PID:-${PPID:-0}}-${SHORT_HOST}"
fi
```

Two details that matter:

- Keep the generic rungs, but **below** the ones that exist — they cost nothing and help other frameworks.
- Drop the `elif [ -n "$AGENT_NAME" ]; then AGENT_NAME="$AGENT_NAME"` branch present in all five scripts. It is a self-assignment, and worse, it lets an unrelated environment variable named `AGENT_NAME` silently hijack the agent's identity. The rename to `LAIN_GENERIC_AGENT_NAME` above removes that collision.

- [ ] **Step 2: Apply the same ladder to every script**

All five scripts carry a copy. `hooks/claude-code/post-edit.sh` matters as much as `pre-edit.sh`: if the two resolve different names, `release` targets a different session than `claim` did and the claim leaks. Keep them byte-identical in this block, with the per-agent `kind` the only difference.

Consider extracting the block to `hooks/common/identity.sh` sourced by each script — but only if every agent harness tolerates a `source` of a sibling path. If unsure, keep the copies and add a test (Step 4) that diffs them.

- [ ] **Step 3: Add `lain doctor --identity`**

This is the check that makes the bug class visible instead of silent:

```
$ lain doctor --identity
resolved agent name : claude-code-397b6de8
via                 : CLAUDE_CODE_SESSION_ID
session file        : ~/.config/lain/hooks/claude-code-397b6de8.session (exists, age 4m)
would claim as      : claude-code-397b6de8 / kind=claude-code
```

Print the rung that matched, not just the result — "via PPID fallback" is the signal that the ladder is broken again.

- [ ] **Step 4: Test**

Add a shell test asserting the identity block is identical across all five scripts, and that with only `CLAUDE_CODE_SESSION_ID` set the ladder does not reach the PID fallback.

**Verification:**
```bash
CLAUDE_CODE_SESSION_ID=abcd1234-... bash hooks/claude-code/pre-edit.sh /tmp/x.rs  # → claude-code-abcd1234
env -u CLAUDE_CODE_SESSION_ID bash hooks/claude-code/pre-edit.sh /tmp/x.rs        # → PID fallback
lain doctor --identity
```

---

## Task 2: Fail open in the binary, and against hangs

**Files:** Modify `src/cli/hooks.rs`; modify all five hook scripts

**Goal:** Nothing in the claim path can block an edit — not an error, and not a hang.

**Context:** The scripts got `set +e` + `trap 'exit 0' ERR` in PR 12, which is correct. But wishlist #1 named `lain hooks claim` itself, and that still exits 1 on an unreachable server. Any integration that calls the binary without the bash wrapper — the Kimi plugin, a Codex hook, someone's own harness — inherits a blocking failure.

The hang case is worse, because no wrapper catches it. `post_mcp` (`src/cli/hooks.rs:112`) builds `reqwest::blocking::Client::new()` with **no timeout**. Connection-refused on localhost returns in 28 ms so it looks fine, but a filtered host, a stopped container, or a laptop that changed networks blocks on the TCP handshake indefinitely — and every `Edit` blocks with it.

- [ ] **Step 1: Give the client a timeout**

```rust
let client = reqwest::blocking::Client::builder()
    .timeout(Duration::from_millis(
        std::env::var("LAIN_HOOK_TIMEOUT_MS").ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
    ))
    .connect_timeout(Duration::from_millis(150))
    .build()
    .unwrap_or_else(|_| reqwest::blocking::Client::new());
```

300 ms is the budget: a coordination layer must be strictly faster than the edit it guards. The env var exists so a slow remote server can be accommodated without a rebuild.

- [ ] **Step 2: Make `hooks claim` and `hooks release` always exit 0**

Route both through a wrapper that logs the error to stderr and returns `Ok(())`. The exit code carries no information anyone acts on, and a non-zero code is the exact thing that caused the original lockout.

Keep the failure *visible*: one line to stderr, prefixed `lain hook:`, never to stdout (stdout is parsed by some harnesses).

- [ ] **Step 3: Belt and braces in the scripts**

```bash
timeout 2 lain hooks claim --url "$LAIN_URL" … 2>&1 | head -1 >&2
exit 0
```

`timeout` guards against a hang in a code path the Rust timeout does not cover (DNS, a wedged binary). If `timeout` is absent from the system, the `command -v` guard pattern already in the scripts applies.

- [ ] **Step 4: Test**

```bash
# unreachable host that drops packets rather than refusing
time lain hooks claim --url http://10.255.255.1:9999/mcp --path x.rs \
     --agent-name t --agent-kind claude-code --intent edit; echo "exit=$?"
# must return in < 1s with exit=0
```

**Verification:** Both commands above return `exit=0` in under a second. `cargo test --workspace --features test-utils` green.

---

## Task 3: Conflicts that name the other agent and when they touched it

**Files:** Modify `src/server/presence.rs`, `src/server/mcp/presence_tools.rs`; modify `tests/presence.rs`

**Goal:** A conflict payload carries enough for an agent to decide. Wishlist #5 asked for *which symbols* and *when they last touched it*; today the name is `<unknown>` and the timestamp is `0`.

- [ ] **Step 1: Resolve the agent name**

`presence.rs:583`, `:602`, and `:626` each hardcode `name: "<unknown>".into()`. The reason is structural — `OccupancyMap` has no handle on `PresenceRegistry`, so it cannot resolve an `AgentId` to a name. `list_occupancy` resolves names correctly because it runs where both are in scope.

Prefer resolving at the call site rather than coupling the two types: `run_claim_files` in `presence_tools.rs` already holds both, so have it fill in `name` on each `ConflictEntry` before serializing. Leave `OccupancyMap::claim` free of the registry dependency.

If a name genuinely cannot be resolved (the holder's session expired between claim and read), emit `"<expired>"` rather than `"<unknown>"` — the two mean different things and the caller should be able to tell them apart.

- [ ] **Step 2: Record a file-level timestamp for symbol-scoped claims**

`presence.rs:643-653` records `last_touched` under `"__file_level__"` **only** in the `req.symbols.is_empty()` branch. A symbol-scoped claim therefore records `last_touched` under the symbol name but never under the file-level sentinel — so when a later *file-level* claim conflicts with it, `last_touched_unix_for(other)` finds nothing and returns the epoch, which serializes as `0`.

Record both: always stamp `"__file_level__"`, and additionally stamp each named symbol.

```rust
entry.last_touched.entry("__file_level__".into()).or_default().insert(agent_id.clone(), now);
for sym in &req.symbols {
    entry.symbols.entry(sym.clone()).or_default().insert(agent_id.clone());
    entry.intents.entry(sym.clone()).or_default().insert(agent_id.clone(), req.intent.clone());
    entry.last_touched.entry(sym.clone()).or_default().insert(agent_id.clone(), now);
}
```

- [ ] **Step 3: Have the hook print a sentence, not JSON**

`lain hooks claim` currently pipes `head -1` of the raw JSON to stderr. Claude Code's `PreToolUse` hooks can return structured output that reaches the model's context — use it, and make the text something an agent can act on:

```
claude-B has held an edit claim on login() in src/auth.rs for 4m.
```

Keep the JSON available behind `--json` for scripts and the e2e harness.

- [ ] **Step 4: Test both failure shapes**

- Two edit claims on the same symbol → conflict carries the holder's real name and a `last_touched_unix` within a second of the claim.
- A symbol-scoped claim followed by a conflicting *file-level* claim → `last_touched_unix != 0`. This is the exact case that regressed.

**Verification:** `cargo test --test presence`, plus a manual two-agent conflict against a running server showing a real name and timestamp.

---

## Task 4: Claim symbols, not whole files

**Files:** Modify `src/cli/hooks.rs`, `hooks/claude-code/pre-edit.sh`

**Goal:** The symbol-level granularity that `docs/multiplayer-e2e-report.md` Scenario C demonstrates becomes reachable through the shipped hooks.

**Context:** `lain hooks claim` takes `--path` and no symbol argument, so every real agent edit takes a **file-level** claim — which overlaps every symbol claim in that file. Scenario C proves the feature by driving the MCP tool directly, not the hook. In practice, two agents in one large file conflict every time, and an agent that sees a warning on every edit learns to ignore warnings.

- [ ] **Step 1: Add `--symbols` to `lain hooks claim`**

Comma-separated, passed straight through to the `claim_files` request's `symbols` array. Absent → today's file-level behavior, unchanged.

- [ ] **Step 2: Add `--resolve-from-content` for the hook path**

Claude Code's `PreToolUse` payload carries `old_string`. Give the hook a way to hand that to lain and let the server resolve the enclosing symbol against the graph it already has — `GraphDatabase::get_node_at_location` and `find_node_by_path` are the primitives.

Resolution failure must fall back to a file-level claim silently. A coordination layer that refuses to claim because it could not resolve a symbol is worse than one that claims coarsely.

> **Dependency:** on a federation-mode server this needs the graph wiring from `2026-08-17-federation-tool-wiring.md`, because today `ToolContext.graph` is empty. Either land that plan first, or resolve against `FederatedIndex` directly here.

- [ ] **Step 3: Extract the symbol in the hook**

```bash
OLD_STRING="$(printf '%s' "$STDIN_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("old_string",""))' 2>/dev/null || true)"
```

Guard on `python3` being present; without it, fall through to a file-level claim. Do not add a JSON-parsing dependency to the hook — the existing `sed` extraction is deliberately dependency-free, and this must degrade the same way.

- [ ] **Step 4: Test** that a hook-driven claim on a file with a known symbol produces a symbol-scoped entry in `list_occupancy`, and that two agents editing different functions in one file do **not** conflict.

**Verification:**
```bash
call list_occupancy '{}'   # symbols array non-empty for hook-driven claims
```

---

## Task 5: Reap orphans, and make persistence real

**Files:** Modify `src/server/presence.rs`, `src/config/mod.rs`; modify `tests/presence.rs`

**Goal:** Occupancy reflects agents that exist. Restarts either preserve claims or drop them — not accumulate ghosts.

**Context:** Three separate defects compound here.

1. **Orphan claims.** On a fresh server, `list_occupancy` returned a live claim on `Cargo.toml` held by an agent with no session and `agent_names: []` — a dead `parent-agent` from an earlier hook run. Nothing drops a claim whose owner is gone, so it produces false conflicts forever.

2. **State key includes the PID.** `state_path_for_workspace` (`src/config/mod.rs:82`) derives the filename from the workspace path's `file_stem()`. In federation mode the workspace is `/tmp/lain-federation-{pid}-{counter}`, so **every server start writes a new state file**. `docs/multiplayer.md:105` claims claims survive restarts; in the only mode that ships, they cannot — and each restart leaks another file.

3. **`intents` and `last_touched` are not persisted.** `save_pair` (`presence.rs:945`) writes `sessions`, `occupancy_by_file`, and `occupancy_by_agent` only. Restored claims lose their intent and timestamp, so after a restart every restored claim behaves like an intent-less, epoch-dated ghost — which then conflicts with everything under the Task 3 logic.

- [ ] **Step 1: Reap claims whose agent has no live session**

On load, and on the existing expiry timer, drop any claim whose `agent_id` is absent from `PresenceRegistry`. Where a claim is deliberately retained across a restart (below), mark it rather than drop it — `list_occupancy` should report `stale: true` with an age so a caller can weigh it.

- [ ] **Step 2: Key state on the config, not the workspace**

Use the `repos.yaml` path — already threaded into both federation constructors as `repos_yaml: Option<PathBuf>` — as the state key. Same project, same state file, across restarts and PIDs. Fall back to today's behavior only when no config path is available.

- [ ] **Step 3: Persist `intents` and `last_touched`**

Add both to the serialized shape. Old state files lack the fields, so deserialize them as empty via `#[serde(default)]` and treat a missing intent as `edit` (the conservative reading) and a missing timestamp as "unknown", which the Task 3 payload must be able to express — use `Option<u64>` rather than overloading `0`.

- [ ] **Step 4: Test**

- A claim by an agent whose session is removed is reaped (or marked stale) on the next sweep.
- Restart with the same `repos.yaml` restores claims *with* intent and timestamp intact.
- An old-format state file without the new fields loads without error.

**Verification:** `cargo test --test presence --test persistence_e2e`

---

## Task 6: Credential and filename hygiene

**Files:** Modify `src/cli/hooks.rs`

- [ ] **Step 1: Write session files `0600`.** `write_session` (`hooks.rs:77`) uses `std::fs::write`, which lands at `0644` — world-readable. The file contains `session_token`, the bearer credential for every subsequent presence call. Use `OpenOptions::new().mode(0o600)` under `#[cfg(unix)]`.

- [ ] **Step 2: Do the same for the presence state file.** `~/.local/lain/state/*.json` also contains session tokens and is also `0644`.

- [ ] **Step 3: Sanitize the agent name before using it as a filename.** `session_path` (`hooks.rs:66`) interpolates `agent_name` straight into a path, and that name comes from `$LAIN_AGENT_NAME`. `LAIN_AGENT_NAME=../../../x` writes outside the hooks directory. Low severity — it is local and self-inflicted — but it is a one-line fix: reuse the same character filter `state_path_for_workspace` already applies.

**Verification:** `stat -c '%a' ~/.config/lain/hooks/*.session` → `600`; a name containing `/` or `..` resolves to a file inside the hooks dir.

---

## Task 7: Prove it against an unconfigured agent

**Files:** Create `tests/e2e/multiplayer-identity.sh`; modify `tests/e2e/multiplayer-full.sh`

**Goal:** The regression that shipped in PR 12 cannot ship again.

**Context:** `tests/e2e/multiplayer-full.sh` is good work and its four scenarios are the right ones — but it sets `LAIN_AGENT_NAME` explicitly per agent, which is precisely the case that was never broken. Nothing exercises the ladder.

- [ ] **Step 1: Assert distinct identities with no configuration.** Two Claude Code sessions, `LAIN_AGENT_NAME` unset, each edits a file; assert `list_active_agents` shows two distinct agents. This test fails on `main` today.

- [ ] **Step 2: Assert fail-open.** With no server running at all, an edit must complete and the hook must exit 0 in under a second.

- [ ] **Step 3: Assert the conflict payload.** Two edit claims on one symbol; assert the conflict carries a real name and a non-zero timestamp.

- [ ] **Step 4: Wire both scripts into CI.** `.github/workflows/ci.yml` runs `cargo test` only — the e2e harness has never run in CI.

**Verification:** `bash tests/e2e/multiplayer-identity.sh` exits 0; CI runs it.

---

## Task 8: Reconcile the docs with the behavior

**Files:** Modify `docs/hooks.md`, `docs/multiplayer.md`, `docs/wish-list.md`

- [ ] **Step 1: Correct `docs/hooks.md`'s multi-instance section.** It says all sessions of one agent kind share one session file and "appear as ONE agent" unless `LAIN_AGENT_NAME` is set per shell. After Task 1 that is no longer true — per-console identity is automatic. This paragraph is the one wishlist #2 called "exactly backwards".

- [ ] **Step 2: Correct the persistence claim.** `docs/multiplayer.md:105` says state is restored on startup so claims survive restarts. True only after Task 5.

- [ ] **Step 3: Update the wishlist status block honestly.** #1, #2, and #5 are currently marked addressed. Re-mark them with what actually shipped and what this plan completes, and record the measurement date — the status block was written without an end-to-end check against a live agent, which is how three items were marked done while failing.

---

## What this plan does *not* fix

- **Wishlist #3 (zero-daemon path)** and **#4 (stateless claims)** stay deferred. They are the right direction and are better served by the filesystem-claims design sketched in `docs/wish-list.md`; that deserves its own plan.
- **Commit-time overlap detection** — the highest-value multiplayer feature for agents working in separate worktrees — is not here. Separate plan.
- **Attribution backends** (`/proc`, `lsof`) are untouched.

## Definition of done

1. Two unconfigured Claude Code consoles register as two distinct agents.
2. `lain hooks claim` returns in under a second with exit 0 against an unreachable server.
3. A conflict payload carries a real agent name and a real timestamp, including the symbol-then-file-level ordering.
4. Hook-driven claims are symbol-scoped when the symbol resolves.
5. `list_occupancy` contains no claims from dead sessions; claims survive a restart keyed on `repos.yaml`.
6. `tests/e2e/multiplayer-identity.sh` passes in CI.
