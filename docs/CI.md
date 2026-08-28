# CI

Notes for whoever wires `lain` into a build pipeline. Nothing here is
enforced by the repository itself — it is operator territory.

## Schema drift

> **Schema drift CI:** every PR must include a step that runs
> ```bash
> make schema
> git diff --exit-code docs/tool-schema.json
> ```
> A non-empty diff fails the build. The drift-detection test in
> `tests/schema_dump_smoke.rs::live_tools_list_byte_matches_on_disk_schema_dump`
> is the local equivalent — it runs as part of `cargo test`.
