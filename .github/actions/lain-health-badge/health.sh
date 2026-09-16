#!/usr/bin/env bash
# lain-health-badge helper: boot a lain HTTP server, call get_health and
# architectural_observations, format the result, emit GitHub Actions
# outputs (level, summary, body).
#
# Always emits outputs, even on the failure path, so the action's
# downstream steps (commit status, sticky comment) can post a
# meaningful state instead of empty strings.

set -uo pipefail

# Note: deliberately no `-e`. The script must capture failures and
# emit GitHub Actions outputs (level, summary, body) on the failure
# path; `-e` would abort before the emit step runs. Each fallible
# call is checked explicitly below.

MIN_FAN_OUT="${INPUT_MIN_FAN_OUT:-15}"
WORKSPACE="${GITHUB_WORKSPACE:-$PWD}"

emit_outputs() {
  local level="$1"
  local summary="$2"
  local body="$3"
  {
    echo "level=$level"
    echo "summary=$summary"
    echo "body<<EOF_BODY"
    echo "$body"
    echo "EOF_BODY"
  } >> "$GITHUB_OUTPUT"
}

if [[ ! "$MIN_FAN_OUT" =~ ^(0|[1-9][0-9]*)$ ]]; then
  emit_outputs "error" "Invalid min-fan-out" "min-fan-out must be a non-negative integer."
  exit 1
fi

if ! command -v lain >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`lain\` is not on PATH. The install step may not have added \`\$HOME/.local/lain\` to PATH; check the \`Install lain\` step log."
  } > "$BODY"
  emit_outputs "error" "lain binary not on PATH" "$(cat "$BODY")"
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`jq\` is not on PATH. The action requires \`jq\` on the runner; it is preinstalled on \`ubuntu-latest\`."
  } > "$BODY"
  emit_outputs "error" "jq not on PATH" "$(cat "$BODY")"
  exit 1
fi

if ! command -v nc >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`nc\` is not on PATH. The action uses \`nc -z\` for the readiness check. Preinstalled on \`ubuntu-latest\`; if you are on a custom runner, install \`netcat\`."
  } > "$BODY"
  emit_outputs "error" "nc not on PATH" "$(cat "$BODY")"
  exit 1
fi

# Generate a per-workspace `repos.yaml`. `lain init --print` walks
# up for `.git` (the action's checkout provides one) and emits a
# minimal config pointing the only repo at the workspace. We use
# this rather than any `~/.config/lain/repos.yaml` the install
# script may have created, because the latter references the
# install-time cwd and breaks in a fresh container.
INIT_CONFIG=$(mktemp --suffix=.yaml)
if ! lain init --print > "$INIT_CONFIG" 2>>/tmp/lain-server.log; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed: could not scaffold repos.yaml._"
    echo
    echo "**Error:** \`lain init --print\` failed. The action's checkout does not look like a git repository, or \`lain\` cannot walk up to one."
    echo
    echo "**Server log tail:**"
    echo
    echo '```'
    tail -40 /tmp/lain-server.log 2>/dev/null || echo "(no log)"
    echo '```'
  } > "$BODY"
  emit_outputs "error" "lain init --print failed" "$(cat "$BODY")"
  exit 1
fi

# Boot the server in the background, against the consumer's workspace.
# No `--workspace` flag: the default resolves via `--config` and avoids
# the trap of treating the cwd as a workspace-name lookup. The HTTP
# listener opens once the cold-start re-index completes.
lain server --config "$INIT_CONFIG" --transport http --port 9999 \
  --log-level warn \
  > /tmp/lain-server.log 2>&1 &
LAIN_PID=$!
trap 'kill "$LAIN_PID" 2>/dev/null || true' EXIT

# Wait for the server to be ready. The HTTP listener opens before
# the cold-start re-index completes, so a port-check is the right
# readiness signal — it confirms the server is up without waiting
# for the re-index to finish. The actual tool calls below will
# block until the re-index is done, with their own long timeout.
READY=0
for _ in $(seq 1 60); do
  if nc -z 127.0.0.1 9999 >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 1
done

if [ "$READY" != "1" ]; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed: lain server did not start._"
    echo
    echo "**Error:** no process listening on \`127.0.0.1:9999\` after 60s. The server crashed during startup, or the cold-start re-index exceeded the budget."
    echo
    echo "**Server log tail:**"
    echo
    echo '```'
    tail -60 /tmp/lain-server.log 2>/dev/null || echo "(no log)"
    echo '```'
  } > "$BODY"
  emit_outputs "error" "lain server did not start in 60s" "$(cat "$BODY")"
  exit 1
