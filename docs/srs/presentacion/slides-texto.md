# LAIN — Texto editable de las diapositivas

> **Fuente editable.** Modifica el contenido de cada slide en este archivo y yo
> actualizo el PDF (`presentacion.pdf`) regenerando `presentacion.html`.
>
> **Convención:**
> - Cada `## Slide N` corresponde a una diapositiva.
> - Los campos `**Título**`, `**Kicker**`, `**Subtítulo**` y `**Autor**` son
>   líneas cortas (1–2 renglones).
> - El `**Contenido**` admite texto, tablas Markdown y viñetas.
> - Las `**Notas del orador**` NO aparecen en la diapositiva; son apoyos para
>   quien expone (puedes escribir lo que quieras).
> - Para agregar una imagen, usa `![texto alternativo](ruta/a/imagen.png)` y
>   ajustamos el tamaño al regenerar.

---

## Slide 1 — Portada

**Kicker:** EMI307-1 · Especificación de Requerimientos · Módulo 03

**Título:** LAIN — Anexos de la Especificación de Requerimientos

**Subtítulo:** Resumen académico de los Anexos A a F del SRS del sistema LAIN,
elaborado conforme al estándar IEEE 830-1998.

**Autor:**
Magíster en Ingeniería Informática · UFRO
EMI307-1 · Especificación de Requerimientos · 2026

**Notas del orador:**
[Aquí puedes escribir lo que dirás al abrir la presentación: por qué es
importante hablar de los anexos, a quién va dirigida, cuánto durará la
exposición, etc.]

---

## Slide 2 — Contexto

**Kicker:** 01 · Contexto

**Título:** ¿Qué es el SRS y qué son sus anexos?

**Contenido (dos columnas):**

Columna izquierda:
- **LAIN** es un sistema de inteligencia de código para asistentes de
  programación. Construye un mapa de conocimiento del código fuente de un
  proyecto y lo pone a disposición del Desarrollador y del Asistente de IA.
- **SRS** (*Software Requirements Specification*) es el documento que
  describe, de manera verificable, qué debe hacer el sistema.
- Se elaboró conforme al estándar **IEEE 830-1998**, que define la
  estructura formal de un SRS. El documento se organiza en tres bloques:
  - **Introducción** — propósito, alcance, definiciones, referencias.
  - **Descripción general** — perspectiva, funciones, usuarios, restricciones y supuestos.
  - **Requisitos específicos** — veintisiete **RF**, diez **RNF** y cinco **restricciones**, con su matriz de trazabilidad.
- Esta especificación integra el trabajo de **actividades previas del
  módulo** —inscripción del proyecto, identificación de stakeholders,
  educción de requerimientos, análisis, modelado de casos de uso, modelado
  del proceso de negocio y diseño preliminar de interfaces—. Cada
  actividad dejó un antecedente que el SRS recoge y referencia desde su
  sección correspondiente.
- El estándar distingue tres categorías de requisito: los **RF**
  definen el alcance funcional, los **RNF** fijan la calidad esperada y las
  **restricciones** son condiciones no negociables. En este SRS hay
  veintisiete RF, diez RNF y cinco restricciones.

Columna derecha (tarjeta):
> **Los seis anexos**
>
> - **A — Diagrama de casos de uso.** Visión gráfica de los CU y sus actores.
> - **B — Especificación de CU.** Descripción textual detallada.
> - **C — Proceso de negocio.** PN-01, situación actual y propuesta.
> - **D — Interfaces.** *Wireframes* de las pantallas principales.
> - **E — Prototipo con IA.** Exploración visual de cuatro vistas.
> - **F — Declaración de uso de IA.** Cómo se usó la IA en la entrega.

**Notas del orador:**
[Puedes recordar brevemente qué es LAIN, a qué problema responde, y por qué
se eligió IEEE 830-1998 como estándar para esta entrega. Mencionar la
tríada RF/RNF/Restricciones y que esta entrega integra el trabajo de las
actividades previas del módulo. Los stakeholders se desarrollan en la
siguiente slide.]

---

