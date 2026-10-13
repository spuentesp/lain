# Phase 2 — consensus, division, implementation (SHARED workspace)

You are agent **{agent}**. The three private analyses are in
`consensus/private-alpha.md`, `consensus/private-beta.md`,
`consensus/private-gamma.md`. You are now all working in the SAME workspace.

1. **Consensus**: compare the three lists. Negotiate ONE agreed AFFECTED list
   with evidence classes. Where you disagree, do not paper over it: keep the
   entry with BOTH positions in a `disagreements:` section of
   `consensus/consensus.md` (write this file only when the three of you agree
   on its content — append your position to `consensus/positions-{agent}.md`
   first, then converge).
2. **Division**: split the code edits so no two agents touch the same file.
   {claims_instructions}
   Commit every change with attribution:
   `git commit --author "{agent} <{agent}@swarm.local>" -m "..."` so each
   agent's work is traceable.
3. **Implement**: make the change (only affected code), run each repo's tests,
   and commit per repo (source changes only). Chain repos must end green.
4. Record in `consensus/consensus.md` a final `INDEX-CHECK:` line describing
   how you verified the change after making it.

End with a line `DONE`.
