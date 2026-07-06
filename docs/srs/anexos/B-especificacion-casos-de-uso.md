# Anexo B — Especificación de casos de uso

Este anexo especifica textualmente los casos de uso principales del diagrama del Anexo A, utilizando una plantilla única. Los casos CU-09, CU-10 y CU-11, de carácter incluido/extensor o utilitario, se describen en forma abreviada al final.

---

## CU-01 — Inicializar espacio de trabajo

| Campo | Contenido |
|---|---|
| **Identificador** | CU-01 |
| **Nombre** | Inicializar espacio de trabajo |
| **Actor principal** | Desarrollador |
| **Actores secundarios** | Repositorio de versiones, Servicio de análisis de lenguaje |
| **Objetivo** | Dejar el sistema operativo sobre un proyecto: configuración creada, integración con el asistente establecida y mapa de conocimiento inicial construido. |
| **Precondiciones** | El sistema está instalado en la máquina del desarrollador. Existe una carpeta de proyecto con código fuente. |

**Flujo principal:**

1. El Desarrollador solicita la inicialización indicando la carpeta raíz del proyecto.
2. El sistema solicita las opciones de configuración: modo de comunicación con los asistentes, asistente de IA a integrar y habilitación de la búsqueda semántica.
3. El Desarrollador confirma las opciones.
4. El sistema persiste la configuración en el espacio de trabajo.
5. El sistema explora el código fuente y construye el mapa de conocimiento inicial (símbolos y relaciones).
6. El sistema registra la integración en la configuración del asistente de IA detectado.
7. El sistema informa al Desarrollador que el espacio de trabajo quedó operativo, con un resumen del mapa construido.

**Flujos alternativos / excepciones:**

- **3a. El Desarrollador opta por la configuración no interactiva:** el sistema aplica los valores indicados por parámetros u omisión y continúa en el paso 4.
- **5a. El proyecto no está bajo control de versiones:** el sistema construye el mapa sin información histórica, advierte la limitación (RS-03) y continúa.
- **5b. No hay servicio de análisis de lenguaje disponible para el lenguaje del proyecto:** el sistema construye el mapa con sus mecanismos propios de menor confianza y lo advierte (RNF-07).
- **6a. No se detecta ningún asistente compatible:** el sistema completa la inicialización y entrega instrucciones de integración manual.

| Campo | Contenido |
|---|---|
| **Postcondiciones** | Existe configuración persistida y un mapa de conocimiento consultable para el proyecto. La integración con el asistente queda registrada cuando fue posible. |
| **RF asociados** | RF-01, RF-02 |
| **Observaciones / supuestos** | Se asume que el desarrollador tiene permisos de escritura sobre la carpeta del proyecto (para los datos internos del sistema) y sobre la configuración de su asistente. |

---

## CU-02 — Evaluar impacto de un cambio

| Campo | Contenido |
|---|---|
| **Identificador** | CU-02 |
| **Nombre** | Evaluar impacto de un cambio |
| **Actor principal** | Asistente de IA (también iniciable por el Desarrollador) |
| **Actores secundarios** | Servicio de análisis de lenguaje |
| **Objetivo** | Conocer, antes de modificar un símbolo, el conjunto de símbolos que se verían afectados directa o transitivamente, para decidir cómo proceder. |
| **Precondiciones** | El espacio de trabajo está inicializado (CU-01) y el símbolo consultado existe en el mapa. |

**Flujo principal:**

1. El actor solicita el radio de impacto de un símbolo, indicando opcionalmente la profundidad máxima.
2. El sistema verifica la frescura del mapa y lo actualiza si corresponde (include CU-09).
3. El sistema resuelve las referencias reales del símbolo con el servicio de análisis de lenguaje, para asegurar relaciones de alta confianza.
4. El sistema calcula el cierre transitivo de los afectados y lo organiza por profundidad y grado de confianza.
5. El sistema retorna la lista jerarquizada de símbolos afectados, con sus ubicaciones.
6. El actor utiliza el resultado para decidir si procede con el cambio, ajusta su plan o consulta análisis complementarios.

**Flujos alternativos / excepciones:**

