# Hooks

Pre-edit hooks claim the file an agent is about to write, so the swarm
sees the intention before the edit lands. The claim goes to
**zoanthrope** — the coordination core (file claims, exclusive leases,
presence, postboard). Lain keeps the hook *wirings* and the tool names;
`lain hooks` remains as a thin forwarding alias so existing
installations keep working.

## What calls what

| Hook | Runs | Purpose |
|---|---|---|
| `<agent>/pre-edit.sh` | `zoanthrope claim --files … --agent … --intent edit` | claim before edit |
| `claude-code/post-edit.sh` | `zoanthrope release --files … --agent …` | release after edit |
| `<agent>/pre-tool.sh` | `lain hooks observe …` | activity feed (lain's own observation layer) |
| `claude-code/pre-commit.sh` | `lain hooks overlap-check …` | symbol-overlap gate (lain code intelligence) |

If `zoanthrope` is not on `PATH`, the claim/release hooks print an
advisory line and exit 0 — **they never block the agent**. The same
fail-soft rule covers a claim refusal: the conflict JSON (which names
the holder) goes to stderr, the exit code stays 0. Advisory vs
blocking enforcement is a zoanthrope hook-layer mode
(`ZOANTHROPE_MODE`); this family is the advisory alias.

## The two-string portability pattern

Every agent framework's `pre-edit.sh` is the same script. To port one
to another framework, change **two strings** — the comment header and
the fallback identity prefix:

```console
$ sed -e 's/claude-code/codex/g' hooks/claude-code/pre-edit.sh > hooks/codex/pre-edit.sh
$ chmod +x hooks/codex/pre-edit.sh
```

If the framework pipes a different stdin envelope, adjust the one
`sed` that extracts `"file_path"` (or pass the path as `$1` if the
framework supports it). Everything else — identity resolution, the
fail-open discipline, the `zoanthrope claim` call — stays identical.

## Identity resolution (all claim hooks)

1. `$ZOANTHROPE_AGENT_NAME` — explicit override (wins);
2. `$LAIN_AGENT_NAME` — legacy override (still honored);
3. framework env vars: `CLAUDE_AGENT_NAME` (prefixed with the kind),
   `MCP_CLIENT_NAME`, `AGENT_NAME`;
4. fallback: `<kind>-<ppid>-<hostname-short>` (stable for the parent
   process).

## `lain hooks` (the forwarding alias)

`lain hooks claim|release|lock|unlock` keep their flags and JSON
shapes. With a lain server on `--url`/`$LAIN_URL` they proxy through
the server's `claim_files`/`release_files` tools; without one they
forward straight to `zoanthrope claim|release` — the same coordination
core either way. `lain hooks observe` and `lain hooks overlap-check`
are lain's own code-intelligence surface and do not involve
zoanthrope.

Agents that go away and come back should run `zoanthrope rejoin
--agent <name>` once after startup — it re-asserts their claims and
reports anything that changed while they were gone.