## Slide 3 — Stakeholders y problema

**Kicker:** 02 · Stakeholders y problema

**Título:** De la fricción operativa a los RF

**Contenido:**

Párrafo introductorio:
> El SRS abre con tres elementos: un **problema** observado, unos
> **stakeholders** identificados (educidos y validados en actividades
> previas del módulo) y una **necesidad de origen** trazable hasta los
> requerimientos. Esta slide condensa la Sección 2.3 del documento.

Tarjeta 1 — *El problema (SRS §1.2)*:
> - Cuando un asistente razona sobre una base de código grande, solo ve el
>   archivo abierto o resultados de búsqueda textual.
> - Sin esa visibilidad, sus propuestas rompen código en partes no
>   consideradas, duplican funcionalidad que ya existía y dejan fuera
>   acoplamientos que solo se ven en el historial.
> - **Costo en tokens** y **baja precisión** en las respuestas. El
>   Desarrollador, por su parte, carecía de una herramienta que
>   respondiera con evidencia a preguntas como «si modifico esta
>   función, ¿qué más se ve afectado?».

Tarjeta 2 — *Stakeholders (SRS §2.3)*:
> - **Principal — Sebastián Puentes.** Desarrollador y propietario del
>   producto (también cumple el rol de usuario «Desarrollador de software»).
>   Influye en alcance y prioridades.
> - **Secundario — Docente EMI307-1.** Evalúa que la especificación sea
>   coherente y trazable. Define los criterios de aceptación del documento.
> - **Terciario — Comunidad.** Desarrolladores usuarios de asistentes de
>   IA, cuyas necesidades se recogen indirectamente a través del
>   stakeholder principal.

Tarjeta 3 — *Necesidad de origen → RF (SRS §2.3)*:
> La necesidad de origen — **reducir el consumo de tokens al entregar
> contexto al asistente** — aterriza en los **RF-21 y RF-22** (provisión
> de contexto curado y acotado) y en el **RF-25** (consulta selectiva del
> mapa). El **CU-08** registra como observación que el paquete de
> contexto debe ser acotado, y la **RG-1** exige ejecución local para que
> el código no salga del equipo del stakeholder.

Cita:
> **Por qué importa en el SRS.** La Sección 2.3 ata la *necesidad de
> origen* de cada stakeholder con los *requerimientos que la resuelven*.
> Esa cadena — problema → stakeholder → RF — es el corazón de la
> trazabilidad.

**Notas del orador:**
[Esta slide es clave porque justifica que la especificación no es un
ejercicio teórico: arranca de una fricción real con los asistentes. Citar
de memoria los tres stakeholders con su nivel de influencia y mencionar
que la trazabilidad llega explícitamente a RF-21, RF-22, RF-25.]

---

## Slide 4 — Demo: LAIN en acción

**Kicker:** 04 · Demo

**Título:** LAIN en acción: una sesión real

**Contenido:**

Párrafo introductorio:
> El sistema ya existe y se ejecuta. Esta es una **sesión típica**:
> inicialización del espacio de trabajo, consulta en lenguaje natural y
> análisis estructural. La captura muestra los comandos y la salida real
> del CLI.

Imagen (mockup de terminal con tema oscuro):
`![Sesión real del CLI de LAIN](img/lain-terminal-crop.png)`

Pie:
> Cada comando materializa un caso de uso del Anexo A: `lain init`
> corresponde a **CU-01**, `lain ask` a **CU-04** (búsqueda semántica),
> `lain blast-radius` a **CU-02** (radio de impacto) y `lain call-chain`
> a **CU-03** (cadena de llamadas).

**Notas del orador:**
[Señalar que no es un mockup genérico: los nombres de comandos, flags y
símbolos (`blast-radius`, `call-chain`, `auth/jwt.rs::validate_token`)
provienen del CLI real. Mencionar la integración con Claude Code como
ejemplo del actor Asistente de IA del Anexo A.]

---

## Slide 5 — Arquitectura de LAIN

**Kicker:** 05 · Arquitectura

**Título:** Cómo está construido LAIN

**Contenido:**