- **1a. El símbolo indicado no existe o es ambiguo:** el sistema informa las coincidencias aproximadas y solicita precisar la consulta.
- **3a. El servicio de análisis de lenguaje no responde:** el sistema calcula el radio con las relaciones disponibles en el mapa, marcándolas con confianza media (RNF-07).
- **5a. El Desarrollador solicita visualización:** el sistema genera la visualización interactiva del radio de impacto (extend CU-11, pantalla P-03).

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El actor dispone del conjunto de afectados con su profundidad y confianza. El mapa queda actualizado como efecto del paso 2. |
| **RF asociados** | RF-07, RF-09, RF-10, RF-11, RF-27 |
| **Observaciones / supuestos** | Este caso de uso materializa el proceso de negocio PN-01 (Anexo C). El grado de confianza distingue relaciones verificadas por el servicio de análisis de aquellas inferidas heurísticamente. |

---

## CU-03 — Trazar cadena de llamadas

| Campo | Contenido |
|---|---|
| **Identificador** | CU-03 |
| **Nombre** | Trazar cadena de llamadas |
| **Actor principal** | Asistente de IA |
| **Actores secundarios** | Servicio de análisis de lenguaje |
| **Objetivo** | Conocer la ruta exacta de invocaciones que conecta un símbolo de origen con uno de destino, para comprender cómo fluye la ejecución. |
| **Precondiciones** | Espacio de trabajo inicializado; ambos símbolos existen en el mapa. |

**Flujo principal:**

1. El actor indica el símbolo de origen y el símbolo de destino.
2. El sistema verifica la frescura del mapa (include CU-09).
3. El sistema busca la ruta de invocaciones entre origen y destino.
4. El sistema retorna la cadena encontrada, paso a paso, con la ubicación de cada invocación.

**Flujos alternativos / excepciones:**

- **3a. No existe ruta entre los símbolos:** el sistema lo informa explícitamente, distinguiendo «no hay ruta» de «símbolo inexistente».
- **4a. El Desarrollador solicita visualización:** el sistema genera la vista interactiva de la cadena (extend CU-11, pantalla P-04).

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El actor conoce la cadena de llamadas o la certeza de su inexistencia. |
| **RF asociados** | RF-08, RF-09, RF-27 |
| **Observaciones / supuestos** | Cuando existen múltiples rutas, se retorna la más corta; la entrega de rutas alternativas queda como mejora futura. Decisión validada por el equipo (ver Anexo F, Sección F.6). |

---

## CU-04 — Buscar código por significado

| Campo | Contenido |
|---|---|
| **Identificador** | CU-04 |
| **Nombre** | Buscar código por significado |
| **Actor principal** | Asistente de IA o Desarrollador |
| **Actores secundarios** | — |
| **Objetivo** | Localizar los símbolos del proyecto relacionados con una intención expresada en lenguaje natural, sin conocer sus nombres. |
| **Precondiciones** | Espacio de trabajo inicializado con la búsqueda semántica habilitada y las representaciones semánticas calculadas. |

**Flujo principal:**

1. El actor formula la consulta en lenguaje natural (por ejemplo, «¿dónde se maneja la autenticación?»).
2. El sistema transforma la consulta en una representación semántica local.
3. El sistema compara la representación contra las de los símbolos del mapa.
4. El sistema retorna los símbolos más afines, ordenados por grado de afinidad, con su ubicación.

**Flujos alternativos / excepciones:**

- **2a. La búsqueda semántica no está habilitada:** el sistema informa la limitación y sugiere la consulta estructurada por nombre (CU-10).
- **4a. Ningún resultado supera el umbral de afinidad:** el sistema lo informa y sugiere reformular la consulta.

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El actor dispone de una lista ordenada de símbolos pertinentes a su intención. |
| **RF asociados** | RF-20 |
| **Observaciones / supuestos** | Todo el procesamiento semántico ocurre localmente (RNF-04). El umbral de afinidad es un parámetro de configuración. |

---

## CU-05 — Consultar arquitectura del sistema

