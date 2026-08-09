# Guion de la presentación — LAIN, Anexos del SRS

> **Uso:** este guion acompaña a `presentacion.html` y `presentacion.pdf`.
> Está pensado para una exposición oral de **11 a 12 minutos** ante el cuerpo
> docente y los pares del módulo **EMI307-1**.
>
> **Cómo leerlo:**
> - **«…»** = texto verbal sugerido. Puedes leerlo tal cual o reformularlo.
> - **[stage direction]** = indicación para el expositor (a dónde mirar,
>   cuándo avanzar, qué señalar).
> - Los tiempos son referenciales; ajústalos según tu ritmo.
>
> **Cobertura de la rúbrica (iii. Detalles):**
> - IEEE 830-1998 y su estructura → Slide 2.
> - Coherencia problemática · objetivo · alcance · stakeholders · RF · anexos
>   → Slides 2 y 3 (cadena problema → stakeholder → RF).
> - Trazabilidad RF · CU · proceso · interfaces · prototipo → Slides 3 a 8.
> - Diferenciación RF / RNF / restricciones → Slide 2.
> - Integración de actividades previas → Slide 2 (cierre).
> - Control de versiones → Slide 10 (cierre).

---

## Apertura (Slide 1 — Portada) · 0:00–0:30

**[stage] Permanecer en la portada mientras se presenta el tema. No
adelantarse al contenido.**

> «Buenos días / Buenas tardes. Mi nombre es [tu nombre] y hoy voy a
> presentar los **anexos** de la Especificación de Requerimientos del sistema
> **LAIN**, una plataforma de inteligencia de código que estoy desarrollando
> como parte del proyecto del módulo. La exposición recorre los seis anexos,
> de la A a la F, y dura entre once y doce minutos. Al final quedo abierto a
> preguntas.»

**[stage] Avanzar a la siguiente slide cuando termines la presentación
personal.**

---

## Contexto (Slide 2) · 0:30–1:25

**[stage] Señalar la columna izquierda primero, luego la tarjeta de la
derecha.**

> «Antes de entrar en los anexos, vale la pena recordar el contexto. **LAIN**
> es un sistema que construye un mapa del código fuente de un proyecto y lo
> pone a disposición de los asistentes de programación y de los propios
> desarrolladores. La pregunta que motiva el proyecto es simple: cuando un
> asistente de IA propone un cambio, ¿cómo sabe qué más se romperá?
>
> Para responderla con rigor, elaboré un **SRS** —especificación de
> requerimientos— siguiendo el estándar **IEEE 830-1998**, que es la plantilla
> que pide el módulo. El documento se organiza en tres bloques: una
> **Introducción** con propósito, alcance, definiciones y referencias; una
> **Descripción general** con perspectiva, funciones, usuarios, restricciones
> y supuestos; y los **Requisitos específicos**, donde están los
> **veintisiete requerimientos funcionales**, los **diez no funcionales** y
> las **cinco restricciones** del sistema, más la matriz de trazabilidad.
> La diferenciación entre estas tres categorías es importante: los
> **RF** definen el alcance funcional, los **RNF** fijan la calidad
> esperada y las **restricciones** son condiciones no negociables. En
> este SRS hay veintisiete RF, diez RNF y cinco restricciones.
>
> Esta especificación no se escribió de una sola vez: integra el trabajo de
> **actividades previas del módulo** —inscripción del proyecto, identificación
> de stakeholders, educción de requerimientos, análisis, modelado de casos
> de uso, modelado del proceso de negocio y diseño preliminar de
> interfaces—. Cada actividad dejó un antecedente que el SRS recoge y
> referencia desde su sección correspondiente.
>
> Los anexos complementan ese documento principal. Son seis y tienen una
> función muy concreta cada uno: el A grafica los casos de uso, el B los
> especifica, el C modela el proceso de negocio donde el sistema aporta
> valor, el D dibuja las pantallas, el E muestra un prototipo exploratorio y
> el F declara cómo se usó la inteligencia artificial durante la entrega.
> Pero antes de entrar a los anexos, conviene detenerse en el problema y en
> los stakeholders que los motivaron.»

**[stage] Pausa breve. Avanzar.**

