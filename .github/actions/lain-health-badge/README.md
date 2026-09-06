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

The pass/fail rule in v0.1 is intentionally narrow: the badge fails only when
`get_health` reports a degraded graph (stale index, failed re-index). This is
the only condition where the badge output would be *actively misleading*.
Thresholds for warn-level rules are reserved for a v0.2 when they are worth
fighting over.

## Inputs

| Input | Required | Default | Purpose |
|---|---|---|---|
| `github-token` | yes | — | Token for the status check and sticky comment |
| `min-fan-out` | no | `15` | Threshold passed to `architectural_observations` |
| `fail-on-warn` | no | `false` | Reserved for v0.2 |
| `comment-header` | no | `lain-health` | Sticky-comment key (change to reset the comment thread) |
| `lain-version` | no | `v0.7.2` | Lain release tag to install |

## Usage

```yaml
name: CI
on:
  pull_request:
    branches: [main]

jobs:
  lain-health:
    runs-on: ubuntu-latest
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
