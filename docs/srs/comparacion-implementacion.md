# Comparación: SRS vs. implementación actual de LAIN (v0.3.0)

> **Documento interno del grupo — NO forma parte de la entrega al profesor.**
> Compara cada requerimiento del `SRS.md` con lo que la versión vigente de LAIN (v0.3.0, rama `main`) realmente implementa, con evidencia en el código. A diferencia de la entrega, aquí sí se nombran tecnologías.

**Leyenda:** ✅ Implementado · 🟡 Parcial · ❌ No implementado · ❓ No verificado/medido

## Requerimientos funcionales

| RF | Resumen | Estado | Evidencia en el código | Observaciones |
|---|---|---|---|---|
| RF-01 | Inicializar espacio de trabajo | ✅ | `src/cmds/init.rs`, `install.sh` | Instalador interactivo y no interactivo; detecta agente (Claude, Cursor, Windsurf, Cline, Gemini). |
| RF-02 | Construir mapa de conocimiento | ✅ | `src/graph.rs`, `src/treesitter.rs`, `src/schema.rs` | Grafo petgraph persistido en `.lain/graph.bin`; nodos con UUID v5 estable. |
| RF-03 | Actualización automática ante cambios | ✅ | `src/watcher.rs`, `src/overlay.rs` | Watcher + capa volátil (overlay); sliding window cada 30 s. |
| RF-04 | Sincronización bajo demanda | ✅ | herramienta `sync_state` (`src/tools/handlers/registry_impl.rs`) | Re-sincroniza con Git HEAD y consolida overlay. |
| RF-05 | Enriquecimiento completo | ✅ | herramienta `run_enrichment`, `src/tools/handlers/enrichment.rs` | |
| RF-06 | Informe de frescura | ✅ | herramienta `get_master_map` | «Staleness report» por módulo (last LSP/Git sync). |
| RF-07 | Radio de impacto | ✅ | herramienta `get_blast_radius`, `src/tools/handlers/impact.rs` | Ingesta de referencias LSP bajo demanda para aristas de alta confianza. |
| RF-08 | Cadena de llamadas | ✅ | herramienta `get_call_chain` | |
| RF-09 | Llamadores de un símbolo | ✅ | herramienta `get_call_sites` | |
| RF-10 | Dependencias transitivas | ✅ | herramienta `trace_dependency` | |
| RF-11 | Llamadores a nivel de protocolo | ✅ | herramienta `get_cross_runtime_callers`, `src/sensors/` (http, graphql, proto, websocket, openapi) | Cobertura por sensores; precisión no auditada. |
| RF-12 | Estructura por niveles | ✅ | `explore_architecture`, `get_layered_map` | |
| RF-13 | Puntos de entrada | ✅ | `list_entry_points` | |
| RF-14 | Anclas y estabilidad | ✅ | `find_anchors`, `get_anchor_score`, `navigate_to_anchor`, `get_context_depth` | |
| RF-15 | Código sin uso | ✅ | `find_dead_code` | La advertencia de falsos positivos del SRS: verificar cómo la reporta la herramienta. |
| RF-16 | Refactorización y observaciones | ✅ | `suggest_refactor_targets`, `architectural_observations`, `compare_modules` | |
| RF-17 | Explicar un símbolo | ✅ | `explain_symbol` | |
| RF-18 | Acoplamiento por co-cambio | ✅ | `get_coupling_radar`, `src/git.rs` | Jaccard sobre conjuntos de cambio por commit. |
| RF-19 | Historial y estado del repo | ✅ | `get_commit_history`, `get_branch_status`, `get_file_diff` (`src/tools/handlers/gitops.rs`) | |
| RF-20 | Búsqueda semántica | ✅ | `semantic_search`, `src/nlp.rs` | ONNX local (all-MiniLM-L6-v2, 384 dim); opcional (~120 MB). |
| RF-21 | Contexto para el agente | ✅ | `get_context_for_prompt`, `get_code_snippet` (`src/tools/handlers/context.rs`) | |
| RF-22 | Esquema del mapa | ✅ | `describe_schema` | |
| RF-23 | Verificación decorada | 🟡 | `run_build`, `run_tests`, `run_clippy` (`src/tools/handlers/execution.rs`, `decoration/`) | **Atado a Cargo/Rust** (descripciones dicen «cargo build/test/clippy»); el SRS lo especifica agnóstico al toolchain del proyecto analizado. Existe `src/toolchains.rs` + `toolchains/` — verificar cobertura real multi-toolchain. |
| RF-24 | Apoyo a cobertura de pruebas | ✅ | `find_untested_functions`, `get_test_template`, `get_coverage_summary` (`src/tools/handlers/testing.rs`) | Estimación estructural, no cobertura instrumentada (coincide con el SRS). |
| RF-25 | Lenguaje de consulta | ✅ | `query_graph`, `src/query/`, `docs/query-language.md`, `lain query` | |
| RF-26 | Preguntas en lenguaje natural | ✅ | `lain ask` (`src/cmds/ask.rs`) | Verificar profundidad real de la capacidad. |
| RF-27 | Visualizaciones interactivas | ✅ | `src/ui/blast-radius.html`, `call-chain.html`, `coupling.html`, `src/mcp/front_end_monitor.html` | Consola «Query Console» + 3 vistas; corresponde a P-02…P-05 del Anexo D. |