---

## Stakeholders y problema (Slide 3) · 1:25–3:00

**[stage] Esta es la slide más importante para entender de dónde viene el
proyecto. Tomarse tiempo.**

> «Esta slide condensa la Sección 2.3 del SRS, y quiero detenerme en ella
> porque la presentación de los anexos se sostiene sobre la cadena que
> resume. Los stakeholders que voy a mencionar fueron educidos y validados
> en actividades previas del módulo, así que esta slide aterriza los que el
> módulo ya conocía.
>
> **El problema — SRS §1.2.** Cuando un asistente de IA razona sobre una
> base de código grande, solo ve el archivo abierto o resultados de
> búsqueda textual. Sin esa visibilidad, sus propuestas rompen código en
> partes no consideradas, duplican funcionalidad que ya existía y dejan
> fuera acoplamientos que solo se ven en el historial. El resultado es
> predecible: alto costo en *tokens* y baja precisión en las respuestas. La
> pregunta que sobrevuela todo el proyecto es directa: cuando modifico
> esta función, ¿qué más se ve afectado?
>
> **Los stakeholders — SRS §2.3.** Identifiqué tres, con influencia
> desigual. El **principal** soy yo — Sebastián Puentes, desarrollador
> y propietario del producto (también cumplo el rol de usuario
> «Desarrollador de software»); influyo en alcance y prioridades. El
> **secundario** es el **docente del módulo EMI307-1**; evalúa que la
> especificación sea coherente y trazable, y define los criterios de
> aceptación del documento. Y el **terciario** es la **comunidad** de
> desarrolladores usuarios de asistentes de IA, cuyas necesidades se
> recogen indirectamente a través mío.
>
> **La trazabilidad — SRS §2.3, cierre.** La necesidad de origen —reducir
> el consumo de *tokens* al entregar contexto al asistente— aterriza en
> los **RF-21 y RF-22** (provisión de contexto curado y acotado) y en el
> **RF-25** (consulta selectiva del mapa). El **CU-08** registra como
> observación que el paquete de contexto debe ser acotado, y la **RG-1**
> exige ejecución local para que el código no salga del equipo.
>
> Esa cadena —problema → stakeholder → necesidad de origen → RF— es el
> corazón de la trazabilidad del documento. Cuando defendamos un
> requerimiento, la pregunta de fondo es siempre la misma: ¿a qué
> necesidad responde?»

**[stage] Pausa breve. Avanzar.**

---

## Demo: LAIN en acción (Slide 4) · 3:00–3:30

**[stage] Esta slide es un respiro visual antes del bloque de anexos. Muestra
que el sistema existe y se ejecuta.**

> «Antes de meternos de lleno en los anexos, vale la pena ver al sistema
> funcionando. Esta captura muestra una sesión típica de LAIN en una
> terminal real: inicialización del espacio de trabajo, búsqueda en
> lenguaje natural, radio de impacto y cadena de llamadas.
>
> Lo que ven son los comandos y la salida del CLI —no es un mockup
> genérico—. Los nombres `lain init`, `lain ask`, `lain blast-radius` y
> `lain call-chain` corresponden a las herramientas que ya implementa el
> sistema, y los símbolos que aparecen —como `auth/jwt.rs::validate_token`—
> son ejemplos del tipo de consultas que el Desarrollador o el Asistente de
> IA formulan en la práctica.
>
> Cada comando materializa un caso de uso del Anexo A: `lain init`
> corresponde a **CU-01**, `lain ask` a **CU-04** (búsqueda semántica),
> `lain blast-radius` a **CU-02** (radio de impacto) y `lain call-chain`
> a **CU-03** (cadena de llamadas). Esa correspondencia es la que justifica
> que cada CU tenga trazabilidad explícita a uno o más RF del SRS.»

**[stage] Avanzar.**

---

## Arquitectura de LAIN (Slide 5) · 3:30–4:15

**[stage] El diagrama de arquitectura aparece renderizado en la slide. Empezar
por la izquierda (código fuente) y avanzar hacia la derecha (MCP, asistentes).**

