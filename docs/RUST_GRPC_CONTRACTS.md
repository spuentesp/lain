# Rust gRPC contract discovery

Federation indexing can identify selected Rust calls to generated tonic-style
clients and match them to protobuf provider facts from other repositories.
This is syntax-based discovery; it does not establish Rust name resolution,
trait dispatch, or complete RPC coverage.

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
