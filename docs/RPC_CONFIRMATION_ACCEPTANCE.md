# RPC inference and confirmation acceptance

`tests/rust_grpc_contracts.rs::inferred_rpc_confirmation_persists_and_rebuilds_without_lsp`
exercises the complete workflow against exact-revision source fixtures and an
explicit accessor mapping. It starts with an inferred protobuf RPC whose
destination is unknown, calls `check_binding`, previews and applies the returned
binding through `apply_contract_context`, and verifies idempotency, confirmed
provenance, and the enclosing caller's impact path.

The test reloads the persisted configuration through a fresh federation loader
and rebuilds the source graph without LSP. It also verifies that an earlier
snapshot retains its original configuration and lacks the new binding, while a
new snapshot serves the confirmed relationship and its impact path. Bindings
change `config_hash`; prepare both sides of a contract diff under the same
configuration rather than comparing these two differently configured snapshots.

Run the acceptance suite from the combined feature tree:

```sh
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --no-default-features --test rust_grpc_contracts
```

Confirmation supplies deployment context. It does not make an unknown RPC
identity valid, establish runtime execution, or remove coverage gaps for
unsupported Rust wrappers. A mapping is explicit source evidence; maintain it
when its dependency revision changes.
