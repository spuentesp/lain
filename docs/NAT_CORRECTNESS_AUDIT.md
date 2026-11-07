# NeMo Agent Toolkit correctness audit

The baseline audit inspected the three clean repository revisions recorded in the captured live view, using static analysis and the existing smoke service/provider declarations [1]. It did not run the NVIDIA applications or enable LSP. The findings below describe that captured pre-fix build; the follow-up records what was subsequently corrected at analyzer revisions 9 and 10.

## Remediation at analyzer revision 9

The three reported detector defects are fixed and verified on the same NVIDIA revisions [28]. The revised FastAPI query accepts plain, unescaped literals with explicit literal methods and a static endpoint; it declines formatted or escaped paths that need language-aware evaluation [28]. An intermediate query captured a dynamic f-string suffix incorrectly, which led to an additional negative regression before final verification [28].

| Finding | Status at revision 9 |
| --- | --- |
| NAT-01: SQL interpreted as GraphQL | Fixed: both false consumers removed; all three baseline MySQL consumer identities preserved |
| NAT-02: callback published as `/async` | Fixed: false provider absent; real WebSocket declarations remain covered by regressions |
| NAT-03: missing FastAPI keyword registrations | Fixed for the reported plain-literal cases; both GET MCP routes present |
| NAT-04: fixture traffic presented as source coverage | Reporting and strict gate corrected; actual non-test HTTP binding coverage remains unproven |
| NAT-05 / NAT-06: imported and local URL expression loss | Open |
| NAT-07: HTTP suggestions for WebSocket references | Open |

Toolkit's corrected catalog contains 22 HTTP endpoints: 13 non-test-source entries, five test-only entries, one mixed-source entry, one example entry, and two declared entries [28]. Its one bound HTTP caller is still a test [28]. The strict `NAT_REQUIRE_NON_TEST_HTTP_BINDINGS=1` gate therefore fails explicitly with zero non-test source bindings; the default smoke verifies route discovery and the stated attribution checks [28]. These source-path categories do not establish runtime deployment.

Unresolved entries fell from 43 to 41; exactly the two SQL-to-GraphQL records disappeared, and no new unresolved IDs appeared [1] [28]. The corrected HTTP, WebSocket, and unresolved results matched exactly after a strict completed-cache restart [28]. OpenShell retained 120 RPC contracts and 21 bound consumers in its existing smoke scenario [28].

Final validation passed 4,122 workspace tests across 113 suites, with 16 ignored, plus all 54 HTTP regressions, nine startup-harness tests, and three source-scope tests [28]. CI-configured Clippy passed across all targets, the 100-tool schema matched its snapshot, and the new Python harness tests were wired into CI [28].

The final capability demo passed 171 checks with zero failures; two semantic-search checks were skipped because its isolated environment had no embedding model [28]. These local checks ran on Linux and did not include live NVIDIA LSP integration.

The baseline detector source links below now show the working-tree fixes; the citation ledger preserves the original quoted evidence. The original findings and all 43 baseline cases remain available for comparison.

## Remediation at analyzer revision 10

Revision 10 resolves repository-local JavaScript URL constants, named imports, CommonJS destructuring, object members, and simple zero-argument helpers through bounded syntax evaluation. It was measured on the same three NVIDIA revisions as the baseline and revision 9 — `nat` at `2fdf56a4`, `nat-examples` at `eadbb757`, `nat-ui` at `41cb8cf8` — with LSP disabled [29].

The federation reached `ready ready ready` with 22 HTTP endpoints and two bound consumers, the same catalog as revision 9: 13 non-test-source providers, five test-only, one mixed, one example, and two declared [29]. The revision-9 detector assertions still hold — no MySQL-to-GraphQL consumers, no `/async` callback provider, both literal FastAPI GET routes present, and no `ANY` garbage routes [29].

| Finding | Status at revision 10 |
| --- | --- |
| NAT-01: SQL interpreted as GraphQL | Fixed: both false consumers remain absent |
| NAT-02: callback published as `/async` | Fixed: the false provider remains absent |
| NAT-03: missing FastAPI keyword registrations | Fixed: both GET MCP routes present |
| NAT-04: fixture traffic presented as source coverage | Partly addressed: one non-test source binding now exists; it is a heuristic match, so runtime coverage is still unproven |
| NAT-05 / NAT-06: imported and local URL expression loss | Addressed for the expression itself: the original source expression and host are now reported instead of a flattened label |
| NAT-07: HTTP suggestions for WebSocket references | Fixed: the seven WebSocket rows keep their protocol |

