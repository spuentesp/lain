# Lo que LAIN necesita de Parallax — contrato de integración

> Este documento **no** es un plan de implementación de Parallax. Es el contrato que la parte de LAIN (plan `docs/superpowers/plans/2026-10-14-graphql-config-bridge.md`) necesita que el software consumidor cumpla para que la flecha `storefront → saleor-api` se resuelva y la demo sea honesta. Cómo se implemente dentro del repo de Parallax queda fuera de este documento.

## 1. Contexto

Diagnóstico completo en el plan de LAIN (§Diagnóstico). Resumen en tres puntos:

1. LAIN **sí** detecta los consumers GraphQL del storefront (sonda del 2026-10-14: 63 consumers emitidos desde los `.graphql` del storefront). El "0 ítems" observado en campo fue un artefacto de paginación de `list_unresolved` (página default de 100 items; el desglose de una página de 500 solo contenía el bucket del dashboard).
2. El bloqueo real de la flecha es de **configuración**: las 4 copias de SDL del dashboard (`schema*.graphql`) registran `(op, field)` como providers del servicio `saleor-dashboard`, así que cada operación del storefront ve 2 providers → `graphql_no_op`. La solución es `services[].provides_graphql: false` (knob que introduce el PR de LAIN de este plan).
3. `bindings:` todavía no puede expresar GraphQL (lo agrega el mismo PR). Antes de ese PR, `check_binding` rechaza claves `graphql:` con `"topic endpoints not supported in 0.9"`; después las acepta y emite YAML.

Por lo tanto el rol de Parallax es exactamente el del §10 de su design doc: *"Reutilizar repos, servicios, paths, env, http_clients y bindings. Construir onboarding acotado y validación de la configuración; no descubrir una organización entera automáticamente."* — cerrar brechas de configuración con YAML validado por el motor, no descubrir la organización.

## 2. Flujo requerido (comportamiento, no implementación)

Para cada workspace analizado, el software consumidor debe ser capaz de:

1. **Escanear** los checkouts buscando las señales de §4 (patrones que el motor no ve o malinterpreta).
2. **Correlacionar** esas señales con lo que LAIN reporta (`list_unresolved`, `list_contracts`).
3. **Proponer** entradas de `repos.yaml` con la forma exacta de §5, cada una con su evidencia (archivos/líneas).
4. **Validar** toda propuesta de `binding` contra `check_binding` antes de mostrarla; solo las `valid: true` se presentan como aplicables (las demás con sus `reasons`).
5. **Emitir** el YAML resultante en la forma de §6, listo para aplicar (aplicar es paso del operador — §8 prohíbe auto-aplicar descubrimientos).
6. **Verificar** tras aplicar con los criterios de §7.

## 3. Contrato de herramientas LAIN

Las tres herramientas involucradas (MCP JSON-RPC sobre `POST /mcp`, `method: "tools/call"`). Todos los ejemplos son la forma real de los argumentos y datos.

### 3.1 `list_unresolved`

```json
{ "name": "list_unresolved",
  "arguments": { "snapshot": "live", "repo": "storefront", "limit": 1000, "cursor": "<opaque|null>" } }
```

- **Paginación obligatoria**: página default = 100 items, `limit` máx = 1000, seguimiento de `cursor` hasta agotar. Agregar el desglose de una sola página es un error (así se generó el falso "storefront tiene 0 ítems").
- Forma del item (lo que se consume): `item.consumer.id` (GlobalId, p. ej. `storefront:Function:src/checkout/graphql/checkout.graphql:graphql-call%3Amutation%3AcheckoutLinesUpdate:205`), `item.consumer.repo`, `item.consumer.path`, `item.reason`.
- El nombre del nodo (`graphql-call:{op}:{field}`) viene **URL-codificado** dentro de `consumer.id`; decodificarlo para derivar claves.
- Códigos `reason` relevantes: `graphql_no_op` (0 o ≥2 providers para `(op, field)`), `env_unmapped`, `no_match`, `stale_binding`, `env_ambiguous`.
- `candidates`: para consumidores GraphQL ahora lista solo endpoints GraphQL (los del mismo `op` primero); sigue siendo advisory — para bindings, la fuente de verdad es `list_contracts`.

### 3.2 `list_contracts`

```json
{ "name": "list_contracts",
  "arguments": { "snapshot": "live", "limit": 1000, "cursor": "<opaque|null>" } }
```

- Paginar igual que arriba.
- Item: `item.endpoint = { "service": "saleor-api", "key": "graphql:query:product" }` (o `key: "http:POST /api/orders"` para HTTP). Claves GraphQL: `graphql:<query|mutation|subscription>:<field>` con `field` = campo raíz de la operación (no el nombre de la operación).
- `bound_consumers` cuando existe indica consumers ya resueltos.

