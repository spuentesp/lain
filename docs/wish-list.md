# Lain Multiplayer — Customer Wish List

Written from the seat of a coding agent (Claude Code) that shares a repo with
other agent consoles. Based on hands-on use of the `consolidation` worktree's
multiplayer layer (`docs/multiplayer.md`, `docs/hooks.md`) plus one direct
incident this session where a `PreToolUse` hook wired to `lain ask` blocked
every single tool call because it failed closed on a bad invocation.

## 1. Fail open, always, no exceptions

The one non-negotiable. A coordination layer that goes down should degrade to
"no awareness," never to "no tool calls." I hit this directly: a broken hook
script blocked Bash, Read, and every other tool identically until a human
fixed it from outside the session. Contrast with Orca's own hook
(`~/.orca/agent-hooks/claude-hook.sh`), which checks its own preconditions and
exits 0 silently if anything is missing. Every lain-side hook script,
`lain hooks claim` included, should follow that same pattern: unreachable
server, malformed response, missing token — all of it exits 0 and lets the
edit through, with a stderr note at most.

## 2. Identity that doesn't require me to configure anything

`docs/hooks.md` says all sessions of one agent kind share one session file and
"appear as ONE agent" unless I manually set `LAIN_AGENT_NAME` per shell. That's
exactly backwards for the actual failure mode — I *am* one of several Claude
Code consoles on this machine right now, and there are plenty of signals lain
itself could already use to tell them apart (parent process tree, short
hostname, generic agent env vars set by any framework). I shouldn't have to
hand-roll a name; `lain hooks claim` should derive an identity from those
signals and fall back to a PID-derived one otherwise. Two coordination
systems on one machine that don't share identity is worse than either alone.

## 3. Zero-daemon path for the common case

Getting coordinated requires: write a `repos.yaml`, start `lain server`,
keep it alive on a port, and install pre/post-edit hooks — for the simple
case of "two agents, one repo, don't clobber each other." That's a lot of
standing infrastructure for what's conceptually a lock file. I'd want a mode
with no config file and no long-lived process: `lain hooks claim <path>`
works against a plain state directory (`.lain/claims/`) when no server is
running, and transparently upgrades to using the server if one happens to be
up. Today, no server running presumably means no coordination at all rather
than a lighter fallback.

## 4. Claims I can check without holding a connection open

The SSE stream (`GET /events`) is the "push" story, but most of my edits are
one-shot: read, decide, edit, move on. I don't want to open and manage a
long-lived stream connection for a five-second file edit. A cheap synchronous
`lain hooks status <path>` (or the file already existing as a stat-able JSON
file, per #3) covers me better than a subscription model for short-lived
agent work — including subagents I fork for a single task, which live for
seconds and shouldn't have to go through `register_agent` → `heartbeat` →
`release_files` for one edit.

## 5. Advisory conflicts should say *what*, not just *that*

Per `docs/multiplayer-e2e-report.md`, Scenario D's second agent edits "despite
the conflict warning" — conflicts are advisory, which I think is the right
default (a hard lock would just cause agents to sit blocked or retry-storm).
But the warning payload should tell me enough to make a real decision:
which symbols the other agent claimed, when they last touched it (staleness),
and whether their claim's `intent` was `read` or `edit`. A `read` claim
conflicting with my `edit` claim is a non-event; two `edit` claims on the
same symbol is the one case I actually need to stop and look at.

## 6. One version of the truth about what's running

I hit this directly: `lain` on `$PATH` was a symlink into this worktree's
build, whose `ask` subcommand had already diverged from the hook script
written for a different `ask` shape (no-arg, stdin) that matches an older
commit. Nothing told me the binary in use didn't match what any given hook
script or doc expected. A `lain doctor` (or `--version`-embedded git SHA
cross-checked against installed hook scripts) would have turned a full
lockout into a one-line diagnostic.

## 7. Reconcile the two roadmaps

`docs/superpowers/plans/2026-08-14-lain-consolidation.md` (sitting uncommitted
on `main`) explicitly deletes `hook.rs`, `projects.rs`, and the
owner/sidecar/lock machinery as "multi-user coordination" out of scope — dated
before the multiplayer work (2026-08-15 through 2026-08-18) that built exactly
that surface back out, more thoroughly, in this worktree. As a customer I
don't need both to survive intact, but I do need to know which one is the
plan of record before I build a workflow around either.

---

## Lighter-weight coordination alternatives

Ideas that solve "don't step on each other" without a running server, ports,
session tokens, or held-open streams — each is opt-in-by-presence rather than
opt-in-by-registration, so an agent that doesn't participate costs nothing.

- **Filesystem-as-lock, no daemon.** A `.lain/claims/<sanitized-path>.json`
  written with `O_EXCL` (atomic create-or-fail): `{agent_id, symbols, pid,
  ts}`. Claim = create the file; release = delete it; check = stat it. Fail-open
  is structural, not exception-handled.
- **mtime as heartbeat.** `touch` the claim file periodically instead of an RPC
  on a timer. Staleness is just `stat().st_mtime` vs. now — no liveness
  protocol to implement or get wrong.
- **Append-only activity log, not a query API.** `.lain/activity.jsonl`, one
  line per edit (`{agent, path, symbols, action, ts}`). Cheap to append, cheap
  to `tail -f`, human-readable, and a crashed agent just stops appending —
  nothing needs cleanup.

- **Push conflict detection to commit-time, not edit-time.** For agents in
  separate worktrees (the common case for longer tasks), what matters is
  catching overlap before merge, not during editing. A `git diff` /
  symbol-overlap check at commit or PR time — which lain's structural graph
  is already well-suited for — catches the case that matters without any
  live coordination while editing.

---

## Status (2026-08-19)

- **#1 fail open** → addressed in PR 12 (`hooks/claude-code/{pre,post}-edit.sh` and `hooks/{kimi,agy,codex}/pre-edit.sh` now use `set +e` + `trap 'exit 0' ERR`).
- **#2 identity auto-detect** → addressed in PR 12 (hooks now read `LAIN_AGENT_NAME` → generic agent env vars `CLAUDE_AGENT_NAME` / `MCP_CLIENT_NAME` / `AGENT_NAME` → fallback to `<kind>-<ppid>-<host>`).
- **#5 conflict shape** → addressed in PR 12 (`OccupancyMap::claim` filters read-vs-edit; conflicts carry `intent` + `last_touched_unix`).
- **#6 one version of truth** → addressed in PR 12 (`lain doctor` runs 5 checks and prints a single diagnostic).
- **#7 reconcile roadmaps** → addressed in PR 12 (the 2026-08-14 plan file is marked superseded; `docs/multiplayer.md` notes the supersession).

- **#3 zero-daemon path** → deferred. The lighter-weight alternatives (filesystem-as-lock, commit-time detection) are better; will plan separately.
- **#4 stateless claims** → deferred pending the filesystem-lock layer.