> «Antes de entrar a los anexos, vale la pena ver **cómo está construido**
> el sistema. LAIN corre como un servidor MCP local en la máquina del
> Desarrollador. El código fuente alimenta tres extractores en paralelo:
> los **adaptadores LSP** (rust-analyzer, pyright, gopls, tsserver), que dan
> referencias de alta confianza; **Tree-sitter**, como fallback
> heurístico cuando no hay LSP; y el **sensor Git**, que extrae historial y
> co-cambio. Los tres alimentan el **grafo de conocimiento**, que se persiste
> en `.lain/graph.bin` con UUIDs v5 estables.
>
> Sobre el grafo, una **capa volátil** mantiene los cambios en memoria hasta
> la siguiente sincronización; el **motor de consultas** (JSON ops-array)
> responde los pedidos; y el **servidor MCP** (stdio o HTTP) es el protocolo
> estándar que usan los asistentes.
>
> Lo que ven a la izquierda —Desarrollador y Asistente de IA— son los dos
> actores principales. Ninguno de los dos envía código afuera: el grafo y
> las respuestas se procesan 100% en local, que es la RG-1 del SRS.»

**[stage] Avanzar.**

---

## Anexo A — Casos de uso (Slide 6) · 4:15–5:30

**[stage] El diagrama de casos de uso aparece renderizado en la slide. Empezar
por el límite del sistema, después señalar los actores y los casos de uso.**

> «El Anexo A es el **diagrama de casos de uso**. Aquí se ve el alcance del
> sistema desde dos ángulos: los **actores** que participan y las
> **interacciones** que el sistema soporta.
>
> Una nota antes de seguir: el diagrama tiene un **límite del sistema**
> explícito —el recuadro «Sistema LAIN»—, y lo que está dentro es lo que el
> sistema hace; lo que está fuera son los actores que lo usan o lo consultan.
> La IA no edita código ni gestiona el repositorio; eso queda fuera del
> límite.
>
> Hay **cuatro actores**. Dos son principales: el **Desarrollador**, que es
> la persona que programa, y el **Asistente de IA**, que es un programa que
> consume las capacidades del sistema mientras trabaja. Los otros dos son
> secundarios: el **repositorio de versiones**, para los análisis históricos,
> y el **servicio de análisis de lenguaje**, para resolver referencias con
> precisión.
>
> En total hay **once casos de uso**. Los ocho más importantes están en la
> tabla: inicializar el espacio de trabajo, evaluar el impacto de un cambio,
> trazar la cadena de llamadas, buscar código por significado, consultar la
> arquitectura, verificar los cambios, analizar el acoplamiento histórico y
> obtener contexto de un símbolo.
>
> Los tres restantes —**mantener el mapa**, **consultar con lenguaje
> estructurado** y **visualizar**— son casos utilitarios. Los dos primeros
> aparecen como **«include»** dentro de los casos de consulta: cada vez que
> el Desarrollador o el asistente piden un análisis, el sistema primero
> garantiza que el mapa esté actualizado. El tercero, **visualizar**, aparece
> como **«extend»** sobre tres casos: cuando el Desarrollador pide una vista
> gráfica, se activa la pantalla correspondiente.
>
> El diagrama es **coherente con los veintisiete RF** del SRS —la
> correspondencia exacta está en la matriz de trazabilidad de la Sección 3.4
> y se replica en el Anexo B, caso por caso.»

**[stage] Avanzar.**

---

## Anexo B — Especificación de casos de uso (Slide 7) · 5:30–6:30

**[stage] Señalar primero la tarjeta de la plantilla, luego el ejemplo
CU-02.**

> «El Anexo B es donde esos once casos se vuelven **descripciones
> textuales**. La decisión metodológica fue aplicar **la misma plantilla a
> los ocho casos principales**, para que la lectura sea predecible y la
> trazabilidad con los requerimientos sea directa.
>
> La plantilla tiene **nueve campos**: identificador y nombre, actor
> principal y actores secundarios, objetivo, precondiciones, flujo principal
> numerado, flujos alternativos con sus excepciones, postcondiciones, los
> **requerimientos funcionales asociados** y un bloque final de
> **observaciones o supuestos pendientes**.
>
> Para mostrar cómo se materializa, miren el caso **CU-02 «Evaluar impacto
> de un cambio»**, que es el caso central del sistema. Su objetivo es
> responder, antes de modificar un símbolo, qué otros símbolos se verían
> afectados. El flujo principal es muy simple: el actor pide el radio de
> impacto, el sistema actualiza el mapa, resuelve las referencias con el
> servicio de análisis, calcula el cierre transitivo de los afectados y
> devuelve la lista jerarquizada. Los RF que cubre son el siete, el nueve,
> el diez, el once y el veintisiete.
>
> Los tres casos utilitarios —CU-09, CU-10 y CU-11— se describen en forma
> abreviada porque su comportamiento es trivial o porque depende de otros
> casos.»

