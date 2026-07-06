# Anexo F — Declaración de uso de Inteligencia Artificial

Conforme a la pauta de la entrega, el grupo declara el uso de herramientas de Inteligencia Artificial durante la elaboración de este documento. La IA fue empleada como **herramienta de apoyo** al proceso de Ingeniería de Requerimientos, no como fuente definitiva de requerimientos ni como reemplazo del análisis del equipo.

## F.1 Herramientas utilizadas

| Herramienta | Uso principal |
|---|---|
| Claude Code (Anthropic) | Estructuración del documento según IEEE 830-1998, redacción asistida de requerimientos y anexos, generación de diagramas en notación textual y del mini prototipo (Anexo E). |

No se utilizaron otras herramientas de Inteligencia Artificial en la elaboración de esta entrega.

## F.2 Propósito de uso

- Organizar la información recopilada en las actividades previas del módulo dentro de la estructura del estándar IEEE 830-1998.
- Apoyar la redacción formal, en tercera persona y verificable, de los requerimientos funcionales y no funcionales.
- Derivar una propuesta inicial de requerimientos a partir del análisis del dominio y de la solución en desarrollo, para su posterior revisión por el grupo.
- Generar las versiones iniciales de los diagramas (contexto, casos de uso, proceso de negocio) en notación textual y de los wireframes.
- Construir el mini prototipo exploratorio descrito en el Anexo E.

## F.3 Prompts o instrucciones generales empleadas

- «Elaborar el documento de especificación de requerimientos del proyecto según la estructura de la pauta, usando la plantilla del estándar IEEE 830-1998, para compararlo con la versión vigente del sistema.»
- «Redactar los requerimientos de forma clara, formal, verificable y coherente con el problema, los stakeholders, el alcance y la solución propuesta, sin definir tecnologías de implementación.»
- «Generar el diagrama de casos de uso con actores principales y secundarios, límite del sistema y relaciones include/extend, coherente con los requerimientos funcionales.»
- «Planificar la creación de una carpeta con la entrega tal como la pide el profesor, para poder comparar la especificación con la última versión del sistema.» (instrucción inicial de la sesión de elaboración).
- «Revisar los pendientes del documento» (sesión de cierre, en la que se resolvieron autoría, control de versiones, decisión de CU-03, capturas del prototipo, stakeholders y datos del curso).
- Respuestas del autor a preguntas de decisión formuladas por la herramienta (ubicación y formato de la entrega, nivel de completitud del contenido, inclusión de la matriz de comparación interna).

## F.4 Información generada, revisada u organizada con IA

- **Generada con IA (y revisada por el grupo):** la redacción base de las secciones del SRS, la formulación individual de los RF/RNF/RS con sus criterios de verificación, los diagramas en notación textual, las plantillas y especificaciones de casos de uso, los wireframes del Anexo D y el mini prototipo.
- **Organizada con IA:** la trazabilidad entre requerimientos, casos de uso, proceso de negocio, pantallas y prototipo (matriz de la Sección 3.4).
- **Revisada con IA:** consistencia de identificadores y referencias cruzadas entre el documento principal y los anexos.

## F.5 Elementos modificados por el grupo

Sobre el material generado por la IA, el autor realizó las siguientes modificaciones y validaciones:

- **Plan de trabajo:** el plan de estructura y contenido propuesto por la herramienta fue revisado y aprobado con ediciones por el autor antes de redactar documento alguno.
- **Portada y control de versiones:** se reemplazó la autoría grupal propuesta por autoría individual y se simplificó la tabla de control de versiones a la versión final, delegando el historial detallado al control de versiones del repositorio.
- **Stakeholders:** la sección de características de usuarios fue corregida con el stakeholder real del proyecto y su necesidad de origen (reducción del consumo de tokens al entregar contexto al asistente), información aportada por el autor y que la IA no podía conocer.
- **Casos de uso:** la observación abierta de CU-03 (ruta única de llamadas) fue resuelta por decisión del autor y retirada de los aspectos pendientes.
- **Prototipo:** se definieron los datos de ejemplo trazables con el Anexo D, la selección de vistas a evidenciar y el encuadre de las capturas (detalle en E.7).

## F.6 Decisiones tomadas por criterio humano

- La selección del problema, el alcance del producto y la lista definitiva de funciones incluidas y excluidas.
- La priorización de los requerimientos (Esencial / Deseable / Opcional) y la aceptación final de cada requerimiento propuesto.
- La elección del proceso de negocio a modelar y del punto de intervención del sistema en dicho proceso.
- La validación con stakeholders de la información educida en actividades previas.
- La decisión de que la cadena de llamadas (CU-03) retorne únicamente la ruta más corta, dejando las rutas alternativas como mejora futura.
- La identificación de la necesidad que origina el proyecto —el costo en tokens de entregar contexto de una base de código de gran tamaño a un asistente de IA—, surgida de la experiencia directa del autor como desarrollador, no de una propuesta de la herramienta.
- La ubicación y el formato de la entrega (carpeta versionada en el repositorio del proyecto, en formato de texto con diagramas en notación textual) y la decisión de mantener un documento interno de comparación entre la especificación y la implementación vigente, separado de la entrega.

## F.7 Aspectos pendientes de validación

- Validación con los stakeholders reales de los umbrales cuantitativos de los RNF (tiempos de respuesta, frescura, escala), que fueron propuestos como metas verificables y no medidos con usuarios.
- Revisión de los falsos positivos aceptables en la detección de código sin uso (RF-15).
- Prueba de usabilidad del proceso de instalación comprometido en RNF-06.
- Validación de las necesidades de la comunidad amplia de desarrolladores (tercer stakeholder de la Sección 2.3 del SRS), recogidas hasta ahora solo a través de la experiencia del stakeholder principal.