### 3.3 `check_binding`

```json
{ "name": "check_binding",
  "arguments": { "snapshot": "live",
    "consumer": "storefront:Function:src/graphql/ProductDetails.graphql:graphql-call:query:product:3",
    "endpoint": { "service": "saleor-api", "key": "graphql:query:product" } } }
```

- `consumer` es el **GlobalId** del nodo consumer (no el nombre simple).
- Respuesta `data`: `{ valid, reasons[], method_match, template_match, bindings_entry? }`.
  - `bindings_entry` (string YAML) aparece **solo cuando `valid: true`** y debe usarse **verbatim** como elemento de `repos.yaml#bindings`. No reescribirlo: su `symbol`/`key` están calibrados para que el matcher del motor (`repo` + `path` + `symbol = graphql-call:{op}:{field}`) lo encuentre.
  - `reasons` posibles: `not_a_consumer`, `same_service`, `already_bound`, `method_mismatch`, `template_mismatch`, y — desde el PR de este plan — `op_mismatch`, `field_mismatch`.
- **Sonda de capacidad**: llamar `check_binding` con una clave `graphql:`. Si el motor responde con error `"topic endpoints not supported in 0.9"`, el motor es anterior al PR (los bindings GraphQL aún no existen); si devuelve un veredicto, está soportado. El advisor debe degradar con elegancia en el primer caso (proponer solo `env`/`provides_graphql`, e indicar que los bindings requieren motor actualizado).

### 3.4 Corrección de contrato pendiente en Parallax

El mirror de la respuesta de `check_binding` en Parallax (`packages/shared/src/lain/tools.ts`, `CheckBindingResponse`) no incluye `bindings_entry` y su comentario afirma que la herramienta no devuelve YAML — ambas cosas son falsas frente al motor actual (`src/server/mcp/contract_tools/schemas/check_binding.out.json` sí lo declara). Alinearlo es requisito para §2.4/§2.5.

## 4. Señales que el advisor debe detectar

**Directiva de diseño (siempre):** los detectores del advisor viven en configuración/plantillas — patrones editables por workspace (archivos de config con los matchers de operaciones, SDL, env-URLs, wrappers, codegen), no hardcodeados en el código. Cambiar o añadir un patrón de detección es un cambio de config, no de código. Misma filosofía que los `<repo>/.lain/patterns/` de LAIN.

Patrones concretos observados en el fixture Saleor (pero no únicos de él). LAIN no los ve, los ve incompleto, o los malinterpreta:

| Señal | Evidencia en el fixture | Por qué importa |
|---|---|---|
| Operaciones `.graphql`/`.gql` completas | `storefront/src/graphql/*.graphql` (32 ops), `src/checkout/graphql/{checkout,user,payment,order}.graphql` | LAIN sí las procesa; el advisor las usa para derivar claves `graphql:op:field` y proponer bindings |
| Documentos solo-fragmento | `storefront/src/graphql/OrderDetailsFragment.graphql`, `UserDetailsFragment.graphql` | LAIN no emite nada por ellos (silencio); el advisor debe reportarlos como "consumidores invisibles" |
| Copias de SDL en repos consumidores | `saleor-dashboard/schema.graphql`, `schema-main.graphql`, `schema-staging.graphql`, `schema-directives.graphql` | Envenenan el key space de providers → el disparador de la propuesta `provides_graphql: false` |
| Variables de entorno como URL base | `src/lib/saleor/client.ts` (`process.env.NEXT_PUBLIC_SALEOR_API_URL`) | Disparador de la propuesta `env:` |
| Fetchers envueltos | `fetchWithTimeout(url, input, …)` en `src/lib/saleor/client.ts` | Corrobora el consumo HTTP aunque el motor no lo detecte por call-shape |
| Configs de codegen | `src/checkout/graphql/codegen.ts`, `saleor-dashboard/codegen-*.ts`, `graphql.config.ts` | Vincula operaciones ↔ schema; corrobora que el SDL del repo es insumo, no superficie |
| Salida de codegen gitignored | `storefront/src/checkout/graphql/generated/` (en `.gitignore`) | El walker de LAIN la saltea; el advisor **no** debe respetar `.gitignore` — su trabajo es ver los puntos ciegos |

Criterio de la propuesta `provides_graphql: false`: proponerla para el servicio de un repo con copias de SDL que además emite señales de consumo (operaciones, wrappers o codegen). Un repo que solo expone un único `schema.graphql` y no tiene señales de consumo (p. ej. `saleor`) no recibe la propuesta.

## 5. Forma exacta de las propuestas

Recordar: `paths` y `env` son campos **por entrada de `services[]`**, no top-level. Los bindings son top-level.

