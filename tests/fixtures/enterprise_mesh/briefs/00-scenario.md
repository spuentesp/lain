# Enterprise mesh change — scenario

You are agent **{agent}** in a three-agent swarm (agents: alpha, beta, gamma).

`orders-svc` renames the field `customer_id` to `customer_ref` (same type)
on every contract surface it owns: its gRPC `protos/orders.proto` (field and
the checked-in generated stubs), its `openapi.yaml` response property, the
payload key of the Kafka topic `order.events`, and the payload key of its
legacy AMQP publish to queue `order-events`. The workspace holds ~10 repos
(`repos.yaml` lists them) plus notes about consumers outside the workspace.
Some repos mention `customer_id` without being coupled to `orders-svc` at
all — same names, different worlds.

Your job across three phases: (1) derive the affected set with evidence,
(2) agree one shared list with the other agents and make the change,
(3) cross-review each other's claims and edits.

## Evidence protocol

Every affected entry you assert must use this exact line format:

```
AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>
```

- `verified` — you can show the concrete coupling (call site, topic, queue,
  contract binding). Name the file and line in a following `reason:` line.
- `needs-investigation` — the coupling is real but you cannot prove the exact
  binding (dynamic keys, unbound hosts, config-driven URLs). Name the reason.
- `missing` — a known-unknown: a consumer or effect you know exists but cannot
  see (outside the workspace, a bus or mechanism nothing resolves). Say it out
  loud; never dress it up as "no impact".

Empty blast radius is NOT proof of no impact. If you cannot rule something
out, classify it — do not drop it. A field name appearing in a repo is NOT
proof of coupling either: same-name decoys exist on purpose.

## Hard rules

- `tests/` directories are oracle tests. NEVER edit them.
- Do not edit code that is not affected by the rename.
- Keep public function names and endpoint paths unchanged.
- Tests per repo: `python3 -m unittest discover -s tests -v` (run from the repo
  directory). The coupled repos start RED and go green only when their code is
  fixed; the decoys stay GREEN throughout.
