# Anexo D — Diseño de interfaces gráficas

Este anexo presenta el diseño preliminar de las principales interfaces del sistema, como wireframes de baja fidelidad. No se exige implementación funcional en esta etapa; los wireframes representan la interacción esperada y su trazabilidad con casos de uso y actores.

## D.1 Resumen de pantallas

| ID | Pantalla | Actor | Caso(s) de uso | RF asociados |
|---|---|---|---|---|
| P-01 | Asistente de configuración inicial | Desarrollador | CU-01 | RF-01, RF-02 |
| P-02 | Consola de consultas y estado | Desarrollador | CU-10, CU-09 (y acceso a CU-04 a CU-08) | RF-25, RF-03 a RF-06, RF-22 |
| P-03 | Visualización de radio de impacto | Desarrollador | CU-02 + CU-11 | RF-07, RF-27 |
| P-04 | Visualización de cadena de llamadas | Desarrollador | CU-03 + CU-11 | RF-08, RF-27 |
| P-05 | Mapa de calor de acoplamiento | Desarrollador | CU-07 + CU-11 | RF-18, RF-27 |

> Nota: el Asistente de IA, segundo actor principal del sistema, no utiliza pantallas: interactúa por el protocolo estándar de comunicación. Las interfaces gráficas están dirigidas al actor humano.

## D.2 P-01 — Asistente de configuración inicial

**Objetivo:** guiar al Desarrollador en la inicialización del espacio de trabajo (CU-01). Se presenta como diálogo paso a paso en la terminal o instalador.

```
┌────────────────────────────────────────────────────────────┐
│  LAIN — Configuración inicial                     [paso 2/5]│
├────────────────────────────────────────────────────────────┤
│                                                            │
│  Carpeta del proyecto:                                     │
│  [ /ruta/al/proyecto___________________________ ] [Examinar]│
│                                                            │
│  Modo de comunicación con el asistente:                    │
│  (•) Local    ( ) Servicio de red local   ( ) Ambos        │
│                                                            │
│  Asistente de IA detectado:  «Asistente X»  [Cambiar ▾]    │
│                                                            │
│  [✓] Habilitar búsqueda por significado (descarga el       │
│      modelo local, ~120 MB)                                │
│                                                            │
│            [ ← Atrás ]              [ Continuar → ]        │
├────────────────────────────────────────────────────────────┤
│  ℹ El código del proyecto nunca abandona este equipo.      │
└────────────────────────────────────────────────────────────┘
```

- **Campos de entrada:** carpeta del proyecto; modo de comunicación; asistente objetivo; habilitación de búsqueda semántica.
- **Acciones:** continuar/atrás; confirmar al final del asistente, lo que dispara la construcción del mapa inicial con barra de progreso.
- **Mensajes y validaciones:** carpeta inexistente o sin permisos → error bloqueante; proyecto sin control de versiones → advertencia no bloqueante («las funciones históricas quedarán deshabilitadas», RS-03); asistente no detectado → instrucciones de integración manual (flujo 6a de CU-01).

## D.3 P-02 — Consola de consultas y estado

**Objetivo:** punto de acceso del Desarrollador al mapa: consultas estructuradas (CU-10), estado y frescura del mapa (CU-09) y accesos a los análisis arquitectónicos (CU-05).

```
┌──────────────────────────────────────────────────────────────────┐
│ LAIN — Consola de consultas                    ● Servicio activo  │
├───────────────────────────────┬──────────────────────────────────┤
│ CONSULTA                      │ ESTADO DEL MAPA                  │
│ ┌───────────────────────────┐ │  Símbolos: 12.480                │
│ │ find Function             │ │  Relaciones: 48.102              │
│ │  | called_by "guardar"    │ │  Última sincronización: hace 12 s│
│ │  | limit 10               │ │  Frescura por módulo   [Ver ▾]   │
│ └───────────────────────────┘ │                                  │
│ [ Ejecutar ]  [ Limpiar ]     │  ACCIONES                        │
│                               │  [Sincronizar ahora]             │
│ RESULTADOS                    │  [Enriquecer mapa]               │
│ ┌───────────────────────────┐ │  [Esquema del mapa]              │
│ │ 1. validar_datos  módulo…│ │                                  │
│ │ 2. registrar_log  módulo…│ │  ANÁLISIS                        │
│ │ …                        │ │  [Puntos de entrada]             │
│ └───────────────────────────┘ │  [Componentes ancla]             │
│                               │  [Código sin uso]                │
├───────────────────────────────┴──────────────────────────────────┤
│ ✔ Consulta ejecutada en 0,4 s — 10 resultados                     │
└──────────────────────────────────────────────────────────────────┘
```