Unresolved entries fell from 41 to 40 [29]. One site left the list entirely: `nat-ui:utils/api/mcpClient.ts:28`, which was unresolved with a dynamic expression and is now bound to `http:GET /mcp/client/tool/list` [29]. Six sites changed reported reason without changing source, and 27 surviving sites now carry their original expression instead of a flattened `GET /` or `POST /` label — for example `f"{config.endpoint.chat}"` where revision 9 reported `POST /`, and `` `/content/${filename}` `` where it reported `GET /content/{}` [29].

Six sites changed their reported reason, and they moved in both directions. Four `DataStreamDisplay` and `database-updates` sites moved from `no_route_in_service` to `no_match`: the host is now the explicit local expression `HTTP_PROXY_PATH` rather than absent, and neither state named a service, so no service attribution was lost. Two sites moved the other way — `nat-ui:pages/api/mcp/clients.ts:18` from `no_match` to `no_route_in_service`, and `nat-ui:proxy/server.js:263` from `no_match` to `no_route_in_service` with `target_service` named as `nat` and host resolved to `env:NAT_BACKEND_URL` [29].

NAT-04 is only partly addressed. The bound consumers are `http:GET /mcp/client/tool/list` and `http:POST /chat/stream`; the first is a non-test source binding at `nat-ui:utils/api/mcpClient.ts:28` and the second is a test [29]. The strict `NAT_REQUIRE_NON_TEST_HTTP_BINDINGS=1` gate fails only when no non-test source binding exists, and one now does, so its failure condition is no longer met [29]. The binding itself is a heuristic match — detector `unbound_host`, confidence 0.6, exact route match — and a static source classification does not establish runtime deployment [29].

The measurement used a debug build of the analyzer, because the release build did not finish inside its time budget. Analyzer output does not depend on the build profile. The revision-9 numbers remain the record for the earlier correction; this section supersedes them only for the findings whose status changed [28] [29].

## Baseline findings

| ID | Priority | Finding | Evidence |
| --- | --- | --- | --- |
| NAT-01 | P1 | One MySQL SQL region produced two false GraphQL consumers (`_bucket_name`, `self`). | SQL source at line 130 [15]; broad `.execute("` extraction [26]; observed output [1]. |
| NAT-02 | P1 | `websocket:/async` is a false server provider: the source is the browser client's `ws.onclose = async (event) => ...` callback. | `Chat.tsx:540` [6], handler extraction [27], provider output [1]. |
| NAT-03 | P1 | Two literal FastAPI `add_api_route(path=..., methods=["GET"])` definitions were missed: `/mcp/client/tool/list` and `/mcp/client/tool/list/per_user`. | Definition lines 221–260 [25]; complete captured catalog [1]. Runtime activation remains deployment-dependent. |
| NAT-04 | P1 | The sole bound HTTP consumer is in `test_unified_api_server.py:389`. Six of twelve HTTP provider entries come from tests. This view establishes no production HTTP binding. | Source [18]; provider and bound-consumer records [1]. |
| NAT-05 | P2 | Five UI call sites lose imported route components and become `GET <dynamic>`. Four use the stream-update route; one uses the MCP route constant. | Constants [23], example source [7], current results [1]. |
| NAT-06 | P2 | The MCP URL helper and proxy `new URL` expression become `GET /` and `POST /`; those labels discard the original path expression. | Call sites [10] [12], helper [24]. |
| NAT-07 | P2 | Seven WebSocket-reference rows suggest twelve HTTP endpoints each. The candidates do not preserve protocol. | Example source references [4] [19]; candidate records [1]. |

These are source-backed findings and observed output mismatches, not a corpus-wide precision/recall estimate [1]. A missed static definition does not prove that every deployment enables that route [25]. Defaults and runtime-selected values must remain conditional when resolving URLs [23].

The captured output fails four stronger source-backed audit probes, while cache equivalence passes [1]. These probes are recorded in the evidence JSON; production analyzer code and CI assertions were not changed during this audit.

