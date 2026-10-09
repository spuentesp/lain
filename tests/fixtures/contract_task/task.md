# Task — a contract change across repositories

You are in a workspace with several service repositories (see `repos.yaml`):
`orders` (Rust, the provider), `billing` (Python, a consumer), plus `reports`
and `platform`. A code-graph / contract tool `lain` is installed and
understands this multi-repo workspace — see its public documentation
(`lain --help`, `lain schema dump`).

Decision made by the team: `orders` renames the RESPONSE field `customer_id`
to `customerId` (same type). The request side keeps `customer_id`. Endpoint
paths and public function names must not change, and nothing under `tests/`
may be modified in any repo. Committing is allowed if the tool's update path
requires it.

## Before editing anything

1. Identify every place affected by this rename across ALL repos, and
   classify the evidence behind each claim:
   - `verified` — the tooling proves the relationship (static binding /
     field read)
   - `needs-investigation` — the tooling flags it but cannot prove it
   - `missing` — the information is not in the graph at all; say so instead
     of claiming "no impact"
2. Print one line per claim BEFORE your first edit:

   `AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>`

## Then

3. Perform the change (provider side and consumer side).
4. Make the tests pass in every repo that has tests:
   `python3 -m unittest discover -s tests -v`
5. Query the tooling again to confirm it reflects your change. If it does
   not yet, bring it up to date as the tool itself documents, then re-query.
   Never claim freshness you have not observed: an answer that warns it may
   be missing recent changes is stale evidence, not confirmation. Print:

   `INDEX-CHECK: <evidence>`

## Finally

Print `DONE` if everything above is satisfied, or `FAILED` with the reason.
