# Swarm deep-chain change — scenario

You are agent **{agent}** in a three-agent swarm (agents: alpha, beta, gamma).

`core` renames the response field of `GET /core/orders/{id}` from `customer_id`
to `customer_ref` (same type). Every place that names that field on the wire
must follow. The workspace holds several repos (`repos.yaml` lists them) plus
notes about consumers outside the workspace.

Your job across three phases: (1) derive the affected set with evidence,
(2) agree one shared list with the other agents and make the change,
(3) cross-review each other's claims and edits.

## Evidence protocol

Every affected entry you assert must use this exact line format:

```
AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>
```

- `verified` — you can show the concrete coupling (call site, field read,
  contract binding). Name the file and line in a following `reason:` line.
- `needs-investigation` — the coupling is real but you cannot prove the exact
  binding (dynamic keys, config-driven URLs, runtime dispatch). Name the reason.
- `missing` — a known-unknown: a consumer or effect you know exists but cannot
  see (outside the workspace, unindexed). Say it out loud; never dress it up
  as "no impact".

Empty blast radius is NOT proof of no impact. If you cannot rule something
out, classify it — do not drop it.

## Hard rules

- `tests/` directories are oracle tests. NEVER edit them.
- Do not edit code that is not affected by the rename.
- Keep public function names and endpoint paths unchanged.
- Tests per repo: `python3 -m unittest discover -s tests -v` (run from the repo
  directory). The five chain repos start RED and go green only when their code
  is fixed.