| Audit probe | Result |
| --- | --- |
| SQL source emits no GraphQL consumers | Fail: two false consumers |
| Client callback does not declare a WebSocket provider | Fail: `/async` exists |
| Both literal FastAPI GET definitions are discovered | Fail: both absent |
| HTTP smoke demonstrates a production caller binding | Fail: only a test caller binds |
| Strict warm cache preserves sampled contract-query data | Pass: all six match |

## What the 43 unresolved entries mean

The categories below are an exclusive primary classification of each captured entry [1]. A test entry can also expose URL-normalization loss; classifying it as test context does not certify the detector output [1]. The raw response, source excerpt, source hash, and per-case assessment remain in the evidence JSON [1].

| Classification | Count |
| --- | ---: |
| Test/mock context | 22 |
| Imported route expression lost | 5 |
| CI / OAuth / Piston services outside the configured federation | 5 |
| Registry metadata or CLI defaults at the reported location | 3 |
| Helper/local URL expression lost | 2 |
| Confirmed SQL-to-GraphQL false positives | 2 |
| Runtime-selected chat route | 1 |
| Feedback provider/model gap requiring deployment review | 1 |
| Dynamic proxy: service known, route uncertain | 1 |
| Static content fetch | 1 |
| **Total** | **43** |

External CI/test services and runtime-selected routes can reasonably remain unbound [16] [22]. Static content fetches also need separate scope decisions [14]. The proxy already preserves a known `nat` target without inventing a runtime route [1] [12].

## All cases

Case numbers follow the captured `list_unresolved` order [1]. Line numbers are the tool's reported source locations; excerpts and pinned commits are retained in the JSON [1].

