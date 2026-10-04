# Phase C — Env aliases — Report

Spec: `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §6
Plan: `docs/superpowers/plans/2026-10-02-coverage-and-protocols.md` §"Phase C"

## Per-task commit hashes

| Task | Commit | Subject |
|---|---|---|
| 1 (EnvBinding + env_sensor scanner) | `a457d806` | feat(sensors): EnvBinding + env_sensor scanner (phase 0) |
| 2 (HostPart::Env join + EnvUnmapped/EnvAmbiguous) | `610c71fb` | feat(contracts): HostPart::Env + env-var join |
| 2 (refinement: env_unmapped authoritative) | `172de807` | fix(contracts): env_unmapped is authoritative for HostPart::Env |
| 3 (clients.rs process.env.X) | `51177838` | feat(contracts): clients.rs recognizes process.env.X |
| 4 (acceptance C1–C5 + C6) | `f4db2c60` | test(contracts): Phase C acceptance scenarios (C1–C5) |
| 5 (chore: check-no-duplicate-sensors accepts macro) | `07b885a3` | chore(check): accept register_sensor! macro pattern |

## Acceptance outcomes (C1–C5)

`tests/env_aliases.rs` runs all 6 scenarios under `cargo test --test env_aliases`:

```
running 6 tests
test c1_docker_compose_env_var_binds_to_service ... ok
test c2_no_env_file_lands_in_unresolved ... ok
test c3_conflicting_env_values_are_ambiguous ... ok
test c4_helm_values_yaml_env_block_binds ... ok
test c5_k8s_manifest_env_block_binds ... ok
test c6_phase_b_registry_base_with_env_var_binds ... ok
test result: ok. 6 passed; 0 failed
```

- **C1** (spec §6): `process.env.ORDERS_API_URL` with
  `ORDERS_API_URL=http://orders:8080` in `docker-compose.yml`
  binds the consumer to the orders service via the env_sensor's
  `services[].hosts` match. Provenance is `Static { source:
  TreeSitter }` (rule 3 wins), confidence 1.0.
- **C2** (spec §6): no env file → the consumer's `target` is
  `Unresolved { reason: EnvUnmapped, target_service: None }`
  and `JoinOutput::unresolved_env_vars["ORDERS_API_URL"] = 1`
  for the orchestrator to fold into the coverage ledger.
- **C3** (spec §6): the same var defined in two sources with
  different hosts → `Unresolved { reason: EnvAmbiguous }` and
  `JoinOutput::ambiguous_env_vars` is populated.
- **C4** (spec §6): helm `helm/values.yaml` env block is
  parsed; `EnvSource::HelmValues` is the source label.
- **C5** (spec §6): k8s `k8s/deploy.yaml` env block under
  `containers:` is parsed; `EnvSource::K8sEnv` is the source
  label.
- **C6** (Phase B + Phase C interaction): a `ClientDef` whose
  `base` is `UrlPart::Env([ORDERS_API_URL])` + the env_sensor
  binding → tier-2 join path emits a Binds edge to orders.

## Gate outcomes

| Gate | Result |
|---|---|
| `cargo test --lib` (2010 tests) | ✓ 2010 passed, 0 failed, 1 ignored |
| `cargo test --test env_aliases` (6 tests) | ✓ 6 passed |
| `cargo test --test wrapper_resolution` (6 tests) | ✓ 6 passed |
| `cargo test --test property_join_pipeline` (4 tests) | ✓ 4 passed |
| `cargo test --test contracts_golden` (42 tests) | ✓ 42 passed |
| `cargo test --test contracts_soundness` (3 tests) | ✓ 3 passed |
| `cargo test --test coverage_ledger` (7 tests) | ✓ 7 passed |
| `cargo test --test federation_contracts_e2e` (40 tests) | ✓ 40 passed |
| `cargo test --test contract_federation_integration --test federation_integration` | ✓ 30 passed |
| `pr13_hermetic_precision_recall_over_t1_fixture` | ✓ 1.000 × 6 |
| `cargo clippy --all-targets -- -D warnings` | ✓ clean |
| `cargo fmt --check` | ✓ clean |
| `scripts/check-mcp-dispatch-shape.py` | ✓ pass |
| `scripts/check-no-duplicate-sensors.py` | ✓ pass (after update) |
| `scripts/check-no-mirror-dtos.py` | ✓ pass |
| `scripts/check-format-duration-once.py` | ✓ pass |
| `scripts/check-mod-resolution.sh` | ✓ pass |

## PR13_METRICS_JSON

```
PR13_METRICS_JSON {"diff_precision":1.0,"diff_recall":1.0,"binds_precision":1.0,"binds_recall":1.0,"reads_field_precision":1.0,"reads_field_recall":1.0}
```

## Notes

- The env_sensor registers at phase 0 (per spec §6 "the
  classifier sees the env bindings first") via the canonical
  `register_sensor!` macro. The macro expands to
  `inventory::submit!($crate::server::sensors::SensorEntry(&EnvSensor))`.
- The new `UnresolvedReason::EnvUnmapped` / `EnvAmbiguous`
  variants on `index::UnresolvedReason` are surfaced through
  the existing mcp `unresolved_reason_label` match arms as
  `"env_unmapped"` / `"env_ambiguous"`.
- The pre-existing `register_sensor!` macro refactor in the
  working tree was not paired with an update to
  `scripts/check-no-duplicate-sensors.py`; the check was
  still looking for the pre-Phase-A literal
  `inventory::submit!(SensorEntry(&…))` pattern. The check
  was updated to accept both shapes; the unit-struct
  invariant (the registered name must be a `pub struct X;`
  declared in the same file) is preserved.
- The pr13 hermetic fixture does not have any `.env`,
  `docker-compose`, helm, or k8s files. The env_sensor is
  a no-op for it (no bindings indexed), so the fixture's
  `services[].env` name match continues to resolve
  `os.environ["ORDERS_URL"]`-style consumers exactly as it
  did pre-Phase-C. Phase C's new path is additive: it only
  fires when the env_sensor has a binding for the var.
- Spec §6 C2 wording ("the call appears in `unresolved`
  with reason `EnvUnmapped`") is satisfied both at the
  consumer resolution surface (`Unresolved { reason:
  EnvUnmapped }`) and at the coverage-ledger surface (the
  var is recorded on `JoinOutput::unresolved_env_vars` for
  the orchestrator to fold into the ledger's `unresolved`
  bucket).

```
PHASE C COMPLETE — env aliases live, hermetic 1.000
```