**[stage] Avanzar.**

---

## Anexo C — Proceso de negocio PN-01 (Slide 8) · 6:30–8:05

**[stage] Esta slide es la más importante para entender el valor del
proyecto. Tomarse tiempo.**

> «El Anexo C modela el **proceso de negocio** donde el sistema LAIN aporta
> su mayor valor. Lo llamé **PN-01 «Evaluación de impacto antes de modificar
> código»**, porque es exactamente el momento del trabajo de un equipo de
> desarrollo en el que hoy se concentra el problema.
>
> Para situarnos, los **elementos del proceso** son los clásicos: el
> evento de inicio (la necesidad de modificar código), los participantes
> (el Desarrollador, el Asistente de IA y, en la variante propuesta, el
> sistema LAIN), las decisiones clave («¿el impacto es alto o
> inesperado?» y «¿aprueba el Desarrollador el plan?») y el evento de
> término (que el cambio quede integrado con su impacto conocido y
> verificado).
>
> En la **situación actual** —sin el sistema— el asistente de IA ve solo el
> archivo abierto y propone el cambio. Los efectos colaterales no aparecen
> hasta que se ejecutan las pruebas, o peor, hasta que el sistema está en
> producción. El equipo termina en ciclos de retrabajo. La causa raíz es
> clara: **la decisión de modificar se toma sin conocer el impacto**.
>
> En la **situación propuesta**, el sistema LAIN introduce lo que yo llamo
> una **«compuerta de evidencia»** entre la intención de cambio y su
> ejecución. Antes de tocar código, el asistente consulta al sistema: se
> actualiza el mapa, se obtiene el radio de impacto, se miran los llamadores
> y el acoplamiento histórico. Con esa información, el plan se elabora o se
> replantea. El Desarrollador aprueba el plan y, después de aplicado el
> cambio, el sistema ejecuta la verificación **decorada con contexto
> arquitectónico** —es decir, cuando algo falla, no solo te dice qué línea
> rompió, sino qué funciones llaman a esa línea.
>
> El punto clave es que el sistema **acompaña al equipo sin reemplazarlo**:
> aporta evidencia en el momento del proceso donde hoy se origina el
> retrabajo, pero la decisión sigue siendo humana.»

**[stage] Pausa breve para que aterrice la idea. Avanzar.**

---

## Anexo D — Interfaces (Slide 9) · 8:05–9:00

**[stage] Recorrer la tabla de pantallas de izquierda a derecha.**

> «El Anexo D es el **diseño preliminar de interfaces**. Son **cinco
> wireframes de baja fidelidad**, no una UI definitiva: sirven para acordar
> la interacción esperada, no para construir la implementación.
>
> La **P-01** es el asistente de configuración inicial. Aparece cuando el
> Desarrollador corre la instalación por primera vez. La **P-02** es la
> consola principal: ahí se hacen las consultas estructuradas y se ve el
> estado del mapa. Las tres últimas son las visualizaciones de los
> análisis: **P-03** muestra el radio de impacto como un grafo por niveles,
> **P-04** muestra la cadena de llamadas entre dos símbolos y **P-05**
> muestra el mapa de calor de acoplamiento histórico.
>
> Cada wireframe declara explícitamente tres cosas: los **campos de
> entrada** —carpeta del proyecto, símbolo consultado, profundidad o ventana
> temporal según corresponda—, las **acciones** disponibles —ejecutar
> consulta, sincronizar, enriquecer, exportar— y los **mensajes y
> validaciones** para los flujos alternativos del Anexo B: símbolo
> inexistente, servicio de análisis no disponible, historial insuficiente
> o consulta mal formada. Eso es lo que vuelve trazable cada pantalla con
> los CU y los RF.
>
> Hay un detalle importante: el Asistente de IA **no usa pantallas**. Su
> interacción es por un protocolo estándar, no por una GUI. Por eso las cinco
> interfaces están pensadas solo para el Desarrollador.»

