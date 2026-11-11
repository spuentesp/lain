# Lain Objective Gap Plan

**Date:** 2026-11-07  
**Goal:** Make Lain's cross-repository answers more accurate, explain incomplete evidence, and verify the release tree against real repositories.

## Working rules

- Keep each experiment pinned to repository revisions and the Lain binary used.
- Preserve existing workspace changes. Do not reset or clean unrelated work.
- Treat unresolved or ambiguous analysis as a result to explain, not as proof that no dependency exists.
- Update documentation only with evidence from the tree that passed the final checks.

## Step 1: Make the Tokio acceptance assertion specific

1. In `scripts/acceptance/run.py`, inspect the result from `get_cross_repo_blast_radius_for_repo` for `Bytes::copy_from_slice`.
2. Assert that a returned Tokio global ID names `examples/connect-udp.rs`. Checking only that the Tokio bucket is nonempty can pass because of a different caller.
3. Assert that `repos.yaml` contains the expected pinned bytes and Tokio revisions after the fixture setup edits the file.
4. Keep the existing tool regression for selecting a type-qualified symbol when a provider has another method with the same name.
5. Run the focused regression tests and multi-repo acceptance flow.

**Commands:**

```bash
cargo test --locked --lib cross_repo_blast_radius_accepts_a_type_qualified_symbol
cargo test --locked --lib a_qualified_external_call_beats_same_named_local_methods
python3 scripts/acceptance/run.py --lain "$PWD/target/debug/lain" \
  --work /data/agents/tmp-lain-tests/objective-gap-acceptance \
  --only 'multi-repo flow'
```

**Pass criteria:** the returned Tokio IDs include `examples/connect-udp.rs`; changing either pin or removing that edge makes the acceptance check fail. Remove the generated acceptance work directory after the run.

## Step 2: Close the NVIDIA contract-analysis findings

1. Re-run the NVIDIA contract smoke against the pinned NeMo Agent Toolkit, UI, and examples repositories. Save the binary identity and repository revisions with the results.
2. For each of the four `HTTP_PROXY_PATH` references in `docs/NAT_CORRECTNESS_AUDIT.md`, trace the expression through its source and configuration. Record a service binding only when the evidence supports it.
3. If a binding remains unknown, record why: for example, the local route is absent, the host is external to the configured federation, or runtime selection prevents static resolution. Keep these cases distinct.
4. Separate production bindings from test, mock, fixture, and example traffic in the evaluator output. Mark convention-based matches as heuristic; keep runtime-selected destinations unresolved.
5. Add or extend fixtures for every corrected classification, then rerun the existing SQL, WebSocket callback, and FastAPI keyword regression cases.
6. Update the audit's remaining-work section and evidence links only after the rerun passes.

**Commands:**

```bash
bash scripts/eval/nat-http-contract-smoke.sh
```

Run the specific regression tests added or changed for the affected sensors and contract resolution alongside the smoke. Record their exact names in the audit.

**Pass criteria:** every audited proxy reference has a supported service attribution or a specific unresolved reason; test-only traffic cannot count as a production binding; all prior regression cases retain their expected answers.

## Step 3: Explain incomplete graph answers

1. Inventory graph tools that return an empty, partial, or truncated result, starting with `get_call_sites`, blast-radius tools, `list_unresolved`, and `explain_dispatch`.
2. Define a small set of observable reasons that the analyzer can support from current data: unresolved name, ambiguous target, dynamic/runtime-selected target, stale or incomplete index, and traversal truncation.
3. Add reason details at the result boundary, using the existing evidence and provenance fields where possible. Do not add a numeric confidence score without a calibrated method behind it.
4. Add fixtures for one correctly resolved call, one ambiguous same-name call, one dynamic call, one stale/incomplete index, and one truncated traversal.
5. Update `docs/quickstart-tools.md` and the relevant API documentation with the new fields and with what an empty result does and does not mean.

**Pass criteria:** tests prove that each case returns the right reason; no dynamic or ambiguous case is reported as a confirmed absence or as a safe change.

## Step 4: Reconcile startup reports and set a measured target

Owner ruling, 2026-11-08: keep the existing targets. No further startup
optimization is requested; the validated post-cache measurements remain the
reference.

1. Compare `docs/NVIDIA_STARTUP_BENCHMARK.md` with the newer post-cache measurements in `docs/STARTUP_CACHE.md`. The older benchmark reports warm OpenShell readiness around 165 seconds; the newer run reports about 3.6 seconds. Preserve both datasets and label their binary, tree, and cache conditions clearly.
2. Verify the newer numbers and graph fingerprints against the referenced raw benchmark artifacts before changing summary text.
3. Set an explicit warm-start readiness target with the project owner before starting another optimization.
4. If the validated results miss that target, profile contract-index reconstruction and service-reference scanning first, since the current phase breakdown identifies them as remaining warm work.
5. Repeat the cold/warm measurement with the same repository revisions and binary settings; compare readiness, memory, and graph fingerprints.

**Pass criteria:** the docs distinguish the pre-cache and post-cache runs without implying they used the same tree or startup path; any optimization has repeatable timing and identical expected graph fingerprints.

## Step 5: Run the release checks on the exact final tree

1. Finish the code and audit changes, then record `git status`, commit identity, toolchain, binary identity, and repository revisions used by external checks.
2. Run the full Rust test suite and the repository's lint checks from the `ci.yml` fast lane.
3. Run the end-to-end acceptance suite and demo:

   ```bash
   python3 scripts/acceptance/run.py --lain "$PWD/target/release/lain"
   bash scripts/demo.sh --quick
   ```

4. Run the incremental mutation gate against the PR base:

   ```bash
   scripts/mutation-gate.sh origin/dev
   ```

5. Run the optional NVIDIA smoke and benchmark harness checks. Preserve raw outputs and clean only the temporary artifacts created by these runs.
6. If any gate fails, fix the failure and rerun all checks affected by that change. Do not treat the docs update as release evidence until the final tree passes.
7. Update the NAT audit, startup benchmark notes, and tool documentation from the final passing evidence. Check links, revision IDs, and claims against the raw results.

**Pass criteria:** required CI fast-lane checks, acceptance, demo, and mutation gate pass on the same final tree; optional NVIDIA checks either pass or have a written, evidence-backed deferral. Docs identify the tested revisions and retain unresolved findings that have not been closed.

Checkpoint, 2026-11-08: the local workspace, lint, schema, acceptance, demo,
and NVIDIA checks passed; the full 608-case incremental mutation gate remains
required. See the [verification record](../../benchmarks/objective-gap-verification-2026-11-08.json)
for source and binary identities, results, and runtime evidence. Step 5 is open.
Both gate phases now validate their unmutated copied-tree baseline; regression
tests prevent build failures from passing as unviable or caught mutants.

## Completion checklist

- [x] Tokio acceptance proves the expected call site and pinned inputs.
- [x] All four `HTTP_PROXY_PATH` cases have evidence-backed outcomes.
- [x] Graph tools explain ambiguity, unresolved calls, stale data, and truncation where applicable.
- [x] Startup reports state comparable conditions and reflect verified raw data.
- [x] Required and optional checks have recorded results from the final tree (the mutation gate is recorded as **waived by the owner** for v0.15.0 after three environment-killed runs — see the release audit's pending inventory; it is not recorded as passed).
- [x] Documentation matches those results; no open audit item is described as closed without evidence.
