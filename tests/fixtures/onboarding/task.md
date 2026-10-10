# Task — change a function used from several files

You are working in a fresh checkout of a small Python service (see `README.md`).
It has a code-graph tool installed (`lain` — see its public documentation:
`lain --help`, and the project README/docs if present). Use it as your source
of truth for code relationships. Do not ask any human for help: everything you
need is either in this repository, in the tool's public documentation, or in
public package documentation.

The function `normalize_token` in `core.py` must also collapse runs of internal
whitespace to a single space: `"  A   B  "` must normalize to `"a b"` (it
already strips and lowercases). Public function names must not change, and
nothing under `tests/` may be modified. Committing your change is allowed if
the tool's update path requires it. The code-graph tooling can also emit
verifiable impact claims (e.g. `lain impact --format claims <symbol>`) —
prefer copying its output over inventing a format.

## Before editing anything

1. Bring the code-graph tool up against this repository and enumerate every
   place that would be affected by a change to `normalize_token` (including
   indirect consumers).
2. Print the list, one per line, BEFORE your first edit:

   `AFFECTED: <file>:<symbol>`

## Then

3. Make the change.
4. Make all tests pass: `python3 -m unittest discover -s tests -v`
   (the tests cover direct and indirect consumers; they are the oracle).
5. Query the code-graph tool again to confirm its index reflects your edit.
   If it does not yet, bring it up to date as the tool itself documents,
   then re-query. Never claim freshness you have not observed: an answer
   that warns it may be missing recent changes is stale evidence, not
   confirmation. Print what you saw on a line:

   `INDEX-CHECK: <evidence>`

## Finally

Print `DONE` if everything above is satisfied, or `FAILED` with the reason.