**[stage] Avanzar.**

---

## Anexo E — Prototipo con IA (Slide 10) · 9:00–10:00

**[stage] Recorrer las cuatro capturas en orden: consola, radio de impacto,
cadena de llamadas, acoplamiento.**

> «El Anexo E cierra la brecha entre los wireframes y la implementación con
> un **mini prototipo exploratorio**. Son cuatro páginas web autónomas que
> implementan, de manera preliminar, las pantallas P-02 a P-05 con datos de
> ejemplo.
>
> ¿Por qué un prototipo y no la interfaz final? Porque la idea era **validar
> la comprensión** de los requerimientos antes de comprometer el diseño: ¿se
> entiende el radio de impacto presentado como un grafo por niveles? ¿Qué
> acciones necesita el usuario junto a cada resultado? Esas son preguntas
> que un wireframe no responde, pero un prototipo interactivo sí.
>
> El prototipo se construyó con apoyo de **Claude Code**, que es la única
> herramienta de IA usada en la entrega. Sobre la propuesta generada, hice
> tres ajustes que vale la pena mencionar: fijé los datos de ejemplo
> idénticos a los del Anexo D para que la comparación sea trazable a simple
> vista, seleccioné las cuatro vistas que mejor representan los casos de uso
> principales, y mantuve el prototipo en inglés —idioma de la propuesta
> generada— documentándolo en español. Decidí no retraducir un artefacto
> exploratorio.
>
> Las **limitaciones** también están declaradas —no son un descuido—.
> El prototipo opera con datos de ejemplo y cubre solo una parte de los
> flujos del Anexo B. Quedan fuera de su alcance la pantalla P-01, la
> interacción del Asistente de IA y la validación de RNF. Las vistas se
> mantienen en inglés mientras la especificación está en español, y el
> código generado **no debe reutilizarse** como base de la
> implementación. Es un apoyo exploratorio, no un producto.»

**[stage] Avanzar.**

---

## Anexo F — Declaración de uso de IA (Slide 11) · 10:00–11:00

**[stage] Tarjeta por tarjeta, sin prisa. Esta slide es de transparencia.**

> «El Anexo F es la **declaración de uso de inteligencia artificial**, y es
> importante leerla con atención porque la pauta del módulo la pide
> explícitamente.
>
> La idea central es esta: la IA fue una **herramienta de apoyo**, con un
> rol acotado. Claude Code apoyó cuatro frentes:
> la estructuración del documento según IEEE 830, la redacción de los RF y
> los RNF, la generación de los diagramas y la construcción del prototipo.
> La modalidad fue iterativa: yo describía cada vista en lenguaje natural,
> evaluaba la propuesta y ajustaba lo que no me convencía.
>
> Para que la declaración sea verificable, incluí los **prompts generales**
> que usé: «elaborar el SRS según IEEE 830-1998», «redactar los RF y los
> RNF de forma clara, formal y verificable», «generar el diagrama de casos
> de uso con relaciones include y extend», y al final «revisar los
> pendientes del documento». La trazabilidad entre lo que pedí y lo que
> obtuve es la forma más honesta de declarar el uso.
>
> Lo que **no** hizo la IA fue decidir el alcance, fijar las prioridades,
> elegir el proceso de negocio a modelar, ni resolver los stakeholders. Eso
> salió del análisis del problema y de la validación con el stakeholder
> principal.
>
> Quedan cuatro **aspectos pendientes de validación** que están declarados
> explícitamente en F.7 y no escondidos: los umbrales cuantitativos de los
> RNF, los falsos positivos aceptables en código sin uso, la prueba de
> usabilidad de la instalación y las necesidades del tercer stakeholder.»

**[stage] Avanzar.**

---

## Cierre (Slide 12) · 11:00–11:45

