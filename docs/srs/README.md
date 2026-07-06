# Entrega Final — Ingeniería de Requerimientos: SRS del sistema LAIN

Esta carpeta contiene la **Entrega Final del Proyecto de Ingeniería de Requerimientos**: la especificación de requerimientos de software (SRS) del sistema **LAIN**, elaborada según el estándar **IEEE 830-1998**, junto con los anexos exigidos por la pauta.

## Contenido de la entrega

| Documento | Contenido |
|---|---|
| [`SRS.md`](SRS.md) | Documento principal: Introducción; Descripción general del producto; Requisitos específicos (27 RF, 10 RNF, 5 restricciones) y matriz de trazabilidad. |
| [`anexos/A-diagrama-casos-de-uso.md`](anexos/A-diagrama-casos-de-uso.md) | Anexo A: diagrama de casos de uso (actores, límite del sistema, relaciones include/extend). |
| [`anexos/B-especificacion-casos-de-uso.md`](anexos/B-especificacion-casos-de-uso.md) | Anexo B: especificación textual de los 8 casos de uso principales y 3 abreviados. |
| [`anexos/C-proceso-de-negocio.md`](anexos/C-proceso-de-negocio.md) | Anexo C: proceso de negocio PN-01 «Evaluación de impacto antes de modificar código» (situación actual y propuesta). |
| [`anexos/D-interfaces.md`](anexos/D-interfaces.md) | Anexo D: diseño preliminar de 5 pantallas (wireframes) con su trazabilidad. |
| [`anexos/E-prototipo-ia.md`](anexos/E-prototipo-ia.md) | Anexo E: mini prototipo exploratorio apoyado con IA. |
| [`anexos/F-declaracion-uso-ia.md`](anexos/F-declaracion-uso-ia.md) | Anexo F: declaración de uso de Inteligencia Artificial. |

**Documento interno (no forma parte de la entrega al profesor):**

- [`comparacion-implementacion.md`](comparacion-implementacion.md) — matriz de comparación entre esta especificación y la versión vigente del producto, para uso del grupo.

## Cómo leer la entrega

1. Comenzar por `SRS.md` (secciones 1 y 2 dan el contexto; la sección 3 contiene los requerimientos).
2. Los anexos se leen en orden A → F; cada uno referencia los RF y CU con los que se traza.
3. Los diagramas están en notación Mermaid y se visualizan directamente en GitHub; los wireframes son bloques de texto de baja fidelidad.

## Trabajo pendiente del grupo antes de la entrega definitiva

Los puntos que requieren información que solo el grupo posee están marcados en los documentos con `⟨PENDIENTE: …⟩`:

- **SRS.md:** nombres de integrantes y curso; fechas y autores del control de versiones; stakeholders reales (Sección 2.3); referencias a los avances anteriores (Sección 1.4).
- **Anexo B:** validación con stakeholders de la decisión de ruta única en CU-03.
- **Anexo E:** capturas de pantalla del prototipo; otras herramientas de IA utilizadas; ajustes reales del grupo (E.7).
- **Anexo F:** prompts adicionales, elementos modificados por el grupo (F.5) y otras decisiones humanas (F.6).

## Control de versiones

El historial de elaboración de esta entrega queda registrado en el control de versiones del repositorio (`git log -- docs/srs/`), lo que da cuenta del trabajo realizado conforme lo solicita la pauta. La tabla de control de versiones del documento se encuentra al inicio de `SRS.md`.
