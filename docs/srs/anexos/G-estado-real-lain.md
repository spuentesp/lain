# Anexo G — Estado real del sistema LAIN

Este anexo deja constancia de un hecho que enmarca toda la entrega: **LAIN no es un sistema propuesto a futuro, sino un sistema que ya existe y funciona.** Por ello, la presente especificación no describe un producto hipotético, sino que documenta —en el formato del estándar IEEE 830-1998— un sistema real y en uso.

## G.1 LAIN existe y funciona

LAIN es un servidor de inteligencia de código operativo. Construye y mantiene el mapa de conocimiento del código de un proyecto y expone sus capacidades a los asistentes de IA mediante un protocolo estándar, tal como se especifica en el cuerpo de este documento. Las capacidades descritas en la Sección 3 (radio de impacto, cadenas de llamadas, dependencias transitivas, acoplamiento por co-cambio, búsqueda semántica, provisión de contexto al agente, verificación asistida, lenguaje de consulta y visualizaciones) están **implementadas y en funcionamiento** en la versión vigente del sistema.

En consecuencia, el mini prototipo del Anexo E no es una maqueta desechable: sus vistas (consola de consultas y las visualizaciones de radio de impacto, cadena de llamadas y acoplamiento) corresponden a la **interfaz real** del sistema en funcionamiento.

## G.2 Cómo se elaboró esta especificación

Esta entrega es una **especificación en retrospectiva** (*reverse specification*): en lugar de imaginar un sistema y luego construirlo, el equipo partió de un sistema ya construido y lo documentó según el estándar. El procedimiento fue:

1. Se tomó la **plantilla del estándar IEEE 830-1998** (y la estructura exigida por la pauta de la entrega) como esqueleto del documento.
2. Se tomó la **documentación propia de LAIN** (README, documentación técnica, guía del lenguaje de consulta y notas del proyecto).
3. Se **completó cada sección de la especificación** a partir de tres fuentes de material real:

    - el **código fuente** del sistema y sus capacidades efectivas;
    - la **documentación** existente del proyecto;
    - la **experiencia directa de haber construido LAIN**, que aportó el problema de origen, las decisiones de alcance y los criterios de prioridad.

Es decir, cada requerimiento funcional, cada caso de uso y cada proceso del documento tiene como respaldo una capacidad que el sistema **ya ejecuta**, no una intención de diseño. Esto explica la coherencia y trazabilidad del documento: no fue necesario inventar, sino describir.

## G.3 Repositorio y disponibilidad

| Dato | Valor |
|---|---|
| **Repositorio** | https://github.com/spuentesp/lain (público) |
| **Versión vigente** | v0.3.0 (rama `main`) |
| **Instalación** | Instalador de una línea, interactivo o no interactivo (ver README del repositorio). |
| **Integración** | Se conecta a asistentes de IA que soportan el protocolo estándar de comunicación entre asistentes y herramientas. |

El repositorio contiene el código, la documentación técnica y el historial de versiones que evidencian el desarrollo del sistema durante los meses previos a esta entrega.

## G.4 Estado de implementación

La versión vigente implementa la práctica totalidad de los requerimientos funcionales especificados en la Sección 3.1. Un pequeño número de aspectos permanece como parcial o pendiente de medición —principalmente algunos requerimientos no funcionales cuantitativos (rendimiento, escala y consumo de recursos), que están declarados como metas verificables y aún no medidos con instrumentación—, en línea con lo indicado en el Anexo F, Sección F.7. El grupo mantiene, además, un documento interno de comparación requerimiento a requerimiento entre esta especificación y el código, con evidencia por archivo; dicho documento no forma parte de la entrega por nombrar tecnologías concretas.

## G.5 Relación con la restricción de la pauta

La pauta pide no indicar en la especificación las tecnologías concretas de implementación. Este anexo respeta esa restricción: el **cuerpo del documento se mantiene independiente de tecnologías**, y los detalles técnicos del sistema real (lenguaje, bibliotecas, formatos de persistencia y mecanismos internos) **no se enuncian aquí**, sino que quedan disponibles públicamente en el repositorio para quien desee contrastarlos. Este anexo se limita a dejar constancia de que el sistema existe, funciona y fue la fuente real de la que se derivó la especificación.