Párrafo introductorio:
> El sistema se ejecuta como un **servidor MCP local** en la máquina del
> Desarrollador. El grafo de conocimiento se construye a partir del código
> fuente mediante tres extractores paralelos, y se expone al Asistente de
> IA por el protocolo MCP estándar.

Imagen (diagrama Mermaid renderizado):
`![Arquitectura del sistema LAIN](img/diag-arch-crop.png)`

Pie:
> **Trazabilidad.** Los extractores (**LSP**, **Tree-sitter**, **Git**)
> alimentan el **grafo de conocimiento** (RF-02); el **modelo ONNX** local
> sostiene la búsqueda semántica (RF-20); el **servidor MCP** estandariza
> la comunicación con los asistentes (RG-2, RG-3).

**Notas del orador:**
[Señalar que el sistema corre local, no como servicio remoto. El grafo
se construye sin tocar el código del proyecto (RG-4). El servidor MCP es
el mismo protocolo estándar que usan Claude Code, Cursor, Windsurf y
Cline, así que la integración es por convención, no a medida.]

---

## Slide 6 — Anexo A — Casos de uso

**Kicker:** 06 · Anexo A

**Título:** Casos de uso: actores, límite y alcance

**Contenido:**

Párrafo introductorio:
> El sistema LAIN se describe mediante **once casos de uso** y
> **cuatro actores**. El **límite del sistema** está representado por el
> recuadro «Sistema LAIN»; los actores principales lo cruzan para iniciar
> CU, y los secundarios son consultados por el sistema desde dentro.

Imagen (diagrama Mermaid renderizado):
`![Diagrama de casos de uso](img/diag-a-crop.png)`

Pie:
> **Relaciones include/extend.** **CU-09 «Mantener el mapa»** es incluido
> por los casos de consulta (asegura frescura antes de responder).
> **CU-11 «Visualizar»** extiende CU-02, CU-03 y CU-07 cuando el
> Desarrollador solicita una vista gráfica. Coherencia con los
> **veintisiete RF** vía matriz de trazabilidad (SRS §3.4).

**Notas del orador:**
[Explica por qué hay dos actores principales (humano y sistema), qué
aportan los secundarios, y por qué CU-09 aparece como include en los
demás. Resaltar que el límite está claramente acotado: la IA no edita
código ni gestiona el repositorio.]

---

## Slide 7 — Anexo B — Especificación de casos de uso

**Kicker:** 07 · Anexo B

**Título:** Especificación de los casos de uso

**Contenido:**

Párrafo introductorio:
> Los ocho casos principales siguen una **plantilla única**; los tres
> utilitarios (CU-09, CU-10 y CU-11) se documentan en forma abreviada
> por su dependencia de los anteriores.

Tarjeta izquierda — *Plantilla común*:
> - Identificador y nombre.
> - Actor principal y actores secundarios.
> - Objetivo del caso de uso.
> - Precondiciones.
> - Flujo principal (pasos numerados).
> - Flujos alternativos o excepciones.
> - Postcondiciones.
> - RF asociados.
> - Observaciones o supuestos pendientes.

Tarjeta derecha — *Ejemplo: CU-02 «Evaluar impacto de un cambio»*:
> - **Objetivo:** conocer, antes de modificar un símbolo, el conjunto de
>   símbolos afectados directa o transitivamente.
> - **Precondición:** espacio de trabajo inicializado y símbolo existente en
>   el mapa.
> - **Flujo principal:** (1) solicitud → (2) actualización del mapa → (3)
>   resolución de referencias → (4) cierre transitivo → (5) retorno
>   jerarquizado.
> - **RF asociados:** RF-07, RF-09, RF-10, RF-11, RF-27.

Pie:
> Detalle en `anexos/B-especificacion-casos-de-uso.md`.

**Notas del orador:**
[Insistir en que la misma plantilla se aplica a los 8 CU principales, lo que
facilita la lectura y la trazabilidad. Mencionar que CU-02 es el CU central
del proceso PN-01.]

---

## Slide 8 — Anexo C — Proceso de negocio PN-01

