# Lain Multiplayer — Customer Wish List

> **Read this as a historical complaint, not as current behavior.**
> Sections 1–12 are the original agent-seat report, kept verbatim because
> the framing is the useful part. Most of what they describe is fixed.
> The verified current state is at the bottom, under
> [Status — verified 2026-08-22](#status--verified-2026-08-22); where a
> section below contradicts it, the bottom section is right.

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

In the end-to-end run this list came from, a second agent edited "despite
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

The 2026-08-14 consolidation plan explicitly deleted `hook.rs`, `projects.rs`,
and the owner/sidecar/lock machinery as "multi-user coordination" out of scope — dated
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
- **#9 tools/list filter for inert tools** → CLOSED 2026-08-23. `semantic_search` is not advertised when no NLP model is loaded (the canonical surface is 67 tools; `tools/list` returns 66 when no model is loaded — 67 once `--embedding-model` is set); it returns as soon as one is. Only fully-inert tools are filtered — see the section at the end.
- **#10 `doctor` should verify the integration surface** → partially addressed. The on-disk hook check now also tries the install layout (`$bindir/../share/lain/hooks/`) so release binaries report OK, not FAIL (commit `f643f68`). **CLOSED 2026-08-22**: `doctor` now calls `tools/list` on the live MCP endpoint and fails on an empty surface. Previously open: The "all checks passed" on a broken MCP registration remains the most embarrassing single failure — the check covers the wrong surface.
- **#11 single-workspace mode was removed** → **CLOSED**: option (a) was taken — `lain mcp` exists. Original note: Two reasonable answers: (a) restore a no-args `lain mcp` for the single-repo case (walk up for `.git`, index, serve stdio), or (b) keep federation-only and rewrite the strategy guide / `SKILL.md` / tool list to match what federation can actually do. Current state is the worst of both. The single-repo binding fix in #8 unblocks option (a) only if we also add a `lain mcp` entrypoint; right now there's no MCP-friendly single-repo mode.
- **#12a `semantic_search` unreachable (NLP model not loaded)** → CLOSED (see bottom).~~open.~~ `lain server` has no `--embedding-model` flag, so the model never loads. The ONNX model is on disk and unused.
- **#12b `query_graph` schema mismatch** → NOT A DEFECT (see bottom).~~open.~~ Documented `{"ops":[{"find":"Function"},{"limit":3}]}` errors with `missing field 'op'`. Either the docs or the handler is wrong.
- **#12c `get_cross_repo_blast_radius` traverses outgoing, not incoming** → CLOSED; the traversal was already incoming, the *descriptions* were wrong (see bottom).~~open.~~ "Blast radius" to an agent means *incoming* callers ("if I change X, what breaks?"). Outgoing edges answer "what does X depend on", which is `trace_dependency`. Either flip it or rename.
- **#12d running the tool can break its own test suite** → **addressed in `6d71a75`**. `find_workspace_root` no longer honors `.lain/` as a workspace anchor; only `.git`. The `/tmp/.lain` CI-red problem is gone.
- **#12e session files accumulate one per PPID** → CLOSED, `doctor` reaps >30d (see bottom).~~open.~~ `~/.config/lain/hooks/` grows unbounded. `doctor` counts them; nothing reaps them. Worth a TTL-based cleanup or at least an "old sessions" warning.

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
the per-repo tools to *that* repo. One change makes all 67 tools work
(66 with no `--embedding-model` set, because `semantic_search` is
filtered from `tools/list`), makes `SKILL.md` accurate as written, and
makes the MCP registration permanent. This is the same
synthetic-staging-workspace root cause as the old per-PID `state_path`
bug.

## 9. Don't advertise tools that cannot answer

`tools/list` returns 67 tools (66 when no model is loaded, since
`semantic_search` is filtered); roughly 30 of them were inert in the mode
the server is actually running. I would rather see 20 working tools than
67 where I must learn by trial which ones lie. This is a `tools/list`
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

*(Historical — this paragraph described the state on 2026-08-19 and is
no longer true. Superseded by the status section below.)*

The original text read: "Not usable: everything structural — blast
radius, anchors, dependency traces, semantic search." Every one of those
answers today; see below.

## Status — verified 2026-08-22

Each line re-probed against a live server (`lain server`, HTTP transport,
real ONNX model loaded, this repo indexed) rather than read off the code.

**Closed:**

- **#1–#7** — closed earlier (PR 12 / PR 18); see the 2026-08-20 section.
- **#8 per-repo tools must answer** → closed, both halves. Single-repo
  (the setup `lain init` scaffolds) was bound first; multi-repo is bound
  per call as of 2026-08-23 — see the section at the end.
  `find_dead_code`, `get_blast_radius`, `find_anchors`, `explain_symbol`,
  `semantic_search`, `get_call_sites` and `query_graph` all answer
  against the right repo's graph.
- **#10 `doctor` should verify the integration surface** → closed.
  `doctor` now calls `tools/list` on the live MCP endpoint when
  `LAIN_URL`/`LAIN_SERVER_URL` is set and reports the advertised tool
  count, failing hard on an error envelope or an empty surface.
  Verified against a stub that answers `/health` 200 with zero tools:
  `[FAIL] MCP surface empty: tools/list advertises 0 tools`, exit 1.
  The "all checks passed on a broken registration" failure is caught.
- **#11 single-workspace mode was removed** → closed. `lain mcp` walks
  up for `.git` and serves the per-repo surface on stdio, so the MCP
  config is `{"command":"lain","args":["mcp"]}`. `lain init` scaffolds
  `repos.yaml` for the server path.
- **#12a `semantic_search` unreachable** → closed. `lain server
  --embedding-model PATH` loads the ONNX bi-encoder; the tool returns
  ranked results. Unset, it says `NLP Model: Not loaded` rather than
  failing silently.
- **#12b `query_graph` schema mismatch** → **not a defect.** The report
  quoted `{"ops":[{"find":"Function"}]}`, a shorthand the docs never
  specified. Every example in `docs/query-language.md` and
  `docs/quickstart-query.md` uses the tagged form
  (`{"op":"find","type":"Function","name":"handle"}`) and all five were
  re-run successfully. No code or doc change was needed.
- **#12c `get_cross_repo_blast_radius` direction** → closed, and the
  reverse of what was reported: the *traversal* was already fixed to
  incoming `Calls` (callers). What was still wrong was the **tool
  description and `docs/FEDERATION.md`, which both still said
  "outgoing"** — while FEDERATION.md's own example listed `caller_*`
  nodes. Both now say incoming.
- **#12d test suite self-break** → closed (`6d71a75`).
- **#12e session files accumulate** → closed. `doctor` reaps session
  JSON older than 30 days via `prune_old_sessions` and reports the count.

**Closed 2026-08-23 — #9, `tools/list` filter for inert tools:**

`tools/list` advertised the full surface regardless of mode, so a caller
learned by trial which tools answer. A tool guaranteed to fail is worse
than one not offered: the agent spends a round trip and then has to
decide whether to trust the next thing lain says.

`semantic_search` returns `Unavailable` on every call without an NLP
model, so it is dropped from `tools/list` when the model is absent — 63
tools rather than 64 — and returns the moment one is loaded. Only
*fully* inert tools are filtered. `find_dead_code` stays advertised:
without the model it refuses the `like` argument and answers normally
otherwise, so hiding it would remove a working tool. A test pins both
directions, because a filter that hides working tools is worse than the
problem it solves.

**Open: nothing.**

**Closed 2026-08-23 — multi-repo binding (#8, second half):**

Per-repo tools now bind to whichever repo the call resolves to.
`ToolContext` carries the federation and grows a `for_repo(repo_id)`
that returns a context with `graph` and `workspace` swapped for that
repo's; `ToolRegistry::dispatch` applies it using the `repo_id` the MCP
dispatcher already resolved and injected. That id had been injected and
then ignored — the code comment said so out loud — which is why every
per-repo tool in a multi-repo federation read the empty staging
placeholder.

Verified on a real two-repo federation (`alpha`, `beta`, both `ready`):

- `explain_symbol alpha_only_helper` and `explain_symbol
  beta_only_helper` each resolve in their own repo.
- `get_blast_radius beta_inner` reports `beta_only_helper` as its
  direct dependent.
- `find_anchors {"repo_id":"alpha"}` lists only alpha's symbols, and
  the same for beta — no cross-repo leakage.
- `get_code_snippet` reads `/tmp/multirepo/alpha/src/lib.rs` vs
  `/tmp/multirepo/beta/src/lib.rs` for the same relative path.

That last one was a second bug the first fix exposed: `get_code_snippet`
passed its path straight to `std::fs`, which resolves a relative path
against the *process* working directory. `src/lib.rs` exists in every
repo, so it returned a same-named file from wherever the server was
launched — lain's own checkout — with no error. Paths now resolve
against the bound repo's workspace.

## Live three-agent sweep — 2026-08-23

Three agents (alpha, beta, gamma) driven against one server, verifying
the server's claims against the repo with `grep` rather than trusting
them. Confirmed working: path canonicalization across spellings,
`conflicts` vs `advisories` as separate arrays, holder `name`/`intent`
on both, node ids round-tripping between `query_graph`,
`explain_symbol` and `get_blast_radius`, and the blast-radius
direct/indirect split (checked caller by caller — complete, nothing
invented).

Found and fixed:

- **`find_dead_code` reported 9 symbols, 7 of them false.** All seven
  were called from *another file*; the reference check only looked
  inside the symbol's own file. One of them, `edge_counts_by_type`,
  generates a section of `get_health`'s own output — the server used
  the function to answer the agent and then said nothing calls it. The
  check is now workspace-wide, and the list is back to the 2 the agent
  independently confirmed as genuinely unreferenced.
- **`get_call_sites` reported enclosing functions as call sites.**
  `build_core_memory at ...:19-360` is a 341-line definition range, and
  a function calling the target twice counted once. It now reports the
  real lines (`sweep_orphans` → 3 calls at 55, 520, 681, matching grep).
- **`release_files` rejected `["src/a.rs"]`** with `expected struct
  ReleaseFilesEntry` — an internal Rust type name, for input that was
  never ambiguous. Both spellings are accepted now.
- **`register_agent` never declared `kind` or `mode`**, which it accepts
  and reports to peers via `list_active_agents`. A schema-following
  agent silently lost its own identity metadata.
- **`_meta.revision` read 0 across eight state-changing presence calls.**
  Correct — it counts overlay diffs, not claims — but undocumented, and
  `docs/multiplayer.md` additionally placed it at the top level rather
  than under `_meta`. Both corrected there.

### Name collisions — closed 2026-08-23

Reported above as a documented limitation; fixed rather than left
documented. Two separate defects were hiding behind one symptom.

**Edges were manufactured.** `resolve_static_edges` looked a reference
name up in an index and emitted an edge to *every* definition sharing
it. With eleven `fn parse` in this repo, each `.parse()` in the tree —
clap's `Args::parse()`, stdlib `str::parse` — produced eleven edges.
`get_call_sites parse` answered with 61 callers and returned the *same*
list for all eleven nodes. A name several definitions share is not
resolvable by name alone, so resolution now prefers a definition in the
calling file and otherwise emits nothing: a missing edge is a gap, N
wrong edges are a lie, and the lie also inflated `find_anchors` and
`get_blast_radius`. `parse` now reports 1 caller; `sweep_orphans`, whose
name is unique, still reports its 3 real call sites.

The knock-on effect is visible in `find_anchors`, which used to rank
`as_str`, `parse`, `default`, `next`, `drop` — one-line accessors riding
fabricated in-edges. It now ranks `required_str_arg`, `resolve_node`,
`calculate_anchor_scores`, `insert_nodes_batch`: actual hubs, which is
what the scoring was designed to surface all along.

**Ambiguity was silent.** `find_node_by_name` returned
`node_weights().find(...)` — petgraph iteration order, so the choice was
both arbitrary and unstable across reindexes. It is now sorted by (path,
id), and `explain_symbol`, `get_anchor_score`, `get_call_sites` and
`get_blast_radius` open with a `⚠` line naming how many definitions
share the name, which one they answered about, and the ids of the rest.
Nobody is refused an answer over it — erroring would break every call
that is perfectly clear — but nobody is silently handed the wrong node
either.

## 13. Federation cross-repo `Calls` edges are never ingested — `get_cross_repo_blast_radius` cannot return cross-repo callers

**Status:** closed (2026-08-29) — see closure notes below for the
fix and the proving test landscape. The verifier found seven
additional bugs along the way; those are tracked as items #14–#20
below.

**Verified against a real two-repo federation fixture** (auth-svc
defining `verify_token`, billing-svc with `charge_invoice` calling
`auth_svc::verify_token(token)` across a Cargo path-dep), the federation
behaves like this:

- `get_workspace_graph` returns 5 nodes, 0 edges. Both repos'
  definitions are visible across the federation, but no `Calls` edge
  connects them.
- `get_cross_repo_blast_radius(verify_token)` →
  `{"by_repo":{}, "total_count":0}` — empty, despite the call existing
  in source.
- `get_blast_radius(verify_token)` (single-repo) → also empty, even
  though the caller is in another repo (and the single-repo tool has no
  business seeing it from there anyway, so that part is correct).
- `search_org("verify_token")` → finds the definition in auth-svc, no
  callers listed.

The first implementer's diagnosis pointed at
`FederatedIndex::project_repo` for only projecting intra-repo edges.
That is a real gap, but the deeper issue is that the federation's
indexer never creates the cross-repo `Calls` edge in the first place:
even within billing-svc's per-repo graph, the
`charge_invoice → verify_token_bridge` and
`verify_token_bridge → auth_svc::verify_token` hops that would carry
the call across the boundary are missing.

This gap is acknowledged in the test that pins it:
`tests/federation_e2e.rs::get_cross_repo_blast_radius_traverses_boundaries`
soft-asserts the result is well-formed but does not require cross-repo
callers to surface, and its doc comment notes that "cross-crate `Calls`
edge propagation is environment-dependent." That phrasing understates
the situation — it does not happen in *any* environment with the
current code, and the soft assertion means a regression in either
direction is invisible.

Net effect: the federation's headline cross-repo tool answers
correctly within a repo but cannot answer the cross-repo question it is
named for. The demo recording team already pivoted around this in
commits `0992a9d` ("make federation fixture a real cross-repo
workspace"), `f77ab38` ("deterministic federation cross-repo probe in
recorder"), and `bc82e09` ("swap Tools-tab call to
`get_workspace_graph` for federation fixture") — the recording shows
`get_workspace_graph` (which works, because it never claims cross-repo
edges) rather than `get_cross_repo_blast_radius` (which doesn't).
The brief assumed the recording would show a cross-repo blast-radius
query returning cross-repo callers; against the current federation
that is literally impossible.

**Wish:** cross-repo `Calls` edges are ingested end-to-end so the
headline tool answers the question it is named for. Concretely:

1. The per-repo indexer resolves cross-crate references (via
   rust-analyzer with the parent Cargo workspace as the anchor) and
   writes a `Calls` edge to the target repo's symbol — not just a
   local bridge.
2. `FederatedIndex::project_repo` preserves cross-repo edges when
   projecting per-repo nodes into the federated petgraph (or the
   federated backend learns to reconstruct them from the local-to-
   global map).
3. `get_cross_repo_blast_radius(verify_token)` against the verified
   fixture returns something like
   `{"by_repo":{"billing-svc":["billing-svc:Function:src/billing.rs:charge_invoice"]}, "total_count":1, "truncated":false}`.
4. `get_workspace_graph` reports the cross-repo edge with
   `cross_repo: true`.
5. The soft-assert test is converted to a hard assertion, and the
   regression pin flips from `total == 0` to `total >= 1` as part of
   the same change.

In the meantime, `docs/FEDERATION.md` calls out the limitation
honestly so users do not assume the headline example output
(`billing-svc: caller_c` under `by_repo`) is reachable against the
current code.

**Closed 2026-08-29.**

The ingestion gap is fixed end-to-end:

1. **`CrossRepoResolver` trait** (`src/server/federation/cross_repo.rs`).
   Federation-aware lookup the resolve phase calls when a reference
   misses the calling repo's `GraphDatabase`. `FederatedIndex`
   implements it; the strategy is path-prefix for LSP refs (find the
   owning repo by absolute path, then `get_node_at_location` at that
   path:line in the other repo's DB) and `symbol_to_repos` for
   tree-sitter refs (single non-source owner narrows to a global id;
   2+ non-source owners return `None` — same-file preference). Both
   branches produce a canonical `GlobalId`. The trait also carries a
   `refresh()` hook (`FederatedIndex` calls
   `rebuild_symbol_index`); see (4) below for why.
2. **`GraphDatabase::insert_edges_batch`** (`src/server/graph.rs`)
   no longer drops edges whose target is missing locally. It stashes
   them in `pending_external_edges` instead — the petgraph can't
   store an edge to a node it doesn't own, but the federation layer
   *can* resolve that target against the rest of the federation, so
   the edge is held for `project_repo` to drain via
   `take_pending_external_edges`. Edges whose source is also missing
   remain true orphans and are still counted in the dropped return,
   so `insert_edges_reporting`'s warning still fires for genuinely
   broken cases.
3. **`FederatedIndex::project_repo`** drains the stash: rewrites
   `source_id` through `local_to_global` and passes the global
   `target_id` through unchanged. It also upserts a placeholder node
   for each unique target first (idempotent on global id) so the
   backend's "both endpoints must exist" invariant holds even when
   the target's owning repo hasn't been projected yet — the real
   projection overwrites the placeholder when it runs. The
   `rebuild_symbol_index` call at the end is gated on a non-empty
   per-repo DB so the federation loader's parallel `add_repo +
   project_repo` over empty DBs can no longer wipe `symbol_to_repos`
   to empty.
4. **`index_one_repo` refreshes the resolver** before the resolve
   phase runs. Without this, the federation loader's parallel
   `add_repo + project_repo` sequence runs over empty per-repo DBs,
   `symbol_to_repos` ends up empty, and the resolve phase's
   cross-repo lookup silently returns `None` — dropping the very
   edge the fix was meant to produce. `index_one_repo` now calls
   `resolver.refresh()` after the per-repo node insert, so
   `symbol_to_repos` is up-to-date for the resolve phase regardless
   of caller ordering (the CLI's manual `index() → project_repo()`
   interleave is no longer load-bearing).
5. **Loader wiring.** `load_federation` and
   `load_federation_with_workspace` install `Arc<FederatedIndex>` as
   each new `RepoIndex`'s cross-repo resolver right after `add_repo`,
   so the resolve phase inside `RepoIndex::index` sees a non-`None`
   resolver from the start.
6. **Tests.**
   - `tests/federation_e2e.rs::get_cross_repo_blast_radius_traverses_boundaries`
     flipped from `assert_eq!(total, 0)` to `assert!(total >= 1)`.
   - `src/server/federation/federated_index_tests.rs` gained four
     unit tests covering the resolve path (path+line lookup),
     source-repo skip, multi-other-repo ambiguity (correctly returns
     `None`), and projection pass-through.
   - `tests/federation_integration.rs::cross_repo_calls_edges_materialize_via_real_lsp_pipeline`
     is the **proving test** the customer asked for. It builds a
     real Cargo workspace (parent `crates/` with `[workspace]
     members = ["a", "b"]`, repo `a` defines `verify_token`, repo
     `b` declares a path-dep and calls `fed_a::verify_token`),
     boots `load_federation`, runs real `RepoIndex::index()` against
     both repos (which spawns rust-analyzer, scans via
     tree-sitter, runs the resolve phase), then asserts the
     federated backend has the cross-repo `Calls` edge from
     `b:Function:src/lib.rs:charge_invoice` to
     `a:Function:src/lib.rs:verify_token`. No interleave, no
     fakery — exactly the headline behavior this wishlist described.

Verified against the e2e fixture (`a` defines `target_fn`; `b` calls
`a::target_fn` via Cargo path-dep):

```
[federation_e2e] blast radius by_repo=["b"] total_count=1 truncated=false
```

And against the integration fixture (identical shape, `verify_token` /
`charge_invoice`):

```
test cross_repo_calls_edges_materialize_via_real_lsp_pipeline ... ok
```

The headline cross-repo caller shape from this very wishlist item
now materializes; `docs/FEDERATION.md`'s Known-limitations note and
the `billing-svc: caller_c` example output are both accurate again
and the demo recording can use `get_cross_repo_blast_radius` directly
(no more pivot to `get_workspace_graph` for the cross-repo probe).

---

## Items surfaced during the #13 close-out (2026-08-29)

The use-case proving test work that closed #13 also exposed seven
real bugs in adjacent code paths. Each is filed below as its own
item so a follow-up pass can address them in priority order. All
were found by `tests/use_cases/*.rs` (a new directory of one-file-per-
use-case proving tests — see `docs/use_cases_inventory.md` for the
full inventory and the verification procedure for each new test).

## 14. `find_anchors` returns 0.000 for every function in small fixtures

**Symptom:** for any fixture where every function has `calls_in = 0`
(small files, or fixtures where the LSP path didn't pick up the
calls), the anchor score collapses to 0. The sort is unstable at
zero, so the top anchors come back in arbitrary order. The
`tests/use_cases/find_anchors.rs` proving test currently works
around this by switching to an in-process graph build, but the
underlying scoring pipeline is broken.

**Root cause (suspected):** score formula
`calls_in * log2(1 + calls_out) * size_factor` collapses for small
files where every function has `calls_in = 0` and `size_factor = 1`.
The `calculate_anchor_scores` body has the right intent (two-pass
percentile normalization) but the pre-normalization raw scores are
all 0 in small fixtures, so the normalized distribution is
indistinguishable.

**Suggested fix:** either include tree-sitter- or LSP-detected
"structural importance" signals (e.g., `is_test_symbol`,
`is_false_positive_name` overrides) in the raw score, or change the
scoring so `size_factor` alone (or a fan-out-only variant) is the
score when calls_in is 0.

**Proving test to add:** none until the fix lands; the
`tests/use_cases/find_anchors.rs` test currently runs only against
an in-process graph build (deterministic); an LSP-driven regression
test would catch the underlying issue.

## 15. `resolve_node` returns NotFound for symbols that exist (by-name lookup broken)

**Symptom:** `get_call_sites(target)` returned `"Node not found for
handle: target"` even though the per-repo DB had a function named
`target` and the federated `search_org` found it. The tool only
worked when called with the node's UUID (`d4037d74-...`).

**Root cause (suspected):** not yet investigated. `resolve_node`
walks the graph via overlay → graph → overlay by name → graph by
name. The per-repo graph's `find_node_by_name` filter
(`n.name == name`) should match — perhaps the per-repo DB lookup
binds to a different graph than `list_repos` reports, or perhaps
`find_node_by_name` doesn't handle the case where the node has
been projected (id rewritten) but `n.name` was never re-populated
from the projection.

**Suggested fix:** add a diagnostic inside `resolve_node` to print
which step missed and which DB it queried. The test
`tests/use_cases/get_call_sites.rs` works around the issue by
querying the federated search to find the node id, then calling
the tool with the id.

**Proving test to add:** a test that calls `get_call_sites` with a
known symbol name (no id lookup) against a single-repo fixture
where the symbol is defined and indexed. The test should pass.

**Closed 2026-08-30.**

Investigation found the resolver code was correct; the bug was the
*test fixture*, not `resolve_node`:

- `resolve_node` canonicalizes `handle` to an absolute path when it
  points at something on disk. Running the test from
  `/home/sebastian/lain`, the literal name `"target"` resolved to
  the real `target/` directory in the working directory, the
  canonical form `/home/sebastian/lain/target` skipped every name
  lookup in the resolver, and the call returned `NotFound` despite
  the per-repo DB containing a node with `name="target"`.
- Two proving tests added in
  `tests/federation_integration.rs` pin the contract at the
  resolver boundary:
  - `resolve_node_finds_indexed_function_by_name` — inserts a node
    with `name="target"`, `chdir`s to the tempdir so the handle is
    unambiguously a name, then asserts name lookup succeeds and
    returns the right node.
  - `resolve_node_ambiguous_returns_other_definitions` — inserts two
    `parse` nodes in different files and asserts
    `resolve_node_ambiguous` returns the chosen node plus exactly
    one alternative definition (so the tool can surface the
    ambiguity to the caller).
- The LSP race the original symptom pointed at (rust-analyzer not
  populating the `detail` field that feeds `name`) is real but is
  a separate defect from the resolver's contract; the resolver now
  has a regression pin at the function level.
- Commit: `b8e4f01 test(federation): prove resolve_node finds
  indexed function by name (#15)`.

## 16. `find_cross_repo_matches` requires a populated `signature` field

**Symptom:** two functions with the same name across repos
(e.g. `shared_helper` in both `a` and `b`) don't get a
`CrossRepoSameSymbol` peer edge. The matcher returns an empty list.

**Root cause:** the matcher tokenizes `node.signature` and computes
cosine similarity. When rust-analyzer's `documentSymbol` doesn't
populate the `detail` field (which happens for some symbols — the
exact criteria aren't clear), the signature is empty, the token
list is empty, similarity is 0.0, no peer edge fires.

**Suggested fix:** fall back to a name-only signal when the signature
is empty. The simplest version: when the signature is empty, treat
it as a single-token signature equal to `node.name` and compare
against the other node's name. This would make the matcher robust
to the LSP detail-population gap.

**Proving test to add:** the existing
`tests/use_cases/workspace_graph_peers.rs` pins the
"both function nodes surface" contract (which works today). A
follow-up test should additionally assert the `CrossRepoSameSymbol`
edge between the two nodes — this would fail until the matcher
fallback lands.

## 17. `repo.index()` short-circuits on unchanged commit hash

**Symptom:** a file edit followed by `repo.index()` re-ran, but the
per-repo DB still had only the pre-edit nodes. The new symbol
wasn't there.

**Root cause:** `index_one_repo` returns early if the latest
commit hasn't changed. File edits without a follow-up `git commit`
leave the commit hash unchanged, so the reindex is a no-op. The
file-watcher in production sees the change and signals a reindex
*event*, but the reindex path also checks the commit hash first.

**Suggested fix:** the watcher should either commit on the user's
behalf (configurable) or `repo.index()` should re-walk the worktree
(not the commit tree) when invoked from the watcher. A simpler
heuristic: if the worktree's files differ from the last-indexed
state, force a re-scan.

**Proving test to add:** the existing
`tests/use_cases/watcher_reindex.rs` test commits the change
explicitly between edit and reindex. A follow-up test should skip
the commit and verify that `repo.index()` still picks up the
new symbol (this will fail until the fix lands).

**Closed 2026-08-30.**

The fix is two parts in `index_one_repo` and a new public method
on `RepoIndex`:

- `index_one_repo(..., force: bool)` gains a `force` parameter.
  When `force=true`, both gates the old code had are bypassed: the
  commit-hash early-return and the incremental diff against the
  previous commit. The indexer calls `get_all_tracked_files()`
  instead so the worktree is the source of truth (the user edited
  on disk; the commit tree is unchanged). `force=false` keeps the
  existing optimization for the CLI boot loop.
- `RepoIndex::index_forced()` is the entry point the watcher uses.
  `RepoIndex::index()` keeps `force=false` so every existing caller
  (boot loop, integration tests) is unchanged in behavior.
- The watcher receiver task in `RepoIndex::start_watcher` now calls
  `me.index_forced()` instead of `me.index()`. The kernel inotify
  event is independent evidence a worktree change happened — the
  watcher doesn't need to wait for a commit.

Proving test:
`tests/use_cases/watcher_reindex.rs::index_forced_picks_up_uncommitted_edits`.
It edits a file, deliberately does not commit, calls
`index_forced()`, and asserts the new symbol appears in the
per-repo DB. It also asserts that a *plain* `index()` with an
unchanged commit hash still short-circuits (no node growth), so
the optimization the boot loop depends on is preserved (a fix
that silently removes the optimization is worse than the bug it
solves).

Fails on the pre-fix code (the short-circuit kicks in before any
worktree scan), passes after. Commit: `d5a60b3`.

## 18. `get_code_snippet` error message doesn't name the missing path

**Symptom:** when the path doesn't exist, the tool returned a
generic `"Error: IO error: No such file or directory (os error 2)"`.
The user couldn't tell which path they were looking at.

**Root cause:** `read_file_range` propagates the raw
`std::io::Error` without wrapping the path into a descriptive
LainError. The `LainError::Io` variant only carries the
`io::Error`, not the path.

**Suggested fix:** wrap the `std::fs::read_to_string` error into
`LainError::NotFound` with the path included in the message, e.g.
`format!("Path not found: {path}")`.

**Proving test to add:** extend
`tests/use_cases/get_code_snippet_paths.rs` to assert the error
message contains the missing path. The test currently asserts
only `isError=true` and explicitly notes this is a follow-up.

## 19. `failure_modes.rs` checks survival, not wire shape

**Symptom:** the survival tests verify the server doesn't panic
or hang on malformed input. They don't verify the server returns
the right error shape.

**Root cause:** the `tools_call_envelope` helper returns the full
JSON-RPC envelope, but the survival tests only assert
`is_alive() == true` after the malformed input.

**Suggested fix:** for each survival test, also assert the
JSON-RPC envelope shape (status code, error code, message
naming the input that was malformed).

**Proving test to add:** the existing
`tests/failure_modes.rs::server_handles_concurrent_overloaded_clients`
is a good template — add wire-shape assertions alongside the
survival check.

## 20. Several proving tests were previously passing for the wrong reason

**Symptom:** during the use-case test verification (stub-and-revert
pass), `tests/use_cases/find_anchors.rs` was found to be passing
trivially — its first-name extraction used `split("**")`, but the
tool's actual output format does not have `**` markers, so
`first_name` was always `""` and the `!contains("")` assertion was
trivially true. The test was passing without actually testing
anything.

**Root cause:** the test was written without verifying it would
fail when the underlying behavior was broken. This is the
`use_cases_inventory.md` lesson "a passing test is necessary but
not sufficient."

**Suggested fix:** a periodic audit of every proving test in
`tests/use_cases/` to confirm each test would fail when the
underlying behavior is broken. The audit procedure is
documented in `docs/use_cases_inventory.md` (see "stub-and-revert
is the only way to know a test proves something").

**Proving test to add:** none directly — the audit is the work.
The audit should also check that assertion messages name the
expected and actual values (so a future reader can see what was
checked) and that the test is robust to non-deterministic ordering
(e.g. `HashMap` iteration) when the underlying contract allows it.
