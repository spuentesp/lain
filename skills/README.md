# Lain skill bundle

Agent-loadable skills for working with Lain's MCP tool surface. Each
skill maps to one or two [capability packages](../docs/USER_MANUAL.md#tool-surface):
it teaches when the package matters, which tools to reach for, and
how to load it if the tools are not visible.

## Install

Copy or symlink the skill directories into your agent's skills
location — whichever it loads at startup:

- Claude Code (user): `~/.agents/skills/` or `~/.claude/skills/`
- Claude Code (project): `.claude/skills/`
- Cursor / others: wherever your agent loads instruction files;
  the `SKILL.md` body works as a rules file too.

```bash
ln -s "$(pwd)/skills/lain-comprehension" ~/.agents/skills/
ln -s "$(pwd)/skills/lain-verify"        ~/.agents/skills/
ln -s "$(pwd)/skills/lain-coordination"  ~/.agents/skills/
ln -s "$(pwd)/skills/lain-architecture"  ~/.agents/skills/
ln -s "$(pwd)/skills/lain-federation"    ~/.agents/skills/
```

## Role recipes

The reliable way to shape an agent's surface is at config time —
set `LAIN_TOOL_PROFILE` in the agent's MCP server entry (values
compose as a comma list):

| Agent role | Profile | Why |
|---|---|---|
| Coding agent | *(default)* | 18-tool core: orient, understand, assess impact |
| Reviewer / architect | `arch,verify` | layered maps, module comparison, then build/test evidence |
| Coordinator (multi-agent) | `session,social,notes` | claims, roster, handoffs |
| Operator / setup | `ops` | reload, status, LSP install, re-enrichment |

Example MCP entry:

```json
{"mcpServers": {"lain": {
  "command": "lain", "args": ["mcp"],
  "env": {"LAIN_TOOL_PROFILE": "arch,verify"}
}}}
```

## The dynamic path

If the surface lacks a tool, call `list_packages` (the skill menu)
then `load_package("<package>")`. The response announces the tools
it brings and flags `tools_list_changed` — **if the new tools do not
appear, reconnect the MCP client** (most clients refetch on the
notification; a few cache `tools/list` for the session). Tools are
callable by name even when hidden — advertising is not dispatch —
but a client that validates against its cached list will refuse
them until it refreshes.