**Kicker:** 08 · Anexo C

**Título:** Proceso de negocio PN-01

**Contenido:**

Párrafo introductorio:
> PN-01 «Evaluación de impacto antes de modificar código» describe cómo
> decide y ejecuta un equipo de desarrollo una modificación sobre código
> existente. Es el proceso donde el sistema LAIN aporta su mayor valor.

Tabla resumen del proceso:

| Elemento | Descripción |
|---|---|
| Evento de inicio | Surgir la necesidad de modificar código existente (funcionalidad, corrección o refactorización). |
| Evento de término | Cambio integrado con su impacto conocido y verificado. |
| Participantes | Desarrollador, Asistente de IA y Sistema LAIN (este último solo en la variante propuesta). |
| Decisiones clave | «¿Impacto alto o inesperado?» (paso 6) y «¿Aprueba el plan?» (paso 8). |

Imagen (diagrama Mermaid renderizado, situación propuesta):
`![Diagrama del proceso de negocio PN-01](img/diag-c-crop.png)`

Tarjeta izquierda — *Situación actual (sin el sistema)*:
> - El asistente de IA revisa solo el archivo abierto y propone el cambio.
> - Los efectos colaterales aparecen tarde: al ejecutar las pruebas o, peor,
>   en operación. Se generan ciclos de **retrabajo**.
> - **Causa raíz:** la decisión se toma **sin conocer el impacto**.

Tarjeta derecha — *Situación propuesta (con LAIN)*:
> - Antes de modificar, el asistente consulta al sistema: actualiza el mapa y
>   obtiene radio de impacto, llamadores y acoplamiento histórico.
> - Con esa evidencia, el plan se elabora o se replantea; el Desarrollador
>   aprueba el plan y luego el sistema decora la verificación con contexto
>   arquitectónico.
> - **El sistema no ejecuta el cambio; informa la decisión** al equipo.

Pie:
> Detalle en `anexos/C-proceso-de-negocio.md` · Tabla C.6 vincula cada
> actividad del proceso con los CU y RF correspondientes.

**Notas del orador:**
[Resaltar el contraste «antes/después» y el principio clave: el sistema
informa la decisión humana, no la reemplaza.]

---

## Slide 9 — Anexo D — Interfaces

**Kicker:** 09 · Anexo D

**Título:** Diseño preliminar de interfaces

**Contenido:**

Párrafo introductorio:
> Se diseñan **cinco *wireframes* de baja fidelidad** que materializan los
> casos de uso principales. Las pantallas están dirigidas al Desarrollador;
> el Asistente de IA interactúa por un protocolo estándar y no requiere GUI.

Tabla:

| Pantalla | Nombre | Actor | CU | RF |
|---|---|---|---|---|
| P-01 | Asistente de configuración inicial | Desarrollador | CU-01 | RF-01, RF-02 |
| P-02 | Consola de consultas y estado | Desarrollador | CU-10, CU-09 | RF-25, RF-03–06, RF-22 |
| P-03 | Visualización de radio de impacto | Desarrollador | CU-02 + CU-11 | RF-07, RF-27 |
| P-04 | Visualización de cadena de llamadas | Desarrollador | CU-03 + CU-11 | RF-08, RF-27 |
| P-05 | Mapa de calor de acoplamiento | Desarrollador | CU-07 + CU-11 | RF-18, RF-27 |

Cita:
> **Elementos por pantalla.** Cada wireframe declara explícitamente: (i)
> **campos de entrada** (carpeta del proyecto, símbolo consultado,
> profundidad, ventana temporal); (ii) **acciones** disponibles (ejecutar
> consulta, sincronizar, enriquecer, exportar); y (iii) **mensajes y
> validaciones** para los flujos alternativos del Anexo B (símbolo
> inexistente, servicio de análisis no disponible, historial insuficiente,
> consulta mal formada, etc.).

**Notas del orador:**
[Recordar que estos son wireframes de baja fidelidad, no UI final. P-01 no
tiene prototipo porque ocurre como diálogo de instalación. Las
validaciones declaradas son las que la rúbrica exige como «mensajes o
estados relevantes».]

