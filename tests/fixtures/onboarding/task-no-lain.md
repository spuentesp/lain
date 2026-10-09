# Task — change a function used from several files

You are working in a fresh checkout of a small Python service (see `README.md`).
No code-graph tool is available in this environment. Analyse the code with
ordinary tools (grep, reading files). Do not ask any human for help.

The function `normalize_token` in `core.py` must also collapse runs of internal
whitespace to a single space: `"  A   B  "` must normalize to `"a b"` (it
already strips and lowercases). Public function names must not change, and
nothing under `tests/` may be modified.

## Before editing anything

1. Enumerate every place that would be affected by a change to `normalize_token`
   (including indirect consumers).
2. Print the list, one per line, BEFORE your first edit:

   `AFFECTED: <file>:<symbol>`

## Then

3. Make the change.
4. Make all tests pass: `python3 -m unittest discover -s tests -v`
   (the tests cover direct and indirect consumers; they are the oracle).
5. After the change, re-check your analysis against the code as it is now,
   and print what you verified on a line:

   `INDEX-CHECK: <evidence>`

## Finally

Print `DONE` if everything above is satisfied, or `FAILED` with the reason.