| Campo | Contenido |
|---|---|
| **Identificador** | CU-05 |
| **Nombre** | Consultar arquitectura del sistema |
| **Actor principal** | Asistente de IA |
| **Actores secundarios** | — |
| **Objetivo** | Obtener una comprensión estructural del proyecto: su organización por niveles, sus puntos de entrada, sus componentes fundamentales, su código sin uso y sus candidatos a refactorización. |
| **Precondiciones** | Espacio de trabajo inicializado. |

**Flujo principal:**

1. El actor solicita una vista arquitectónica, indicando el tipo de análisis (estructura por niveles, puntos de entrada, componentes ancla, código sin uso, observaciones arquitectónicas o explicación de un símbolo) y sus parámetros.
2. El sistema verifica la frescura del mapa (include CU-09).
3. El sistema ejecuta el análisis solicitado sobre el mapa.
4. El sistema retorna el resultado en forma estructurada y comprensible.

**Flujos alternativos / excepciones:**

- **3a. El análisis requiere métricas aún no calculadas:** el sistema dispara el enriquecimiento correspondiente (RF-05) e informa que el resultado puede tardar o entregarse parcial.
- **3b. Reporte de código sin uso:** el sistema adjunta la advertencia de posibles falsos positivos (puntos de entrada, usos externos al proyecto).

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El actor dispone de la vista arquitectónica solicitada, apta para fundamentar decisiones o respuestas. |
| **RF asociados** | RF-12, RF-13, RF-14, RF-15, RF-16, RF-17 |
| **Observaciones / supuestos** | Las métricas de estabilidad son estimaciones estructurales; su interpretación final corresponde al Desarrollador o al líder técnico. |

---

## CU-06 — Verificar cambios del proyecto

| Campo | Contenido |
|---|---|
| **Identificador** | CU-06 |
| **Nombre** | Verificar cambios del proyecto |
| **Actor principal** | Asistente de IA |
| **Actores secundarios** | — |
| **Objetivo** | Ejecutar la compilación, las pruebas o el análisis estático del proyecto y, ante fallas, obtenerlas enriquecidas con contexto arquitectónico para diagnosticarlas con rapidez. |
| **Precondiciones** | Espacio de trabajo inicializado; el proyecto dispone de mecanismos de compilación/pruebas ejecutables localmente. |

**Flujo principal:**

1. El actor solicita una verificación, indicando su tipo (compilación, pruebas con filtro opcional, análisis estático).
2. El sistema ejecuta la verificación sobre el proyecto.
3. El sistema informa el resultado exitoso con su resumen.

**Flujos alternativos / excepciones:**

- **3a. La verificación falla:** el sistema identifica los símbolos involucrados en la falla, adjunta su contexto arquitectónico —llamadores y relaciones (extend CU-08)— y retorna la falla decorada.
- **2a. El proyecto no dispone del mecanismo solicitado:** el sistema informa la imposibilidad sin alterar el estado del proyecto.

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El actor conoce el resultado de la verificación; ante falla, dispone del contexto de los símbolos afectados. El proyecto no sufre modificaciones. |
| **RF asociados** | RF-23, RF-24, RF-21 |
| **Observaciones / supuestos** | La decoración con contexto es lo que distingue este caso de una ejecución directa de las herramientas del proyecto: el asistente puede razonar sobre los llamadores y no solo sobre la línea que falla. |

---

## CU-07 — Analizar acoplamiento histórico

| Campo | Contenido |
|---|---|
| **Identificador** | CU-07 |
| **Nombre** | Analizar acoplamiento histórico |
| **Actor principal** | Desarrollador |
| **Actores secundarios** | Repositorio de versiones |
| **Objetivo** | Descubrir archivos que tienden a cambiar en conjunto con un archivo o símbolo dado, revelando acoplamientos que el código no muestra explícitamente. |
| **Precondiciones** | Espacio de trabajo inicializado; el proyecto está bajo control de versiones con historial suficiente. |

**Flujo principal:**

1. El Desarrollador indica el archivo o símbolo de interés.
2. El sistema consulta la matriz de co-cambio construida a partir del historial del repositorio.
3. El sistema calcula el grado de acoplamiento histórico del elemento con el resto de los archivos.
4. El sistema retorna los pares acoplados relevantes, ordenados por intensidad, con la evidencia (frecuencia de co-cambio).

