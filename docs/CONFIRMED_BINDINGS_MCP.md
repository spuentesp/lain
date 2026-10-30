# Persist an explicit contract mapping through MCP

`apply_contract_context` accepts `bindings` using the existing `repos.yaml`
format. An agent can inspect an unresolved consumer, check a proposed endpoint,
preview the YAML change, and apply that declaration to the live federation.
The tool does not infer a destination.

Start with `list_unresolved` on `snapshot: "live"`, then inspect the consumer
and provider evidence. Pass the consumer's existing GlobalId to `check_binding`:

```json
{
  "snapshot": "live",
  "consumer": "<existing consumer GlobalId>",
  "endpoint": {"service": "orders", "key": "rpc:orders.v1/Get"}
}
```

A valid check returns `bindings_entry`, a YAML sequence. Convert its entries to
JSON objects and pass them as `bindings`; do not pass the YAML string itself:

```json
{
  "snapshot": "live",
  "bindings": [
    {
      "consumer": {
        "repo": "billing",
        "path": "src/client.py",
        "symbol": "get_order",
        "key": "rpc:orders.v1/Get"
      },
      "provider": {"service": "orders", "key": "rpc:orders.v1/Get"}
    }
  ]
}
```

The default is `dry_run: true`. The response includes the proposed YAML and
`added_bindings`; the tool leaves the file and live configuration untouched.
Review that preview, then send the same request with `dry_run: false` to apply
it. That explicit call supplies application intent; the tool requires no extra
approval dialog. `check_binding` remains a pure check, and a previous successful
check never exempts an apply request from validation against the current index.

`applied: true` means every requested declaration is currently served, including
on a duplicate request that adds nothing. Inspect `get_contract` for the endpoint
to see the consumer's `binding.kind: "confirmed"` and its `source` location in
`repos.yaml#bindings`. `trace_impact` can expose the confirmed `Binds` hop wherever
the existing graph topology supports that traversal. Restarting or reloading the
federation reads the persisted declaration from the same config.

“Confirmed” identifies an explicit configuration declaration. An agent-applied
mapping does not establish human review or approval. The existing provenance
points to the declaration in `repos.yaml`; it does not carry a reviewer identity
or a separate evidence attachment. Keep supporting source evidence with the
review that motivated the declaration rather than inventing unsupported config
fields.

## Selector limits and rejected mappings

The legacy consumer selector is `(repo, path, symbol, key)`. It has no line or
site identifier, so two calls with that same selector cannot receive independent
mappings. Apply refuses an ambiguous selector that matches several sites; inspect
the reported matches and resolve the declaration manually if broader scope is
intentional. It never promises precision that this config format cannot express.

Apply also refuses missing or stale consumers and endpoints, incompatible keys,
same-service assignments, and a conflicting destination for an already assigned
consumer. RPC matching requires the actual current consumer fact to carry the
same RPC system, service, and method. An identical existing mapping is idempotent;
changing the destination does not silently replace it. `provides` and
`host_aliases` remain available in the same request, and the merge preserves
existing declarations and unrelated YAML sections.

Live application validates the merged config, guards against changes made since
it read `repos.yaml`, writes atomically with a `.bak`, and rejoins the index before
reporting success. A failed rejoin triggers guarded recovery; if another writer
changed the file, recovery leaves that writer's content intact and tries to reload
its valid configuration. Cooperating apply calls share a process and OS advisory
lock through publication and recovery. External editors can ignore that lock;
byte checks detect observed edits, but cannot provide compare-and-swap against
an editor replacing the file between the last check and atomic rename. Inspect the
returned recovery details before retrying a failed request.

## Snapshots and diffs

Only `snapshot: "live"` accepts writes. Existing snapshots keep their original
configuration and graph; applying a live mapping never mutates an old snapshot.
Bindings participate in `config_hash`, so an old base snapshot and a newly
prepared head can have different configuration identities even when the source
commits are otherwise comparable.

After applying mappings, prepare both base and head again under the same updated
configuration before calling `diff_contracts`. Use the new snapshot IDs for that
comparison. Retain the older snapshots when their original configuration matters;
do not modify them to make a diff appear compatible.
