# `lain-health-badge`

A composite GitHub Action that runs [lain](https://github.com/spuentesp/lain) on
your workspace, posts an architecture-health summary as a sticky PR comment, and
reports a pass/fail commit status. The action is the smallest possible
"add lain to CI" surface: one job, five lines of YAML, no lain-specific config
in the consumer repo.

## What it does

On every pull request, the action:

1. Restores (or builds) lain's on-disk index for the workspace.
2. Boots a `lain server --transport http` against your repo.
3. Calls `get_health` and `architectural_observations` over JSON-RPC.
4. Posts a sticky PR comment with both results.
5. Posts a `lain/health-badge` commit status (pass/fail).

The badge fails on a degraded graph, invalid configuration, or failed MCP health or language-server installation calls. Architecture thresholds remain informational.

## Inputs

| Input | Required | Default | Purpose |
|---|---|---|---|
| `github-token` | yes | — | Token for the status check and sticky comment |
| `min-fan-out` | no | `15` | Threshold passed to `architectural_observations` |
| `fail-on-warn` | no | `false` | Reserved for v0.2 |
| `comment-header` | no | `lain-health` | Sticky-comment key (change to reset the comment thread) |
| `lain-version` | no | `v0.7.3` | Exact published Lain release tag to install |
| `lsp-languages` | no | `auto` | Detect from project files; comma-separated names to select languages; empty string to skip |
| `reindex-timeout` | no | `300` | Reindex timeout in seconds |

## Usage

```yaml
name: CI
on:
  pull_request:
    branches: [main]

jobs:
  lain-health:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      statuses: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v4
      - uses: spuentesp/lain/.github/actions/lain-health-badge@v0.7.3
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
```

That's the whole integration. Override `min-fan-out` if the default doesn't
match your codebase:

```yaml
      - uses: spuentesp/lain/.github/actions/lain-health-badge@v0.7.3
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
          min-fan-out: '25'
```

Keep `lain-version` pinned to an existing release while preparing a new Cargo version. Update the default after the new assets are published; the main-branch version-drift check compares this pin with the latest release. The behavior described here is from the current source; use a release containing these fixes when adopting the new inputs.

## Requirements

- `jq` on the runner PATH (preinstalled on `ubuntu-latest`)
- The first run on a repo pays a full re-index cost (seconds to minutes,
  bounded by `LAIN_REINDEX_TIMEOUT`, default 300s). Subsequent runs restore
  from the GitHub Actions cache in under a second.

## Output

The sticky PR comment renders as:

```
## Architecture health

_Computed by lain — thresholds: min-fan-out=15_

### Server health

\`\`\`
## Lain Server Health

- **Workspace:** /home/runner/work/<repo>/<repo>
- **Status:** Operational
- **Static Nodes:** ...
- **Static Edges:** ...
- ...
\`\`\`

### Architectural observations (fan-out >= 15)

\`\`\`
## Architectural Observations

### High Fan-Out Modules
...
\`\`\`
```

The commit status appears in the PR list as `lain/health-badge` with a
one-line summary.

## Why this is a useful signal

Plain LSP / RAG / file-diff CI tools tell you what changed. This badge tells
you whether your *graph* is still trustworthy. A green badge means the
workspace was re-indexed successfully and the numbers above are real. A red
badge means the indexer hit a problem and the numbers — if any were
generated — are not to be trusted.

The architectural-observations section is a workspace-level signal no other
CI tool produces: it surfaces high-fan-out modules and cross-boundary
patterns from the call graph, with thresholds you control. It's *orientative*
per the tool's own footer — useful for review, not for gating merges.

## See also

- [`docs/COOKBOOK.md`](../../../docs/COOKBOOK.md) for the broader context:
  when to use this in CI vs. lain as an MCP server vs. lain as a long-running
  HTTP server, with recipes for each.
- [`docs/CI.md`](../../../docs/CI.md) for the operator-facing CI contract
  lain itself enforces.
