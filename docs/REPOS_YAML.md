# `repos.yaml` Configuration Reference

`repos.yaml` is the config file `lain server` reads to know which repos to
index in **federation mode**. Federation mode is the additive multi-repo
alternative to single-workspace mode (`lain --workspace ./myrepo`, which is
unchanged). Every repo entry tells `lain` *where* the code lives and *how*
to keep it up to date.

This document is the single source of truth for the `repos.yaml` schema.
The Rust types that parse it live in `src/federation/config.rs`; whenever
they change, this file must change too.

---

## How it's loaded

```bash
lain server --config /etc/lain/repos.yaml --transport http --port 9999
```

- `--config <path>` (required) — path to a `repos.yaml` file
- `--transport http|stdio` (default `http`)
- `--port <u16>` (default `9999`)
- `--log_level <EnvFilter>` (default `info`)

If the file is missing, malformed, or contains an unknown `source.type`,
`lain server` exits with a `LainError::Config` message and a non-zero code.
Indexing errors for individual repos are demoted to `Degraded` and do not
block startup.

---

## Schema (top-level `FederationConfig`)

```yaml
data_dir: <path>                    # optional, default: ./.lain/federation
max_concurrent_indexers: <usize>    # optional, default: 8
ready_threshold: <float 0.0..1.0>   # optional, default: 0.8
repos:
  - id: <string>                    # required
    source: <SourceConfig>          # required (see "Source kinds")
```

| Field                  | Type             | Required | Default                | Notes |
|------------------------|------------------|----------|------------------------|-------|
| `data_dir`             | path string      | no       | `./.lain/federation`   | Per-repo clones and `.lain/graph.bin`-style state files live here. Each repo gets a subdirectory named after its `id`. |
| `max_concurrent_indexers` | unsigned int  | no       | `8`                    | Caps how many repos index in parallel at startup. |
| `ready_threshold`      | float 0.0–1.0    | no       | `0.8`                  | Fraction of repos that must reach `Ready` health before the federation reports `healthy`. |
| `repos`                | list of `RepoConfig` | yes   | —                      | At least one repo is required for a useful federation. |

### `RepoConfig`

```yaml
- id: auth-svc            # required, see rules below
  source:                 # required
    type: local_clone     # one of: local_clone | shallow_clone | workspace_dir
    ...
```

**`id` rules** (enforced by `RepoId::new`):

- Non-empty.
- Must **not** contain `:` (reserved as a separator inside global node IDs).
- Must **not** contain `/` (avoids path-traversal ambiguity).
- Hyphens, dots, and underscores are fine (e.g. `auth-svc`, `billing.api`).

The `id` becomes the prefix for every global node ID in that repo, so
pick something stable and human-readable — agents will see it in tool
output.

---

## Source kinds

The `source` block is a tagged enum. The discriminator is `type`, with
snake_case variants. Each variant has its own fields.

### `workspace_dir`

Point at a repo that is **already on disk**. No `git` operations are
performed — lain indexes the directory as-is. This is the back-compat
path: every test that uses `lain --workspace ./myrepo` translates 1:1 to
a `workspace_dir` entry.

```yaml
- id: hello-rust
  source:
    type: workspace_dir
    path: /srv/code/hello-rust
```

| Field  | Type | Required | Notes |
|--------|------|----------|-------|
| `path` | path string | yes | Absolute or relative to the working directory when `lain server` was launched. Must point at a non-empty directory. If the directory is not a git working tree, the repo will degrade to `Degraded` health during indexing rather than failing at startup. |

**Use when:** the repo lives on the machine, you don't want lain to
manage git state, or you want to test the indexing pipeline without
network access.

**Cost:** zero disk churn; you are responsible for keeping the directory
up to date.

### `local_clone`

Full clone with full history. lain `git clone`s the repo on first run
and `git fetch --all && git reset --hard origin/<ref>` on subsequent
runs. Higher disk cost but you can browse history with any git tool.