**Flujos alternativos / excepciones:**

- **2a. Historial insuficiente:** el sistema informa que la evidencia es débil y entrega el resultado con esa advertencia.
- **4a. El Desarrollador solicita visualización:** el sistema genera el mapa de calor de acoplamiento (extend CU-11, pantalla P-05).

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El Desarrollador conoce los acoplamientos ocultos del elemento consultado y su evidencia histórica. |
| **RF asociados** | RF-18, RF-19, RF-27 |
| **Observaciones / supuestos** | El co-cambio es correlación, no causalidad; el anexo lo advierte para evitar decisiones automáticas basadas solo en esta señal. |

---

## CU-08 — Obtener contexto de un símbolo

| Campo | Contenido |
|---|---|
| **Identificador** | CU-08 |
| **Nombre** | Obtener contexto de un símbolo |
| **Actor principal** | Asistente de IA |
| **Actores secundarios** | — |
| **Objetivo** | Recibir un paquete de contexto curado sobre un símbolo (firma, documentación, relaciones, fragmento de código) para fundamentar una respuesta o propuesta de cambio. |
| **Precondiciones** | Espacio de trabajo inicializado; el símbolo existe en el mapa. |

**Flujo principal:**

1. El Asistente de IA solicita el contexto de un símbolo. Al inicio de su sesión, puede además solicitar el esquema del mapa para conocer las capacidades disponibles.
2. El sistema reúne la firma del símbolo, su documentación y sus relaciones relevantes (llamadores, dependencias, módulo contenedor).
3. El sistema adjunta, si se solicita, el fragmento de código con las líneas circundantes.
4. El sistema retorna el paquete en un formato apto para ser incorporado al razonamiento del asistente.

**Flujos alternativos / excepciones:**

- **2a. El símbolo carece de documentación:** el paquete se entrega sin ese elemento, indicándolo.
- **1a. Solicitud de esquema:** el sistema retorna los tipos de nodos, tipos de relaciones y ejemplos de consulta (RF-22).

| Campo | Contenido |
|---|---|
| **Postcondiciones** | El asistente dispone de contexto estructural real del símbolo, en lugar de inferirlo del archivo abierto. |
| **RF asociados** | RF-21, RF-22, RF-17 |
| **Observaciones / supuestos** | El tamaño del paquete debe ser acotado para respetar los límites de contexto de los asistentes; el criterio de curación es parte del diseño posterior. |

---

## Casos de uso abreviados

### CU-09 — Mantener el mapa actualizado (incluido)

Caso incluido por los casos de consulta y disparado también de forma autónoma. Ante cambios en archivos del espacio de trabajo, el sistema los refleja en una capa volátil del mapa (RF-03); periódicamente o bajo demanda consolida dicha capa y se sincroniza con el estado del repositorio (RF-04), ejecuta pasadas de enriquecimiento (RF-05) e informa la frescura por módulo (RF-06). **Actores secundarios:** Repositorio de versiones, Servicio de análisis de lenguaje. **Postcondición:** el mapa refleja el estado reciente del código dentro del plazo comprometido en RNF-02.

### CU-10 — Consultar el mapa con lenguaje estructurado

El Desarrollador o el Asistente formula una consulta en el lenguaje estructurado del sistema (localizar nodos por tipo/nombre/etiqueta/ruta, recorrer relaciones, filtrar, ordenar, limitar) y recibe los resultados en forma tabular o estructurada (RF-25). El Desarrollador puede, alternativamente, formular la pregunta en lenguaje natural y recibir una respuesta apoyada en el mapa (RF-26). **Precondición:** espacio de trabajo inicializado. **Excepción:** consulta mal formada → el sistema retorna el error indicando la posición y un ejemplo válido.

### CU-11 — Visualizar análisis en forma gráfica (extensor)

Extiende CU-02, CU-03 y CU-07 cuando el Desarrollador solicita la presentación gráfica: el sistema genera una visualización interactiva del radio de impacto, la cadena de llamadas o el mapa de calor de acoplamiento (RF-27), navegable en el navegador del usuario (pantallas P-03, P-04, P-05 del Anexo D). **Precondición:** existe un resultado de análisis que visualizar.
