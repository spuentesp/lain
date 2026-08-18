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

## Status (2026-08-19, refreshed 2026-08-20)

- **#1 fail open** → addressed in PR 12 (`hooks/claude-code/{pre,post}-edit.sh` and `hooks/{kimi,agy,codex}/pre-edit.sh` now use `set +e` + `trap 'exit 0' ERR`).
- **#2 identity auto-detect** → addressed in PR 12 (hooks now read `LAIN_AGENT_NAME` → generic agent env vars `CLAUDE_AGENT_NAME` / `MCP_CLIENT_NAME` / `AGENT_NAME` → fallback to `<kind>-<ppid>-<host>`).
- **#3 zero-daemon path** → addressed in PR 18`feat(cli): zero-daemon fallback for hooks claim/release`. `lain hooks claim|release` now probes `--url/health` with a 200ms timeout; when no server is reachable, the call falls through to the filesystem lock layer (`<workspace>/.lain/locks/<sanitized>.json`). Subagents editing in a parent-less process tree no longer need `register_agent` + `heartbeat` for one edit.
- **#4 stateless claims** → addressed in PR 18 (same commit as #3). Subagents and short-lived agents can now claim without going through `register_agent` → `heartbeat` → `release_files`; the filesystem lock layer carries the coordination when the daemon isn't running.
- **#5 conflict shape** → addressed in PR 12 (`OccupancyMap::claim` filters read-vs-edit; conflicts carry `intent` + `last_touched_unix`).
- **#6 one version of truth** → addressed in PR 12 (`lain doctor` runs 5 checks and prints a single diagnostic).
- **#7 reconcile roadmaps** → addressed in PR 12 (the 2026-08-14 plan file is marked superseded; `docs/multiplayer.md` notes the supersession).

### Status (2026-08-20, after Round 2)

- **#8 per-repo tools must answer** → **partially addressed in `577a444`**. When the federation has exactly one repo, the executor's `ToolContext::graph` is now that repo's indexed `GraphDatabase`, not the empty staging dir. `find_anchors` / `explain_symbol` / `get_blast_radius` / etc. now answer against the real graph for the single-repo case (the typical `lain` user setup). **Multi-repo federation still binds to the placeholder** — per-repo tools with no explicit `repo_id` will return the wrong repo's data or empty. Round-2 follow-up: pass `&FederatedIndex` to per-repo handlers and have them pick the right `RepoIndex::db()` based on `repo_id` (already in args from the round-1 fix).
- **#9 tools/list filter for inert tools** → open. Quick win on top of #8 round-2.
- **#10 `doctor` should verify the integration surface** → partially addressed. The on-disk hook check now also tries the install layout (`$bindir/../share/lain/hooks/`) so release binaries report OK, not FAIL (commit `f643f68`). **Open**: `doctor` still doesn't call `tools/list` on a live MCP endpoint. The "all checks passed" on a broken MCP registration remains the most embarrassing single failure — the check covers the wrong surface.
- **#11 single-workspace mode was removed** → **design decision still owed**. Two reasonable answers: (a) restore a no-args `lain mcp` for the single-repo case (walk up for `.git`, index, serve stdio), or (b) keep federation-only and rewrite the strategy guide / `SKILL.md` / tool list to match what federation can actually do. Current state is the worst of both. The single-repo binding fix in #8 unblocks option (a) only if we also add a `lain mcp` entrypoint; right now there's no MCP-friendly single-repo mode.
- **#12a `semantic_search` unreachable (NLP model not loaded)** → open. `lain server` has no `--embedding-model` flag, so the model never loads. The ONNX model is on disk and unused.
- **#12b `query_graph` schema mismatch** → open. Documented `{"ops":[{"find":"Function"},{"limit":3}]}` errors with `missing field 'op'`. Either the docs or the handler is wrong.
- **#12c `get_cross_repo_blast_radius` traverses outgoing, not incoming** → open. "Blast radius" to an agent means *incoming* callers ("if I change X, what breaks?"). Outgoing edges answer "what does X depend on", which is `trace_dependency`. Either flip it or rename.
- **#12d running the tool can break its own test suite** → **addressed in `6d71a75`**. `find_workspace_root` no longer honors `.lain/` as a workspace anchor; only `.git`. The `/tmp/.lain` CI-red problem is gone.
- **#12e session files accumulate one per PPID** → open. `~/.config/lain/hooks/` grows unbounded. `doctor` counts them; nothing reaps them. Worth a TTL-based cleanup or at least an "old sessions" warning.

---

# Round 2 — integration (2026-08-20)

The first seven items were about the multiplayer layer. These are about
*getting lain in front of an agent at all*. Written after a live session
with the MCP server actually connected and its tools loaded — every
result below is a real call, not a code read.

## 8. Per-repo tools must answer about the repo

**The single highest-value item on this page.** Everything else here is
cosmetic next to it.

