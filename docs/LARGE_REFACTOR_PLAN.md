# Large Refactor Plan

This plan addresses the remaining API-shape warnings without hiding them with
additional lint suppressions. The goal is to make ownership, repository
identity, and lifecycle boundaries visible in the types.

## Goals

- Remove the remaining `too_many_arguments` warnings through meaningful domain
  objects.
- Replace repeated nested generic types with named aliases or structs.
- Preserve MCP behavior, serialized formats, and public compatibility during
  migration.
- Keep each change small enough to review and revert independently.

## Phase 1: name the shared dependencies

Introduce `ToolContextDeps` in `server/tools/registry.rs`. It will contain the
graph, overlay, embedder, cross-encoder, Git sensor, LSP pool, tuning config,
caches, sessions, jobs, webhooks, and refresh state currently passed through
`ToolContext::new` and `ToolExecutor::new`.

Replace `ToolContext::new` with `ToolContext::from_deps`. There will be no
compatibility constructor; all callers and tests move to the explicit
dependency object in this refactor.

Add focused tests that confirm the new object preserves the existing shared
`Arc` instances. This matters because changing ownership here could silently
disconnect live presence, job, or refresh state from the server.

## Phase 2: group federation-server configuration

Introduce `FederationServerConfig` for the inputs to
`build_federation_server`:

- transport and port
- repository configuration path
- attribution backend
- embedding model
- workspace selection
- reindex timeout

The four public federation constructors construct this value and call the
internal builder. The old internal argument list is removed rather than
preserved behind a wrapper.

## Phase 3: group indexing inputs

Introduce `IndexRequest` for `index_one_repo`:

- repository path
- graph
- LSP pool
- Git sensor
- overlay
- cross-repository resolver
- source repository ID
- repository namespace
- force mode

Introduce `ScanContext` for the per-file scanner. It should carry the
workspace, sync timestamps, commit hash, namespace, and LSP handle. The scan
functions should receive the context plus the file batch, rather than a long
sequence of unrelated scalar arguments.

The constructors should enforce these invariants:

- every production node has a repository namespace;
- the namespace belongs to the source repository;
- graph and overlay use the same workspace path convention;
- forced scans cannot accidentally reuse the incremental commit shortcut.

## Phase 4: remove transitional code

Once all internal callers and integration tests use the new types:

- remove the old high-argument functions;
- make the canonical constructors private where possible;
- update API documentation and examples.

This phase should be separate from the initial migration so reviewers can see
the behavior-preserving transition before cleanup removes the old path.

## Phase 5: test-fixture cleanup

The unused test helpers in `tests/common/mod.rs` should be handled after the
production migration. First split helpers into purpose-specific modules:

- process and HTTP server fixtures;
- federation fixtures;
- single-repository fixtures;
- Git and workspace fixtures.

Then remove helpers only after checking every test target with `rg` and running
the complete workspace suite. Do not delete helpers merely because one test
target does not use them; shared integration modules are compiled separately
for each target.

## Validation gates

Each phase must pass:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix tests/js test
npm --prefix npm-shim test
```

Additional regression coverage must verify static/live node ID equality,
repository-scoped overlay lookup, LSP fallback, concurrent indexing, and
manifest persistence.

## Commit sequence

1. `refactor: introduce tool and federation request types`
2. `refactor: migrate indexing and scanning to typed contexts`
3. `test: split fixtures and remove unused helpers`
4. `docs: record API migration and lifecycle invariants`

The first two commits are the substantive refactor. The fixture and
documentation commits should remain separate so they do not obscure behavior
changes during review.
