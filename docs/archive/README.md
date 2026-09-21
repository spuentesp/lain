# Archive

This directory holds design plans whose work has **landed** on the
`dev` branch. Each plan was the active contract while the work was
in flight; once the corresponding code shipped and the regression
fixtures passed, the plan was moved here so the maintained docs in
`docs/` stay focused on current behavior.

If you need to understand *why* a feature exists, the archived
plan is the right place to start — it has the design rationale,
the alternative approaches considered, and the original acceptance
criteria. The corresponding tests and the maintained
documentation are the place to understand *how* the feature works
now.

## Contents

| Plan | Status |
|---|---|
| [COORDINATION_CONSISTENCY_PLAN.md](COORDINATION_CONSISTENCY_PLAN.md) | **Landed (PRs 1–6).** File-lock primitive that powers RED coordination. Superseded by the intent layer for the user-facing surface; the primitive itself is still authoritative for exclusive ownership. |
| [INTENT_AND_OBSERVABILITY_PLAN.md](INTENT_AND_OBSERVABILITY_PLAN.md) | **Landed (PRs 1–6, 2026-09-21).** The `lain_intent` / `list_active_intents` MCP tools, the `POST /hook` endpoint, and the GREEN/YELLOW/RED evaluation engine. Now documented inline in [docs/multiplayer.md](../multiplayer.md#intent-layer-pr-15-of-docsintent_and_observability_planmd) and [docs/hooks.md](../hooks.md#activity-observation-pr-2-of-docsintent_and_observability_planmd). |

## Maintenance rule

A plan moves here once its code has shipped and its regression
fixtures pass. Do not modify an archived plan — its content is
historical. If the design changes again, write a new plan and
archive this one.