| # | Reported reason | Source | Primary assessment |
| ---: | --- | --- | --- |
| 1 | `no_match` | `nat-examples:examples/agent_identity_tool/scripts/mock_registry_server.py:52` [2] | Registry metadata or CLI defaults at the reported location |
| 2 | `no_match` | `nat-ui:__tests__/api/routes.test.ts:335` [3] | Test/mock context |
| 3 | `no_match` | `nat-ui:__tests__/components/Chat.websocket.test.tsx:41` [4] | Test/mock context |
| 4 | `no_match` | `nat-ui:__tests__/components/Chat.websocket.test.tsx:246` [4] | Test/mock context |
| 5 | `no_match` | `nat-ui:__tests__/components/Chat.websocket.test.tsx:191` [4] | Test/mock context |
| 6 | `no_match` | `nat-ui:__tests__/components/Chat.websocket.test.tsx:194` [4] | Test/mock context |
| 7 | `no_match` | `nat-ui:__tests__/proxy/proxy-integration.test.js:26` [5] | Test/mock context |
| 8 | `no_match` | `nat-ui:__tests__/proxy/proxy-integration.test.js:45` [5] | Test/mock context |
| 9 | `no_match` | `nat-ui:__tests__/proxy/proxy-integration.test.js:74` [5] | Test/mock context |
| 10 | `no_route_in_service` | `nat-ui:components/Chat/Chat.tsx:1165` [6] | Runtime-selected chat route |
| 11 | `no_route_in_service` | `nat-ui:components/DataStreamDisplay/DataStreamDisplay.tsx:62` [7] | Imported route expression lost |
| 12 | `no_route_in_service` | `nat-ui:components/DataStreamDisplay/DataStreamDisplay.tsx:82` [7] | Imported route expression lost |
| 13 | `no_route_in_service` | `nat-ui:components/DataStreamDisplay/DataStreamManager.tsx:88` [8] | Imported route expression lost |
| 14 | `no_match` | `nat-ui:hooks/useFeedback.ts:26` [9] | Feedback provider/model gap requiring deployment review |
| 15 | `no_match` | `nat-ui:pages/api/mcp/clients.ts:18` [10] | Helper/local URL expression lost |
| 16 | `no_route_in_service` | `nat-ui:pages/database-updates.tsx:109` [11] | Imported route expression lost |
| 17 | `no_match` | `nat-ui:proxy/server.js:263` [12] | Helper/local URL expression lost |
| 18 | `no_route_in_service` | `nat-ui:proxy/server.js:234` [12] | Dynamic proxy: service known, route uncertain |
| 19 | `no_route_in_service` | `nat-ui:utils/api/mcpClient.ts:28` [13] | Imported route expression lost |
| 20 | `no_match` | `nat-ui:utils/app/content.ts:19` [14] | Static content fetch |
| 21 | `graphql_no_op` | `nat:packages/nvidia_nat_mysql/src/nat/plugins/mysql/mysql_object_store.py:130` [15] | Confirmed SQL-to-GraphQL false positives |
| 22 | `graphql_no_op` | `nat:packages/nvidia_nat_mysql/src/nat/plugins/mysql/mysql_object_store.py:130` [15] | Confirmed SQL-to-GraphQL false positives |
| 23 | `env_unmapped` | `nat:ci/scripts/gitlab/artifactory_upload.py:76` [16] | CI / OAuth / Piston services outside the configured federation |
| 24 | `env_unmapped` | `nat:ci/scripts/gitlab/artifactory_upload.py:104` [16] | CI / OAuth / Piston services outside the configured federation |
| 25 | `no_match` | `nat:examples/MCP/simple_calculator_fastmcp_protected/tests/test_simple_calculator_fastmcp_protected.py:116` [17] | Test/mock context |
| 26 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:342` [18] | Test/mock context |
| 27 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:353` [18] | Test/mock context |
| 28 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:367` [18] | Test/mock context |
| 29 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:413` [18] | Test/mock context |
| 30 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:424` [18] | Test/mock context |
| 31 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:437` [18] | Test/mock context |
| 32 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:456` [18] | Test/mock context |
| 33 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:467` [18] | Test/mock context |
| 34 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:489` [18] | Test/mock context |
| 35 | `no_match` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:573` [18] | Test/mock context |
| 36 | `no_route_in_service` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:528` [18] | Test/mock context |
| 37 | `no_route_in_service` | `nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py:388` [18] | Test/mock context |
| 38 | `no_match` | `nat:packages/nvidia_nat_mcp/scripts/check_mcp_auth_cookie.py:138` [19] | Registry metadata or CLI defaults at the reported location |
| 39 | `no_match` | `nat:packages/nvidia_nat_mcp/scripts/check_mcp_auth_jwt.py:169` [20] | Registry metadata or CLI defaults at the reported location |
| 40 | `no_match` | `nat:packages/nvidia_nat_memmachine/tests/test_memmachine_integration.py:51` [21] | Test/mock context |
| 41 | `no_match` | `nat:packages/nvidia_nat_test/src/nat/test/plugin.py:833` [22] | CI / OAuth / Piston services outside the configured federation |
| 42 | `no_match` | `nat:packages/nvidia_nat_test/src/nat/test/plugin.py:770` [22] | CI / OAuth / Piston services outside the configured federation |
| 43 | `no_match` | `nat:packages/nvidia_nat_test/src/nat/test/plugin.py:843` [22] | CI / OAuth / Piston services outside the configured federation |

## Cache validation

A second process required the completed cache using `LAIN_REQUIRE_STARTUP_CACHE=1` [1]. It logged a cache hit [1]. `list_unresolved`, the complete `list_contracts` catalog, and four `get_contract` responses matched the cold process exactly in their semantic `data` fields [1]. Timing metadata was excluded [1]. The preserved responses include the existing detector errors [1].

## Next fixes and acceptance criteria

Items 1 and 2 are complete: the SQL, WebSocket-callback, and FastAPI-keyword regressions are in the tree, and revision 10 keeps them passing [29].

1. Strengthen the NVIDIA smoke evaluation to distinguish production bindings from fixture traffic and to report test-only providers separately [1]. Done in revision 9; the positive HTTP binding is no longer test-only at revision 10, but it is a heuristic match rather than a proven production caller [1] [18] [29].
2. Resolve the four `HTTP_PROXY_PATH` sites to a service, or report them as unresolved for a reason that distinguishes an unresolved local host from a missing route [23] [24] [12]. Their expressions are preserved; their service attribution is still absent [29].
3. Preserve uncertain service/route results for genuinely runtime-selected values [6]. The revision-10 measurement keeps the proxy and chat-route cases unresolved rather than guessing [29].

The baseline audit made no analyzer changes. The remediation section above records the subsequent fixes and the remaining open gaps; the original broad pass did not cover these newly identified counterexamples.

Artifacts: [raw responses and full triage](benchmarks/nat-correctness-audit-2026-11-07.json), [citation ledger](benchmarks/nat-correctness-citations-2026-11-07.json), [revision 10 measurement](benchmarks/nvidia-correctness-rev10-2026-11-07.json).

## Sources

[1] /data/agents/orca/workspaces/lain/booby/docs/benchmarks/nat-correctness-audit-2026-11-07.json — Captured live contract tool responses
[2] https://github.com/NVIDIA/NeMo-Agent-Toolkit-Examples/blob/eadbb7573b9ba12c01a9df85c601a32a886d7b25/examples/agent_identity_tool/scripts/mock_registry_server.py — nat-examples:examples/agent_identity_tool/scripts/mock_registry_server.py
[3] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/__tests__/api/routes.test.ts — nat-ui:__tests__/api/routes.test.ts
[4] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/__tests__/components/Chat.websocket.test.tsx — nat-ui:__tests__/components/Chat.websocket.test.tsx
[5] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/__tests__/proxy/proxy-integration.test.js — nat-ui:__tests__/proxy/proxy-integration.test.js
[6] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/components/Chat/Chat.tsx — nat-ui:components/Chat/Chat.tsx
[7] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/components/DataStreamDisplay/DataStreamDisplay.tsx — nat-ui:components/DataStreamDisplay/DataStreamDisplay.tsx
[8] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/components/DataStreamDisplay/DataStreamManager.tsx — nat-ui:components/DataStreamDisplay/DataStreamManager.tsx
[9] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/hooks/useFeedback.ts — nat-ui:hooks/useFeedback.ts
[10] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/pages/api/mcp/clients.ts — nat-ui:pages/api/mcp/clients.ts
[11] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/pages/database-updates.tsx — nat-ui:pages/database-updates.tsx
[12] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/proxy/server.js — nat-ui:proxy/server.js
[13] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/utils/api/mcpClient.ts — nat-ui:utils/api/mcpClient.ts
[14] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/utils/app/content.ts — nat-ui:utils/app/content.ts
[15] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_mysql/src/nat/plugins/mysql/mysql_object_store.py — nat:packages/nvidia_nat_mysql/src/nat/plugins/mysql/mysql_object_store.py
[16] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/ci/scripts/gitlab/artifactory_upload.py — nat:ci/scripts/gitlab/artifactory_upload.py
[17] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/examples/MCP/simple_calculator_fastmcp_protected/tests/test_simple_calculator_fastmcp_protected.py — nat:examples/MCP/simple_calculator_fastmcp_protected/tests/test_simple_calculator_fastmcp_protected.py
[18] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py — nat:packages/nvidia_nat_core/tests/nat/server/test_unified_api_server.py
[19] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_mcp/scripts/check_mcp_auth_cookie.py — nat:packages/nvidia_nat_mcp/scripts/check_mcp_auth_cookie.py
[20] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_mcp/scripts/check_mcp_auth_jwt.py — nat:packages/nvidia_nat_mcp/scripts/check_mcp_auth_jwt.py
[21] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_memmachine/tests/test_memmachine_integration.py — nat:packages/nvidia_nat_memmachine/tests/test_memmachine_integration.py
[22] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_test/src/nat/test/plugin.py — nat:packages/nvidia_nat_test/src/nat/test/plugin.py
[23] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/constants/index.js — nat-ui:constants/index.js
[24] https://github.com/NVIDIA/NeMo-Agent-Toolkit-UI/blob/41cb8cf843091a24bd06d0a5f844a3cb824049d8/utils/app/const.ts — nat-ui:utils/app/const.ts
[25] https://github.com/NVIDIA/NeMo-Agent-Toolkit/blob/2fdf56a41a4a3237a4dd83492fed8e11be7eec99/packages/nvidia_nat_mcp/src/nat/plugins/mcp/client/fastapi_routes.py — nat:packages/nvidia_nat_mcp/src/nat/plugins/mcp/client/fastapi_routes.py
[26] /data/agents/orca/workspaces/lain/booby/src/server/sensors/graphql_consumer_sensor.rs — src/server/sensors/graphql_consumer_sensor.rs
[27] /data/agents/orca/workspaces/lain/booby/src/server/sensors/websocket_sensor.rs — src/server/sensors/websocket_sensor.rs
[28] /data/agents/orca/workspaces/lain/booby/docs/benchmarks/nvidia-correctness-fixes-2026-11-07.json — Analyzer revision 9: corrected NVIDIA results and final verification
[29] /data/agents/orca/workspaces/lain/booby/docs/benchmarks/nvidia-correctness-rev10-2026-11-07.json — Analyzer revision 10: NAT measurement, unresolved diff against revision 9, and source scopes
