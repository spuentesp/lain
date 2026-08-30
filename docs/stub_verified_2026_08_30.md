# Stub-verified tests — 2026-08-30

For every test below, the production code was edited to break the
test's contract, the test was observed to fail with a specific
message, the production code was reverted, and the test was observed
to pass again. Each row is empirical evidence, not just "the test
passes green."

## Wishlist fix tests (6)

| Test | Stub applied | Failure observed |
|---|---|---|
| `anchor_hub_tests::dead_function_baseline_weight` (#14) | `size_factor * 0.5` → `size_factor * 0.0` | `dead function must have a non-zero baseline (0)` |
| `resolve_node_finds_indexed_function_by_name` (#15) | commented out `find_node_by_name` branch in `resolve_node` | `NotFound("Node not found for handle: target ...")` |
| `resolve_node_ambiguous_returns_other_definitions` (#15) | same stub | `NotFound("Node not found for handle: parse ...")` |
| `cross_repo_peers_match_by_name_when_signature_missing` (#16) | name-fallback → `vec![]` | `matches.len() = 0 (expected 1)` |
| `index_forced_picks_up_uncommitted_edits` (#17) | `index_one_repo(..., true)` → `false` | `must contain added_after_uncommitted_edit` |
| `cross_repo_calls_edges_materialize_via_real_lsp_pipeline` (#13) | `project_repo` drops `pending_external_edges` | `Saw 4 total edges; expected target was 'a:Function:src/lib.rs:verify_token'` |

## Battery tests — Phase 1 / Phase 2 (this session)

| Test | Stub applied | Failure observed |
|---|---|---|
| `battery_mcp_tools::graph_database_node_count_matches_inserts` | `node_count()` returns `+1` | `left: 8, right: 7` |
| `battery_federation::get_repo_info_rejects_unknown_repo` | `get_repo_info` always `Ok(...)` | `left: ..., right: ... Err` |
| `battery_cli::lain_schema_dump_writes_default_path` | `dump` skips file write | assertion fired (file absent) |
| `battery_presence::heartbeat_with_wrong_token_errors` | heartbeat skips token check | `is_err` was false |
| `battery_hooks::claude_code_pre_edit_exits_zero_with_no_input` | hook early exit returns `99` | `success()` was false |
| `battery_audit::audit_append_then_read_round_trips` | `audit_log_present_and_readable` always false | assertion fired |

## Success-metrics tests — Phase 1 (this session, 9 of 19)

| Test | Stub applied | Failure observed |
|---|---|---|
| `find_anchors_returns_real_hub_at_position_1` | `find_anchors` returns empty | position-1 line had no real_hub |
| `get_call_sites_returns_each_call_line_separately` | `get_call_sites` returns empty string | 3 callers not found |
| `graph_database_node_count_is_exactly_seven` | `node_count()` returns `/2` | count != 7 |
| `graph_database_anchor_score_normalized_to_100` | set all anchor_scores to `50.0` | top score != 100.0 |
| `get_blast_radius_actually_lists_known_callers` | `get_blast_radius` returns "no dependents" | real_hub + do_stuff not in response |
| `trace_dependency_actually_lists_known_callees` | `trace_dependency` returns empty | helper_a + helper_b not in response |
| `find_dead_code_data_surface_counts_exactly_two_dead` | `get_edges_to` returns empty | unreferenced=0 != 2 |
| `query_graph_data_surface_lists_all_five_functions` | `get_nodes_by_type` returns empty | function count != 6 |
| `lain_schema_dump_writes_valid_json_with_tools_array` | `dump` writes `[]` | tools.len() < 30 |

## Weak assertions (Phase 2: tighten these)

Tests that PASS even when production code is stubbed to break them,
because the assertion is too lenient (`!contains(X)` trivially true,
or skip-on-error path).

| Test | Weakness | Fix |
|---|---|---|
| `trace_dependency_does_not_list_unrelated_nodes` | asserts "not contains dead_one, Config" — passes when stub returns empty | Assert response is non-empty AND lists helper_a/helper_b first |
| `get_blast_radius_does_not_list_non_callers` | same | Assert response IS non-empty AND lists the actual callers |
| `get_blast_radius_for_unused_function_is_empty_or_zero` | assertion matches "no dependents" trivially | Assert count == 0 specifically |
| `find_dead_code_actually_lists_dead_symbols` | skip-on-error path | Assert the dead code handler is callable with this fixture |
| `explain_symbol_actually_describes_the_symbol` | skip-on-error path | Same |
| `lain_version_output_contains_version_string` | skips if binary not built | Build the binary or use `--version` directly |
| `find_anchors_score_ratio_real_hub_above_dead` | my stub didn't fire it | Make real_hub score lower than dead_one — needs stronger stub |
| `find_anchors_dedup_count_matches_distinct_names` | need a stub that produces duplicates | Insert node with duplicate name |
| `find_anchors_test_path_appears_with_zero_score` | need a stub that removes test_helper | Filter test_path |
| `graph_database_edge_count_is_exactly_three` | easy | Stub to drop one edge |

## Total

**21 stub-verified tests** across the wishlist fixes (6), earlier
batteries (6), and new success-metrics battery (9). 10 of the
19 success-metric tests have stub-verified empirical evidence; the
remainder need stronger assertions (Phase 2) or are intrinsically
hard to stub (the binary-level tests).
