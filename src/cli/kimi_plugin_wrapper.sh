#!/usr/bin/env bash
# Kimi plugin wrapper for Lain. Re-execs the real binary so the plugin
# manifest can use a `./` relative command.
#
# Kimi's plugin security model pins this subprocess's `cwd` to the plugin
# root (`./`), so `--workspace auto` inside `lain` would resolve to the
# plugin directory instead of the project. Resolve the workspace here
# from the parent agent's cwd (read via /proc/$PPID/cwd on Linux), which
# is the directory the user opened Kimi in.

set -e

agent_cwd=$(readlink "/proc/$PPID/cwd" 2>/dev/null || true)
if [[ -z "$agent_cwd" ]]; then
  echo "lain: could not determine agent cwd from /proc/$PPID/cwd" >&2
  exit 1
fi

if ! git_root=$(git -C "$agent_cwd" rev-parse --show-toplevel 2>/dev/null); then
  echo "lain: --workspace auto requires a git repository, but none was found from $agent_cwd" >&2
  echo "Open the agent inside a git repository, or pass --workspace <path> explicitly." >&2
  exit 1
fi

# Replace the --workspace <sentinel> pair in the forwarded args with the
# resolved repo. Accepts "--workspace auto" or any other literal value.
args=()
skip=0
for a in "$@"; do
  if [[ $skip -eq 1 ]]; then skip=0; continue; fi
  if [[ "$a" == "--workspace" ]]; then
    args+=("--workspace" "$git_root")
    skip=1
    continue
  fi
  args+=("$a")
done

exec "lain" "${args[@]}"
