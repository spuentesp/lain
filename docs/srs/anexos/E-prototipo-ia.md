# Anexo E — Mini prototipo apoyado con IA

## E.1 Objetivo del prototipo

El prototipo tiene por objetivo **explorar y validar de manera temprana** la interacción del Desarrollador con los análisis del sistema, en particular: (a) la consola de consultas y estado del mapa, y (b) las visualizaciones de radio de impacto, cadena de llamadas y acoplamiento histórico. Busca responder preguntas de requerimientos —¿es comprensible el radio de impacto presentado como grafo por niveles?, ¿qué acciones necesita el usuario junto a cada resultado?— antes de comprometer decisiones de diseño definitivas.

El prototipo **no es una implementación definitiva** del sistema: es un apoyo visual y exploratorio para comprender mejor los requerimientos y las posibles interacciones del usuario con la solución.

## E.2 Herramienta de IA utilizada

- **Herramienta:** asistente de programación basado en IA (Claude Code, de Anthropic). ⟨PENDIENTE: confirmar y completar otras herramientas usadas por el grupo, con sus versiones.⟩
- **Modalidad de uso:** generación asistida de páginas web interactivas y autónomas (sin dependencias externas) a partir de descripciones en lenguaje natural de cada vista, iterando sobre la propuesta generada.

## E.3 Funcionalidad y flujo representado

El prototipo consiste en cuatro vistas web interactivas que representan las pantallas P-02 a P-05 del Anexo D:

1. **Consola de consultas** («Query Console»): editor de consultas estructuradas con botón de ejecución, panel de resultados y panel de estado del servicio; representa el flujo de CU-10 y el monitoreo de CU-09.
2. **Radio de impacto** («Blast Radius»): grafo interactivo con el símbolo consultado al centro y los símbolos afectados dispuestos por profundidad; representa el resultado de CU-02.
3. **Cadena de llamadas** («Call Chain»): visualización de la ruta de invocaciones entre un símbolo de origen y uno de destino; representa el resultado de CU-03.
4. **Mapa de calor de acoplamiento** («Coupling Heatmap»): listado de archivos co-cambiantes con la intensidad de su acoplamiento histórico; representa el resultado de CU-07.

**Flujo representado (extremo a extremo):** el usuario formula una consulta o solicita un análisis en la consola → el sistema responde con datos del mapa → el usuario abre la visualización correspondiente y navega el resultado (selección de nodos, profundidad, leyendas de confianza). Es el mismo flujo «consultar → evidenciar → decidir» del proceso PN-01 (Anexo C).

## E.4 Evidencia visual

⟨PENDIENTE: insertar capturas de pantalla de las cuatro vistas del prototipo, con una leyenda por captura indicando la pantalla del Anexo D que representa.⟩

| Captura | Vista | Pantalla del Anexo D |
|---|---|---|
| Figura E-1 | Consola de consultas | P-02 |
| Figura E-2 | Radio de impacto | P-03 |
| Figura E-3 | Cadena de llamadas | P-04 |
| Figura E-4 | Mapa de calor de acoplamiento | P-05 |

## E.5 Relación con casos de uso y requerimientos

| Vista del prototipo | Caso(s) de uso | RF asociados |
|---|---|---|
| Consola de consultas | CU-10, CU-09, CU-08 (esquema) | RF-25, RF-04, RF-05, RF-06, RF-22 |
| Radio de impacto | CU-02, CU-11 | RF-07, RF-09, RF-27 |
| Cadena de llamadas | CU-03, CU-11 | RF-08, RF-27 |
| Mapa de calor de acoplamiento | CU-07, CU-11 | RF-18, RF-27 |

## E.6 Elementos generados, sugeridos o apoyados por IA

- **Generados por IA:** la estructura de las páginas web, el código de presentación de los grafos y tablas, la disposición inicial de paneles de la consola y los estados visuales (activo/error/vacío).
- **Sugeridos por IA:** la organización del radio de impacto en anillos por profundidad; la distinción visual entre relaciones de confianza alta y media; la inclusión de un panel de estado del servicio junto a la consola.
- **Apoyados por IA:** la redacción de los textos de advertencia (por ejemplo, correlación vs. causalidad en el acoplamiento) y de los estados vacíos.

## E.7 Ajustes realizados por el grupo

⟨PENDIENTE: completar con los ajustes reales realizados por el grupo sobre la propuesta generada. Ejemplos del tipo de ajuste a documentar: cambios de disposición o de terminología en español, eliminación de controles innecesarios, ajuste de leyendas y umbrales, decisión de qué vistas conservar.⟩

## E.8 Limitaciones del prototipo

- Las vistas operan con **datos de ejemplo o resultados puntuales**; no constituyen la interfaz definitiva ni cubren todos los flujos alternativos especificados en el Anexo B.
- No implementa la pantalla P-01 (configuración inicial) ni la interacción del actor Asistente de IA, que ocurre por protocolo y no por interfaz gráfica.
- No valida requerimientos no funcionales (rendimiento, escalabilidad, consumo de recursos); solo aspectos de comprensión e interacción.
- La accesibilidad y la adaptación a distintos tamaños de pantalla no fueron objetivos de esta exploración.
- Al ser generado con apoyo de IA, el código del prototipo no sigue estándares de calidad de producción y **no debe reutilizarse como base de la implementación**.
