# Phase 3 — cross-review (SHARED workspace)

You are agent **{agent}**. Review the work of agent **{reviewee}**:
their claims (the AFFECTED lines they asserted in `consensus/`) and their
edits (git log/show in the repos).

For every claim of {reviewee}, issue a verdict against the CODE (and the
graph, if available): `verified` (coupling demonstrable), `refuted` (you can
show it is wrong), or `unevidenced` (asserted without a checkable reason).
Also list: affected entries {reviewee} MISSED (including known-unknowns that
should have been declared), decoy/noise claims {reviewee} wrongly made, and
edits {reviewee} made outside the affected set (including noise repos,
trap repos or `tests/`).

Write `consensus/review-{agent}.md` and end with a line `DONE`.
