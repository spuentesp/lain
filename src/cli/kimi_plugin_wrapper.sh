#!/usr/bin/env bash
# Kimi plugin wrapper for Lain. Re-execs the real binary so the plugin
# manifest can use a `./` relative command.
#
# Kimi's plugin security model pins this subprocess's `cwd` to the plugin
# root (`./`), so `lain mcp` walking up from its own cwd would resolve to
# the plugin directory instead of the project. Resolve the workspace here
# from the parent agent's cwd (read via /proc/$PPID/cwd on Linux), which
# is the directory the user opened Kimi in.
#
# The `--workspace` flag belongs to the `mcp` subcommand in clap, not the
# top-level binary. We therefore insert `--workspace <git_root>` *after*
# the `mcp` token, not before. Earlier versions of this wrapper rewrote
# `--workspace <sentinel>` in place, which produced `lain --workspace
# <path> mcp` and clap rejected with `unexpected argument '--workspace'`.
#
# Usage:
#   lain-kimi-wrapper.sh mcp
#   lain-kimi-wrapper.sh --embedding-model /path/to/model.onnx mcp
# Anything else passes through verbatim with --workspace inserted after
# the first occurrence of "mcp".

set -e

agent_cwd=$(readlink "/proc/$PPID/cwd" 2>/dev/null || true)
if [[ -z "$agent_cwd" ]]; then
  echo "lain: could not determine agent cwd from /proc/$PPID/cwd" >&2
  exit 1
fi

if ! git_root=$(git -C "$agent_cwd" rev-parse --show-toplevel 2>/dev/null); then
  echo "lain: could not determine git workspace from $agent_cwd" >&2
  echo "Open the agent inside a git repository, or pass --workspace <path> explicitly." >&2
  exit 1
fi

# Insert --workspace <git_root> right after the first "mcp" token. If
# "mcp" isn't present, append at the end (handles `lain server --config
# ... --transport stdio` invocations through the same wrapper).
new_args=()
injected=0
for a in "$@"; do
  new_args+=("$a")
  if [[ "$a" == "mcp" && $injected -eq 0 ]]; then
    new_args+=("--workspace" "$git_root")
    injected=1
  fi
done
if [[ $injected -eq 0 ]]; then
  new_args+=("--workspace" "$git_root")
fi

exec "lain" "${new_args[@]}"
