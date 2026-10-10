# Task — a contract change across repositories

You are in a workspace with several service repositories (see `repos.yaml`):
`orders` (Rust, the provider), `billing` (Python, a consumer), plus `reports`
and `platform`. No code-graph tool is available in this environment — analyse
the code with ordinary tools (grep, reading files). Do not ask any human for
help.

Decision made by the team: `orders` renames the RESPONSE field `customer_id`
to `customerId` (same type). The request side keeps `customer_id`. Endpoint
paths and public function names must not change, and nothing under `tests/`
may be modified in any repo. Committing is allowed if it helps you verify
the result.

## Before editing anything

1. Identify every place affected by this rename across ALL repos, and
   classify the evidence behind each claim:
   - `verified` — the code proves the relationship (you read the call/read)
   - `needs-investigation` — it looks related but you cannot prove it
   - `missing` — you cannot know from the code you can see; say so instead
     of claiming "no impact"
2. Print one line per claim BEFORE your first edit:

   `AFFECTED: <repo>:<file>:<symbol>  EVIDENCE: <verified|needs-investigation|missing>`

## Then

3. Perform the change (provider side and consumer side).
4. Make the tests pass in every repo that has tests:
   `python3 -m unittest discover -s tests -v`
5. Re-check your analysis against the code as it is now, and print what you
   verified on a line:

   `INDEX-CHECK: <evidence>`

## Finally

Print `DONE` if everything above is satisfied, or `FAILED` with the reason.
