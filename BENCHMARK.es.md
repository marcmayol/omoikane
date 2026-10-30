# Omoikane frente a engram

[English](BENCHMARK.md)

Comparación directa con [engram](https://github.com/Gentleman-Programming/engram) (v3.6.0), la memoria en Go + SQLite para agentes de programación con la que se comparó Omoikane desde el principio. Medido el 28 de octubre de 2026 en un PC con Windows 11. **Omoikane lo he escrito yo.** Léelo sabiéndolo. El método de abajo está pensado para que eso no incline los números, y se cuenta cada resultado en el que engram sale mejor.

**Lo último (29-oct-2026, Omoikane 0.9.1).** Un 7.º conjunto de 100 preguntas escritas a ciegas, con 130 notas, que no se usó para ajustar nada: Omoikane acierta a la primera el 90 % de las veces (engram, el 75 %), entre las tres primeras el 97 % (87 %) y en paráfrasis el 86,7 % (70 %), con una mediana de 43 ms frente a 173. Con 3.183 memorias no pierde calidad. **Duplicados:** desde la 0.4.0, al guardar otra vez una nota se señala o se lista entre las relacionadas el 100 % de las veces, como engram, pero con un 14 % de avisos de más frente a su 49 % (la tabla de 90 % de abajo es de la 0.3.1). Lo que sigue es la historia completa desde la primera comparación, con 127 notas. El recall de la versión actual (algo inyectado en el 7 % de 300 mensajes reales) está en el [README](README.es.md#el-ruido-lo-viejo-y-lo-erróneo).

## Método

- **Datos.** 127 notas de memoria reales del trabajo diario de un desarrollador (apps Android, impresión 3D, servidores, escritura), casi todas en español. Son privadas y no se publican. Las mismas notas en los dos sistemas, con el contenido cortado a 6.000 caracteres en ambos (el CLI de engram solo acepta el contenido como argumento, y Windows limita la línea de comandos a 32 K).
- **Preguntas.** Los 126 resúmenes de una línea de las notas (el índice del propio desarrollador) y **80 preguntas escritas a ciegas** por otro agente que solo vio las notas, nunca ninguno de los dos sistemas: 20 cortas tipo palabras clave, 25 naturales, 15 paráfrasis que evitan las palabras de la nota y 20 sin respuesta en las notas.
- **Uso justo de engram.** Su comprobación de actualizaciones (una llamada a GitHub en cada comando) se apagó con su propia `ENGRAM_NO_UPDATE_CHECK=1`. La búsqueda se midió de tres formas: la de por defecto (todas las palabras), `--match any`, y `--match any` con la consulta limpia (sin palabras vacías ni de menos de 4 letras), que es como la escribiría un agente cuidadoso.
- **Tiempos.** Un proceso nuevo por llamada en los dos (así los llaman los agentes y los hooks), intercalados A/B para que ninguno vaya siempre primero. Medianas.
- **De punta a punta.** 20 de las preguntas a ciegas respondidas por Claude con un único servidor de memoria (el MCP de cada sistema, la misma instrucción, sin más herramientas, sin plugins ni memoria automática). Otro Claude corrigió cada respuesta contra la nota **sin saber qué sistema la había dado**, en orden barajado.

## Resultados

### Búsqueda (CLI, 5 primeros)

| Preguntas | Omoikane | engram por defecto | engram `--match any` | engram any, palabras clave |
|---|---|---|---|---|
| 126 resúmenes: top-1 / top-3 | **98,4 % / 100 %** | 4,0 % / 4,8 % (95 % sin resultado) | 3,2 % / 6,3 % | **99,2 % / 100 %** |
| 20 cortas: top-1 / top-3 | **90 % / 95 %** | 55 % / 65 % | 30 % / 35 % | 80 % / 85 % |
| 25 naturales: top-1 / top-3 | **88 % / 96 %** | 0 % (sin resultado) | 0 % / 4 % | **88 %** / 92 % |
| 15 paráfrasis: top-1 / top-3 | 80 % / 93 % | 0 % (sin resultado) | 0 % / 7 % | **87 % / 93 %** |
| 20 sin respuesta: devuelve algo | 100 % | 0 % | 100 % | 100 % |

engram ordena bien cuando la consulta es una lista de palabras clave; con la pregunta tal como la escribe una persona, por defecto no encuentra nada y `--match any` cambia de modo (según su documentación, los términos cortos lo pasan a una búsqueda LIKE de lo más nuevo a lo más viejo). Omoikane acepta la pregunta tal cual. Ninguna de las dos búsquedas se niega a responder: cuando nada encaja, las dos devuelven lo más parecido (engram por defecto no devuelve nada en la mayoría de consultas, tengan respuesta o no).

### Velocidad (proceso nuevo por llamada)

| | Omoikane | engram |
|---|---|---|
| Buscar, mediana | **34 ms** | 120 ms (por defecto) · 155 ms (`any`) |
| Guardar, mediana | **39 ms** | 135 ms |

### Duplicados: 126 notas guardadas otra vez como lo haría un agente (otro título, el principio del texto) — Omoikane 0.3.1, superado en la 0.4.0 (más abajo)

| | Omoikane | engram CLI | engram MCP (`mem_save`) |
|---|---|---|---|
| Señala la original | 90 % (114/126) | 0 % | **100 %** entre sus candidatas (la primera en el 97 %) |
| Señala algo con 90 frases que no son duplicadas | **1 %** (1/90) | 0 % | 49 % (44/90) |
| Qué hace | No guarda; el agente actualiza o fuerza | Guarda | Guarda y pide al agente que juzgue cada candidata (`mem_judge`) |

Dos diseños: engram siempre guarda y le pasa al agente candidatas por BM25 para que las juzgue, así que no se le escapa ninguna duplicada a cambio de preguntar en la mitad de los casos en que no la hay; Omoikane decide él (vector más cobertura de palabras) y casi nunca pregunta en vano, pero se le escapa 1 de cada 10.

### Recall automático (memorias inyectadas en cada mensaje)

Solo Omoikane: los hooks de engram recuerdan al agente que guarde y cargan el contexto reciente, pero no inyectan las memorias que tienen que ver con cada mensaje. Con las 80 preguntas a ciegas: la nota buena en 15/20 cortas, 18/25 naturales y 3/15 paráfrasis; algo inyectado en 2 de las 20 sin respuesta. Con 300 mensajes reales del mismo desarrollador, el 11 % recibió alguna inyección.

### De punta a punta: Claude respondiendo con cada sistema (20 preguntas, corrección a ciegas)

| | Omoikane | engram |
|---|---|---|
| Correctas | 20/20 | 19/20 |
| Tiempo por respuesta, mediana | 11,7 s | 12,6 s |
| Turnos, mediana | 4 | 4,5 |
| Coste total (mismo modelo) | **1,40 $** | 1,94 $ |

Con un agente de por medio los dos funcionan bien: el agente convierte las preguntas en palabras clave para engram. Una pregunta de diferencia sobre 20 no es significativa. La diferencia de coste (+39 % en engram) cuadra con su mayor número de herramientas (27 por defecto) y sus respuestas más largas.

## 0.4.0: una segunda vuelta, con preguntas que no se usaron para ajustar

Las 80 preguntas a ciegas de arriba se usaron después para ajustar la 0.4.0. Para comprobarla, otro agente escribió **130 preguntas nuevas a ciegas** (25 cortas, 35 naturales, 25 paráfrasis, 15 en inglés sobre las notas en español y 30 sin respuesta, preguntando por detalles del medio y del final de las notas), que solo se pasaron al final. Otra IA revisó el código: encontró que los hooks nuevos de Codex y Gemini no se habrían ejecutado en Windows, y se arregló antes de publicar.

### Búsqueda con las preguntas nuevas (127 notas, 5 primeros)

| Preguntas | Omoikane | engram any, palabras clave |
|---|---|---|
| 25 cortas: top-1 | 96 % | **100 %** |
| 35 naturales: top-1 / top-3 | **86 % / 94 %** | 83 % / 89 % |
| 25 paráfrasis: top-1 / top-3 | **92 % / 96 %** | 84 % / 88 % |
| 15 en inglés sobre notas en español: top-1 / top-3 | **87 % / 100 %** | 67 % / 80 % |
| Tiempo, mediana | **34 ms** | 158 ms |

### Recall automático con las preguntas nuevas (Omoikane 0.3.1 frente a 0.4.0)

| | 0.3.1 | 0.4.0 |
|---|---|---|
| Nota buena inyectada, de 100 | 51 | **64** |
| …de ellas paráfrasis, de 25 | 5 | **14** |
| Algo inyectado, de 30 sin respuesta | **5** | 7 |

La 0.4.0 añade al recall un canal por significado (embeddings estáticos con un mínimo de palabras en común, solo en mensajes cortos) y solo cuenta los primeros 6.000 caracteres de cada memoria. Un tope de 3.000 parecía mejor con las preguntas de ajuste, pero aquí perdía 7 de 35 naturales: era sobreajuste y se descartó.

### Duplicados: el método de engram, sin su ruido

Cuando la comprobación propia de Omoikane no está segura, la 0.4.0 guarda la memoria y devuelve hasta 3 relacionadas (el método de engram, pasado a Rust: BM25 por las palabras del título y al menos dos palabras de título en común).

| | Omoikane 0.4.0 | engram MCP |
|---|---|---|
| Notas guardadas otra vez que se señalan o se listan | **100 %** | **100 %** |
| Relacionadas con 90 frases que no son duplicadas | **14 %** | 49 % |
| Relacionadas con 127 notas que no son duplicadas | **52 %** | 95 % |

### Escala e inglés: 3.183 memorias

Las 127 notas más 3.055 párrafos de READMEs técnicos en inglés, cargados en los dos con su propio importador; 50 preguntas en inglés escritas a ciegas sobre esos párrafos.

| | Omoikane | engram any, palabras clave |
|---|---|---|
| 80 preguntas de ajuste (español): top-1 cortas / naturales / paráfrasis | **90 / 88 / 80 %** | 85 / 80 / 80 % |
| 50 preguntas en inglés: top-1 cortas / naturales / paráfrasis | 87 / **95** / 47 % | 87 / 85 / 47 % |
| Buscar, mediana | **50 ms** | 203 ms |
| Guardar, mediana | **38 ms** | 149 ms |
| Recall por mensaje, mediana | 46 ms | no tiene |

Los dos flojean con paráfrasis en inglés (47 % de top-1): es el límite de buscar por palabras y de un modelo de embeddings estático.

### De punta a punta con las preguntas nuevas (30, corrección a ciegas)

| | Omoikane | engram |
|---|---|---|
| Correctas | 30/30 | 29/30 (un «no lo sé» que añadió datos de un cliente que no venían a cuento) |
| Tiempo, mediana | 12,4 s | 12,0 s |
| Coste total | **2,16 $** | 3,03 $ |

## 0.5.0: un tercer conjunto, sin tocar

En la 0.5.0 el recall inyecta como mucho 2 memorias, y solo las que llegan al 85 % de la mejor. Lo comprobó un **tercer conjunto de 100 preguntas escritas a ciegas** (30 cortas, 30 paráfrasis y 40 sin respuesta, una cuarta parte con el vocabulario de las notas), con otra IA juzgando, sin saber la versión, si cada memoria inyectada era contexto útil.

| Recall | 0.3.1 | 0.4.0 | 0.5.0 |
|---|---|---|---|
| Nota buena inyectada, de 60 | 32 | **37** | 36 |
| Memorias útiles inyectadas | 38 | **46** | 44 |
| Ruido inyectado | 23 | 28 | **21** |
| Precisión | 62 % | 62 % | **68 %** |

| Búsqueda con el tercer conjunto | Omoikane | engram any, palabras clave |
|---|---|---|
| 30 cortas: top-1 | 100 % | 100 % |
| 30 paráfrasis: top-1 / top-3 / top-5 | **80 %** / 83 % / 90 % | 60 % / **93 % / 97 %** |
| Tiempo, mediana | **35 ms** | 158 ms |

En paráfrasis Omoikane pone la nota buena la primera más veces, pero cuando falla, engram la tiene más a menudo entre las 3-5 primeras.

## Los tres conjuntos juntos: 310 preguntas

El de ajuste, el de comprobación y el tercero (310 preguntas, 0.5.1, las mismas condiciones para los dos: CLI, notas cortadas a 6.000 caracteres):

| Preguntas | Omoikane | engram any, palabras clave |
|---|---|---|
| 75 cortas: top-1 / top-5 / MRR | **96,0 % / 98,7 % / 0,97** | 94,7 % / 97,3 % / 0,96 |
| 15 en inglés sobre notas en español: top-1 / top-5 / MRR | **86,7 % / 100 % / 0,93** | 66,7 % / 86,7 % / 0,75 |
| 60 naturales: top-1 / top-3 / MRR | **86,7 % / 95,0 % / 0,91** | 85,0 % / 90,0 % / 0,89 |
| 70 paráfrasis: top-1 / MRR | **84,3 % / 0,88** | 74,3 % / 0,82 |
| 70 paráfrasis: top-3 / top-5 | 90,0 % / 94,3 % | **91,4 % / 95,7 %** (una pregunta en cada uno) |
| Tiempo, mediana | **37 ms** | 162 ms |

La 0.5.1 reserva el último de los 5 puestos para una memoria que el vector ve muy parecida (≥ 0,4) cuando las palabras la dejan más abajo: una paráfrasis más de 70 entra en los 5 primeros y no cambia nada más.

## 0.6.0: un cuarto conjunto, sin usar para nada

100 preguntas más escritas a ciegas (40 paráfrasis, 25 naturales, 20 cortas, 15 en inglés), pasadas una sola vez, después de todas las decisiones. Notas completas en los dos (cada uno con su importador), la misma consulta limpia de palabras clave para engram, 10 resultados como los reciben los agentes.

| 100 preguntas nuevas | Omoikane 0.6.0 | engram |
|---|---|---|
| top-1 | **79 %** | 76 % |
| top-3 | **94 %** | 86 % |
| top-5 | **95 %** | 88 % |
| top-10 | **97 %** | 89 % |
| MRR | **0,859** | 0,813 |
| 40 paráfrasis: top-1 / top-3 / top-10 | **72,5 % / 92,5 % / 92,5 %** | 67,5 % / 85 % / 87,5 % |
| 15 en inglés: top-3 / top-10 | **80 % / 100 %** | 53 % / 60 % |

La 0.6.0 también busca por una raíz sencilla (la palabra sin sus dos últimas letras, desde 6 letras), porque el español flexiona al final y el prefijo solo no cubría «reinicia» ↔ «reinicios». Con este conjunto suma 2 puntos en top-3, top-5 y top-10 y cuesta una paráfrasis en top-1.

## Un quinto conjunto: 80 paráfrasis, pasadas una vez

Las paráfrasis eran lo único en lo que engram seguía cerca (con las 310 preguntas de ajuste iba una pregunta por delante en top-5). Un quinto conjunto de **80 paráfrasis escritas a ciegas** (10 en inglés) se pasó una sola vez contra la 0.6.0 tal como está publicada, sin cambiar nada antes ni después.

| 80 paráfrasis nuevas | Omoikane 0.6.0 | engram |
|---|---|---|
| top-1 | **76,2 %** | 65,0 % |
| top-3 | **87,5 %** | 76,2 % |
| top-5 | **90,0 %** | 83,8 % |
| top-10 | **91,2 %** | 90,0 % |
| MRR | **0,817** | 0,721 |

Con las 190 paráfrasis de todos los conjuntos, en top-5: Omoikane 176 (92,6 %), engram 169 (88,9 %).

## 0.7.0: todos los conjuntos otra vez, ya publicada

La 0.7.0 cambia adónde van las memorias cuando las sesiones se abren en la carpeta del usuario, no cómo ordena la búsqueda. Para comprobarlo se pasaron la 0.7.0 publicada, la 0.6.0 y engram sobre las mismas 130 notas completas, rotando el orden en cada pregunta, con los seis conjuntos (526 preguntas con respuesta conocida).

| 526 preguntas | Omoikane 0.7.0 | Omoikane 0.6.0 | engram |
|---|---|---|---|
| top-1 | **87,3 %** | 87,3 % | 82,7 % |
| top-3 | **95,6 %** | 95,6 % | 91,4 % |
| top-5 | **96,8 %** | 96,8 % | 94,1 % |
| top-10 | **97,7 %** | 97,7 % | 96,0 % |
| MRR | **0,915** | 0,915 | 0,875 |
| 190 paráfrasis, MRR | **0,838** | 0,838 | 0,769 |
| búsqueda, mediana | **45 ms** | 45 ms | 178 ms |

La 0.9.0, que añade la sincronización entre equipos, se volvió a pasar igual después de publicarla: las mismas cifras en las 526 preguntas (top-1 87,3 % frente a 82,7 %, MRR 0,915 frente a 0,875) y una búsqueda mediana de 43 ms frente a 173 ms.

La 0.7.0 y la 0.6.0 dan la misma respuesta a todas las preguntas. engram solo va por delante en los resúmenes (top-1 98,4 % frente a 97,6 %, una pregunta) y en el top-3 del tercer conjunto (96,7 % frente a 93,3 %, dos preguntas).

## Límites

- Las notas de un solo desarrollador, casi todas en español: 127 al principio y 130 en las últimas pasadas, más los 3.055 párrafos en inglés de la prueba de escala. Con otros corpus (inglés, miles de memorias, mucho código) puede cambiar.
- 20 preguntas de punta a punta es una muestra pequeña; la búsqueda y los duplicados usan más.
- engram hace mucho más de lo que se mide aquí: TUI, API HTTP, sincronización y nube, integraciones con 14 agentes, revisión de conflictos. Esto compara la calidad y la velocidad de la memoria, no las funciones.
- Los umbrales de Omoikane se recalibraron ese mismo día con estas notas: los duplicados con las notas, y el del recall con las preguntas a ciegas (comprobado también con los 300 mensajes reales), así que esas dos filas de Omoikane son optimistas. La búsqueda, de la que salen las tablas de búsqueda y de punta a punta, no cambió.