In federation mode (`lain server --config repos.yaml` — now the *only*
launchable mode, see #11) the per-repo tools bind to the synthetic
staging dir `/tmp/lain-federation-<pid>-<counter>`, not to the repo the
server just indexed. From one session, seconds apart:

| Call | Result |
|---|---|
| `search_org("sanitize_agent_name")` | ✅ found — `src/cli/hooks.rs`, kind `Function` |
| `list_repos` | ✅ `node_count: 3007, edge_count: 12641, health: ready` |
| `get_health` | ❌ `Workspace: /tmp/lain-federation-2510397-0 · Static Nodes: 0` |
| `find_anchors` | ❌ `No anchors found in Merged Brain.` |
| `get_blast_radius("sanitize_agent_name")` | ❌ `Node not found for handle` |
| `get_cross_repo_blast_radius` | ⚠️ `{"by_repo":{},"total_count":0}` for every symbol tried |

The same server, in the same second, reported that a symbol exists and
that it does not. That is the worst possible failure shape for an agent:
not an error I would route around, but a confident false negative I
would act on. An agent that trusts `get_blast_radius` here concludes the
symbol has no callers and edits accordingly.

**Wish:** when the config resolves to a single `workspace_dir` repo, bind
the per-repo tools to *that* repo. One change makes all 61 tools work,
makes `SKILL.md` accurate as written, and makes the MCP registration
permanent. This is the same synthetic-staging-workspace root cause as the
old per-PID `state_path` bug.

## 9. Don't advertise tools that cannot answer

`tools/list` returns 61 tools; roughly 30 of them are inert in the mode
the server is actually running. I would rather see 20 working tools than
61 where I must learn by trial which ones lie. This is a `tools/list`
filter, not a refactor — if a tool can't answer in the current mode,
don't offer it.

## 10. `doctor` should verify the integration surface, not its own files

`lain doctor` reported **`all checks passed`** on an installation where
the MCP server could not start at all and two-thirds of the tool surface
was dead. It checks that its own hook script exists on disk; it never
checks the thing that is actually load-bearing — the MCP registration and
whether the server answers.

**Wish:** `doctor` should spawn its own MCP endpoint, call `tools/list`,
and issue one real structural query. If `get_blast_radius` on a symbol
that `search_org` just found returns "not found", that is a `[FAIL]`.
Concretely, the wiring that was broken here for an unknown number of
sessions:

```
$ claude mcp get lain
  Status: ✘ Failed to connect — CONNECTION_CLOSED
  Args: --workspace auto --transport stdio --embedding-model …
$ lain --workspace auto --transport stdio
  error: unexpected argument '--workspace' found
```

The consolidation moved `--workspace`/`--transport` under `lain server`
and nothing updated the installed registration. Fixed by re-registering as
`lain server --config ~/.config/lain/repos.yaml --transport stdio`.

## 11. Single-workspace mode was removed; everything still assumes it

This is the root cause behind #8 and #9, and it needs an explicit
decision rather than a patch.

`get_agent_strategy` — the server's own operational manual, which its MCP
instructions tell the agent to read first — builds its entire Decision
Flow on `find_anchors` → `get_blast_radius` → `trace_dependency` →
`get_coupling_radar`, all of which are inert in federation mode. It also
says verbatim:

> switch to federation mode by launching the server with `lain server
> --config repos.yaml` instead of single-workspace mode (`lain
> --workspace PATH`)

`lain --workspace PATH` no longer parses. So the tool surface, `SKILL.md`,
and the strategy guide are all written for a mode the consolidated binary
cannot launch, while the only launchable mode can't serve them.

**Wish:** pick one. Either restore a single-repo mode (`lain mcp` with no
args — walk up for `.git`, index, serve stdio, so the MCP config is
`{"command":"lain","args":["mcp"]}` and can never drift again), or keep
federation-only and rewrite the strategy guide, `SKILL.md`, and the tool
list to match what federation can actually do. The current middle state
is the worst of both.

## 12. Smaller things found on the way in

- **`semantic_search` is unreachable.** `lain server` has no
  `--embedding-model` flag, so the model never loads
  (`NLP Model: Not loaded`). The old registration passed one; the new
  subcommand rejects it. The ONNX model is on disk and unused.
- **`query_graph`'s schema doesn't match the documented example.**
  `{"ops":[{"find":"Function"},{"limit":3}]}` →
  `Error: JSON error: missing field 'op'`.
- **`get_cross_repo_blast_radius` traverses outgoing `Calls` edges.**
  "Blast radius" to an agent means *incoming* callers — "if I change X,
  what breaks?". Outgoing edges answer "what does X depend on", which is
  `trace_dependency`. Either flip it or rename it.
- **Running the tool can break its own test suite.** `lain hooks claim`
  with no server on a path under `/tmp` creates `/tmp/.lain/`, which
  `find_workspace_root` then honors as a workspace marker — after which
  `find_workspace_root_walks_up_to_git_or_lain` fails until the directory
  is removed. Cheap fix now, confusing CI red later.
- **Session files accumulate one per PPID** in `~/.config/lain/hooks/`
  with no pruning. `doctor` counts them; nothing reaps them.

## What a customer actually uses today

Working and genuinely useful: `search_org` (better than grep — it
distinguishes a `Function` from a similarly-named test), `list_repos`,
and the entire multiplayer surface (`claim_files`, `list_occupancy`,
`who_am_i`, `list_subagents`). That layer is solid.

Not usable: everything structural — blast radius, anchors, dependency
traces, semantic search. Which is the frustrating part, because that is
precisely the work lain exists to replace, and the graph is right there:
3007 nodes, 12641 edges, `health: ready`. It simply isn't wired to the
tools that would read it.