```yaml
- id: auth-svc
  source:
    type: local_clone
    url: https://github.com/acme/auth-svc.git
    ref: main           # optional, default: "main"
```

| Field | Type   | Required | Default  | Notes |
|-------|--------|----------|----------|-------|
| `url` | string | yes      | —        | Any URL `git clone` accepts (https, ssh, file://). Must be non-empty. |
| `ref` | string | no       | `"main"` | Branch, tag, or remote-tracking branch. lain resets `HEAD` to `origin/<ref>`. |

**Use when:** you need full git history (e.g. co-change mining across
many commits) or you want operators to inspect the cloned repo
locally.

**Cost:** disk grows linearly with history. For a 10-year-old monorepo
this can be hundreds of MB per repo.

### `shallow_clone`

Like `local_clone` but `git clone --depth 1` on first run. On
subsequent runs, `git fetch --depth 1 origin <ref>` then
`git reset --hard origin/<ref>` — same fetch cadence as `local_clone`,
but bounded by `--depth 1` from the initial clone onward. Lower disk
cost; co-change mining across history is not possible because history
is shallow.

```yaml
- id: billing-svc
  source:
    type: shallow_clone
    url: https://github.com/acme/billing-svc.git
    ref: main                          # optional, default: "main"
    refresh_interval_secs: 600         # optional, default: 300
```

| Field                   | Type   | Required | Default | Notes |
|-------------------------|--------|----------|---------|-------|
| `url`                   | string | yes      | —       | Same rules as `local_clone`. |
| `ref`                   | string | no       | `"main"` | Same rules as `local_clone`. |
| `refresh_interval_secs` | u64    | no       | `300`   | Captured and exposed via `source.refresh_interval()`; the loader does not currently throttle fetches based on this value. (Future enhancement.) |

**Use when:** the repo is large, you only need the latest commit's
nodes/edges, and disk matters. The default 5-minute refresh interval
keeps a fleet of services reasonably current without hammering the git
remote.

**Cost:** low disk; one network round-trip per refresh.

---

## Examples

### 1. Minimal: one local repo

The smallest valid `repos.yaml`. Every field except `repos` uses its
default.

```yaml
repos:
  - id: hello-rust
    source:
      type: workspace_dir
      path: /srv/code/hello-rust
```

### 2. Two services, one local + one remote

```yaml
data_dir: /var/lib/lain
repos:
  - id: auth-svc
    source:
      type: local_clone
      url: https://github.com/acme/auth-svc.git
      ref: main
  - id: legacy-monolith
    source:
      type: workspace_dir
      path: /srv/legacy
```

### 3. Mixed fleet (all three source kinds)

```yaml
data_dir: /var/lib/lain
max_concurrent_indexers: 4
ready_threshold: 0.9
repos:
  - id: auth-svc
    source:
      type: local_clone
      url: https://github.com/acme/auth-svc.git
      ref: main
  - id: billing-svc
    source:
      type: shallow_clone
      url: https://github.com/acme/billing-svc.git
      refresh_interval_secs: 600   # ref defaults to "main"
  - id: legacy-monolith
    source:
      type: workspace_dir
      path: /srv/legacy
```

### 4. Many repos, all shallow, faster refresh

A typical "index a whole GitHub org every minute" config.

```yaml
data_dir: /var/lib/lain
max_concurrent_indexers: 8
ready_threshold: 0.8
repos:
  - { id: svc-a,    source: { type: shallow_clone, url: https://example.com/a.git } }
  - { id: svc-b,    source: { type: shallow_clone, url: https://example.com/b.git } }
  - { id: svc-c,    source: { type: shallow_clone, url: https://example.com/c.git } }
  - { id: svc-d,    source: { type: shallow_clone, url: https://example.com/d.git } }
  - { id: svc-e,    source: { type: shallow_clone, url: https://example.com/e.git } }
  - { id: svc-f,    source: { type: shallow_clone, url: https://example.com/f.git } }
  - { id: svc-g,    source: { type: shallow_clone, url: https://example.com/g.git } }
  - { id: svc-h,    source: { type: shallow_clone, url: https://example.com/h.git } }
  - { id: svc-i,    source: { type: shallow_clone, url: https://example.com/i.git } }
  - { id: svc-j,    source: { type: shallow_clone, url: https://example.com/j.git } }
```

### 5. Pin to a tag, not a branch

Use `ref` for tags the same way as branches. lain always resets to
`origin/<ref>`, so for an annotated tag named `v1.4.0` you write the
bare name (no `tags/` prefix).

```yaml
repos:
  - id: payments
    source:
      type: local_clone
      url: https://github.com/acme/payments.git
      ref: v1.4.0
```

### 6. SSH URL

`git clone` accepts ssh URLs the same as https. Useful when the repo
is private and the operator has registered a key.

```yaml
repos:
  - id: internal-tool
    source:
      type: local_clone
      url: git@github.com:acme/internal-tool.git
      ref: main
```

---

## Validation rules (cheat sheet)

- `id`: required, non-empty, no `:`, no `/`.
- `source.type`: required, must be one of `local_clone`, `shallow_clone`,
  `workspace_dir`. Anything else is a config error and `lain server`
  refuses to start.
- `url`: required for `local_clone` / `shallow_clone`, must be non-empty.
- `path`: required for `workspace_dir`, must point at a non-empty
  directory. The federation loader does not verify the directory is a git
  working tree; if it isn't, the repo will degrade to `Degraded` health
  during indexing rather than failing at startup.
- All other fields are optional and fall back to the defaults in the
  table above.

---

## Smoke test

These commands verify the example configs in this document are valid.
Run from the repo root.

```bash
# 1. Write the "mixed fleet" example to a temporary file.
cat > /tmp/repos.yaml <<'YAML'
data_dir: /tmp/lain-smoketest
max_concurrent_indexers: 4
ready_threshold: 0.9
repos:
  - id: auth-svc
    source:
      type: local_clone
      url: https://example.com/auth-svc.git
      ref: main
  - id: legacy-monolith
    source:
      type: workspace_dir
      path: /tmp/lain-smoketest/legacy
YAML

# 2. Validate YAML syntax and that the top-level shape matches the schema.
#    Requires `python3` (PyYAML is in the stdlib for `python3 -c`).
python3 - <<'PY'
import yaml, sys
with open("/tmp/repos.yaml") as f:
    cfg = yaml.safe_load(f)
assert "repos" in cfg and isinstance(cfg["repos"], list) and cfg["repos"], "repos must be a non-empty list"
for r in cfg["repos"]:
    assert "id" in r and r["id"], f"repo missing id: {r}"
    assert "source" in r and "type" in r["source"], f"repo missing source.type: {r}"
    assert r["source"]["type"] in {"local_clone", "shallow_clone", "workspace_dir"}, \
        f"unknown source.type: {r['source']['type']}"
print(f"OK: {len(cfg['repos'])} repos, types = {[r['source']['type'] for r in cfg['repos']]}")
PY

# 3. Confirm `lain server` accepts the file (parse-only check via a short
#    stdio session — Ctrl-D exits cleanly, timeout forces exit otherwise).
#    Skipped automatically if `lain` is not on PATH.
if command -v lain >/dev/null 2>&1; then
    timeout 3s lain server --config /tmp/repos.yaml --transport stdio --log_level warn </dev/null \
        || true   # any exit code is fine; we only care that it did NOT print "yaml:" / "config:"
    echo "lain server exited without a parse error"
else
    echo "skipping lain launch (not on PATH); rely on `cargo test --lib federation::config`"
fi
```

`bash -n` should report no syntax errors against the block above. The
canonical parser check is the unit tests in
`src/federation/config.rs` (`parses_minimal_config`,
`build_sources_returns_correct_impls`, `rejects_unknown_source_type`) —
run with:

```bash
cargo test --lib federation::config
```