## Requerimientos no funcionales

| RNF | Resumen | Estado | Observaciones |
|---|---|---|---|
| RNF-01 | Consulta < 2 s (p90, ~100 KLOC) | ✅ | Medido en `tests/coordination_benchmark.rs` (2026-08-20): `get_blast_radius` con traversal completo sobre cadena de 10 K funciones p50=47 ms / p99=68 ms; handlers MCP p99 ≤ 38 ms (peor caso `get_audit_log`, que relee el JSONL en cada llamada); `claim_files` con 8 agentes concurrentes p99 = 1.6 ms. |
| RNF-02 | Frescura ≤ 60 s | 🟡 | Jobs periódicos: sliding window 30 s, background sync 60 s (`src/server/jobs.rs`); el peor caso puede exceder 60 s — medir. |
| RNF-03 | Persistencia e IDs estables | ✅ | `.lain/graph.bin`; UUID v5 determinista por (tipo, ruta, nombre). |
| RNF-04 | Procesamiento 100 % local | ✅ | Embeddings ONNX locales; sin llamadas a servicios externos en runtime (la descarga del modelo es en instalación). Verificar con inspección de red. |
| RNF-05 | ≥ 10 lenguajes analizables | ✅ | 11 lenguajes vía LSP (`docs/TECHNICAL.md`): Rust, Go, TS/JS, Python, C/C++, C#, Java, Kotlin, Ruby, Scala, Svelte. |
| RNF-06 | Instalación ≤ 10 min guiada | ❓ | Instalador one-line interactivo existe; falta prueba de usabilidad cronometrada. |
| RNF-07 | Degradación elegante | 🟡 | Fallback tree-sitter cuando no hay LSP (confianza media); comportamiento sin git y sin modelo ONNX: verificar caso a caso. |
| RNF-08 | Escala a 1 MLOC | ❓ | Sin evidencia de pruebas a esa escala. |
| RNF-09 | ≥ 4 asistentes integrables | ✅ | Claude Code, Cursor, Windsurf, Cline (+ Gemini desde commit `728969f`); hooks en `hooks/`. |
| RNF-10 | Consumo de fondo ≤ 10 % núcleo | ❓ | Sin monitoreo registrado. |

## Restricciones

| RS | Estado | Observaciones |
|---|---|---|
| RS-01 Ejecución local | ✅ | Binario local; transportes stdio/HTTP en localhost. |
| RS-02 Protocolo estándar | ✅ | MCP (JSON-RPC sobre stdio/HTTP), crate rust-mcp. |
| RS-03 Degradación sin VCS | 🟡 | Verificar comportamiento de `lain init` y de los análisis históricos en carpeta sin `.git`. |
| RS-04 No modificar el código analizado | ✅ | Escrituras limitadas a `.lain/`; `run_clippy` tiene opción de auto-fix — **excepción a auditar**: con auto-fix sí modifica el código del proyecto. |
| RS-05 Especificación sin tecnologías | ✅ | La entrega no nombra tecnologías; este documento interno sí. |

## Brechas y acciones sugeridas

1. **RF-23 / verificación multi-toolchain:** el SRS promete verificación del «proyecto analizado» en general; la implementación está descrita en términos de Cargo. Auditar `src/toolchains.rs` y decidir: generalizar la implementación o acotar el requerimiento.
2. **RS-04 vs. auto-fix de clippy:** el auto-fix contradice la restricción de no modificar el código analizado. Decidir si se excluye del alcance, se condiciona a confirmación explícita del usuario, o se ajusta la restricción.
3. **RNF cuantitativos (01, 02, 06, 08, 10):** definir y ejecutar un plan de medición; hoy son metas sin evidencia.
4. **RNF-07:** matriz de pruebas de degradación (sin LSP / sin git / sin modelo) y documentar el comportamiento observado.