```yaml
# provides_graphql — para el servicio del repo con copias de SDL
services:
  - name: saleor-dashboard
    repo: saleor-dashboard
    provides_graphql: false

# env — variables usadas como URL base y no declaradas
  - name: saleor-storefront
    repo: storefront
    env: [NEXT_PUBLIC_SALEOR_API_URL]

# bindings — SIEMPRE el bindings_entry devuelto por check_binding, verbatim
bindings:
  - consumer:
      repo: storefront
      path: src/graphql/ProductDetails.graphql
      symbol: graphql-call:query:product
      key: "graphql:query:product"
    provider:
      service: saleor-api
      key: "graphql:query:product"
```

Notas de forma:
- `consumer.symbol` es el **nombre del nodo** `graphql-call:{op}:{field}` (no el nombre de la operación — `ProductDetails` no sirve; el campo raíz `product` sí).
- `consumer.path` es la ruta del archivo `.graphql` relativa a la raíz del repo.
- `consumer.repo` es el id del repo en `repos.yaml`, `provider.service` el nombre del servicio.
- Claves en ambos lados: `graphql:{op}:{field}`.

## 6. YAML objetivo del caso Saleor

Estado final esperado de `repos.yaml` tras aplicar las propuestas (el estado inicial está en `test-repos/repos-saleor.yaml`):

```yaml
data_dir: /tmp/lain-verify/data        # o el data_dir del operador

repos:
  - id: saleor
    source: { type: workspace_dir, path: <path>/saleor }
  - id: saleor-dashboard
    source: { type: workspace_dir, path: <path>/saleor-dashboard }
  - id: storefront
    source: { type: workspace_dir, path: <path>/storefront }

services:
  - name: saleor-api
    repo: saleor
    paths: ["saleor"]
    env: [NEXT_PUBLIC_SALEOR_API_URL, SALEOR_API_URL]
  - name: saleor-dashboard
    repo: saleor-dashboard
    provides_graphql: false          # ← propuesta del advisor
  - name: saleor-storefront
    repo: storefront

bindings: []                          # opcional: entradas validadas por
                                      # check_binding si quedan (op, field)
                                      # sin resolver tras el reindex
```

Con este config, el join deja `graphql_no_op` para los consumers del storefront: `(op, field)` tiene exactamente un provider (`saleor-api`).

## 7. Criterios de aceptación (demo)

Con el YAML de §6 aplicado y `lain reindex` corrido:

1. `list_unresolved {"snapshot":"live","repo":"storefront","limit":1000}` (paginado a exhaustión) → **cero** items `graphql_no_op` del storefront.
2. `list_contracts` → el endpoint `graphql:query:product` (y el resto de campos raíz de las operaciones del storefront) con `bound_consumers >= 1`.
3. `graphql:query:shop` y demás campos del schema tienen **exactamente un** provider (`saleor-api`); las copias del dashboard ya no aparecen como providers.
4. Toda propuesta de binding exhibida proviene de un `check_binding` con `valid: true` y usa su `bindings_entry` sin reescritura.
5. Sin aplicar nada (config actual), el advisor explica el gap con evidencia (archivos de las copias de SDL, ops sin resolver) — es el "Parallax aporta algo que LAIN solo no resuelve" del §10.

## 8. Restricciones (del design doc de Parallax, §3/§10)

- No escribir archivos internos de `.lain` ni reconstruir un grafo paralelo — el único producto de Parallax es config (`repos.yaml`) + explicación.
- No descubrir la organización entera: solo cerrar brechas de configuración sobre `repos`, `services`, `paths`, `env`, `http_clients`, `bindings`.
- No auto-aplicar: el advisor propone; el operador aplica (el script de demo puede aplicar mecánicamente, pero eso es demo, no producto).
- En la respuesta al usuario, no presentar como resuelto nada que el motor no confirme (todo binding pasa por `check_binding`; todo "aparece la flecha" se verifica con los criterios de §7).

## 9. Estado actual observado en Parallax (referencia, read-only)

Solo para orientar a quien implemente; no son tareas ni prescripciones:

- Cliente LAIN (MCP JSON-RPC): `packages/api/src/lain-adapter/client.ts` — ya declara `checkBinding` (:296-301) y `listUnresolved` (:289-294); hoy sin uso en rutas de producto.
- Mirror de esquemas: `packages/shared/src/lain/tools.ts` (corrección pendiente §3.4) y `docs/lain-tool-schemas.json`.
- Generación actual de `repos.yaml`: `packages/api/src/routes/workspaces.ts:127-172` (solo `repos` + `services`; string-join, sin librería YAML).
- Fixture de demo (gitignored): `test-repos/` con los tres clones de Saleor + `test-repos/repos-saleor.yaml` (el estado inicial de §6).
- Gates manuales existentes con el patrón a imitar: `packages/api/scripts/gate-g1.ts`.