**[stage] Mostrar la tabla resumen de los seis anexos, después las
referencias. Mantener el tono sobrio.**

> «Para cerrar, los seis anexos cubren el sistema desde ángulos
> distintos: el A desde los casos de uso, el B desde la especificación
> textual, el C desde el proceso de negocio PN-01, el D y el E desde
> las interfaces y el prototipo, y el F desde la declaración de uso de
> IA. Cada anexo referencia los RF y los CU con los que traza, lo que
> permite moverse del documento a los modelos y viceversa sin perderse.
>
> Dos detalles finales sobre el proceso, que la rúbrica pide declarar.
> Primero, el documento tiene **control de versiones** al inicio —una
> tabla con versión, fecha, autor y descripción del cambio, donde la
> versión final es la 1.0—. El historial detallado de la elaboración
> queda en el repositorio, en `git log -- docs/srs/`, y da cuenta del
> trabajo realizado conforme lo solicita la pauta. Segundo, el SRS
> integra el trabajo de las actividades previas del módulo
> (stakeholders educidos, proceso modelado, interfaces diseñadas), y cada
> sección del documento lo referencia de forma trazable.
>
> Las referencias principales, en formato APA 7, son el estándar IEEE
> 830-1998, la guía del lenguaje iStar 2.0 de Dalpiaz, Franch y Horkoff,
> el documento SRS de Sebastián Puentes (2026), la pauta del curso de
> la Universidad de La Frontera, la documentación de Claude Code y el
> repositorio del proyecto en GitHub.
>
> Con eso termino. Gracias por su atención. ¿Preguntas?»

**[stage] Quedarse en la slide de cierre mientras se responden preguntas.
Tener a mano los enlaces a los anexos y al SRS principal.]**

---

## Anexo del guion — Tabla de tiempos

| Slide | Título | Inicio | Fin | Duración |
|---|---|---|---|---|
| 1 | Portada | 0:00 | 0:30 | 0:30 |
| 2 | Contexto | 0:30 | 1:25 | 0:55 |
| 3 | Stakeholders y problema | 1:25 | 3:00 | 1:35 |
| 4 | Demo — LAIN en acción | 3:00 | 3:30 | 0:30 |
| 5 | Arquitectura de LAIN | 3:30 | 4:15 | 0:45 |
| 6 | Anexo A — Casos de uso | 4:15 | 5:30 | 1:15 |
| 7 | Anexo B — Especificación | 5:30 | 6:30 | 1:00 |
| 8 | Anexo C — PN-01 | 6:30 | 8:05 | 1:35 |
| 9 | Anexo D — Interfaces | 8:05 | 9:00 | 0:55 |
| 10 | Anexo E — Prototipo | 9:00 | 10:00 | 1:00 |
| 11 | Anexo F — Uso de IA | 10:00 | 11:00 | 1:00 |
| 12 | Cierre | 11:00 | 11:45 | 0:45 |
| **Total** | | | | **11:45** |

**Duración total:** 11:45. Si necesitas recortar, las candidatas son la Slide 9
(Anexo D, Interfaces) o la Slide 5 (Arquitectura) —esta última admite
verse en 30 segundos si ya cubriste los componentes—. La Slide 8 (PN-01) no
conviene tocarla: es donde reside el valor del proyecto. Si quieres
extender, abre el prototipo en vivo o profundiza en la matriz de
trazabilidad.

---

## Recursos de apoyo

- **Documento principal:** `docs/srs/SRS.md` (IEEE 830-1998, 27 RF, 10 RNF,
  5 restricciones, matriz de trazabilidad en §3.4).
- **Anexos:** `docs/srs/anexos/A-…F-…md` (seis archivos, en este orden).
- **Prototipo:** las cuatro capturas están en `docs/srs/anexos/img/`. Si
  tienes la versión web disponible, puedes abrirla en vivo.
- **Control de versiones:** tabla al inicio de `SRS.md`; historial completo
  con `git log -- docs/srs/`.
- **Repositorio:** github.com/spuentesp/lain — contiene el historial
  completo de la entrega.
- **Si una pregunta te toma por sorpresa:** es válido responder «queda
  registrado para la siguiente iteración» y anotarla al final.