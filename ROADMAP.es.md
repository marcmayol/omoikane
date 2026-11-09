# Hoja de ruta

Hacia dónde va Omoikane, qué no va a hacer y por qué. No hay fechas: una versión sale cuando está medida y probada en Windows, Linux y macOS. Lo que cambió de verdad cada versión está en el [CHANGELOG](CHANGELOG.md). Los mismos puntos están en el [tablero del proyecto](https://github.com/users/marcmayol/projects/1).

[Read in English](ROADMAP.md)

## Cómo entra algo

- Una idea se mide antes de salir. Varias se descartaron porque, en conversaciones reales, no arreglaban nada (ver [Descartado](#descartado)).
- La 1.x no se rompe: cada versión lee las bases y sincroniza con todas las 1.x anteriores. La CI lo comprueba en cada commit contra un binario 1.0.0 fijo.
- El recall corre en cada mensaje, así que tiene un presupuesto (unos 30 ms). Lo que costaría más ahí va a otro sitio: la revisión, un proceso de fondo o una orden explícita.
- ¿Quieres algo? Abre una issue con lo que probaste y lo que te faltó. [CONTRIBUTING.es.md](CONTRIBUTING.es.md) explica cómo probar un cambio.

## Lo siguiente

Huecos conocidos, sin orden fijo:

- Una carpeta sin remoto de git que ya se sincronizó por el repositorio general antes de entrar en el espacio de un cliente conserva su clave de siempre, así que dos carpetas con el mismo nombre en dos clientes siguen pausándose la una a la otra en ese caso (se resuelve con `omoi projects keep`). Las que entran en un espacio antes de salir del equipo ya llevan el espacio en la clave desde la 1.9.0.
- Mientras un equipo siga en una versión anterior a la 1.9.0, la misma carpeta de un cliente puede quedar como dos proyectos (`omoi projects merge` los junta).

## Más adelante, quizá

- Una imagen de Docker, solo si Omoikane llega a tener una parte de servidor. Hoy no hay servidor: la sincronización va por tu propio repositorio git, y los hooks y el MCP corren junto al agente, así que un contenedor no aporta nada. Antes podría llegar un Dockerfile para reproducir [omoikane-bench](https://github.com/marcmayol/omoikane-bench).

## Descartado

Decidido que no, con el motivo:

- **Un servidor, un demonio o una nube.** Un binario y un fichero SQLite; la sincronización va por un repositorio git tuyo.
- **Entornos externos** (Python, Node, ONNX Runtime). Cero dependencias es un principio: el modelo de significado corre en Rust puro.
- **Que los agentes verifiquen memorias.** Solo una persona puede marcar una memoria como verificada, desde su terminal o desde la vista web. No hay herramienta MCP para eso, a propósito.
- **Un glosario por proyecto** (sinónimos para ampliar la consulta). En 280 mensajes reales arregló 0 y rompió 0.
- **Aprender entre proyectos** (traer memorias de otros proyectos). En 168 mensajes reales ayudó una vez y en el resto metió ruido.
- **Guardar los mensajes del usuario.** En 30 días de conversaciones reales, 0 preguntas necesitaban un mensaje anterior que las memorias no respondieran ya.
- **Un resumen por sesión.** En 151 actualizaciones de estado reales, lo que se quedaba fuera de los resúmenes no volvió a hacer falta nunca.

## Publicado

| Versión | Fecha | Qué trajo |
|---|---|---|
| 1.9.0 | 2026-11-09 | Papelera y espacios en la interfaz de terminal, lo editado en Obsidian vuelve, carpetas con el mismo nombre en dos clientes, copias cifradas con la clave de cada cliente, la traza en la procedencia, revisión 3,6 veces más rápida con 50.000 memorias, guardia de rendimiento en la CI |
| 1.8.6 | 2026-11-08 | Enlace directo para verificar una memoria, verificaciones que sobreviven al cambio de claves, oficios duplicados, contraseñas escritas a mano redactadas, tiempo máximo en las descargas |
| 1.8.5 | 2026-11-08 | Memorias verificadas por una persona, exportar e importar OKF, código que cambió bajo una memoria, procedencia |
| 1.8.0 | 2026-11-07 | Oficios (memoria por stack), nombres de proyecto que toleran erratas, fechas de revisión, memorias con clave |
| 1.7.0 | 2026-11-06 | Agentes sin nadie delante (CI, servidores), vista por cliente, muchos agentes sobre una misma base |
| 1.6.0 | 2026-11-06 | Decisiones abiertas, historial por fichero, recall desde tu propia documentación |
| 1.5.0 | 2026-11-05 | Recetas propuestas a partir de pasos repetidos |
| 1.4.0 | 2026-11-04 | Los cambios llegan a mitad de sesión, subagentes, diario de sesiones, mantenimiento que avisa |
| 1.3.0 | 2026-11-02 | Rápida otra vez: motor propio para el modelo de significado, cargado solo cuando ayuda |
| 1.2.0 | 2026-11-01 | Entiende preguntas hechas con otras palabras (EmbeddingGemma 300M, Rust puro) |
| 1.1.0 | 2026-10-31 | Benchmark público, comprobación de compatibilidad con la 1.0 en cada commit |
| 1.0.0 | 2026-10-31 | Primera versión estable |
