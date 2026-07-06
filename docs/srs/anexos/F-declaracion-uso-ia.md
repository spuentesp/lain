# Anexo F — Declaración de uso de Inteligencia Artificial

Conforme a la pauta de la entrega, el grupo declara el uso de herramientas de Inteligencia Artificial durante la elaboración de este documento. La IA fue empleada como **herramienta de apoyo** al proceso de Ingeniería de Requerimientos, no como fuente definitiva de requerimientos ni como reemplazo del análisis del equipo.

## F.1 Herramientas utilizadas

| Herramienta | Uso principal |
|---|---|
| Claude Code (Anthropic) | Estructuración del documento según IEEE 830-1998, redacción asistida de requerimientos y anexos, generación de diagramas en notación textual y del mini prototipo (Anexo E). |
| ⟨PENDIENTE: otras herramientas usadas por el grupo (p. ej., para transcripción de entrevistas, mockups, revisión ortográfica), con versión y propósito.⟩ | |

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
- ⟨PENDIENTE: agregar los prompts adicionales empleados por el grupo en actividades previas (educción, análisis, prototipo).⟩

## F.4 Información generada, revisada u organizada con IA

- **Generada con IA (y revisada por el grupo):** la redacción base de las secciones del SRS, la formulación individual de los RF/RNF/RS con sus criterios de verificación, los diagramas en notación textual, las plantillas y especificaciones de casos de uso, los wireframes del Anexo D y el mini prototipo.
- **Organizada con IA:** la trazabilidad entre requerimientos, casos de uso, proceso de negocio, pantallas y prototipo (matriz de la Sección 3.4).
- **Revisada con IA:** consistencia de identificadores y referencias cruzadas entre el documento principal y los anexos.

## F.5 Elementos modificados por el grupo

⟨PENDIENTE: completar con las modificaciones reales del grupo. Registrar al menos: requerimientos reformulados o eliminados respecto de la propuesta de la IA, prioridades ajustadas, casos de uso agregados o simplificados, cambios de terminología del dominio y correcciones a los diagramas.⟩

## F.6 Decisiones tomadas por criterio humano

- La selección del problema, el alcance del producto y la lista definitiva de funciones incluidas y excluidas.
- La priorización de los requerimientos (Esencial / Deseable / Opcional) y la aceptación final de cada requerimiento propuesto.
- La elección del proceso de negocio a modelar y del punto de intervención del sistema en dicho proceso.
- La validación con stakeholders de la información educida en actividades previas.
- ⟨PENDIENTE: registrar otras decisiones humanas relevantes tomadas durante el módulo.⟩

## F.7 Aspectos pendientes de validación

- Validación con los stakeholders reales de los umbrales cuantitativos de los RNF (tiempos de respuesta, frescura, escala), que fueron propuestos como metas verificables y no medidos con usuarios.
- Validación de la decisión de retornar solo la ruta más corta en la cadena de llamadas (observación de CU-03).
- Revisión de los falsos positivos aceptables en la detección de código sin uso (RF-15).
- Prueba de usabilidad del proceso de instalación comprometido en RNF-06.
- Los marcadores ⟨PENDIENTE⟩ distribuidos en el documento, que señalan información que debe ser completada o confirmada por el grupo antes de la entrega definitiva.