---

## Slide 10 — Anexo E — Prototipo con IA (capturas)

**Kicker:** 10 · Anexo E

**Título:** Prototipo exploratorio apoyado con IA

**Contenido:**

Párrafo introductorio:
> Para validar de manera temprana la interacción del Desarrollador, se
> construyó un **miniprototipo** con cuatro vistas web (P-02 a P-05),
> generado con apoyo de Claude Code y ajustado por el autor.

Cuatro tarjetas con imágenes:

- **Figura E-1 · Consola de consultas (P-02)**
  `![Consola de consultas](../../anexos/img/e1-query-console.png)`
- **Figura E-2 · Radio de impacto (P-03)**
  `![Radio de impacto](../../anexos/img/e2-blast-radius.png)`
- **Figura E-3 · Cadena de llamadas (P-04)**
  `![Cadena de llamadas](../../anexos/img/e3-call-chain.png)`
- **Figura E-4 · Acoplamiento histórico (P-05)**
  `![Mapa de calor de acoplamiento](../../anexos/img/e4-coupling.png)`

Pie:
> Las capturas usan los mismos datos de ejemplo que los wireframes del Anexo D,
> para facilitar la comparación visual. Detalle en `anexos/E-prototipo-ia.md`.

Nota (limitaciones declaradas):
> **Limitaciones del prototipo.** El prototipo opera con datos de ejemplo
> y cubre solo una parte de los flujos del Anexo B. Quedan fuera de su
> alcance la pantalla P-01, la interacción del Asistente de IA y la
> validación de RNF. Las vistas se mantienen en inglés mientras la
> especificación está en español, y el código generado **no debe
> reutilizarse** como base de la implementación.

**Notas del orador:**
[Insistir en que el prototipo es exploratorio: ayuda a validar comprensión e
interacción, no es la implementación definitiva. Mencionar los datos de
ejemplo trazables y la decisión consciente de mantenerlo en inglés.]

---

## Slide 11 — Anexo F — Declaración de uso de IA

**Kicker:** 11 · Anexo F

**Título:** Declaración de uso de Inteligencia Artificial

**Contenido:**

Párrafo introductorio:
> La IA se usó como **herramienta de apoyo** al proceso de Ingeniería
> de Requerimientos. Su rol fue acotado: nunca fue fuente definitiva de
> requerimientos ni sustituto del análisis del autor.

Tarjeta 1 — *F.1 – F.3 · Qué se usó y para qué*:
> - **Herramienta:** Claude Code (Anthropic), única herramienta de IA
>   empleada.
> - **Propósito:** apoyar la estructuración del SRS según IEEE 830-1998, la
>   redacción de RF/RNF, la generación de diagramas y la construcción del
>   prototipo.
> - **Modalidad:** generación iterativa a partir de descripciones en lenguaje
>   natural.
> - **Prompts generales:** entre otros, «elaborar el SRS según IEEE 830-1998»,
>   «redactar RF/RNF claros, formales y verificables», «generar el diagrama
>   de CU con relaciones include y extend» y «revisar los pendientes del
>   documento».

Tarjeta 2 — *F.4 – F.5 · Qué generó y qué ajustó el autor*:
> - **Generado:** redacción base, diagramas, wireframes, prototipo.
> - **Ajustado por el autor:** autoría individual, stakeholders reales, datos
>   de ejemplo trazables, selección de vistas.
> - **Decisiones humanas:** alcance, prioridades, proceso PN-01, decisión de
>   CU-03 (ruta única).

Tarjeta 3 — *F.7 · Aspectos pendientes de validación*:
> - Umbrales cuantitativos de los RNF con stakeholders reales.
> - Falsos positivos aceptables en la detección de código sin uso.
> - Prueba de usabilidad del proceso de instalación (RNF-06).
> - Necesidades del tercer stakeholder (comunidad amplia de desarrolladores).

**Notas del orador:**
[Esta slide es importante para la transparencia: deja claro qué hizo la IA
y qué no. Mencionar que los pendientes están declarados, no ocultos.]

