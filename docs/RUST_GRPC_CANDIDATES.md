# Rust gRPC candidate discovery (PR 1)

`grpc_consumer_sensor::discover_rust_grpc_candidates(root)` scans Rust snapshots
using the shared Tree-sitter parser and file walker. The source-level helper
`detect_rust_grpc_candidates` accepts source, graph path and a loaded
`RustGrpcConfig`. Neither API needs LSP or mutates the graph. Rust candidates are
separate from `GrpcStubCall` and `RpcConsumer` contract facts.

Each candidate retains the method token's exact byte range, one-based line and
byte column, receiver expression, generated type path, and source sites for its
type/import/accessor evidence. Multiple receivers and sites remain separate.
The method spelling is unchanged; no protobuf catalog validates it as an RPC.

The bundled conventions live in `src/server/sensors/patterns/rust-grpc.yaml` and
`rust/grpc-candidate.scm`. A repository can replace either file independently
under `.lain/patterns/`, using those same relative filenames. YAML replaces the
whole configuration, rather than merging arrays. Invalid configuration or a query
missing `@call`, `@receiver`, or `@method` returns an error. The generated-type
regex requires a generated module path; a bare `Client` suffix or an RPC-like
method name supplies no identity evidence. Constructors, async constructors
(`connect(...).await?`), and non-RPC utility method exclusions are configurable.
Async constructors require an await expression before supplying client evidence.

Supported evidence includes explicit local/parameter types, generated-client
constructors, fields on locally declared structs, explicit imports (including
aliases and nested import lists), local type aliases, and methods explicitly
returning a generated-client type. Alias/type/expression traversal stops after
eight steps. Duplicate bindings, cycles, damaged syntax, unknown receivers and
inferred locals reassigned before a call remain unproven. Shadowed bindings are
conservatively rejected instead of attempting Rust name resolution.

Cross-file accessor knowledge is opt-in through `accessors` entries with
`receiver_type`, `method`, `return_type`, and nonempty `provenance`. Match uses the
qualified receiver type, not just the accessor name. See
[`tests/fixtures/rust_grpc/README.md`](../tests/fixtures/rust_grpc/README.md) for the
pinned NVIDIA example and reproducible exact-SHA retrieval. No NVIDIA-specific
names are built into the detector. The mapping is user-supplied evidence and
must be maintained when dependency revisions change.

## Wrappers

A wrapper type defined in the repository (`CachedOpenShellClient` around the
generated `OpenShellClient`) is followed automatically: a method whose body
makes exactly one generated call maps to that RPC (`poll_settings` ->
`get_sandbox_config`), and callers of the wrapper resolve to it. Calls on
the wrapper type itself inside such a body (`self.workspace()`) do not count
as a second RPC, `x.clone().rpc()` keeps `x`'s type, and an unmapped call on
a locally defined wrapper type is ordinary code, not an RPC stub. A file the
bundled grammar cannot parse (for example `#[cfg(..)]` on a field of a
destructuring pattern) is retried with attributes blanked at the same byte
offsets and used only if that parses cleanly. Explicit `wrappers` entries
still win.

This is syntactic candidate discovery, not semantic binding or complete RPC
coverage. It does not traverse external crates, resolve glob imports, trace
closures/general wrappers, infer destination dataflow, normalize protobuf
methods, emit/join contract facts, create caller edges, integrate LSP, or persist
semantic evidence. Existing Python smoke/caller checks do not validate this
Rust path. The dedicated `rust_grpc_candidates` integration tests exercise it
without network access or generated bindings.
