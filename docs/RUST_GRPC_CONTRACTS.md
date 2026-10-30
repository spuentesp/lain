# Rust gRPC contract discovery

Federation indexing can identify selected Rust calls to generated tonic-style
clients and match them to protobuf provider facts from other repositories.
Live indexing also queries method definitions through the existing LSP pool.
Syntax-only discovery remains available for snapshots and unavailable language
servers. Neither path establishes complete RPC coverage.

## Mapping configuration

The editable file `src/server/sensors/patterns/rust-grpc.yaml` contains the
candidate syntax patterns and generated-client mapping templates. A repository
can override them in `.lain/patterns/rust-grpc.yaml`; a more specific syntax
query can be supplied at `.lain/patterns/rust/grpc-candidate.scm`. The default
templates describe protobuf Rust generation conventions:

```yaml
client_module_template: "{service_snake}_client"
client_type_template: "{service_camel}Client"
```

`service_snake` and `service_camel` are derived from the protobuf service name.
Identity is established only when the generated module, generated type, and
normalized RPC method match a unique provider key in the federated catalog.
Method spelling or a `Client` suffix alone is insufficient. Multiple matching
service/package identities and normalization collisions remain unresolved.

## Unresolved calls

`list_unresolved` reports an identified RPC with `rpc.status: resolved`, its
contract key, source evidence, enclosing function or method, provider evidence,
and compatible endpoint candidates. The Rust call destination is left unknown;
the tool does not borrow an unrelated environment read or infer a service
destination. `check_binding` can validate an explicit candidate against the
consumer's RPC key and rejects a wrong RPC or same-service binding.

Calls whose identity is unknown or ambiguous are also listed with their
candidate evidence and provider keys, but have no `RpcConsumerFact`. Missing
source or mapping evidence is reported as `missing_evidence`. These cases keep
the RPC coverage ledger incomplete instead of claiming no impact.

The per-site synthetic node has a `Calls` edge from its enclosing real function
or method. This lets existing call traversal reach upstream callers while
preserving real symbol nodes. Node facts are rebuilt during federation joining;
snapshot joining reads source and mapping files from the exact pinned Git blobs
and requires no language server.

The analyzer cache revision advances when the syntax anchors are introduced.
No serialized federation graph fields or graph schema version change. Existing
repositories regenerate the anchors on reindex. This discovery does not trace
arbitrary wrappers, closures, dynamic dispatch, destination dataflow, or LSP
definitions; review coverage evidence before treating an empty result as proof
of no RPC calls.

## Live LSP identity

After protocol discovery, live indexing uses `textDocument/definition` through
Lain's existing local or remote LSP bridge. It probes method calls, including
closure receivers the syntax detector cannot type. A concrete definition must
identify a generated client module/type and a tonic RPC method with transport
dispatch and a literal `PathAndQuery` RPC path. The path narrows protobuf catalog
matching to the actual package/service and method. Names alone do not supply this
evidence. The bridge also supports `textDocument/typeDefinition` for identity
queries; RPC enrichment currently uses method definitions.

`list_unresolved` exposes `rpc.semantic` with definition source locations,
content hashes, the extracted RPC key and the evidence status. A current LSP
answer without a unique generated tonic definition suppresses syntax identity
for that site (`unproven_definition`). Empty/error/unavailable responses retain
the conservative syntax path, with no semantic evidence claimed.

The pass is serial and bounded: at most 32 probes per file, 256 per repository,
and 15 seconds including lock waits and requests. It uses the existing LSP pool;
it does not create another pool. Sites outside those limits retain syntax
coverage and do not acquire an LSP claim. Cancellation stops the pass.

Each probed file is opened in the language server once and re-announced only
when its content changes, so a file with many call sites does not pay a
document open per probe. The multiplexer tracks what it announced; that record,
not the bridge's own document map, decides between opening and changing. A
change notification reaches only a server that was told about the document, so
a language server that died and was restarted gets the file opened again rather
than changed.

Semantic evidence is held only in memory and checked against caller, mapping and
definition contents before reuse. It is cleared/replaced on enrichment and never
serialized into graphs. Restart/reindex is needed to repopulate it. Snapshots
never consume live LSP evidence: they rebuild from their exact source/mapping
revision. A call proven only through a live definition may therefore remain a
snapshot coverage gap, explicitly reported as missing evidence.

The editable `tonic_dispatch_patterns` and `rpc_path_pattern` in
`.lain/patterns/rust-grpc.yaml` customize these live definition checks, alongside
the generated-client and method patterns. Repository overrides replace the full
configuration, so copy the bundled file before changing individual fields.

This enriches method identity, not deployment destination. Use the existing
operator-confirmed mapping workflow for a destination that cannot be inferred.