- **Campos de entrada:** editor de consulta estructurada; parámetros de los análisis (profundidad, símbolo).
- **Acciones:** ejecutar consulta; sincronizar; enriquecer; abrir esquema; lanzar análisis arquitectónicos; abrir un resultado en su visualización (P-03/P-04/P-05).
- **Mensajes y estados:** indicador de servicio activo/detenido; consulta mal formada → error con posición y ejemplo válido (CU-10); mapa desactualizado → aviso con acceso a «Sincronizar ahora»; frescura por módulo con marcas de tiempo (RF-06).

## D.4 P-03 — Visualización de radio de impacto

**Objetivo:** presentar gráficamente el resultado de CU-02: el símbolo consultado al centro y los afectados por anillos de profundidad.

```
┌──────────────────────────────────────────────────────────────────┐
│ Radio de impacto: «procesar_pago»          Profundidad: [2 ▾]     │
├──────────────────────────────────────────────────────────────────┤
│                    ┌────────────┐                                 │
│      ╭─────────────│ notificar()│── nivel 2 ──╮                   │
│      │             └────────────┘             │                   │
│  ┌──────────┐   ┌────────────────┐   ┌──────────────┐             │
│  │ cobrar() │───│ procesar_pago()│───│ registrar()  │  nivel 1    │
│  └──────────┘   └───────●────────┘   └──────────────┘             │
│                    símbolo consultado                             │
│                                                                   │
│  Leyenda: ── confianza alta   ┄┄ confianza media                  │
├──────────────────────────────────────────────────────────────────┤
│ Afectados: 14 símbolos (3 directos, 11 transitivos)               │
│ [Exportar]  [Abrir en consola]  [Ver llamadores del nodo]         │
└──────────────────────────────────────────────────────────────────┘
```

- **Campos de entrada:** símbolo consultado; selector de profundidad.
- **Acciones:** navegar/ampliar el grafo; seleccionar un nodo para ver sus llamadores (RF-09) o su contexto; exportar.
- **Mensajes y estados:** símbolo sin afectados → estado vacío explícito; relaciones heurísticas marcadas como confianza media (flujo 3a de CU-02).

## D.5 P-04 — Visualización de cadena de llamadas

**Objetivo:** presentar la ruta de invocaciones entre dos símbolos (CU-03).

```
┌──────────────────────────────────────────────────────────────────┐
│ Cadena de llamadas: «main» → «guardar_registro»                   │
├──────────────────────────────────────────────────────────────────┤
│  [main]──▶[iniciar_servicio]──▶[atender_solicitud]                │
│                                       │                           │
│                                       ▼                           │
│                          [validar]──▶[guardar_registro]           │
│                                                                   │
│  Pasos: 4 · Cada flecha indica archivo y línea de la invocación   │
├──────────────────────────────────────────────────────────────────┤
│ [Invertir dirección]  [Copiar ruta]  [Abrir paso en consola]      │
└──────────────────────────────────────────────────────────────────┘
```

- **Campos de entrada:** símbolos de origen y destino.
- **Acciones:** invertir dirección; abrir un paso en la consola; copiar la ruta.
- **Mensajes y estados:** «no existe ruta entre los símbolos» como estado explícito, distinto de «símbolo inexistente» (flujo 3a de CU-03).

## D.6 P-05 — Mapa de calor de acoplamiento

**Objetivo:** presentar los acoplamientos históricos de un archivo o símbolo (CU-07).

```
┌──────────────────────────────────────────────────────────────────┐
│ Acoplamiento histórico: «modelo_pedidos»        Ventana: [1 año ▾]│
├──────────────────────────────────────────────────────────────────┤
│  Archivo co-cambiante              Intensidad                     │
│  vista_pedidos          ██████████████████░░  0,91                │
│  pruebas_pedidos        ██████████████░░░░░░  0,74                │
│  configuracion_envios   ████████░░░░░░░░░░░░  0,42                │
│  utilidades_fechas      ███░░░░░░░░░░░░░░░░░  0,15                │
├──────────────────────────────────────────────────────────────────┤
│ ⚠ La intensidad refleja co-cambio histórico (correlación),        │
│   no dependencia verificada en el código.                         │
│ [Ver confirmaciones compartidas]  [Abrir en consola]              │
└──────────────────────────────────────────────────────────────────┘
```

- **Campos de entrada:** archivo o símbolo; ventana temporal del historial.
- **Acciones:** ordenar por intensidad; abrir las confirmaciones que evidencian cada par; saltar a la consola.
- **Mensajes y estados:** historial insuficiente → advertencia de evidencia débil (flujo 2a de CU-07); advertencia permanente de correlación vs. causalidad.

## D.7 Coherencia con el resto de la especificación

Cada pantalla materializa uno o más casos de uso del Anexo A y sus RF asociados (tabla D.1); las validaciones y mensajes descritos corresponden a los flujos alternativos del Anexo B; y las pantallas P-03/P-04/P-05 son la manifestación de la actividad «presentar evidencia al Desarrollador» del proceso PN-01 (Anexo C). El mini prototipo del Anexo E implementa de forma exploratoria las pantallas P-02, P-03, P-04 y P-05.