---

## Slide 12 — Cierre y referencias

**Kicker:** Cierre

**Título:** Anexos cubiertos y referencias

**Contenido:**

Párrafo introductorio:
> Los seis anexos cubren el sistema desde ángulos distintos: el A
> desde los casos de uso, el B desde la especificación, el C desde el
> proceso de negocio, el D y el E desde las interfaces, y el F desde la
> transparencia sobre el uso de IA.

Tabla resumen:

| Anexo | Contenido | Archivo |
|---|---|---|
| A | Diagrama de casos de uso (once CU, cuatro actores) | `A-diagrama-casos-de-uso.md` |
| B | Especificación de casos de uso | `B-especificacion-casos-de-uso.md` |
| C | Proceso de negocio PN-01 (actual y propuesto) | `C-proceso-de-negocio.md` |
| D | Diseño preliminar de interfaces (cinco *wireframes*) | `D-interfaces.md` |
| E | Prototipo apoyado con IA (cuatro vistas) | `E-prototipo-ia.md` |
| F | Declaración de uso de IA | `F-declaracion-uso-ia.md` |

Párrafo sobre control de versiones:
> **Control de versiones.** El documento incluye al inicio una tabla de
> control con versión, fecha, autor y descripción del cambio, donde la
> versión final es la 1.0. El historial detallado de la elaboración queda
> registrado en el repositorio, accesible con
> `git log -- docs/srs/`, lo que da cuenta del trabajo realizado a lo largo
> del módulo conforme lo solicita la pauta.

Párrafo sobre integración con actividades previas:
> **Integración con actividades previas.** El SRS integra el trabajo de las
> actividades previas del módulo (identificación de stakeholders, educción,
> análisis, modelado de casos de uso, modelado del proceso de negocio y
> diseño preliminar de interfaces), y cada sección del documento lo
> referencia de forma trazable.

Referencias (APA 7):
- Anthropic. (s. f.). *Claude Code* [Software de asistencia de
  programación]. https://www.anthropic.com/claude-code
- Dalpiaz, F., Franch, X., & Horkoff, J. (2016). *iStar 2.0 language guide*
  (arXiv:1605.07767). arXiv. https://arxiv.org/abs/1605.07767
- Institute of Electrical and Electronics Engineers. (1998). *IEEE Std
  830-1998: IEEE recommended practice for software requirements
  specifications*. IEEE.
  https://doi.org/10.1109/IEEESTD.1998.88286
- Puentes, S. (2026, julio 7). *Especificación de requerimientos de
  software: Sistema LAIN — Plataforma de inteligencia de código para
  asistentes de desarrollo* (Versión 1.0) [Documento SRS].
  https://github.com/spuentesp/lain/blob/main/docs/srs/SRS.md
- spuentesp. (2026). *lain: Plataforma de inteligencia de código para
  asistentes de desarrollo* (v0.3.0) [Software]. GitHub.
  https://github.com/spuentesp/lain
- Universidad de La Frontera. (2026). *Entrega final — Proyecto de
  Ingeniería de Requerimientos* [Pauta de evaluación]. Curso EMI307-1
  Especificación de Requerimientos, Módulo 03: Inteligencia Artificial
  aplicada a Ingeniería de Requerimientos.

**Notas del orador:**
[Cerrar agradeciendo, invitando a preguntas y ofreciendo los enlaces a los
documentos fuente. Mencionar que el repositorio contiene todo el historial
de la entrega.]

---

## Notas generales para el expositor

- **Duración total sugerida:** 8–10 minutos (≈ 1 minuto por slide).
- **Público objetivo:** cuerpo docente y compañeros del módulo EMI307-1.
- **Tono:** académico, claro, sin jerga sin definir la primera vez que
  aparece.
- **Si el tiempo aprieta:** saltar la Slide 7 (prototipo) o la Slide 8
  (declaración de IA); ambas son profundizaciones.
- **Si sobra tiempo:** abrir el prototipo en vivo (las vistas web son
  archivos HTML autónomos en `anexos/img/` referenciados desde el prototipo).