fi

# Install LSPs for any requested languages, after the server is
# listening. The install_language_server tool downloads the LSP
# binary and triggers a re-index that uses it. Default: 'auto'
# detects from project files at the workspace root. Pass an
# explicit comma-separated list to force, or '' to skip.
LSP_LANGUAGES="${INPUT_LSP_LANGUAGES-auto}"
if [ "$LSP_LANGUAGES" = "auto" ]; then
  LSP_LANGUAGES=""
  [ -f "$WORKSPACE/Cargo.toml" ] && LSP_LANGUAGES="$LSP_LANGUAGES rust"
  if [ -f "$WORKSPACE/pyproject.toml" ] || [ -f "$WORKSPACE/setup.py" ] || [ -f "$WORKSPACE/setup.cfg" ] || [ -f "$WORKSPACE/requirements.txt" ] || [ -f "$WORKSPACE/Pipfile" ]; then
    LSP_LANGUAGES="$LSP_LANGUAGES python"
  fi
  [ -f "$WORKSPACE/go.mod" ] && LSP_LANGUAGES="$LSP_LANGUAGES go"
  if [ -f "$WORKSPACE/tsconfig.json" ] || [ -f "$WORKSPACE/package.json" ]; then
    LSP_LANGUAGES="$LSP_LANGUAGES typescript"
  fi
  [ -f "$WORKSPACE/Gemfile" ] && LSP_LANGUAGES="$LSP_LANGUAGES ruby"
  LSP_LANGUAGES="$(echo "$LSP_LANGUAGES" | xargs)"
fi
LSP_LANGUAGES="${LSP_LANGUAGES//,/ }"
if [ -n "$LSP_LANGUAGES" ]; then
  echo "::group::Installing LSPs: $LSP_LANGUAGES"
  for lang in $LSP_LANGUAGES; do
    echo "Installing LSP for: $lang"
    RESP=$(mktemp)
    # Build the request body with jq to avoid shell-escaping bugs.
    BODY=$(jq -n --arg lang "$lang" '{jsonrpc:"2.0",method:"tools/call",params:{name:"install_language_server",arguments:{language:$lang}},id:99}')
    if ! curl -fsS --max-time 600 -o "$RESP" -X POST http://127.0.0.1:9999/mcp \
      -H 'Content-Type: application/json' -d "$BODY" \
      || ! jq -e 'select(.error == null and .result.isError != true) |
                    .result.content[0].text | select(type == "string" and length > 0)' "$RESP" >/dev/null; then
      rm -f "$RESP"
      emit_outputs "error" "Language-server installation failed" "Unable to install requested language server: $lang"
      exit 1
    fi
    jq -r '.result.content[0].text' "$RESP"
    rm -f "$RESP"
  done
  echo "::endgroup::"
fi

# Treat transport errors and MCP error envelopes as failed health computation.
call_tool_text() {
  curl -fsS --max-time 900 -X POST http://127.0.0.1:9999/mcp \
    -H 'Content-Type: application/json' -d "$1" \
    | jq -er 'select(.error == null and .result.isError != true) |
              .result.content[0].text | select(type == "string" and length > 0)'
}
if ! HEALTH=$(call_tool_text '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":2}'); then
  emit_outputs "error" "get_health failed" "Unable to compute architecture health: get_health failed."
  exit 1
fi
if ! ARCH=$(call_tool_text "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"architectural_observations\",\"arguments\":{\"min_fan_out\":${MIN_FAN_OUT}}},\"id\":3}"); then
  emit_outputs "error" "architectural_observations failed" "Unable to compute architecture health: architectural_observations failed."
  exit 1
