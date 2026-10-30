These excerpts exercise Rust gRPC candidate discovery and protobuf identity
matching without generated bindings, an LSP server, or a network connection
during tests. They retain upstream source text; comments mark each original line
range. Added scaffold closes excerpted Rust `impl` blocks and the protobuf
service. The Rust files parse but do not form a compilable crate. The protobuf
excerpt supplies the service and create/get RPC declarations used by the tests.
`provenance.json` maps fixture lines to upstream lines and records SHA-256 hashes
of the original files and excerpts.

NeMo-Fabric revision `4fdf65a4b4c96024327501ae28243f2d206e7ec7` pins OpenShell
revision `6648bd0c290efbc41ba131ee9831ee45cd431f94` in `Cargo.lock`.
OpenShell shared-checkout HEAD `d4e32263eb4554c46c5bae702acada2f4af03eb0`
is a different revision; these fixtures use the lockfile revision.

In NeMo-Fabric's `environment-providers/openshell/src/lib.rs`, the `SdkGateway`
field uses the imported SDK `OpenShellClient` at lines 1967-1969. The SDK wrapper
exposes a generated client through `raw_grpc()`: the create call's method token
occurs at line 2013 and the get call at line 2036. OpenShell's
`crates/openshell-sdk/src/client.rs:99-104` declares the accessor return type;
`raw.rs:22` and `raw.rs:39-44` establish its generated-client alias chain.

`rust-grpc.yaml` records that explicit cross-file accessor mapping. Its receiver
type restricts the mapping to the SDK type, and its provenance names the pinned
upstream evidence. It is fixture configuration, not a built-in OpenShell rule.
Candidate recognition still does not establish semantic binding or complete RPC
coverage.

To retrieve the exact source excerpts again, run from the repository root:

```sh
python3 tests/fixtures/rust_grpc/retrieve.py
python3 tests/fixtures/rust_grpc/retrieve.py --check
```

Retrieval uses exact-SHA URLs from `raw.githubusercontent.com`; it never checks
out a branch or changes the shared NVIDIA clones. `--check` downloads and compares
the expected bytes without modifying fixtures. Tests consume the checked-in files
and need no downloads.