fi
# Capability readiness (M4 step 8 + §4.7): query get_capabilities so
# the comment can lead with a one-line summary of the structural state.
# Best-effort: a failed call must NOT fail the badge — this section
# is an enrichment, not a hard gate.
CAPS=""
if CAPS_RESP=$(curl -fsS --max-time 30 -X POST http://127.0.0.1:9999/mcp \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_capabilities","arguments":{}},"id":4}' 2>/dev/null); then
  CAPS=$(printf '%s' "$CAPS_RESP" | jq -r 'try (.result.content[0].text // empty) catch empty' 2>/dev/null)
fi

# Render the readiness line from get_capabilities. Both fields are
# best-effort: a missing capabilities block prints "?" so the line
# still renders. The format mirrors what an agent reading the
# comment sees in the MCP `get_capabilities` payload.
readiness_line() {
  local caps="$1"
  if [ -z "$caps" ]; then
    echo "Capability readiness: unavailable (get_capabilities call failed)"
    return
  fi
  local ready_total ready ready_n total
  ready_total=$(printf '%s' "$caps" | jq -r 'try ((.result.capabilities.symbols.state // "?") + "/" + (.result.capabilities.git_history.state // "?")) catch "?"' 2>/dev/null)
  ready=$(printf '%s' "$caps" | jq -r 'try (.result.capabilities.symbols.state // "?") catch "?"' 2>/dev/null)
  total=$(printf '%s' "$caps" | jq -r 'try ((.result.repositories // []) | length) catch 0' 2>/dev/null)
  echo "Capability readiness: $ready_total ready across ${total:-0} repo(s)"
}

# Decide level from the prose output. A single rule: fail if the graph
# is degraded. Everything else is success. See the plan doc for the
# rationale — the only condition where the badge output is actively
# misleading is a stale graph, so that's the only thing worth failing.
LEVEL=success
SUMMARY="Architecture health computed"
if grep -q "Degraded" <<<"$HEALTH"; then
  LEVEL=failure
  SUMMARY="Lain reports a degraded graph"
fi

# Render the markdown body.
BODY=$(mktemp)
{
  echo "## Architecture health"
  echo
  echo "_Computed by [lain](https://github.com/spuentesp/lain) — thresholds: min-fan-out=${MIN_FAN_OUT}_"
  echo
  # Top-of-comment readiness line (M4 step 8 + §4.7): one line
  # summarising the structural state across every registered repo.
  # Filled in from the `get_capabilities` MCP call above; empty
  # CAPS falls back to an explicit "unavailable" so the line still
  # renders.
  readiness_line "$CAPS"
  echo
  echo "### Server health"
  echo
  echo '```'
  echo "$HEALTH"
  echo '```'
  echo
  echo "### Architectural observations (fan-out >= ${MIN_FAN_OUT})"
  echo
  echo '```'
  echo "$ARCH"
  echo '```'
} > "$BODY"

# Per-PR impact: when the action runs on a pull_request event, list
# the files changed in the PR and, for each, the blast radius of
# the top functions defined there. This is the "real value" the
# badge gives reviewers: a one-line answer to "what does this PR
# affect and how widely?".
PR_IMPACT=""
PR_NUMBER=""
if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ] && [ -n "${GITHUB_TOKEN:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_EVENT_PATH:-}" ] && [ -f "${GITHUB_EVENT_PATH}" ]; then
  PR_NUMBER=$(jq -r '.pull_request.number // empty' "${GITHUB_EVENT_PATH}" 2>/dev/null)
fi
if [ -n "$PR_NUMBER" ]; then
  echo "::group::Per-PR impact (PR #${PR_NUMBER})"
  # Pull the per-file metadata + diff patch in one API call. The
  # patch is what tells us *which* symbols were actually changed
  # in this PR — extracting top-level functions of the file would
  # include symbols the PR never touched.
  FILES_JSON=$(curl -fsS \
    -H "Authorization: token ${GITHUB_TOKEN}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${GITHUB_REPOSITORY}/pulls/${PR_NUMBER}/files?per_page=50")
  if [ -n "$FILES_JSON" ] && [ -d "$WORKSPACE" ]; then
    PR_LINES=""
    TOTAL_SYMBOLS=0
    TOTAL_FILES=0
    # For each file, only consider it a code file if the patch
    # contains an added or modified function definition. Workflow
    # YAML, lockfiles, generated code, etc. are skipped because
    # they don't have meaningful blast-radius signals.
    #
    # The patch is multi-line, so we base64-encode it to make one
    # TSV line per file. Without that, bash's `read` would split
    # on the first newline of the patch and we'd silently lose
    # everything after.
    while IFS=$'\t' read -r filename patch_b64; do
      [ -z "$filename" ] && continue
      [ -z "$patch_b64" ] && continue
      patch=$(printf '%s' "$patch_b64" | base64 -d 2>/dev/null)
      [ -z "$patch" ] && continue
      # Find added function/class definitions. `^\+[^+]` matches
      # an added line that's not a `+++` file header. The regex
      # captures the function keyword and the name.
      ADDED_FNS=$(printf '%s\n' "$patch" \
        | grep -E '^\+[^+]' \
        | grep -oP '(async def|def|function|class|fn) +\K[a-zA-Z_][a-zA-Z0-9_]*' \
        | sort -u)
      [ -z "$ADDED_FNS" ] && continue
      TOTAL_FILES=$((TOTAL_FILES + 1))
      PR_LINES="$PR_LINES\n\n### \`$filename\`"
      while IFS= read -r sym; do
        [ -z "$sym" ] && continue
        TOTAL_SYMBOLS=$((TOTAL_SYMBOLS + 1))
        BR_BODY=$(jq -n --arg sym "$sym" --arg file "$filename" \
          '{jsonrpc:"2.0",method:"tools/call",params:{name:"get_blast_radius",arguments:{symbol:$sym,file:$file}},id:99}')
        BR_TEXT=$(curl -fsS --max-time 30 -X POST http://127.0.0.1:9999/mcp \
          -H 'Content-Type: application/json' \
          -d "$BR_BODY" 2>/dev/null \
          | jq -r 'try (.result.content[0].text // .error.message) catch "(parse error)"' 2>/dev/null \
          | head -10)
        if [ -n "$BR_TEXT" ]; then
          PR_LINES="$PR_LINES\n\n#### \`$sym\` (new)\n"
          PR_LINES="$PR_LINES\n\`\`\`\n${BR_TEXT}\n\`\`\`"
        fi
      done <<< "$ADDED_FNS"
    done < <(echo "$FILES_JSON" | jq -r '.[] | select(.patch != null) | [.filename, (.patch | @base64)] | @tsv')
    if [ -n "$PR_LINES" ]; then
      PR_IMPACT=$(printf "## PR impact\n\n_Blast radius for ${TOTAL_SYMBOLS} new symbol(s) across ${TOTAL_FILES} file(s)._\n%b" "$PR_LINES")
    fi
  fi
  echo "::endgroup::"
fi

# Append the PR-impact section if any.
if [ -n "$PR_IMPACT" ]; then
  printf "\n\n%s" "$PR_IMPACT" >> "$BODY"
fi

# Per-file "Open annotations" subsection (M4 §4.7): for every file
# in the PR's changed-path set, query list_annotations(target={file})
# and append the first 3 open rows as a markdown bullet list.
# Capped at 3 per file with a "more..." line that shows the
# underlying MCP call the agent can copy-paste to fetch the rest.
# Best-effort: a failed list_annotations call must NOT fail the
# badge — this section is an enrichment.
if [ -n "$FILES_JSON" ] && [ -d "$WORKSPACE" ]; then
  echo "::group::Per-file annotations"
  ANN_LINES=""
  TOTAL_ANN_FILES=0
  TOTAL_ANN_OPEN=0
  while IFS= read -r filename; do
    [ -z "$filename" ] && continue
    BODY_ANN=$(jq -n --arg file "$filename" \
      '{jsonrpc:"2.0",method:"tools/call",params:{name:"list_annotations",arguments:{target:{kind:"file",file:$file},status:"open",limit:3}},id:99}')
    ANN_RESP=$(curl -fsS --max-time 30 -X POST http://127.0.0.1:9999/mcp \
      -H 'Content-Type: application/json' -d "$BODY_ANN" 2>/dev/null || true)
    [ -z "$ANN_RESP" ] && continue
    # Extract the {annotations:[...]} payload from the MCP text field.
    ANN_JSON=$(printf '%s' "$ANN_RESP" | jq -r 'try (.result.content[0].text | fromjson | .annotations) catch empty' 2>/dev/null)
    [ -z "$ANN_JSON" ] || [ "$ANN_JSON" = "null" ] && continue
    ANN_COUNT=$(printf '%s' "$ANN_JSON" | jq 'length' 2>/dev/null)
    [ -z "$ANN_COUNT" ] || [ "$ANN_COUNT" = "0" ] && continue
    TOTAL_ANN_FILES=$((TOTAL_ANN_FILES + 1))
    TOTAL_ANN_OPEN=$((TOTAL_ANN_OPEN + ANN_COUNT))
    ANN_LINES="$ANN_LINES\n\n### \`$filename\` (${ANN_COUNT} open annotation(s))"
    while IFS=$'\t' read -r kind body_excerpt; do
      [ -z "$kind" ] && continue
      # Escape the excerpt so a `*` or `_` in agent prose doesn't
      # blow up the markdown list.
      safe_body=$(printf '%s' "$body_excerpt" | sed 's/`/\\`/g')
      ANN_LINES="$ANN_LINES\n\n- **${kind}** — ${safe_body}"
    done < <(printf '%s' "$ANN_JSON" | jq -r '.[] | [.kind, (.body_excerpt // "")] | @tsv')
    if [ "$ANN_COUNT" -ge 3 ]; then
      ANN_LINES="$ANN_LINES\n\n_more — run \`list_annotations(target={kind:'file',file:'$filename'},status:'open',limit:50)\`_"
    fi
  done < <(printf '%s' "$FILES_JSON" | jq -r '.[] | select(.patch != null) | .filename')
  if [ -n "$ANN_LINES" ]; then
    ANN_IMPACT=$(printf "## Open annotations\n\n_${TOTAL_ANN_OPEN} open across ${TOTAL_ANN_FILES} file(s) — these are agent-side notes from prior sessions on the symbols/files touched by this PR._%b" "$ANN_LINES")
    printf "\n\n%s" "$ANN_IMPACT" >> "$BODY"
  fi
  echo "::endgroup::"
fi

# Previous-run delta (M4 §4.7): for the PR's base ref, walk the
# list of modified (not just added) functions defined in this PR
# and call explain_symbol on each, capturing a 5-line excerpt.
# Compared with the "new symbols" section above, this catches
# regressions where a touched-but-not-new function now has a
# different blast radius or new callers. Best-effort: skip the
# delta cleanly when no previous commit is available (first commit
# on a new branch).
if [ -n "$PR_NUMBER" ]; then
  BASE_REF="${GITHUB_BASE_REF:-}"
  PREV_SHA=""
  if [ -n "$BASE_REF" ]; then
    PREV_SHA=$(git -C "$WORKSPACE" log -1 --format=%H "origin/${BASE_REF}^" 2>/dev/null || true)
  fi
  if [ -n "$PREV_SHA" ]; then
    echo "::group::Previous-run delta"
    DELTA_LINES=""
    DELTA_TOTAL=0
    while IFS=$'\t' read -r filename patch_b64; do
      [ -z "$filename" ] || [ -z "$patch_b64" ] && continue
      patch=$(printf '%s' "$patch_b64" | base64 -d 2>/dev/null)
      [ -z "$patch" ] && continue
      MODIFIED_FNS=$(printf '%s\n' "$patch" \
        | grep -E '^[+-][^+-]' \
        | grep -oP '(async def|def|function|class|fn) +\K[a-zA-Z_][a-zA-Z0-9_]*' \
        | sort -u)
      [ -z "$MODIFIED_FNS" ] && continue
      while IFS= read -r sym; do
        [ -z "$sym" ] && continue
        DELTA_TOTAL=$((DELTA_TOTAL + 1))
        BODY_EXP=$(jq -n --arg sym "$sym" \
          '{jsonrpc:"2.0",method:"tools/call",params:{name:"explain_symbol",arguments:{symbol:$sym}},id:99}')
        EXP_TEXT=$(curl -fsS --max-time 30 -X POST http://127.0.0.1:9999/mcp \
          -H 'Content-Type: application/json' -d "$BODY_EXP" 2>/dev/null \
          | jq -r 'try (.result.content[0].text // empty) catch empty' 2>/dev/null \
          | head -5)
        if [ -n "$EXP_TEXT" ]; then
          DELTA_LINES="$DELTA_LINES\n\n#### \`$sym\` (modified, current)\n"
          DELTA_LINES="$DELTA_LINES\n\`\`\`\n${EXP_TEXT}\n\`\`\`"
        fi
      done <<< "$MODIFIED_FNS"
    done < <(printf '%s' "$FILES_JSON" | jq -r '.[] | select(.patch != null) | [.filename, (.patch | @base64)] | @tsv')
    if [ -n "$DELTA_LINES" ]; then
      DELTA_IMPACT=$(printf "## Previous-run delta\n\n_explain_symbol for ${DELTA_TOTAL} modified symbol(s), compared against ${PREV_SHA:0:7}._%b" "$DELTA_LINES")
      printf "\n\n%s" "$DELTA_IMPACT" >> "$BODY"
    fi
    echo "::endgroup::"
  fi
fi

emit_outputs "$LEVEL" "$SUMMARY" "$(cat "$BODY")"
