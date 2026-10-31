# Omoikane · plan de desarrollo

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente de IA.

Documento de trabajo. Lo que está aquí es lo decidido; lo que no está, no existe todavía.

## 1. Qué es

Memoria persistente y local para agentes de programación (Claude Code primero; cualquier cliente MCP después). Un binario `omoi`, un fichero SQLite. El agente guarda lo que aprende; Omoikane se lo devuelve cuando vuelve a hacer falta, **sin que el agente tenga que acordarse de buscar**.

Tres ideas que lo definen:

1. **Búsqueda híbrida de serie**: palabras (FTS5) + significado (vectores) fusionados. Funciona en español.
2. **Recall automático**: un hook del agente llama a `omoi recall` en cada mensaje; es un subcomando compilado, sin bash ni curl, y responde en milisegundos.
3. **Pequeño a propósito**: ≤ 8 herramientas MCP, sin servidor residente, sin puerto, sin nube.

No es una adaptación de ningún otro sistema de memoria. El modelo de datos es propio.

## 2. Decisiones cerradas

| Tema | Decisión |
|---|---|
| Nombre | Omoikane; binario y crate `omoikane`, comando `omoi` |
| Lenguaje | Rust 2024, MSRV 1.88 |
| Licencia | MIT |
| Almacenamiento | SQLite (rusqlite `bundled`) + FTS5 + sqlite-vec, un fichero en `~/.omoikane/omoikane.db` |
| Concurrencia | una conexión por proceso, WAL, `BEGIN IMMEDIATE`, `busy_timeout` 5 s. Sin demonio |
| Embeddings | model2vec (estáticos, Rust puro, sin ONNX). Modelo por defecto multilingüe. **Cero dependencias externas es un principio**: descartado EmbeddingGemma 2 / ONNX Runtime (el de Troba) aunque tenga mejor calidad (27-oct-2026) |
| MCP | `rmcp` (SDK oficial), transporte stdio |
| CLI | `clap` |
| Ámbitos | `project` (raíz git canónica) o `global`. Nada más |
| Historial | sí, tabla `memory_version`, con `reason` opcional |
| Importación/conversión | **aparcado**. Más adelante, aparte del núcleo: convertidor desde otros sistemas y JSON/JSONL/Markdown |
| Fuera de v1 | nube, sincronización, TUI, export a Obsidian, relaciones entre memorias, caducidades por tipo |
| Idioma del código | identificadores y comentarios en español (como Troba); README en inglés y español |
| Idioma de la CLI | ayuda, mensajes y errores en **inglés por defecto**, en español con `OMOIKANE_LANG=es` (decidido 27-oct-2026). Los comandos son siempre en inglés |
| Proyecto | raíz del repo git que contiene la carpeta (`.git` carpeta o fichero, para worktrees); si no hay git, la propia carpeta. Ruta canónica (en Windows sin `\\?\` y con la unidad en mayúscula) |

## 3. Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

```
memory
├─ id            ULID
├─ project       raíz git canónica (ruta absoluta normalizada); '' = global
├─ kind          fact | decision | gotcha | preference
├─ title         una línea
├─ content       markdown corto: qué, por qué, dónde
├─ paths         JSON array de rutas relativas a la raíz del proyecto (opcional)
├─ tags          JSON array de etiquetas libres (opcional)
├─ pinned        0/1: se inyecta siempre en su proyecto
├─ source        agente + sesión que lo guardó ("claude-code:<session_id>")
├─ created_at    ISO-8601 UTC
├─ updated_at    ISO-8601 UTC
├─ recalled_at   última vez que el recall o una búsqueda lo devolvió (lo mantiene Omoikane)
└─ recall_count  veces devuelto (lo mantiene Omoikane)
```

**Lo que decide el agente al guardar**: `title`, `content`, `kind`; opcionalmente `paths`, `tags`. Todo lo demás lo pone Omoikane.

**Qué significa cada `kind`** (cada uno cambia cómo se recupera; un tipo que no cambia el comportamiento no existe):

| kind | Qué es | Cómo entra en el recall |
|---|---|---|
| `preference` | cómo quiere trabajar el usuario | siempre, en su ámbito |
| `gotcha` | trampa, bug no obvio, «no hagas X porque Y» | cuando el prompt o los `paths` coinciden |
| `decision` | elección con sus motivos | por relevancia |
| `fact` | cómo es algo del proyecto | por relevancia |

`pinned` fuerza la inyección sea cual sea el `kind`.

**Historial**: cada `update` guarda antes la foto anterior.

```
memory_version
├─ memory_id, version (1, 2, 3…)
├─ title, content, kind, paths, tags   (la versión que se reemplaza)
├─ changed_at
└─ reason        opcional: por qué cambia
```

Solo se lee con `history`. Búsqueda y recall miran la versión viva. El número de versiones **no** es una señal de ranking.

**Duplicados**: al guardar, si existe una memoria del mismo ámbito (el proyecto y las globales; si la nueva es global, solo las globales) con similitud **≥ 0,70 entre vectores centrados** (§9), no se guarda: se devuelve la candidata y se propone `update` (en la CLI, código de salida 2; `--force` guarda igualmente). En el MCP de la fase 3, lo mismo: se devuelve la parecida y **el agente decide** entre `update` o forzar (decidido por Marc el 28-oct-2026). Historia del umbral: el 0,92 del primer borrador era un error; 0,65 sin centrar salía bien con 40 pares de frases cortas pero, con 127 notas reales, el 31 % de los pares de notas distintas lo pasaba. Centrando, 1 de 8.001 pares pasa de 0,70.

## 4. Esquema SQL

El que crea `src/db.rs` (esquema 1, `PRAGMA user_version`). La conexión usa WAL, `synchronous=NORMAL`, `foreign_keys=ON` y `busy_timeout` de 5 s; toda escritura va en una transacción `IMMEDIATE`.

```sql
CREATE TABLE memory (
  n            INTEGER PRIMARY KEY,          -- enlace estable con FTS (y con vec en la fase 2)
  id           TEXT NOT NULL UNIQUE,         -- ULID: lo que ven el agente y la persona
  project      TEXT NOT NULL DEFAULT '',     -- '' = global
  kind         TEXT NOT NULL CHECK (kind IN ('fact','decision','gotcha','preference')),
  title        TEXT NOT NULL,
  content      TEXT NOT NULL,
  paths        TEXT NOT NULL DEFAULT '[]',   -- JSON
  tags         TEXT NOT NULL DEFAULT '[]',   -- JSON
  pinned       INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,                -- ISO-8601 UTC con milisegundos
  updated_at   TEXT NOT NULL,
  recalled_at  TEXT,
  recall_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX memory_project ON memory(project, kind, updated_at);
CREATE INDEX memory_pinned  ON memory(project) WHERE pinned = 1;

CREATE TABLE memory_version (
  memory_id  TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  version    INTEGER NOT NULL,
  title TEXT NOT NULL, content TEXT NOT NULL, kind TEXT NOT NULL, paths TEXT NOT NULL, tags TEXT NOT NULL,
  changed_at TEXT NOT NULL,
  reason     TEXT,
  PRIMARY KEY (memory_id, version)
);

CREATE TABLE memory_path (
  memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  path      TEXT NOT NULL,
  PRIMARY KEY (memory_id, path)
);
CREATE INDEX memory_path_path ON memory_path(path);

CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags,
  content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
```

**Esquema 2** (fase 2, migración automática):

```sql
-- FTS5 no deja añadir columnas: se rehace con las rutas y se rellena desde memory.
CREATE VIRTUAL TABLE memory_fts USING fts5(title, content, tags, paths, content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2');
CREATE TABLE memory_vec (
  n INTEGER PRIMARY KEY REFERENCES memory(n) ON DELETE CASCADE,
  e BLOB NOT NULL                              -- 256 f32 little-endian, normalizado
);
-- meta('modelo') = carpeta del modelo con el que se calcularon los vectores
```

Se descartó `vec0`: no sabe filtrar por proyecto dentro del KNN, así que había que pedir de más y filtrar después. Un BLOB con `vec_distance_cosine` de sqlite-vec filtra en el mismo SQL, dio los mismos resultados en 30 de 30 consultas y fue el doble de rápido (A/B intercalado: 3,3 frente a 7,1 ms con 5.000 memorias; 15 frente a 26 ms con 20.000).

Notas:
- **Cambio respecto al primer borrador** (fase 1): en vez de una tabla `memory_rowid` aparte, `memory` lleva su propio `n INTEGER PRIMARY KEY`. Es una tabla menos, y un alias de rowid no cambia con `VACUUM` (el rowid implícito de una tabla con clave `TEXT` sí puede cambiar).
- `memory_fts` no guarda texto (`content=''`): el texto ya está en `memory`. `contentless_delete=1` (SQLite ≥ 3.43) permite borrar por rowid sin pasar los valores viejos.
- `remove_diacritics 2` hace que «autenticación» y «autenticacion» sean la misma palabra. Sin stemming: en la fase 2 el vector cubre las variantes morfológicas.
- Fijar o desfijar no crea versión ni cambia `updated_at` (que da la recencia).
- Borrar es borrar: `memory_version` y `memory_path` caen por `ON DELETE CASCADE` y el FTS se borra en la misma transacción.

## 5. Búsqueda y ranking

```
consulta        = palabras de lo escrito, sin palabras vacías (castellano, inglés, catalán),
                  cada una entre comillas; desde 3 letras, exacta OR prefijo ("token" OR "token"*)
candidatos_fts  = top 40 por bm25(memory_fts, 3.0, 1.0, 2.0)   -- título pesa 3, etiquetas 2
relevancia      = -bm25 + cobertura                            -- cobertura: parte de las palabras presentes (0-1)
candidatos_vec  = top 40 por coseno en memory_vec, del mismo ámbito y kind
fusion          = RRF con k = 60:  score = Σ 1 / (k + rango_i)
                  lo que solo trae el vector entra si coseno ≥ 0,28; lo que sale en las dos listas, siempre
boosts          = ×1.5 si pinned · ×(1 + 0.3·recencia) con recencia = 1/(1 + días/90)
resultado       = top N tras filtrar por proyecto (+ globales) / kind / etiquetas
```

- **Exacta y prefijo a la vez**: «memoria» encuentra «memorias», pero quien dice «memoria» suma las dos y queda delante. Sin esto, buscar «marca20» sacaba antes «marca200» (lo destapó el test de coherencia).
- **Cobertura**: con pocas memorias BM25 no distingue (si una palabra está en la mitad de los documentos, su IDF es 0 y todo puntúa casi 0). La parte de las palabras buscadas que aparece en la memoria desempata; con muchas memorias pesa poco al lado de BM25.
- Ninguna sintaxis FTS5 de lo que escribe la persona llega al índice: todo va entre comillas.
- **Fusión: rescate, no RRF** (ajuste del 28-oct-2026). El orden por palabras manda; el vector solo añade, detrás, lo que las palabras no encontraron y se parece mucho (coseno centrado ≥ 0,25). Lo que sale por las dos listas lleva su similitud en `--json`.
- **Calibración**, en dos conjuntos:
  - `tests/busquedas.tsv` (público, escrito a mano: 18 memorias, 26 consultas con respuesta, 5 sin ella);
  - 127 notas reales de memoria de Marc, **solo en local** (no se suben): cada resumen de `MEMORY.md` como consulta y su nota como respuesta (`examples/fase2.rs real <carpeta>`).

  | configuración | sintético: 1.º acierto | sintético: sin respuesta con algo | real: 1.º acierto |
  |---|---|---|---|
  | solo palabras | 85 % | 0 de 5 | **98 %** |
  | RRF, vectores sin centrar, ≥ 0,28 (la primera versión) | 88 % | 1 de 5 | 87 % |
  | RRF peso 1, centrados | 85 % | 0-4 de 5 | 84 % |
  | RRF peso 0,5, centrados | 85 % | 0-4 de 5 | 90 % |
  | **rescate, centrados, ≥ 0,25 (la elegida)** | **85 %** | **0 de 5** | **98 %** |

- **Conclusión honesta: con model2vec el vector no mejora la búsqueda de forma medible.** Con las notas reales, cualquier fusión que deje opinar al vector sobre el orden lo empeora; el rescate al menos no empeora nada. La semántica de un modelo estático es débil: «fallo de login» frente a «Bug en auth: el token caduca…» da 0,139 sin centrar y −0,017 centrando. Donde el vector sí sirve es en los **duplicados** (§3). Marc aceptó este alcance el 28-oct-2026: es lo mejor que hay sin dependencias externas.
- El recall automático de la fase 4 tendrá que calibrarse otra vez (las similitudes centradas están en otra escala) y ser más estricto que la búsqueda: ahí el ruido ensucia el contexto del agente.
- Sin modelo instalado, la búsqueda es FTS sola. Nunca falla por falta de modelo.
- Cada resultado devuelto por `search` o `recall` actualiza `recalled_at` y `recall_count`.

## 6. Recall automático

Es la pieza que justifica el proyecto. Va en el **plugin** de Claude Code (el propio repo: `.claude-plugin/plugin.json`, que declara el servidor MCP y los dos hooks); los dos hooks llaman al binario directamente (`omoi recall`), sin scripts:

| Hook | Comando | Qué inyecta |
|---|---|---|
| `SessionStart` (`startup`, `resume`, `clear`, `compact`) | `omoi recall --session-start` | cuántas memorias hay y cómo usarlas, más las `preference` y las fijadas del proyecto y globales |
| `UserPromptSubmit` | `omoi recall` (lee el JSON del hook por stdin) | hasta 3 memorias que de verdad tengan que ver con el mensaje, más las que tengan una ruta nombrada en él; o **nada** |

Formato: JSON con `hookSpecificOutput.additionalContext`, dentro de `<omoikane>…</omoikane>`, una línea por memoria (tipo, rutas, título, 200 caracteres e id). El proyecto sale del `cwd` que manda Claude Code. Un fallo nunca molesta: se escribe en stderr y no se inyecta nada (código 0).

**Cuándo inyectar en cada mensaje** (calibrado el 28-oct-2026 con 127 notas reales y 300 mensajes reales de Marc, en local, `examples/fase4.rs`). Inyectar de más ensucia el contexto del agente; el criterio:
1. el mensaje tiene al menos **3 palabras útiles** (≥ 3 letras, no solo cifras, sin palabras vacías ni muletillas de charla como *pues*, *dale*, *vale*, *mira*, *sabes*, *estoy pensando*: una coletilla de relleno diluía la cobertura y en la prueba con Claude Code ocultó la memoria que sí era del tema);
2. la memoria comparte al menos **2 palabras raras** (presentes en ≤ 20 % de las memorias del ámbito);
3. esas palabras pesan, por IDF, al menos **el 40 %** de lo que pesa el mensaje. IDF de Lucene, siempre positivo (con la clásica, una base de 1-2 memorias no inyectaba nunca), y una palabra que no está en ninguna memoria pesa como si estuviera en una (con todo su IDF, en una base pequeña hundía la memoria que sí era del tema; sin contarla, los mensajes largos traían algo el 53 % de las veces);
4. solo las que se acercan (≥ 70 %) a la mejor, como mucho 3; empate → la que tiene más palabras del mensaje en el título;
5. prefijo solo desde 5 letras («pon» coincidía con «pone»).

| criterio | positivos (resúmenes de `MEMORY.md`) con su nota | mensajes reales con algo inyectado |
|---|---|---|
| cualquier palabra en común (top 3) | 100 % | 97 % (2,9 memorias por mensaje) |
| ≥ 3 palabras en común | 100 % | 66 % |
| cobertura IDF ≥ 0,35 | 100 % | 24 % |
| **el criterio completo** | **99 %** (1,5 por mensaje) | **20 %** (0,29 por mensaje) |

En la muestra revisada a mano casi todo lo inyectado es pertinente (la app del gimnasio → su nota; «PETG» y «riser» → la K2; «carteles KISS» → posters); queda algo de ruido en mensajes de charla con alguna palabra rara. El vector no se usa en el recall: no mejoró la búsqueda de forma medible (§5) y así el hook ni abre el modelo.

Cada memoria se inyecta **una vez por sesión** (tabla `recall_log`, esquema 3); `SessionStart` (también tras compactar) vacía la de su sesión, porque ese contexto se ha perdido. Lo que se inyecta cuenta como recuperado.

**Latencia** (medida en la fase 4, proceso entero como lo lanza Claude Code, base de 127 notas reales, Windows): `SessionStart` 15 ms; mensaje que inyecta algo 26 ms; mensaje que no inyecta nada 15 ms. El presupuesto era 50 ms. (En la fase 0.5 se resolvió que el modelo abre en 2,7 ms, pero al final el recall no lo usa.)

Todos los procesos (hook, CLI, servidor MCP) cargan el modelo igual de rápido, así que no hay vectores pendientes ni carga en segundo plano: cada memoria se guarda con su vector en la misma transacción. La tabla está mapeada y compartida por el sistema operativo entre procesos: medido, 4 procesos a la vez ocupan **13 MB privados cada uno** (el resto del working set, ~68 MB, son páginas del modelo compartidas). model2vec-rs, para comparar: 1.014 MB privados por proceso.

**Caché fría** (primer mensaje tras reiniciar). En el PC (NVMe, Windows): abrir 4,7 ms, primer vector ~15 ms. En un servidor Linux casero (SSD SATA barato): abrir ~6-10 ms, primer vector **~45 ms**, así que el primer mensaje del día en un disco así se pasa algo del presupuesto (≈ 55-60 ms en total). Pasa una vez por arranque del equipo; después todo es caché. `madvise(RANDOM)` en la tabla y los pesos ayuda poco pero ayuda (A/B intercalado, 6 pasadas: primer vector ~50 → ~44 ms, los 10 siguientes ~170 → ~130 ms); precargar el trie (`WILLNEED`) no ayudó. Una medida anterior que daba ~21 ms estaba sesgada por el orden de las pruebas (la caché interna del SSD): por eso las comparaciones se hacen siempre intercaladas.

**Compactación**: `SessionStart` con `matcher: compact` vuelve a inyectar las `preference`/`pinned`, que es lo que se pierde al compactar.

Para otros agentes (Codex, Gemini CLI, OpenCode…) v1 ofrece solo el MCP; sus hooks se añaden cuando alguien los pida.

## 7. Herramientas MCP (8)

Nombres cortos, sin prefijo: el cliente ya lo pone (`mcp__omoikane__remember`).

| Tool | Parámetros | Devuelve |
|---|---|---|
| `remember` | title, content, kind, paths?, tags?, pinned? | id, o `duplicate_of` + candidata si hay una muy parecida |
| `search` | query, kind?, tags?, limit? (10), all_projects? | lista compacta: id, kind, title, 200 letras, paths |
| `get` | id | memoria completa |
| `update` | id, title?, content?, kind?, paths?, tags?, pinned?, reason? | nueva versión |
| `forget` | id | borrado (duro; el historial se va con ella) |
| `history` | id | versiones |
| `context` | (nada) | lo mismo que el recall de SessionStart, por si el agente lo pide |
| `recent` | limit?, kind? | últimas guardadas en el proyecto |

El servidor MCP resuelve el proyecto por el `cwd` del proceso en cada llamada. Instrucciones del servidor (`instructions`): cuándo guardar y qué `kind` usar, en 6 líneas.

## 8. CLI

```
omoi setup claude          instala MCP + hooks en ~/.claude (pide confirmación, muestra el diff)
omoi setup codex|gemini    solo MCP
omoi remember|search|get|update|forget|history|recent   espejo de las tools
omoi recall [--session-start]                           lo usan los hooks
omoi mcp                   servidor stdio
omoi projects              proyectos con memorias y cuántas
omoi reindex               recalcula FTS y vectores (cambio de modelo)
omoi model [nombre]        descarga/cambia el modelo de embeddings
omoi doctor                versiones, ruta de datos, estado del modelo, latencia del recall
omoi export                JSONL de todo (lo que haga falta para no quedarse atrapado)
```

`setup` toca ficheros del usuario: enseña lo que va a escribir y espera un sí.

## 9. Embeddings

- Crate `model2vec-rs` (MIT). Modelos estáticos: tokenizar + sumar vectores. Sin ONNX, sin GPU, microsegundos por frase.
- Modelo por defecto: **potion-multilingual-128M** cuantizado a int8 (256 dims, 101 idiomas). Oficial f32: 512 MB de pesos + 18,6 MB de tokenizer. Se descarga una vez a `~/.omoikane/models/`.
  - **Cuantización propia** (decidido en la fase 0). La comunitaria (`alikia2x/…-int8`) no sirve: solo trae int8 en ONNX, su safetensors sigue en f32. La nuestra: int8 con una escala por fila, plegada en el tensor `weights` para que model2vec-rs la aplique sin cambios. **130,1 MB**, fidelidad frente a f32: coseno mínimo **0,99996**.
  - Por qué int8 y no f32 (confirmado 27-oct-2026): cada sesión del agente arranca su propio `omoi mcp` con su copia del modelo. 3 sesiones = ~1,6 GB en f32 frente a ~450 MB en int8, con la misma calidad y velocidad.
  - Publicada el 27-oct-2026 en [natzx94/potion-multilingual-128M-int8](https://huggingface.co/natzx94/potion-multilingual-128M-int8) (la cuenta de Hugging Face de Marc).
- **Carga en 2,7 ms (fase 0.5)**. `tokenizers` gastaba 1,3 s de los 1,78 en construir el vocabulario Unigram (500.353 piezas) en cada arranque. `src/embeber.rs` hace ese trabajo una vez (`compilar`, 2,7 s) y lo deja en ficheros planos que luego solo se mapean (`memmap2`): trie de doble array (18,5 MB), puntuaciones (4 MB), tabla int8 (128 MB), pesos (2 MB), y el `tokenizer.json` **sin vocabulario** (0,3 MB, carga en 3 ms) para normalizar y pre-tokenizar. El troceado (Viterbi) y el trie reproducen los de model2vec-rs (MIT, The Minish Lab).
  - Equivalencia comprobada (`examples/fase05.rs`): 165.282 frases, 3,6 M de tokens, **0 ids distintos** frente a `tokenizers`; coseno mínimo **0,999998** frente a model2vec-rs. Vector medio: 0,027 ms.
  - model2vec-rs pasa a ser solo dependencia de desarrollo (para el test de equivalencia).
  - Lo que se distribuye es ya el formato compilado (≈ 153 MB): el usuario no compila nada.
- **Versiones del modelo en disco.** Windows no deja sobrescribir ni borrar un fichero mapeado mientras algún proceso lo usa, y siempre puede haber una sesión abierta. Por eso nunca se reemplaza en sitio:
  - cada modelo vive en su carpeta inmutable `~/.omoikane/models/<nombre>-<hash8>/`, con el hash del contenido;
  - `~/.omoikane/models/actual` es un fichero de texto con el nombre de la carpeta en uso; se cambia escribiendo `actual.tmp` y renombrándolo (atómico);
  - cada proceso lee `actual` al arrancar, así que las sesiones abiertas siguen con su versión y las nuevas cogen la nueva;
  - `meta.json` guarda el nombre del modelo, y la base guarda en `meta` con qué modelo se calcularon sus vectores: si no coincide con `actual`, `omoi reindex` (o el arranque del servidor MCP) recalcula los vectores;
  - las carpetas viejas se borran con `omoi model --limpiar`; si alguna sigue mapeada, el borrado falla sin ruido y se reintenta otro día.
- **Instalación** (fase 2): `omoi model install` descarga el int8 (150 MB, 8 s en el PC; `ureq` con rustls, Rust puro; Marc decidió el 28-oct mantenerlo frente a usar el `curl` del sistema) y lo compila en local (unos segundos). **Dos fuentes**, en orden: Hugging Face y, si falla, la Release [`modelo-potion-int8-v1`](https://github.com/marcmayol/omoikane/releases/tag/modelo-potion-int8-v1) de este repo, con los mismos ficheros (las dos dan la huella `7af456ef`). `OMOIKANE_MODEL_SOURCE` las sustituye por una o varias separadas por comas (espejos propios, pruebas). Se distribuye el int8 y no el formato compilado: así el formato puede cambiar entre versiones de omoi sin volver a publicar nada. `--from <carpeta>` instala sin red (lo usan la CI y los tests). La carpeta del modelo se llama por la huella de su contenido (`potion-multilingual-128M-int8-7af456ef`): la descarga y el int8 local dieron la misma.
- `omoi model` dice cuál está en uso; `omoi model clean` borra los demás; `omoi reindex` rehace palabras y vectores.
- Los vectores se mantienen solos: cada orden que los usa (`search`, `remember`, `update`) mira si faltan o si son de otro modelo y los calcula (≈ 0,03 ms por memoria). Guardar sin modelo no rompe nada: el vector llega la próxima vez.
- Alternativa ligera para quien solo escriba en inglés: `potion-base-8M` (30 MB). Sin hacer: no hay demanda todavía.
- Vector = `title + "\n" + content`, normalizado (coseno).
- **Centrado** (ajuste del 28-oct-2026): para comparar se resta la media de los vectores de la base, mezclada con una media genérica (la del corpus de los tests, ~50 ms una vez por modelo, guardada en `meta`) con peso de 20 memorias, para que con pocas memorias no salga cero. Los modelos estáticos comparten una dirección común que hace que todo se parezca: con 127 notas reales, sin centrar, 3.134 de 8.001 pares de notas distintas pasaban de 0,65; centrando, 4. Con una media genérica fija en vez de la de la base, 259: la de la base captura el estilo de las notas de cada uno, por eso se calcula al comparar.
- La similitud centrada se calcula en Rust sin copiar los vectores (cos(a−μ, q) con dos productos escalares). Con 2.000 memorias, `search` tarda ~38 ms por proceso y `remember` con control de duplicados ~30 ms; con 100, menos de 25.
- Si no hay modelo, todo funciona sin vectores. `doctor` lo dice.

## 10. Estructura del repo (plana)

```
Omoikane/
├─ Cargo.toml
├─ LICENSE
├─ README.md          inglés
├─ README.es.md       español
├─ PLAN.md            este documento
├─ publicar_release.py  (fase 5, como en Troba)
└─ src/
   ├─ main.rs         CLI (clap) y arranque
   ├─ db.rs           conexión, PRAGMA, esquema, migraciones
   ├─ memoria.rs      tipos, guardar, actualizar, historial, borrar
   ├─ buscar.rs       FTS + vec + RRF + boosts
   ├─ lib.rs          módulos (para que los examples y tests los usen)
   ├─ embeber.rs      modelo compilado y mapeado: compilar, abrir, ids, vector
   ├─ proyecto.rs     raíz git canónica desde un cwd
   ├─ recall.rs       selección e inyección (SessionStart / UserPromptSubmit)
   ├─ hook.rs         leer el JSON de Claude Code por stdin, escribir la respuesta
   ├─ mcp.rs          servidor stdio con rmcp
   └─ setup.rs        instalar en ~/.claude y otros agentes
```

## 11. Fases

### Fase 0 · medir antes de decidir ✅ (27-oct-2026)
- [x] Esqueleto que compila con rusqlite + sqlite-vec + model2vec-rs + rmcp (`omoi doctor`).
- [x] Medir carga del modelo int8 en frío y en caliente, y encode de una frase.
- [x] Medir `vec0` con 5.000 / 20.000 / 50.000 vectores de 256 dims.
- [x] Decidir: el hook **no** carga el modelo (§6). Cuantización **propia** (§9).

Medidas en el PC de desarrollo (Windows 11, release), reproducibles con `cargo run --release --example fase0 -- cuantizar | cargar DIR | buscar N`:

| Qué | Resultado |
|---|---|
| carga modelo f32 / int8 propio | 1.896 ms / 1.650-1.805 ms (igual en caliente: es CPU, el tokenizer) |
| encode de una frase | 0,037 ms |
| fidelidad int8 vs f32 | coseno mínimo 0,99996 |
| abrir base + 1 consulta FTS | 1,7 ms |
| FTS5 bm25 top 40 | 0,68 ms (5k) · 4,7 ms (50k) |
| KNN vec0 k=40, fuerza bruta | 5,8 ms (5k) · 24 ms (20k) · 53 ms (50k) |
| tamaño base | 10 MB (5k) · 100 MB (50k) |

### Fase 0.5 · cargar el modelo en milisegundos ✅ (27-oct-2026)
- [x] Perfil de la carga: 1.321 ms de 1.780 en `Tokenizer::from_bytes` (construir el vocabulario); el mismo tokenizer sin vocabulario carga en 3 ms.
- [x] Formato compilado y mapeado (`src/embeber.rs`): **abrir 2,7 ms**, primer vector 0,4 ms.
- [x] Equivalencia: 0 ids distintos en 165.282 frases; coseno ≥ 0,999998.
- [x] Decisión: el hook sí hace búsqueda semántica (§6).
- [x] Caché fría: Windows/NVMe primer vector ~15 ms; Linux/SSD SATA ~45 ms, una vez por arranque (§6).
- [x] Linux (servidor casero, Ubuntu 24.04): compila sin avisos, `doctor` y `cargo test` pasan, int8 bajado de Hugging Face; RAM: PSS ~23 MB y ~2 MB privados por proceso con 4 a la vez.
- [x] Guardia de compilación para plataformas big-endian.
- [ ] macOS: sin máquina; en la CI de la fase 5.
- [x] RAM: 13 MB privados por proceso con el modelo compartido (§6).
- [x] Test de equivalencia en `cargo test` (`tests/equivalencia.rs` + `tests/corpus.txt`, 1.840 frases públicas). Se salta si no está el modelo. Comprobado que falla: cambiar un `>` por `>=` en el desempate del Viterbi rompe 1 frase.
- [x] Diseño de versiones del modelo en disco para Windows (§9).

Similitud semántica de muestra (int8), con «fallo de login en la pantalla de acceso»: «login failure on the sign-in screen» **0,558** · «bug en auth: el token caduca antes de tiempo» **0,245** · «el middleware de autenticación…» 0,142 · temas ajenos entre −0,04 y 0,12. Ordena bien y cruza idiomas, pero con márgenes cortos en español: por eso va siempre fusionado con BM25 y nunca solo.

### Fase 1 · núcleo ✅ (27-oct-2026)
- [x] `db.rs` (esquema 1, WAL, transacciones IMMEDIATE), `memoria.rs`, `proyecto.rs`, `buscar.rs` (solo FTS), `idioma.rs`.
- [x] CLI: remember, search, get, update, forget, history, recent, projects, export, doctor; todas con `--json`; `--db` y `--dir` globales; contenido por argumento o por la entrada estándar; ids por su principio (la CLI enseña 12 caracteres).
- [x] Tests (28 en total): 8 unitarios, 12 de núcleo (tildes, orden, versiones, fijar, borrar, ámbitos, filtros, ids, recuperadas, export, **coherencia** tras 400 operaciones al azar, **concurrencia** con 4 conexiones escribiendo a la vez), 7 de CLI de punta a punta (incluido el criterio de «hecho»), y el de equivalencia de la 0.5.
- [x] CI en GitHub Actions: Windows, Linux y macOS (adelantado de la fase 5), con el modelo int8 descargado de Hugging Face y en caché.
- [x] Hecho: `omoi remember` + `omoi search "fallo de autenticación"` encuentra «Bug en auth» con y sin tilde.
- [x] Revisión del 28-oct (decisiones de Marc sobre el informe de la noche):
  - `forget` pide confirmación: en una terminal pregunta `[y/N]`; en cualquier otro sitio (un agente, un script) se niega sin `--yes`. El MCP de la fase 3 borrará sin preguntar, porque el agente ya lo ha decidido.
  - Ayuda en español completa con `OMOIKANE_LANG=es`, incluidos los títulos fijos de clap («Uso», «Comandos», «Argumentos», «Opciones») y los errores del motor de embeddings.
  - `clippy` instalado y en la CI con los avisos como errores.
  - Las rutas entran en el índice de búsqueda en la **fase 2** (ver allí).
- Medidas en el PC (Windows, release): binario de 2,9 MB; arrancar el proceso (`omoi --version`) 11 ms; `search` con 100 memorias ~20 ms de punta a punta (mediana y p90 en 20-21 ms); `remember` ~21 ms. Queda margen para el hook de la fase 4 (presupuesto de 50 ms, más ~5 ms de abrir el modelo).

### Fase 2 · vectores ✅ (28-oct-2026)
- [x] Esquema 2: rutas en el índice FTS (decisión de Marc del 28-oct) y `memory_vec` como BLOB; migración automática que rehace el índice.
- [x] `modelos.rs`: carpetas inmutables por huella + fichero `actual` atómico; `omoi model [status|install [--from]|clean]`; descarga desde Hugging Face con `ureq`.
- [x] `vectores.rs`: guardar en la misma transacción, `sincronizar` (completa los que faltan, rehace si cambió el modelo), `parecidas` y `duplicada`.
- [x] Búsqueda híbrida con RRF y filtro para lo que solo trae el vector (§5).
- [x] Duplicados al guardar: umbral 0,65, código 2 y `--force` en la CLI.
- [x] `omoi reindex`; `doctor` enseña el modelo y si los vectores están al día.
- [x] Tests (36 en total): 6 de vectores (al día, texto nuevo, duplicados por ámbito, **umbral de duplicados** y **calidad de la búsqueda** como tests de regresión sobre los ficheros de calibración, coherencia) y 1 de CLI de punta a punta.
- [x] Calibración con datos en vez de umbrales a ojo: `tests/pares_duplicados.tsv` (40 pares) y `tests/busquedas.tsv` (31 consultas), ambos escritos a mano y públicos.
- Hecho (criterio revisado): «problema de zona horaria con la autenticación» encuentra «Bug en auth: el token caduca…» sin compartir palabras, y guardar lo mismo con otras palabras propone `update`. El criterio original («fallo de login» → «bug en auth») no se cumple con un modelo estático (§5).
- Medidas en el PC: binario de 8,4 MB (2,9 en la fase 1: TLS para la descarga y `tokenizers`); `search` con modelo y 100 memorias, 19-31 ms por proceso; `remember` con control de duplicados, 19 ms.

**Ajustes del 28-oct-2026** (tras el borrador de resultados; decisiones de Marc: 1 aceptar el alcance semántico, 2 recalibrar con datos reales, 3 segunda fuente en GitHub, 4 mantener `ureq`, 5 que el agente decida ante un duplicado, 6 recall más estricto):
- [x] Recalibración con 127 notas reales (en local): destapó que el umbral de duplicados daba un 31 % de falsos avisos con notas largas y que RRF empeoraba la búsqueda (98 % → 87 %).
- [x] Vectores centrados (§9): falsos avisos de duplicado de 3.134 a 4 de 8.001 pares a 0,65; umbral subido a 0,70 (1 de 8.001). Con frases cortas detecta 9 de 20 duplicados sin ningún falso aviso: es el precio de no molestar.
- [x] Fusión por rescate (§5): la búsqueda con modelo nunca es peor que sin él; los tests lo comprueban.
- [x] Release `modelo-potion-int8-v1` como segunda fuente y prueba de que, si la primera falla, se usa la segunda.
- [x] Similitud centrada sin copias: de ~43 a ~38 ms con 2.000 memorias.
- [x] Criterio de «hecho» final: guardar lo mismo dicho de otra forma propone `update`; la búsqueda con modelo no es peor que sin él en ningún conjunto; y la ruta de un fichero encuentra sus memorias. El de «fallo de login» → «bug en auth» queda descartado: no es posible con un modelo estático.

### Fase 3 · MCP ✅ (28-oct-2026)
- [x] `src/mcp.rs`: `omoi mcp`, servidor stdio con `rmcp` 3.8 y las 8 herramientas de §7 (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`) más las `instructions` (cuándo guardar y qué `kind` usar).
- [x] Cada llamada abre la base en un hilo de bloqueo, con el modelo abierto una vez y compartido, y resuelve el proyecto por la carpeta del proceso (la del proyecto en el que el cliente arranca el servidor; `--dir` para forzar otra).
- [x] Ante un duplicado, `remember` no guarda y devuelve `saved: false`, `duplicate_of` (compacta), `similarity` y un `hint`: decide el agente, con `update` o con `force: true` (decisión de Marc, 28-oct).
- [x] `forget` borra sin preguntar: el agente ya lo ha decidido (la CLI sí pregunta).
- [x] Los fallos de la herramienta (id que no existe, tipo mal escrito) vuelven como resultado con `isError` y un texto legible, no como error de protocolo, para que el agente los corrija.
- [x] Listas compactas (id, kind, title, 200 caracteres, rutas, etiquetas, fijada, global, similitud): el agente pide `get` si quiere el resto.
- [x] `memoria::contexto`: preferencias y fijadas del proyecto y globales (la usará también el recall de la fase 4).
- [x] Tests (`tests/mcp.rs`, 2): un cliente MCP de verdad (el de `rmcp`, con `transport-child-process`) arranca `omoi mcp` y prueba las 8 herramientas, los errores, la ruta y el aviso de duplicado con `force`. En CI en los tres sistemas.
- [x] **Hecho**, con Claude Code de verdad (2.1.312) y sin tocar su configuración: dos sesiones `claude -p` con `--mcp-config` temporal y `--strict-mcp-config`, en este repo. La primera guardó un `gotcha` sobre `tests/equivalencia.rs` (proyecto `D:\Omoikane`, origen `mcp`); la segunda, sin contexto, lo encontró sola y citó su id.
- No se usó el inspector MCP: los tests ya hablan el protocolo con el cliente oficial de Rust, y Claude Code es el cliente que importa.

### Fase 4 · recall automático y plugin ✅ (28-oct-2026)
- [x] **Plugin** (decisión de Marc del 28-oct): `.claude-plugin/plugin.json` en la raíz del repo, con el servidor MCP (`omoi mcp`) y los dos hooks dentro, sin scripts. Enlazado desde su marketplace `drac` con `source: github`. El binario `omoi` va aparte (por ahora `cargo install`; en la fase 5, binarios en las Releases). `claude plugin validate .` pasa.
- [x] `src/recall.rs` + `omoi recall [--session-start]`: lee el JSON del hook, saca el proyecto de su `cwd`, escribe `hookSpecificOutput.additionalContext` o nada.
- [x] Criterio de inyección calibrado con datos reales (§6): 99 % de positivos con su nota, 20 % de los mensajes reales con algo inyectado. Antes de calibrar, el primer criterio inyectaba en el 97 %. La prueba con Claude Code destapó que en una base de 3 memorias «cambiar la pasarela de pago a Stripe» no traía la decisión sobre la pasarela (las palabras que no estaban en ninguna memoria pesaban demasiado): corregido y con test.
- [x] Una vez por sesión (`recall_log`, esquema 3); tras compactar se vuelve a poder.
- [x] Rutas nombradas en el mensaje (también con `\` de Windows) traen sus memorias.
- [x] Tests (`tests/recall.rs`, 6): contexto al empezar, relevancia y charla, una vez por sesión, rutas, ámbito, y el hook de punta a punta con el JSON de Claude Code (incluida una entrada rota, que no debe dar error).
- [x] Latencia del hook: 15-26 ms (§6).
- **Hecho**, con Claude Code de verdad y el plugin instalado (`omoikane@drac`), sin herramientas para que no pudiera buscar por su cuenta: al nombrar `src/pago.rs` contestó con el aviso guardado de ese fichero; al hablar de «cambiar la pasarela de pago a Stripe», con la decisión de usar Redsys y sus motivos; la preferencia global llegó al empezar. Cada memoria se inyectó una vez por sesión.

### Fase 5 · publicar ✅ (28-oct-2026)
- [x] **Instalación en una línea** (decisión de Marc del 28-oct): un plugin de Claude Code no puede ejecutar nada al instalarse (el hook `Setup` solo corre con `--init`/`--maintenance`) ni elegir binario por sistema, así que se invierte: el instalador trae el binario y el binario instala el plugin.
  - `install.ps1` (Windows) e `install.sh` (Linux, macOS): bajan el binario de la última Release, comprueban su SHA-256, lo dejan en `%LOCALAPPDATA%\Programs\omoikane` (y en el PATH del usuario) o en `~/.local/bin`, instalan el modelo y ejecutan `omoi setup claude --yes`. `OMOIKANE_NO_MODEL`, `OMOIKANE_NO_SETUP`, `OMOIKANE_NO_PATH`, `OMOIKANE_INSTALL_DIR`, `OMOIKANE_VERSION`.
  - `omoi setup claude [--yes] [--dry-run]`: añade el marketplace `drac` (o lo actualiza si ya estaba, por si es una copia anterior al plugin) e instala `omoikane@drac`; si ya está, no hace nada. Pregunta en una terminal; fuera de ella, exige `--yes`.
- [x] `release.yml`: con un tag `v*`, binarios para Windows x64, Linux x64 y ARM64 (compilados en Ubuntu 22.04: glibc 2.35) y macOS ARM e Intel; cada uno arranca y pasa `doctor` en su runner; `SHA256SUMS`. A mano (`workflow_dispatch`) solo compila. Paquetes de 4,1-4,6 MB.
- [x] `omoi … | head` ya no revienta en Unix (SIGPIPE).
- Sin hacer, a propósito: el GIF del README. Publicar en crates.io se decidió después: está desde la 0.2.0 (`cargo install omoikane`).
- **Hecho**: Release [`v0.1.0`](https://github.com/marcmayol/omoikane/releases/tag/v0.1.0) publicada con los cinco binarios. Probado ejecutando los instaladores desde la URL pública de GitHub, en carpetas temporales: Windows, 17 s (binario con su suma, modelo y `setup`, que vio que el plugin ya estaba); Ubuntu 24.04, 25 s (binario, modelo, guardar y buscar). macOS solo en la CI (el binario arranca y pasa `doctor`): no hay un Mac a mano para probar el instalador.

### Pruebas a fondo en el PC y versión 0.2.0 (28-oct-2026)
Diez escenarios con Claude Code de verdad y el plugin instalado (base temporal; los de recall, sin herramientas para que no pudiera buscar por su cuenta): guardar tres memorias de tipos distintos (preferencia global, gotcha con ruta, decisión), recall al nombrar un fichero, preferencia global al empezar, no duplicar (el agente vio la memoria inyectada y no la volvió a guardar), cambiar una decisión (`update` con motivo, versión anterior en el historial), aislamiento entre proyectos, buscar en inglés lo guardado en español, olvidar, y dos sesiones guardando a la vez. **9 de 10 bien a la primera.**

El que falló: el agente **guardaba en inglés** aunque se le hablaba en español (las instrucciones del MCP están en inglés) y el recall por palabras no encontraba la memoria al preguntar en español. Arreglo: las instrucciones le piden guardar en el idioma en que le escribe el usuario. Repetido: guarda en español y el recall por tema funciona.

Idea tomada de engram (que usa sus hooks para que el agente no se olvide de la memoria, no para inyectar memorias relevantes): **recordatorio de guardar**. Si una sesión lleva al menos 15 minutos y 10 mensajes sin que se haya guardado ni cambiado ninguna memoria del proyecto, el hook añade una línea recordándolo; como mucho una vez cada 30 minutos; compactar no reinicia el reloj. Tabla `session` (esquema 4). La captura automática al terminar la sesión de engram no se copia: rompe que sea el agente quien decide qué merece la pena.

### 0.3.0 · el plugin se instala solo (28-oct-2026)
Marc pidió que instalar el plugin bastara. Probado antes de decidir:
- Un plugin normal no puede: no ejecuta nada al instalarse, y su servidor MCP arranca **a la vez** que el hook `SessionStart` (medido: el MCP a los 3 s, el hook de 4 a 10 s), así que en la primera sesión no habría binario.
- `$.mcp.connect` desde un mod tampoco lo arregla: el fallo del primer arranque queda cacheado 15 minutos, **también en las sesiones siguientes**.

Solución: el plugin pasa a ser un **mod** (`hooks/hooks.json` + `hooks/omoikane.ts`), sin MCP ni hooks de shell:
- En `session.start` (Claude Code lo espera antes del primer mensaje) busca un `omoi` de la versión del plugin: `OMOIKANE_BIN`, `~/.omoikane/bin/omoi`, el del PATH. Si no hay, baja de la Release `v<versión>` el paquete de su sistema con `curl`, lo comprueba contra `SHA256SUMS` (`certutil` en Windows, `sha256sum`/`shasum` en Unix) y lo abre con `tar` (en Windows el de System32, que también abre zip). Después, el modelo en segundo plano.
- Las 8 herramientas se registran con `$.tool.register` tal como las lista `omoi tool` (nombre, descripción, esquema): una sola fuente. Cada llamada ejecuta `omoi tool <nombre>` con los argumentos en JSON por stdin, que llama a los mismos métodos que el servidor MCP: mismas respuestas.
- El recall va en `classic.SessionStart` y `classic.UserPromptSubmit` (el mismo `omoi recall` con el JSON del hook), y las instrucciones que antes daba el MCP van en el contexto de inicio.
- Todo con `$.process.run` (sin shell): igual en Windows, macOS y Linux, tenga o no Git Bash.
- Requiere Claude Code 2.1.310 o posterior. `omoi mcp`, los instaladores y `cargo install` siguen para otros agentes o para usar la CLI.

### 0.3.1 · recalibrado y comparado con engram (28-oct-2026)
- **Duplicados**: el vector solo no separaba (con 0,70, 3,9 % de avisos en falso con 127 notas reales y solo el 66 % de las notas guardadas otra vez detectadas). Ahora: coseno ≥ 0,78, o coseno ≥ 0,40 y cobertura IDF de las palabras ≥ 0,60 (la del recall). 0 avisos en falso con las notas, 1 de 90 con frases cortas, 89-90 % detectadas, nunca la memoria equivocada; 7 ms de mediana. `examples/fase2.rs dupcob` (rejilla) y `dupprod` (el código de producción).
- **Recall**: cobertura 0,40 → 0,50. Con 80 preguntas escritas a ciegas por otro agente (sin ver ningún sistema): algo inyectado en 2 de 20 sin respuesta (antes 8), 36 de 60 con la nota buena (antes 40). Con 300 mensajes reales: del 21 % al 11 % con inyección; positivos 99 % → 98 %. La nota gigante de Building My Future (87 KB) es la que más ruido mete: una memoria larga lo «cubre» todo.
- **Comparación con engram v3.6.0** ([BENCHMARK.es.md](BENCHMARK.es.md)): mismos datos, mismas preguntas, tiempos intercalados, respuestas de Claude corregidas a ciegas. Búsqueda con la pregunta tal cual: Omoikane 88 % top-1 en naturales, engram 0 % por defecto y 88 % si se le pasan palabras clave (y 87 % frente a 80 % en paráfrasis: ahí gana). 34 ms frente a 120-155 ms. Duplicados: engram por MCP encuentra el 100 % pero pide revisar el 49 % de lo que no lo es. De punta a punta, 20/20 frente a 19/20 (no significativo) y un 39 % más barato.

### 0.4.0 · para superar a engram (28-oct-2026)
Las mejoras que salieron de la comparación, salvo sincronizar entre equipos (aparcado). Método: las 80 preguntas a ciegas de la 0.3.1 solo para ajustar; **130 preguntas nuevas escritas a ciegas por otro agente, que no se miraron hasta el final**, para comprobar; otra IA revisó el código.

- **Paráfrasis en la búsqueda**: probado que el vector reordene (`Fusion::Mezcla`, rejilla de pesos): ninguna combinación se distinguía del azar (±1 pregunta). No se activa; el límite es el modelo estático. Queda en el código para medir otros modelos.
- **Paráfrasis en el recall**: canal por vector (parecido centrado ≥ 0,35 y cobertura ≥ 0,25, mensajes de ≤ 500 caracteres). Con las preguntas de comprobación: paráfrasis con la nota buena de 5 a 14 de 25; en total 64 de 100 frente a 51 de la 0.3.1, a cambio de 7 de 30 preguntas sin respuesta con algo inyectado (antes 5).
- **Memorias muy largas**: para el recall solo cuentan sus primeros 6.000 caracteres (una nota de 87 KB lo «cubría» todo); mensajes reales con inyección del 12 % al 7 %. Se probó 3.000: mejor con las preguntas de ajuste, pero las de comprobación (detalles del medio y del final de las notas) perdían 7 de 35 naturales; sobreajuste, descartado. Guardar avisa si una memoria pasa de ese largo.
- **Duplicados como engram, sin su ruido**: lo que `duplicada` no ve con seguridad se guarda y se devuelven hasta 3 «relacionadas» (el método de engram en Rust: BM25 por las palabras del título y al menos dos palabras de título en común). Notas guardadas otra vez cubiertas: 100 % (como engram). Relacionadas sin serlo: 14 % con frases cortas (engram 49 %) y 52 % con notas largas (engram 95 %). Exigir además parecido por vector quitaba ruido a costa de duplicadas sin cubrir; no se exige.
- **Otros agentes**: `omoi setup codex|gemini|cursor|opencode` (MCP, hooks del recall donde se puede, párrafo en sus instrucciones; copia de cada fichero, idempotente). `omoi recall --format gemini|cursor`. OpenCode probado de verdad (1.19.5): el plugin mete el recall en el prompt de sistema (`experimental.chat.system.transform`; como parte nueva del mensaje fallaba el servidor y cambiar el texto no llegaba). Codex, Gemini y Cursor, según su documentación. La revisión encontró que en Windows sus hooks no se habrían ejecutado (PowerShell no ejecuta `"ruta" args`): ahora la ruta va tal cual, o con `&` si tiene espacios; comprobado en PowerShell y bash.
- **Importar desde engram**: `omoi import` (exportación de engram o de `omoi export`): tipos, ámbito personal → global, `topic_key` → etiqueta, fechas, borradas fuera, duplicadas fuera (exactas si no hay modelo), entradas inválidas arregladas o contadas sin parar.
- **`omoi tui`**: visor con ratatui (buscar mientras se escribe, memoria entera, historial, fijar, olvidar con confirmación).
- **Escala e inglés**: 3.183 memorias (las notas más 3.055 párrafos de READMEs técnicos en inglés del disco) y 50 preguntas en inglés escritas a ciegas. Omoikane no pierde calidad con 25 veces más memorias; buscar 50 ms (engram 203 ms), guardar 38 ms (149 ms), recall 46 ms. En inglés, empate en cortas, 95 % frente a 85 % en naturales, y los dos flojos en paráfrasis (47 % top-1). macOS solo en la CI.

### 0.5.0 · catorce agentes y un recall más preciso (28-oct-2026)
- `omoi setup` para 13 agentes además de Claude Code (los 14 de engram, con Amp en lugar de CommandCode): Qwen Code y Kiro con recall en cada mensaje; Copilot (CLI y VS Code) y Antigravity solo al empezar; Windsurf/Devin, Kimi Code, Pi, Amp y Kilo Code con MCP e instrucciones. `omoi recall --format copilot|antigravity|plain`, y entradas con `conversationId`, `workspacePaths` y `USER_PROMPT`. Todos según su documentación salvo Claude Code y OpenCode.
- Recall: como mucho 2 memorias (antes 3), y solo las que llegan al 85 % de la mejor (antes 70 %). Con las preguntas de ajuste y de comprobación, las mismas notas buenas con un 39 % menos de memorias de más. Validado con un **tercer conjunto** de 100 preguntas a ciegas y otra IA de juez: precisión del 62 % al 68 %, 7 inyecciones de ruido menos, 1 nota buena menos.
- Comparación final con engram: gana Omoikane en búsqueda con la pregunta tal cual, velocidad, ruido de duplicados, recall automático y coste; empate en preguntas cortas, duplicados cubiertos, aciertos de punta a punta, agentes y visor; engram gana en paráfrasis en top-3/5 y en sincronización, nube y API HTTP.

### 0.5.1 · un hueco para el vector en la búsqueda (28-oct-2026)
- `Fusion::Reserva { huecos: 1, desde: 0.4 }`: el último de los puestos pedidos es para lo que el vector ve muy parecido aunque las palabras lo dejen más abajo. Paráfrasis en los 5 primeros de 65 a 66 de 70; nada más cambia en 310 preguntas a ciegas.
- Con los tres conjuntos juntos (310), Omoikane gana a engram en top-1 y MRR de todos los tipos y en velocidad; engram queda una pregunta por delante en paráfrasis en top-3 y top-5. Los fallos que quedan son huecos de vocabulario («auriculares inalámbricos» → Bluetooth, «pantalla azul» → BSOD) que solo cubriría un modelo semántico mejor.

### 0.5.2 · cobertura con IDF en la búsqueda (28-oct-2026)
- El desempate de la búsqueda pondera cada palabra por su IDF (como el recall): las genéricas ya no suben la nota larga que lo contiene todo. Con las 310 preguntas y las notas completas: paráfrasis en primer puesto 82,9 % → 84,3 %, MRR 0,877 → 0,884; nada empeora.
- Comparación justa final (notas completas, cada uno con su importador, la misma consulta limpia para los dos, 10 resultados como usan los agentes): Omoikane gana o empata en top-1, top-3, top-10 y MRR de todos los tipos salvo paráfrasis en top-10 (67 frente a 68 de 70).

### 0.6.0 · raíz en la búsqueda, `omoi serve` y un cuarto conjunto (28-oct-2026)
- Búsqueda también por raíz (sin las dos últimas letras, desde 6): «reinicia» encuentra «reinicios», «tokens» encuentra «token».
- `omoi serve`: las herramientas por HTTP en 127.0.0.1:7438 (token opcional), como la API de engram.
- **Cuarto conjunto** de 100 preguntas a ciegas, usado una sola vez después de decidir: Omoikane gana a engram en top-1 (79/76), top-3 (94/86), top-5 (95/88), top-10 (97/89) y MRR (0,859/0,813), también en paráfrasis. La raíz, +2 puntos en top-3/5/10 y −1 paráfrasis en top-1; la cobertura con IDF de la 0.5.2 queda validada aquí con datos limpios.
- Lo único de engram que no tiene: sincronización y nube (aparcadas por Marc).

### 0.7.0 · sesiones abiertas en la carpeta del usuario (28-oct-2026)
- El problema: Marc abre casi todas sus sesiones en `C:\Users\marcm` (132 de ~137 en octubre), que no es un repositorio. El proyecto salía de la carpeta de la sesión, así que lo aprendido de cada proyecto acababa en global y, al revés, mover una memoria a su repositorio la habría escondido de esas sesiones.
- **Carpeta general** (`proyecto::es_general`): la del usuario, una por encima o la raíz de una unidad, sin `.git`. Desde ahí `search`, `recent`, la TUI y el recall miran **todos los proyectos**; una carpeta suelta sin git sigue siendo su propio proyecto.
- **Al guardar** desde una carpeta general, la memoria va al repositorio al que apuntan sus rutas absolutas (`paths` o el texto) si es uno solo; si no, global. `remember` acepta `project` (carpeta) y `update` puede mover (`project`, o `--project`/`--global` en la CLI) sin crear versión.
- El recall desde una carpeta general marca `[project <nombre>]` en las memorias de un proyecto, y el aviso de inicio explica cómo guardar en un proyecto.
- Comprobado sobre una copia de la base real: desde el home se recuerda lo mismo que en 0.6.0; dentro de un repositorio ya no se cuela lo de otros proyectos (la ficha del taxi desde Faltis, DracPaste desde Rodet). La búsqueda no cambia: el ranking es el mismo.

### 0.8.0 · la memoria en el navegador (28-oct-2026)
- `omoi serve` sirve en `/` una página incluida en el binario (`src/web.html`, sin dependencias): todas las memorias, búsqueda (la misma que el agente), filtros por proyecto, tema (etiqueta), tipo y fijadas, historial, y fijar, mover de proyecto y olvidar (con confirmación en la página). `GET /api/memories` da la lista entera.
- `omoi serve --open` abre el navegador (si ya hay un servidor en ese puerto, solo lo abre). En Claude Code, `/omoikane` lo hace desde el chat: el mod registra la orden y el servidor vive lo que la sesión.
- Seguridad: solo atiende peticiones con `Host` `127.0.0.1`/`localhost` (contra el DNS rebinding) y rechaza escrituras con un `Origin` ajeno (un formulario de otra web no puede olvidar memorias). Con `OMOIKANE_HTTP_TOKEN`, el token va en `#token=` de la URL y la página lo manda en cada petición.
- Sirve para todos los agentes (la base es la misma); la orden con barra solo existe en Claude Code.

## Fase 6 · memoria compartida por git, y `/omoikane` en todos los agentes (diseño, 28-oct-2026)

Producto global: Omoikane no aloja nada y no depende de la red de nadie; usa el git del usuario. Coste 0.

### 0.10.0 · la vista web, rediseñada (29-oct-2026)
- Diseñada con Claude Design (proyecto «Omoikane · visor web», dos rondas) a partir del banner: negro cálido, cobre y ámbar, crema; en claro, papel cálido y cobre oscuro (sigue `prefers-color-scheme`). Marc eligió: Memorias como cronología por días (2b) de entrada, Proyectos como pestaña con tarjetas «dónde se quedó» (1c) que llevan a la página del proyecto (2a), y el modo claro. Descartados: la portada de proyectos como entrada y las tres columnas (1b, «lo mismo con más decoración»).
- **Memorias:** eventos por día (nueva, cambiada · vN y, si se pide, recordada); la memoria se abre en su sitio con Markdown (renderizador propio que escapa todo y solo enlaza http/https), su último cambio como diff por líneas (LCS, una línea de contexto) y el historial completo. Teclado: `/`, `j`/`k`, Esc.
- **Proyectos:** la última memoria `status` de cada uno, cuántas tiene y la mezcla de tipos; la página del proyecto muestra su estado entero y las memorias en cuatro columnas por tipo, con el detalle en un panel lateral.
- **La orden pasa de `/omoikane` a `/omoikane-web`** (Marc: «parece un comando para que recuerde algo que se acaba de decir»). En Claude Code la registra el mod; en los otros 9 agentes, `omoi setup` escribe `omoikane-web.md`/`.toml` y borra la vieja `omoikane.md`/`.toml` solo si su texto es el que escribió Omoikane.
- `/api/memories` añade `versions` por memoria. Fuentes e icono servidos desde el binario (`/fuentes/*.woff2`, `/icono.png`, unos 105 KB, OFL en `assets/fuentes`), porque la vista funciona sin internet.
- Probado con Playwright contra una copia de la base real: oscuro, claro, 400 px de ancho, búsqueda, fijar, historial completo, olvidar con confirmación y teclado, sin errores de consola.

### 0.9.2 · en qué punto se quedó cada proyecto (29-oct-2026)
- El caso: «vamos a seguir con Omoikane» desde la carpeta del usuario. Con la 0.9.1 el agente hizo una búsqueda, dos `get` y dos comandos de git, y propuso como pendientes cosas ya hechas: la única nota con el estado del proyecto se había quedado en la 0.2.0, y «Last worked on» solo enseñaba títulos.
- Las memorias con la etiqueta **`status`** dicen dónde se quedó el trabajo. Al empezar sesión sale **«Where work was left»**: la más reciente de cada proyecto del ámbito (como mucho 5, la de la sesión y las globales si se está en un proyecto), con fecha, título y 400 caracteres de contenido. No cuentan como inyectadas.
- Los estados viejos (no son el último de su proyecto) ya no salen en «Last worked on»: llevarían a retomar desde lo desfasado.
- Las instrucciones (inicio de sesión, MCP y el recordatorio de guardar) piden guardar el punto en que se quedó con la etiqueta `status` y uno por proyecto: si ya hay uno, `update`.

### 0.9.1 · retomar sin explorar (29-oct-2026)
- El caso: en una sesión nueva, «vamos a seguir con el tema de la impresión 3D»; el agente lanzó `recent`, búsquedas, leyó carpetas y consultó la impresora para acabar preguntando qué proyecto. No había ninguna nota del DXC2 y el inicio de sesión no decía en qué se había trabajado.
- Al empezar sesión se inyecta **«Last worked on»**: las 5 memorias cambiadas más recientemente en el ámbito (todos los proyectos desde la carpeta del usuario), una línea cada una con fecha, tipo, proyecto y título, sin contenido; no cuentan como inyectadas, así que su contenido aún puede llegar con el recall.
- Instrucción nueva (inicio de sesión y MCP): si la petición de continuar es ambigua, como mucho una búsqueda y, si no lo aclara, preguntar en vez de explorar; al terminar un trabajo o al cerrar la sesión, guardar en qué punto se quedó (hecho y siguiente paso). El recordatorio de «guarda algo» lo pide también.
- **7.º conjunto a ciegas (29-oct-2026), solo para comprobar:** 100 preguntas nuevas (60 paráfrasis, 40 de un dato del medio o del final de la nota) sobre 97 de las 130 notas, escritas por otro agente sin ver ningún buscador ni los conjuntos anteriores. Nada se ajustó con ellas. Omoikane 0.9.1: top-1 90 %, top-3 97 %, top-10 99 %, MRR 0,934; engram: 75 / 87 / 96 %, MRR 0,820. Paráfrasis 86,7 % frente a 70 % en top-1. Mediana 43 ms frente a 173. Con las 526 de ajuste la 0.9.0 daba 87,3 % en top-1, así que no hay sobreajuste. La única que no sale en el top-10 («comparar dos versiones para saber cuál es más rápida sin engañarnos» → la nota de medir intercalado) es otro hueco de vocabulario; las demás que no quedan primeras están en 2.º o 3.er puesto, menos dos de detalle (4.º y 6.º).

### 0.9.0 · Fase 6 hecha (29-oct-2026)
- **Sincronización por git** (`src/sync.rs`, `src/reloj.rs`, esquema 5): `omoi sync init <url>` o `--create <nombre>` (repositorio privado con `gh`/`glab`), `join <código>` (o `join -` por la entrada estándar, `--url` para llegar por otra vía), `code`, `status`, `restore`, `off`; estado también en `doctor` y en la web (`/api/sync`). Preguntas en `init`/`join` con resumen de lo que se sube; sin terminal exigen `--yes`.
- **En el repositorio:** `.gitattributes`, `omoikane.check` (comprueba la clave; `init` se niega en un repositorio ya usado), `global/`, `p-<hash>/` por proyecto y `olvidos/`, un fichero por equipo, líneas ordenadas y **cifradas con AES-SIV** (deterministas: una línea que no cambia queda igual). Nombres de carpeta opacos.
- **Juntar:** reloj lógico híbrido comparado por partes (los sellos remotos de más de 1 h en el futuro no adelantan el reloj); la versión local anterior queda en el historial `sync: <equipo>`; olvidos con marca que no resucitan, y lo que olvida otro equipo se puede recuperar (`omoi sync restore`). Líneas ilegibles o inválidas se saltan y se avisan, sin parar la sincronización. El filtro de qué se comparte vale también para lo que llega.
- **Automático para todos los agentes:** tras cada escritura (CLI, MCP, `omoi tool`, HTTP, TUI) `omoi` lanza la sincronización en segundo plano (3 s para juntar cambios, un proceso por equipo con cerrojo, sin ventanas en Windows, git sin hooks del usuario y con límite de tiempo); al empezar sesión, una bajada en segundo plano; aviso si lleva más de 1 h fallando. Guardar con la sincronización activa: 33 ms (28 sin ella).
- **Autor** por memoria (sí por defecto); lo de otra persona sale `[by Ana]` en el recall y con su autor en la web.
- **`/omoikane` en 9 agentes más** (Gemini, Qwen, Codex, Cursor, OpenCode, Kiro, Windsurf, Pi, Kilo; comprobado en la documentación de cada uno); Copilot, Kimi, Amp y Antigravity no tienen órdenes de usuario documentadas.
- **Pequeños:** rutas con espacios, rutas relativas al repositorio deducido, el agente en «Origen» (`mcp:<cliente>`), verde de «preference» con contraste AA en modo claro.
- **Revisado dos veces por otra IA** (9 problemas en la primera pasada, 10 en la segunda; arreglados) y probado de verdad entre el PC (Windows, HTTPS) y marc-server (Ubuntu, SSH con clave de despliegue) a través de un repositorio privado de GitHub.

### Memoria compartida por git (decisiones de Marc, 28-oct-2026)
- **Un repositorio solo para el conocimiento**, nunca el del código. Cualquier remoto git (GitHub, GitLab, Codeberg, un Gitea de empresa, una carpeta). Credenciales: las de git del usuario (SSH o credential helper); Omoikane no guarda tokens.
- **Configurar:** `omoi sync init <url>` (o `--create` con `gh`/`glab` si están). La configuración **pregunta**: qué se comparte (por tipo y proyecto; las preferencias, personales salvo que se diga), si cada línea lleva autor (**sí por defecto**) y qué se sube de lo que ya existe (con un resumen: «102 globales y 29 de proyectos»). Apagado hasta hacer `init`. Un segundo equipo o un compañero: `omoi sync join <url> <código>`.
- **Automático y asíncrono:** después de cada `remember`/`update`/`forget`, el propio `omoi` lanza en segundo plano pull, juntar, commit y push; la consulta no espera. Al empezar sesión, un pull en segundo plano. **Omoikane hace los commits solo** (el repositorio es suyo, no el del código). Agrupados: un commit cada pocos segundos, no uno por memoria, y un solo proceso por equipo (fichero de bloqueo). Lo lanza `omoi`, no el plugin, así que vale para **todos los agentes**.
- **Nunca se sube el SQLite.** Se sube texto: JSONL, una línea por memoria y ordenado, **partido por proyecto y por equipo**: `<clave del proyecto>/<equipo>.jsonl` y `global/<equipo>.jsonl`. Cada equipo solo escribe los suyos (sin conflictos de git); al bajar se leen solo los ficheros que cambiaron (`git diff --name-only`). Vectores e índice se recalculan en cada equipo; la base local se puede reconstruir entera desde el repositorio.
- **Cifrado solo en el remoto:** cada línea se cifra antes de subir y se descifra al bajar; en local todo sigue en claro. Algoritmo estándar (nada propio). Clave generada en `init`; otro equipo o compañero la recibe con un código de invitación. Si se pierde, lo del remoto no se lee, pero cada equipo conserva su copia local. Si alguien sale del grupo, se cambia la clave para lo que venga (lo ya leído no se puede retirar).
- **Autor por línea** (si está activado): el nombre de `git config user.name`, sin correo. No es una firma: la misma confianza que el autor de un commit. La auditoría completa es el `git log` del repositorio.
- **Lo de otros, marcado:** el recall y la web dicen de quién es una memoria que no es tuya («de Ana»), para que el agente la trate con cuidado.
- **Orden de los cambios: relojes lógicos** (no la hora del sistema), para que un reloj mal puesto no decida qué versión gana.
- **Avisa:** si no puede sincronizar, aviso al empezar sesión, y el estado (última sincronización, cambios pendientes, error) en `omoi doctor` y en la web.
- **Proyectos sin remoto git:** no se mezclan entre personas (dos carpetas `web` de dos personas no son el mismo proyecto); entre los equipos de una misma persona, por el nombre de la carpeta.
- **Pruebas:** entre dos equipos de verdad, y en la CI con repositorios desnudos locales en Windows, Linux y macOS. **Documentación** y aviso de privacidad al activarlo.
- **Más adelante, cuando merezca la pena un cloud:** límites de tamaño y repositorios de cientos de personas.
- **Base técnica:** el prototipo de abajo, con sus tres pendientes (resueltos en la 0.9.0).

### `/omoikane` en los demás agentes (hecho en la 0.9.0, ver arriba)
- Antes de la 0.9.0 `omoi setup <agente>` pone MCP, hooks del recall e instrucciones, pero ninguna orden: `/omoikane` solo existe en Claude Code (la registra el mod).
- Que `setup` cree también una orden `omoikane` en cada agente que las admita, con su formato. **Comprobar cada ruta y formato en la documentación de cada agente antes de escribirlo**; los candidatos: Gemini (`~/.gemini/commands/omoikane.toml`), Qwen (`~/.qwen/commands/omoikane.toml`), Codex (`~/.codex/prompts/omoikane.md`), Cursor (`.cursor/commands/omoikane.md`), OpenCode (`~/.config/opencode/command/omoikane.md`); Copilot, Windsurf, Kiro y los demás tienen prompt files, workflows o steering, cada uno a su manera.
- En casi todos, una orden es un texto para el modelo, no un comando: la orden le pediría al agente ejecutar `omoi serve --open` con su herramienta de terminal (puede pedir permiso). Menos directo que el mod de Claude Code.
- Mismas reglas que el resto de `setup`: idempotente, copia `.antes-de-omoikane`, `--dry-run`, y un test por agente con `OMOIKANE_SETUP_HOME`.
- De paso: el campo «Origen» de la web decía `mcp` para todo lo que llega por MCP; desde la 0.9.0 dice qué agente lo guardó (`mcp:<cliente>`, del `clientInfo` del MCP).

### Otros pendientes de la Fase 6 (hechos en la 0.9.0)
- Rutas con espacios en el texto de una memoria.
- Al deducir el proyecto por una ruta absoluta, guardarla relativa al repositorio.
- Ver la web en modo claro.

### Sincronizar entre equipos · primer prototipo (28-oct-2026, base de la Fase 6)
Se escribió y probó antes de la Fase 6 en una rama local, `prueba-sync` (último commit 6dc09ef), que se borró el 29-oct-2026: la Fase 6 lo rehízo en `master`. Su diseño:

- **Sin servicio propio.** Cada equipo escribe su copia completa en `<carpeta>/<equipo>.jsonl` (una línea por memoria, más las marcas de borrado) y lee las de los demás. La carpeta puede ser un repositorio git (`omoi sync <carpeta> --git`: pull, juntar, commit y push; un repositorio privado hace de «nube»), Syncthing o una unidad de red. Como cada equipo solo escribe su fichero, nunca hay conflictos de edición en la carpeta.
- **Nombre del equipo:** el del sistema más un sufijo fijo, guardado en `meta`.
- **Juntar:** gana la versión cambiada más tarde (`updated_at`); si viene de fuera, la anterior queda en el historial con el motivo `sync: <equipo>` y se conserva la fecha de origen para que no rebote. Hace falta guardar con el id y las fechas de origen (la misma memoria tiene el mismo ULID en todos los equipos).
- **Olvidos:** `olvidar` deja una marca (`tombstone(id, deleted_at)`, esquema 5); las marcas viajan y se aplican antes que las memorias, así lo olvidado no resucita desde otro equipo. Si aquí se cambió después de que allí se olvidara, se queda.
- **Proyectos:** se reconocen por el remoto `origin` de git normalizado (`git:github.com/dueño/repo`) o, si no hay, por el nombre de la carpeta: `D:\Omoikane` y `~/omoikane` son el mismo proyecto. También cuenta el proyecto de la carpeta desde la que se sincroniza, aunque aún no tenga memorias.
- **Límites conocidos:** depende de que los relojes estén razonablemente en hora; si dos equipos cambian la misma memoria entre dos sincronizaciones, gana el más reciente y el otro cambio solo queda en el historial si era el local.
- **Probado:** un test con dos bases en el mismo PC (nuevas con el mismo id y en la ruta propia del proyecto, cambio con historial, nada rebota, olvido que no resucita en ningún orden, un fichero por equipo) pasó a la primera. La prueba real PC (Windows) ↔ marc-server (Ubuntu), con bases temporales y un repositorio git desnudo en el servidor, también pasó: las 2 memorias llegaron con el mismo id, la del proyecto en la ruta del servidor (remoto por SSH en un lado y HTTPS en el otro), el cambio volvió con historial `sync: <equipo>` y el olvido viajó sin resucitar en varias pasadas.
- **Pendientes del prototipo, resueltos en la 0.9.0:** (1) si git no tiene identidad (`user.name`/`user.email`), el commit falla y no se sube nada: avisar claro o firmar con una identidad propia de Omoikane; (2) con el repositorio vacío, el primer `pull` falla si la rama del clon no coincide con la del remoto: saltarse el pull cuando el remoto no tiene ramas; (3) recrear `tests/sync.rs`.
- El plugin podría sincronizar solo al empezar cada sesión si se define `OMOIKANE_SYNC_DIR`.

Después, si los datos lo piden: relaciones entre memorias, limpieza asistida de lo que nunca se recupera, un modelo de embeddings mejor para las paráfrasis. (Las dos primeras pasan a la Fase 7.)

## Fase 7 · una memoria que se mantiene viva (diseño, 29-oct-2026)

Hasta la 0.10.0, Omoikane hace mejor lo que hace engram: buscar, recordar sin que el agente lo pida y en Windows. Marc quiere que haga **algo que engram no intenta**. Las cuatro ideas salen de fallos reales del 29-oct: una nota de estado que se quedó en la 0.2.0 me hizo proponer como pendientes cosas ya hechas; el recordatorio de «no has guardado nada» saltó varias veces en una sesión larga; la publicación (tag, crates.io, Release, plugin, exe bloqueado) se repitió de memoria tres veces; y las notas importadas ya se enlazan con `[[...]]` sin que nadie lo aproveche. Marc las quiere todas; van en este orden porque cada una se apoya en la anterior.

Reglas que no cambian: cero dependencias nuevas, **ningún LLM dentro de Omoikane** (lo que necesite entender texto lo hace el agente, que ya es un modelo), **8 herramientas MCP** (lo nuevo entra en las que hay), todo opcional y explicable, y cada cosa se mide con datos reales antes de darla por buena.

### 7.1 · 0.11.0 · memoria que se cuida sola ✅ (29-oct-2026)
**Hecho así** (`src/revisar.rs`, esquema 6, `tests/revisar.rs`):
- Calibrado con un prototipo en Python sobre sus 137 memorias reales antes de escribirlo en Rust. Primera versión: 5 avisos, 4 falsos («pendiente» en una y «hecho» en otra hablando de cosas distintas) y sin el caso que lo motivó (las importadas tienen todas la misma fecha). Tres cambios lo dejaron en **1 aviso, el bueno** («version 0.2.0 → 0.14.0» en la nota de Omoikane): la frase pendiente y la hecha tienen que compartir dos palabras raras; la fecha que cuenta es la más reciente que menciona el texto (`29-oct-2026`, `2026-10-29`) y no `updated_at`; y una versión solo se compara con otra de la misma cosa (la palabra de delante dentro de la frase: «rusqlite 0.41.0» no es «omoi 0.9.2»).
- **Sin vector**: con palabras raras (df ≤ 5 %), fechas y versiones bastó; dentro de un proyecto se comparan todas las parejas, entre las globales solo las que comparten una palabra rara (índice invertido).
- Las desfasadas se guardan en `review` y se recalculan cuando cambia la huella de `memory`, desde las escrituras (MCP, CLI, fin de cada sync, la vista). **El recall solo lee** lo guardado: recalcular cuesta ~47 ms con 137 memorias y el recall sigue igual que en la 0.10.0 (56-57 ms el proceso entero, medido intercalado). Estado viejo, rutas rotas, sin uso y largas se miran al leer (dependen de la hora o del disco).
- Al guardar, si la nueva deja anticuada a otra, `remember` devuelve `supersedes` con el porqué. `context` pasa a `{memories, review}`. `omoi review [--all] [--dismiss <id> --kind … --other …]`, `GET /api/review`, `POST /api/review/dismiss` y la pestaña **Revisar** con las dos memorias lado a lado.
- «Está bien así» vale hasta que cambie cualquiera de las dos memorias.
- **Pendiente respecto al diseño:** que lo descartado viaje por la sincronización (de momento es de cada equipo), y el criterio de «7 de cada 10 avisos útiles», que necesita que Marc revise la lista (hoy: 1 desfasada, verdadera, y 20 notas largas, verdaderas por definición; estado viejo y sin uso no pueden salir aún porque todas sus memorias son del 28-29 oct).

**Qué detecta** (en `src/revisar.rs`, sin modelo salvo el vector que ya hay):
- **Desfasada:** dos memorias del mismo proyecto que hablan de lo mismo (parecido por vector ≥ 0,55 o títulos con ≥ 2 palabras raras en común) y se contradicen en algo comprobable: versiones distintas (`\d+\.\d+\.\d+`), cifras con la misma unidad, o «pendiente / sin hacer / falta» frente a «hecho / publicado / ya». La más vieja queda marcada como posiblemente superada por la nueva. Es el caso real de la nota de la 0.2.0.
- **Estado viejo:** una memoria `status` con cambios posteriores en su proyecto (otras memorias guardadas o cambiadas después, o un commit más nuevo en el repo si es git) y más de 7 días sin tocar.
- **Rutas rotas:** `paths` que ya no existen en disco (comprobación barata y que no falla).
- **Sin uso:** creada hace más de 30 días y nunca recordada ni consultada.
- **Demasiado larga:** más de 6.000 caracteres (el recall ya solo mira esos); se propone partirla.

**Dónde se ve:**
- **En el recall:** una memoria marcada sale con una línea delante: `[may be outdated: <id> «título» says …]`. El agente decide y, si procede, la actualiza. Es lo que habría evitado el fallo del 29-oct.
- **Visor:** pestaña **Revisar** con cada aviso y su arreglo a un clic: juntar dos memorias (la nueva absorbe a la vieja y la vieja queda en el historial), actualizar, olvidar o «está bien así» (se apunta y no vuelve a avisar hasta que cambie algo).
- **CLI:** `omoi review [--json]`. En MCP, `context` devuelve también los avisos del proyecto; no hay herramienta nueva.

**Esquema 6:** tabla `review(memory_id, kind, other_id, detail, found_at, dismissed_at)`, recalculada al guardar o cambiar y con `omoi review --all`. Viaja por la sincronización solo lo que se descarta.

**Hecho cuando:** con sus 136 memorias reales, Marc revisa la lista y al menos 7 de cada 10 avisos le parecen útiles; con el corpus público de los tests, cero avisos en memorias que no se contradicen; y el recall sigue por debajo de 30 ms.

### 7.2 · guardar aunque el agente no lo haga ✅ (29-oct-2026, sale con 7.3 y 7.4)
**Hecho así** (`src/candidatas.rs`, esquema 7 con `candidate`, `tests/candidatas.rs`):
- Tres prototipos en Python sobre sus conversaciones reales. Las palabras de decisión («me quedo con…») perdían casi todas las decisiones, que son **respuestas cortas a una pregunta del agente** («¿Cuál te gusta? → Me gusta mucho la 1C, pero no la veo como portada…»), y lo que pillaban no se entendía sin la pregunta. Los «error y luego arreglo» eran casi todos fallos de scripts de un solo uso: fuera. Queda: respuesta a pregunta (con la pregunta), respuestas de `AskUserQuestion`, frases de preferencia o corrección, y el último commit y publicación como hitos.
- Puntúa: preferencia 3, `AskUserQuestion` 2,5, respuesta con opinión («gusta», «pero», «mejor», «veo»…) +1 y por largo, encargos («dame…», «publica…») −1,5; se quedan 5 más los hitos. Fuera lo trivial («vale», «sí»), lo que mete el sistema y **lo que parezca un secreto** (formatos conocidos o un trozo de ≥ 20 caracteres sin separadores con minúsculas, mayúsculas y cifras: en sus conversaciones había un token dentro de un `ssh … echo`, y el primer filtro lo dejó pasar). Lo ya cubierto por una memoria (cobertura ≥ 0,6) no se propone.
- Hooks del mod `classic.SessionEnd` y `classic.PreCompact` → `omoi recall --session-end` (tope 1,2 s; con los 20 MB de una sesión larga tarda 214 ms). Repetir no duplica (`UNIQUE(session, text)`).
- Al empezar sesión en el mismo proyecto (otra sesión), una vez: «From your previous sessions here, possibly worth keeping…»; caducan a los 7 días. Visor: grupo **De tus últimas sesiones** en Revisar con *Guardar como memoria* y *Descartar*; `omoi review` las lista; `GET /api/candidates`, `POST /api/candidates/dismiss`.
- Con esta misma sesión, las 5 propuestas incluyen la decisión de diseño de la ronda 1 y «me gustan todas, haz un plan…»; se escapan las que no responden a una pregunta (el `/btw` sobre el nombre de la orden). El agente siguiente filtra el resto; el criterio de «6 de cada 10 útiles» queda para que Marc lo juzgue con el uso.

**Al cerrar** (hook `SessionEnd` y también `PreCompact`, que es cuando se pierde contexto), Omoikane lee la conversación (`transcript_path` del hook) y saca **candidatas** sin modelo, por señales:
- Frases del usuario que suenan a preferencia o corrección: «prefiero», «no quiero», «siempre», «nunca», «no me gusta», «hazlo así», «te he dicho».
- Decisiones: «vamos con», «me quedo con», «elijo», y la respuesta a una pregunta del agente con opciones (la opción elegida y su motivo).
- Trampas: un error en la salida de una herramienta seguido de un arreglo que funciona (mismo comando que pasa a dar código 0).
- Hitos: commits, tags y publicaciones hechos en la sesión, como estado del proyecto.
- Lo que ya está guardado, o se parece mucho a algo guardado, no cuenta.

**Al empezar la sesión siguiente** en ese proyecto se inyecta, como mucho, 5 candidatas: «From your last session, maybe worth keeping: …». El agente las convierte en memorias de verdad (con `remember`, en el idioma del usuario y con sus palabras) o las descarta, preguntando al usuario si hay duda. Las que no se usan caducan a los 7 días. En el visor se ven en **Revisar**.

**Esquema 6:** tabla `candidate(id, project, session, kind, text, evidence, created_at, shown_at)`. No se sincroniza: es de este equipo.

**Agentes:** primero Claude Code, porque su `.jsonl` lo conocemos. Después los que dan la ruta de la conversación en sus hooks (Codex, Gemini, Qwen), cada uno comprobado en su documentación.

**Hecho cuando:** con las 30 últimas conversaciones reales de Marc (sus `.jsonl`), una media de 5 candidatas o menos por sesión, al menos 6 de cada 10 que Marc guardaría, y ninguna con datos sensibles (contraseñas o tokens: se filtran con los mismos patrones que ya usa la sincronización para las URL).

### 7.3 · recetas ✅ (29-oct-2026, sale con 7.2 y 7.4 en la 0.12.0)
**Hecho así** (`tests/recetas.rs`):
- `procedure` es el quinto tipo. El `CHECK` de `memory` no lo admitía y SQLite no cambia un `CHECK` con `ALTER TABLE`: la migración al esquema 7 lo afloja reescribiendo la definición con `writable_schema` (el procedimiento de la documentación de SQLite para quitar o aflojar restricciones sin tocar los datos). Probado sobre una copia de su base real: 137 memorias intactas, `integrity_check` ok, FTS cuadra.
- `when:` (o `cuando:`) con palabras separadas por comas; el recall la trae **entera** (hasta 4.000 caracteres) cuando el mensaje contiene una palabra entera («publica» sí, «publicación» no), una vez por sesión y solo en su ámbito.
- La sincronización comparte las recetas donde comparte los hechos (las configuraciones de antes no conocían el tipo). El visor les da color propio, filtro y columna en la página del proyecto solo si hay alguna.
- Las instrucciones del agente explican cuándo guardar una receta y que la actualice cuando un paso falle o cambie.

- **Tipo nuevo `procedure`**: título («Publicar una versión de Omoikane») y pasos numerados en el contenido; opcionalmente, una línea `when:` con las palabras que lo disparan («publica», «release», «sacar versión»). Es el único cambio en el modelo de memoria (§3): cinco tipos en lugar de cuatro.
- **Recall:** cuando el mensaje coincide con su `when:` o su título, sale **entera**, no un fragmento de 200 caracteres, con la instrucción de seguirla como lista de comprobación.
- **Aprende:** si un paso falla o cambia, el agente actualiza la receta con `update` y el motivo (por ejemplo, «en Windows renombrar antes omoi.exe, está bloqueado»). El historial guarda cada versión, y el visor la enseña como lista con lo que cambió la última vez.
- **Primera receta, de verdad:** la publicación de Omoikane tal como se hizo con la 0.9.2 y la 0.10.0.

**Hecho cuando:** «publica la 0.10.1» en una sesión nueva trae la receta y el agente la sigue sin explorar; «qué tal la publicación de ayer» no la trae (no es una petición de hacerlo).

### 7.4 · memorias conectadas ✅ (29-oct-2026, sale con 7.2 y 7.3 en la 0.12.0)
**Hecho así** (`src/enlaces.rs`, `memory_link` en el esquema 7, `tests/enlaces.rs`):
- Con sus 137 memorias: 229 `[[enlaces]]` en 113 memorias; resueltos contra las etiquetas (minúsculas, sin tildes, `-` = `_`) quedan 208 enlaces y 9 rotos, que salen en Revisar como *Enlaces rotos*. Se rehacen con la revisión (cuando cambia la base) y al empezar cada sesión.
- Vecina en el recall: una sola, `gotcha` o `preference`, enlazada con una de las inyectadas. Si la enlaza la inyectada (lo dijo quien la escribió) basta cubrir el 10 % del mensaje; si es al revés, el 25 %. Con sus 300 últimos mensajes reales: sale en 2, y aporta en 1 (la K2 trae «comprobar `print_stats` antes de cada G1»; la otra, la preferencia de «gratis para siempre» tras la nota de social-publish, sobra). Los mensajes con algo inyectado siguen siendo 20 de 300.
- El 8.º conjunto a ciegas no se hizo: la 7.4 no toca el orden de la búsqueda (para eso era), solo añade al recall, y eso se midió con los mensajes reales.
- Visor: «Enlaza con» / «La enlazan» en el detalle (llevan a la memoria) y, en la página del proyecto, un grafo SVG propio con las memorias del proyecto que tienen enlaces y las de fuera con las que conectan; etiquetas al pasar el ratón cuando hay muchas. `/api/memories` añade `links`.

- **Enlaces:** los `[[nombre]]` del contenido, resueltos contra las etiquetas y los títulos (las notas importadas usan su nombre de fichero como etiqueta), en una tabla `memory_link(from_id, to_id)` que se actualiza al guardar. Los enlaces rotos salen en **Revisar**.
- **Recall:** cuando se inyecta una memoria, puede llegar también una vecina, solo si es `gotcha` o `preference` y supera el mismo listón de relevancia; nunca más de una por mensaje.
- **Visor:** en el detalle, «Enlaza con» y «La enlazan»; en la página del proyecto, un grafo sencillo (SVG propio, sin librerías).

**Hecho cuando:** un 8.º conjunto de preguntas a ciegas (escrito por otro agente sin ver nada) no empeora en top-1 ni en inyecciones de más frente a la 0.13.0, y en los mensajes reales de Marc la vecina aporta algo en al menos la mitad de las veces que sale.

### 7.5 · 0.13.0 · guardar al cerrar también en los demás agentes ✅ (29-oct-2026)
**Hecho así:** `candidatas::turnos` normaliza la conversación de cada formato en turnos (usuario, agente, pregunta → respuesta, comando) y la puntuación de la 7.2 no cambia. `tests/candidatas.rs` pasa la misma conversación con la forma de Codex, Gemini (JSONL con ids repetidos y el JSON de antes), Qwen, Copilot, Cursor y el genérico, y comprueba que sale la misma decisión y que lo que mete el agente (`<environment_context>`, skills, `<git_status>`) no. `tests/cli.rs`: los hooks de cierre de los siete agentes, también sobre lo que dejó un `setup` de la 0.12 (Kiro y Copilot completados, el plugin viejo de OpenCode rehecho, uno ajeno intacto), y la conversación de Copilot encontrada por su id. Con la conversación real de esta sesión (24 MB, Claude Code) saca lo mismo que la 0.12 en ~240 ms. Sin conversaciones reales de los otros agentes en el PC: las muestras siguen su documentación; si alguno cambia de formato, el lector genérico recoge lo que tenga `role`/`type` y texto.

Lo investigado:
La 7.2 solo lee la conversación de Claude Code. Lo que ofrece cada agente según su documentación (y, donde faltaba, proyectos que leen sus ficheros):

| Agente | Al cerrar | Conversación | Formato |
|---|---|---|---|
| Codex | `SessionEnd` (1 s por defecto, máx. 3 s) y `PreCompact` | `transcript_path` | JSONL `{type:"response_item", payload:{type:"message", role, content:[{type:"input_text"\|"output_text", text}]}}`; comandos en `payload.type:"function_call"` con `arguments` en JSON; el contexto inyectado empieza por `<environment_context>` |
| Gemini CLI | `SessionEnd` (no espera) y `PreCompress` | `transcript_path` (vacío hasta dic. 2025, issue #14715) | `~/.gemini/tmp/<hash>/chats/session-*.jsonl` desde junio de 2026 (antes un `.json` con `messages[]`): `{id, type:"user"\|"gemini", content}`, el mismo `id` se repite mientras crece (vale el último) y líneas `$set` |
| Qwen Code | `SessionEnd` y `PreCompact` | `transcript_path` (`~/.qwen/projects/<slug>/chats/<id>.jsonl`) | registros `type:"user"\|"assistant"` con `message.parts[].text` (estilo Gemini) |
| Copilot CLI | `sessionEnd` **sin** ruta (`agentStop` sí la trae) | `~/.copilot/session-state/<sessionId>/events.jsonl` (antes `<id>.jsonl`) | `{type:"user.message"\|"assistant.message", data:{content}}`; fuera los `source:"skill-…"` y los de subagente (`parentToolCallId`) |
| Cursor | `sessionEnd` y `preCompact` | `transcript_path` (y `CURSOR_TRANSCRIPT_PATH`) | JSONL estilo Anthropic `{role, message:{content:[{type:"text"\|"tool_use"}]}}`; el mensaje real va dentro de `<user_query>…</user_query>`, lo demás lo mete el agente |
| Kiro CLI | `Stop` (solo `session_id`, `cwd`) | `~/.kiro/sessions/cli/<session_id>.jsonl` (lo usa aidlc-workflows; no documentado) | sin documentar: lector genérico |
| OpenCode | plugin: eventos `session.idle` y `session.compacted` | en SQLite (`opencode.db`); el plugin pide los mensajes con `client.session.messages` | `{info:{role}, parts:[{type:"text", text}]}` |
| Antigravity | — | `~/.gemini/antigravity/conversations/*.pb` (protobuf sin esquema público) | **no se puede** |
| Windsurf, Kimi, Pi, Amp, Kilo | sin hooks que sirvan | — | **no** (como el recall automático) |

**Plan:**
- `candidatas` lee una conversación **normalizada en turnos** (usuario, agente, pregunta → respuesta, comando): un lector por formato (Claude Code, Codex, Gemini/Qwen, Copilot, Cursor) que se elige solo por la forma de las líneas, y uno **genérico** (`role`/`type` + `text`/`content`/`parts`) para Kiro, para formatos nuevos y para el JSONL neutro que escribe el plugin de OpenCode (`{"role":"user","text":"…"}`). El resto de la 7.2 (puntuar, secretos, ya guardado, tope) no cambia.
- `omoi recall --session-end` acepta `transcriptPath` y `workspace_roots` (Cursor), y si no llega ruta la busca por el id de sesión: Copilot (`$COPILOT_HOME` o `~/.copilot`) y Kiro.
- `omoi setup` añade los hooks de cierre a Codex (con `timeout: 3`), Gemini, Qwen, Cursor, Copilot y Kiro, y el plugin de OpenCode los eventos (una vez cada 10 min por sesión con `session.idle`, y siempre con `session.compacted`). Para quien ya lo hizo, `setup` los añade a lo que hay; el plugin de OpenCode y los hooks de Kiro y Copilot (que son ficheros solo de Omoikane) se rehacen si son una versión anterior nuestra.
- Pruebas: una conversación de muestra por formato, construida con la forma documentada (en este PC no hay conversaciones reales de estos agentes: los JSONL de `~/.gemini` son de una extensión de VS Code y solo tienen la cabecera), y tests de `setup` para cada agente.
- Fuera: Antigravity (protobuf), y los agentes sin hooks.

### 7.6 · 0.14.0 · se actualiza solo ✅ (29-oct-2026)
El caso: al publicar, nadie que ya tenga Omoikane recibe la versión nueva. Claude Code deja apagada la actualización automática de los marketplaces que no son de Anthropic, y los demás agentes usan el binario que dejó `omoi setup`, que no se actualiza solo (y las funciones con hooks nuevos, como la 7.5, piden además volver a hacer `setup`).

**Hecho así** (`src/actualizar.rs`, `omoi upgrade`):
- La primera ejecución de `omoi` cada día (hook, MCP, CLI) lanza aparte `omoi upgrade --background` (sin ventana, sin quedarse con las salidas del hook) y no espera. Pregunta a GitHub la última Release; si es más nueva **y de la misma versión mayor**, baja `SHA256SUMS`, `SHA256SUMS.sig` y el paquete de su sistema.
- **Firma:** la Release firma `SHA256SUMS` con una clave Ed25519 propia (secreto `OMOIKANE_FIRMA` en GitHub; la privada fuera del repositorio, en el PC de Marc); la pública va dentro de `omoi` y se comprueba con `ring`, que ya venía con rustls (ninguna dependencia nueva). Sin firma o con una firma que no cuadra, no se instala; sin el secreto, el flujo de la Release falla en vez de publicar sin firmar.
- Desempaqueta con el `tar` del sistema (en Windows el de System32, que abre zip), comprueba que el nuevo dice ser la versión esperada y lo pone en su sitio: en Windows renombrando el que está en uso (`omoi.viejo-<versión>-<hora>.exe`, que se borra en una ejecución posterior) y volviendo atrás si falla.
- Luego ejecuta el **nuevo** (`omoi upgrade --after <anterior>`), que añade lo que traiga a los agentes que ya tenían Omoikane (`agentes::configurados`: su fichero de MCP lo nombra) y, si el usuario dijo que sí, la actualización automática del plugin en Claude Code. La sesión siguiente lo avisa una vez.
- **No se toca:** si se apaga (`omoi upgrade --off`, o `OMOIKANE_AUTO_UPDATE=0`), si lo gestiona otro (cargo, Homebrew, Scoop, Nix, snap, `/usr/bin`, o un `target/` de `cargo build`), ni en las compilaciones de depuración (salvo contra el servidor de las pruebas).
- **Antes de migrar** una base con datos a un esquema nuevo se guarda una copia (`<base>.antes-del-esquema-N`, con `VACUUM INTO`): una versión vieja no abre la nueva.
- `omoi setup claude` **pregunta** si se quiere que Omoikane se actualice solo (Intro = sí; `--yes` o sin terminal, sí; `--no-auto-update`, no; en los instaladores, `OMOIKANE_NO_AUTO_UPDATE=1`). La respuesta vale para el binario y para el plugin (`"autoUpdate": true` en la entrada `drac` de `extraKnownMarketplaces` de `~/.claude/settings.json`). El plugin acepta un `omoi` igual o más nuevo que él (antes, uno más nuevo lo cambiaba por el suyo y deshacía la actualización).
- **Probado de punta a punta** con la clave real y un servidor local que hacía de GitHub: un 0.13.0 instalado fuera de `target/` vio la 0.13.1, se negó con la firma manipulada, se actualizó con la buena y, por el camino diario en segundo plano, lo hizo en 2 s y añadió a Codex los hooks de cierre apuntando al binario nuevo. La copia antes de migrar, probada con una base suya en el esquema 5. Un primer intento tocó los ajustes reales de Claude Code (no aislaba `CLAUDE_CONFIG_DIR`): se deshizo y desde entonces solo se tocan con un sí explícito.
- Las versiones hasta la 0.13.0 no tienen firma ni actualización: quien las tenga actualiza a mano una vez, y desde la 0.14.0 ya va solo.

### 0.15.0 · `/omoikane` guarda cuando se lo pides ✅ (30-oct-2026)
Marc, al renombrar la orden de la vista web: «parece un comando para que recuerde algo que se acaba de decir». Con eso no quería quitar `/omoikane`, sino que hiciera eso: forzar un guardado. La vista web se queda en `/omoikane-web`.
- **Qué hace:** el agente repasa la sesión y guarda lo que valga y aún no esté guardado (decisiones, trampas con sus ficheros, preferencias, recetas, hechos), primero con `search` y `update` en vez de copias, y pone al día la memoria `status` del proyecto. Con texto detrás (`/omoikane el deploy va por ssh`) guarda eso. El encargo es el mismo en todos los agentes (`TEXTO_GUARDAR` en `src/agentes.rs`, copiado en `hooks/omoikane.ts`).
- **Claude Code:** la registra el mod. Un `command.run` no puede llamar a `$.prompt.submit` (el motor lo rechaza: esperaría al turno que tiene retenido), ni lanzar otra orden; y una orden Markdown del plugin solo se llama `/omoikane:omoikane`. Lo que funciona es responder `Omoikane: saving.` y enviar el encargo desde un `$.clock.after(0, …)`: entra como turno propio en cuanto la sesión queda libre. Probado con `claude --input-format stream-json` y una base aislada: guardó la memoria y la `status`. Con `claude -p` de un solo mensaje el proceso termina antes de ese turno.
- **Los otros 9 agentes:** `omoi setup` escribe `omoikane.md`/`.toml` junto a `omoikane-web`, con su formato. Lo que se escribe detrás de la orden lo añaden ellos al texto. Una `omoikane.*` de antes de la 0.15 (la de la vista web que escribió Omoikane) se cambia por la nueva; una del usuario no se toca.

### 0.16.0 · Grok Build ✅ (30-oct-2026)
Marc: «¿Omoikane se puede usar con Grok?». Grok Build (`grok`, el agente de terminal de xAI, código en xai-org/grok-build) admite MCP, hooks y órdenes. Comprobado en su documentación (`docs/user-guide/07`, `10-hooks`, `12-project-rules`, `08-skills`, `17-sessions`) y en su código (`ConversationItem` en `xai-grok-sampling-types`):
- **`omoi setup grok`:** MCP en `~/.grok/config.toml` (`[mcp_servers.omoikane]`, como Codex; `GROK_HOME` lo cambia), instrucciones en un fichero propio, `~/.grok/rules/omoikane.md` (Grok carga todos los `.md` de `rules/` en cualquier proyecto), `/omoikane` y `/omoikane-web` en `~/.grok/commands/` (Markdown con `description`, como las órdenes de Claude Code) y hooks en `~/.grok/hooks/omoikane.json`.
- **Sin recall automático:** la salida de su `SessionStart` se ignora y la de un `UserPromptSubmit` que deja pasar se descarta (lo dice su documentación). Por eso las instrucciones piden llamar a `context` al empezar, y `context` devuelve ahora también lo apartado al cerrar sesiones anteriores (`set_aside`, una vez, como el recall). Sirve igual para Windsurf, Kimi, Pi, Amp y Kilo.
- **Guarda al cerrar:** `SessionEnd` (timeout 3; por defecto 1,5 s) y `PreCompact` lanzan `omoi recall --session-end`. Grok no manda la ruta de la conversación, solo `sessionId` y `cwd`: `omoi` la busca en `~/.grok/sessions/<carpeta codificada>/<id>/chat_history.jsonl`. Cada línea es un `ConversationItem` (`type` user/assistant/tool_result); lo que mete Grok (instrucciones, recordatorios) lleva `synthetic_reason` y no cuenta como del usuario; los comandos salen de `tool_calls[].arguments`.
- En Windows, Grok ejecuta los hooks con PowerShell (el binario va con `&` si su ruta tiene espacios).
- **Sin probar con Grok de verdad** (no está instalado en el PC y pide SuperGrok o X Premium+): probado con los formatos de su documentación y su código.

### 0.17.0 · veredictos: lo que engram hace con lo viejo, sin LLM ni herramientas nuevas (30-oct-2026)
La primera crítica pública (la IA de un conocido) dio como pega que el recall «arrastra lo erróneo». Comparado con la documentación de engram (`DOCS.md`, `AGENT-SETUP.md`): engram guarda siempre y devuelve candidatas por FTS5 que el agente juzga con `mem_judge` (`supersedes`, `conflicts_with`, `compatible`, `related`, `scoped`, `not_conflict`); el veredicto se guarda, se sincroniza y sale en `mem_search`; `mem_compare` juzga dos cualesquiera; `mem_review` lista las que pasaron su `review_after` (cada tipo caduca a su ritmo) y salen `needs_review` en la búsqueda; `conflicts scan --semantic` usa un LLM. Omoikane ya avisaba solo en el recall, pero `search` no llevaba la marca y nada de lo que el agente sabía quedaba apuntado. Hecho así (`src/veredictos.rs`, esquema 8, `tests/veredictos.rs`):
- **Tabla `verdict(memory_id, other_id, relation, at, hlc)`**: `superseded` (memory_id es la vieja), `contradicts`, `compatible` y `ok:<aviso>` para «está bien así» en una memoria sola. Los «está bien así» que había en `review` pasan ahí al migrar (probado con una copia de la base real: esquema 8, `integrity_check` ok).
- **El agente los pone con `remember` (`supersedes`, `contradicts`) y `update` (además `compatible`)**: siguen siendo 8 herramientas. Un `update` con solo veredictos no cambia la memoria ni deja una versión vacía en el historial.
- **Vale mientras no cambie lo que juzgó**: `superseded` y `ok:*`, mientras no cambie la juzgada; `contradicts` y `compatible`, mientras no cambie ninguna. De una pareja manda el más nuevo por sello, en cualquier orden, así que no hay que borrar nada y dos equipos acaban de acuerdo.
- **Viajan**: línea `verdict` en la carpeta de la memoria juzgada, solo si las dos memorias se comparten; al importar entra si es más nuevo. Las versiones de antes la ignoran (una línea que no entienden se salta).
- **Dónde salen**: recall (`⚠ superseded by …`, `⚠ contradicts …`), `search` (campo `note`, también con las desfasadas y los estados viejos, que antes solo iban en el recall), `context` y Revisar (grupos Sustituidas, Se contradicen y Toca revisarlas). Una pareja con veredicto ya no sale como desfasada.
- **Fecha de revisión por tipo (`due`)**: decisiones y recetas a los 120 días sin tocar, hechos y trampas a los 180, preferencias nunca, `status` con su aviso de siempre. «Está bien así» pone el reloj a cero. Sale en Revisar y en `search`, **no en el recall**: ahí solo va lo que dice que la memoria puede estar mal, para no meter ruido.
- **Lo que no se copia**: el juicio en cada guardado (en el benchmark, el 49 % de las candidatas de engram no lo eran) y el escaneo con LLM (regla: ningún LLM dentro).
- Con la copia de la base real, el eslogan salía «desfasado» frente a la memoria de estado solo por mencionar otra versión: `update` con `compatible` lo quita, que es justo el caso.

### 0.17.1 · el detector de desfasadas comparaba versiones de temas distintos (30-oct-2026)
Con la base real de Marc salían 6 parejas desfasadas y ninguna lo era: el eslogan («desde la 0.14.1»), la clave de firma («desde la 0.14.0») y la nota de historia (0.2.0), todas contra la memoria de estado y entre sí. Dos causas en `revisar.rs`:
- **Dentro de un proyecto se daban por relacionadas todas las parejas** (`relacionadas = !vieja.project.is_empty() || …`), y el motivo «versión a secas» (la del propio proyecto) salta entre dos memorias cualesquiera que nombren versiones distintas. Ahora una versión a secas solo cuenta si las dos memorias comparten palabras (el mismo criterio que ya se usaba entre globales); una con nombre («rusqlite 0.41.0») sigue contando siempre, porque el nombre ya las ata. El caso que motivó el detector (nota en la 0.2.0 frente a estado en la 0.9.2, que sí comparten palabras) sigue saliendo: lo cubre su test.
- **Un `compatible` caducaba cuando cambiaba cualquiera de las dos** (`veredictos.rs`), y frente a una memoria de estado, que cambia a diario, no duraba nada. Ahora solo caduca si cambia la memoria juzgada; `contradicts` sigue caducando si cambia cualquiera.
- De paso: dos memorias guardadas en el mismo milisegundo no tenían orden y el detector las saltaba. Desempata por el sello `hlc` (y por id), con una consulta más de `(id, hlc)`. Medido intercalado con la copia de la base real, detector recalculado en cada pasada: 81 ms frente a 83 ms de mediana (proceso entero), sin diferencia.
- Comprobado con la copia de la base real sin ningún veredicto: 0 desfasadas (antes 6). Tests nuevos en `tests/veredictos.rs`.

### Para cada versión

Probar en el PC y en marc-server; el visor, con capturas en oscuro, en claro y a 400 px; tests y clippy en los tres sistemas; publicar con la receta de 7.3 en cuanto exista; PLAN y README al día.

## Fase 8 · mejor que engram en lo que le faltaba a Omoikane (plan, 30-oct-2026)

El 30-oct-2026, con la 0.17.0 publicada, se leyó entera la documentación de engram v3.6.0 (README, DOCS, ARCHITECTURE, PLUGINS, AGENT-SETUP, PROJECT-ALIASES, TEAM-USAGE, HTTP-AGENT-WORKFLOWS, CLOUD-README, DOCTOR) y se contrastó cada función con `src/`. Salieron diez carencias reales. Marc: «tenemos que ser mejores en esos 10 puntos, y esto es un producto real y escalable». Así que cada punto se diseña para **superar** lo que hace engram, no para igualarlo, y para miles de memorias, varios equipos y compañeros.

Reglas que no cambian: ningún LLM dentro, **8 herramientas MCP** (lo nuevo entra en las que hay), cero dependencias nuevas salvo necesidad probada, Windows igual que Linux y macOS, migración probada con una copia de la base real antes de publicar, y cada cosa medida con datos reales (el recall sigue por debajo de 30 ms; nada nuevo en su camino caliente).

Qué queda fuera a propósito: el escaneo de conflictos con un LLM (`conflicts scan --semantic`) y Engram Cloud (Postgres, panel multiusuario, permisos). Faltas menores que no entran aquí: `topic_key`, historial de prompts, `mem_stats` por MCP, export a Obsidian, subproyectos, CommandCode.

### 8.1 · 0.18.0 · nada se pierde: papelera y copia fiel ✅ (30-oct-2026)
**Hecho así** (`src/copia.rs`, `tests/copia.rs`, y la papelera en `memoria.rs`), con tres cambios sobre el diseño de abajo:
- **La papelera no es una columna `deleted_at` en `memory`**: es la tabla `forgotten` que ya existía para lo que olvidaba otro equipo, ahora también para los olvidos de aquí (`by` = el equipo que olvidó). Así ninguna de las 25 lecturas de `memory` cambia (ni un `WHERE` más en el recall, ni un sitio donde se pueda colar una memoria borrada), no hay esquema nuevo, y `restaurar` ya devolvía con un sello posterior al olvido, que gana en los demás equipos. `omoi restore [id]` (y `omoi sync restore`, que es lo mismo), `omoi trash [--empty]`, `forget --purge`, `update(id, restore: true)` por MCP, `GET /api/trash` + `POST /api/restore`, y el grupo **Papelera** en Revisar. La purga (30 días, `OMOIKANE_TRASH_DAYS`) es un solo `DELETE` al empezar la primera sesión del día: no hacen falta tandas de 200.
- **El export usa las mismas líneas que la sincronización** (`sync::Linea`, con `versions` solo en el export, y las nuevas `forgotten`, `header` y `end`), y el import las aplica con el mismo motor, `sync::Aplicador`, sacado de `sync::importar`: una sola fusión para lo que llega por git y por fichero. Cabecera con formato, versión y esquema; cierre con recuento y SHA-256 del contenido (cifrado, va cifrado también). Se lee dos veces: entera para comprobar, y luego aplicando. `--encrypt` con la clave de la sync. La ida y vuelta con la base real de Marc (144 memorias, 1 marca, 6 veredictos): 152 líneas idénticas, y el segundo import no cambia nada. Lo que no viaja: `source` y las estadísticas de recall (son de cada equipo).
- **El detector de desfasadas era cuadrático** y salió al medir: con 50.000 memorias, el `al_dia` que corre tras cada escritura tardaba 108 s y 1,2 GB (31 millones de parejas dentro de los proyectos). Ahora todos los proyectos usan el índice invertido que ya usaban las globales (palabras raras más la clave `ver:<cosa>` de las versiones con nombre), que es exacto porque cada motivo exige una de las dos, y `tope` se queda en 100 a partir de 2.000 memorias: 3,5 s y 140 MB. Sigue siendo un recálculo entero por escritura; el incremental (solo las memorias cambiadas contra sus candidatas por FTS) queda para cuando alguien tenga decenas de miles.
- **Medido** (release, intercalado no: una pasada por medida, 50.000 memorias sintéticas de 40 proyectos, fichero de 35,7 MB): export 12 MB de pico y 5,9 s; import en una base vacía 143 MB y 20,7 s (de ellos, 3,5 s y casi toda la memoria son el detector al final); export de la copia 12 MB y 4,8 s. Con la copia diaria: `VACUUM INTO` a un fichero temporal y renombrado, 7 copias, `omoi backup [--list]` y una línea en `doctor`.
- Pendiente del diseño: la TUI no tiene todavía la papelera (sí la web, la CLI y MCP).


**Papelera (punto 2).** engram borra en blando por defecto y en duro si se pide; nosotros solo en duro, y `sync restore` solo recupera lo que olvidó *otro* equipo.
- `forget` deja de borrar: pone `deleted_at` (esquema 9, columna en `memory` con índice parcial) y saca la memoria del FTS y del índice vectorial; la fila, su historial, sus enlaces y sus veredictos se quedan. Toda lectura (`recientes`, `buscar`, recall, `contexto`, `estados`, web, TUI) filtra `deleted_at IS NULL`; es un `WHERE` con índice, no cuesta nada a 30.000 memorias.
- **Restaurar en cualquier sitio:** `omoi restore <id>` (se funde con el `sync restore` de hoy), `restore` por HTTP, botón en la web (grupo **Papelera** en Revisar, con lo olvidado en los últimos 30 días) y en la TUI. Por MCP, sin herramienta nueva: `update(id, restore: true)`. Restaurar reindexa FTS y vector y, por sync, es un cambio con sello posterior a la marca de olvido, así que **gana en los demás equipos** (hoy ya pasa: `hlc_local > tombstone` conserva la memoria). Un `forget` hecho por error en una máquina se deshace desde cualquiera.
- **Purga:** a los 30 días (`OMOIKANE_TRASH_DAYS`, 0 = nunca) o con `forget --purge` / `omoi trash empty`. La purga corre en segundo plano al empezar sesión, de 200 en 200, y es lo único que borra filas.
- Mejor que engram: la papelera es visible y restaurable desde la web, la TUI y la CLI; el historial y los veredictos sobreviven; la restauración viaja.

**Copia fiel (punto 1).** `omoi export` no lleva veredictos y `omoi import` de un export propio pierde historial, ids y fechas. engram tiene un JSON 0.2.0 atómico con pins, relaciones, sesiones y prompts.
- **Formato `omoi-export/2`:** JSONL por líneas, primera línea `{"format":"omoi-export","version":2,"schema":9,"exported_at":…,"machine":…}` y una línea por cosa: `memory` (con `hlc`, `author`, `deleted_at` y su historial), `verdict`, `tombstone`, `candidate` (opcional, `--with-candidates`) y al final `{"end":true,"count":N,"sha256":…}` del contenido. Se escribe y se lee **en streaming**, una línea en memoria cada vez: 100.000 memorias no cargan la RAM. Opción `--encrypt` con la misma AES-SIV de la sync, para guardar la copia en cualquier sitio.
- **Importar un export propio restaura exactamente:** mismos ids, fechas, sellos, historial y veredictos. Sobre una base vacía, `export → import → export` da el mismo fichero (test de ida y vuelta, byte a byte salvo la cabecera). Sobre una base viva, **funde** con las mismas reglas que la sync (gana el sello más nuevo; los olvidos no resucitan), así que un import es idempotente y sirve para juntar dos bases. El import de la versión 1 (el formato de hoy) y el de engram siguen funcionando.
- **Copia automática:** una vez al día, la primera vez que se ejecuta, `VACUUM INTO ~/.omoikane/backups/omoikane-AAAA-MM-DD.db` y se conservan las 7 últimas (`OMOIKANE_BACKUPS`, 0 = apagado). `omoi doctor` dice cuándo fue la última y cuánto ocupan. engram no hace copias solas.
- Hecho cuando: ida y vuelta idéntica con la base real de Marc (142 memorias) y con el corpus de 3.183; import sobre base viva sin cambios la segunda vez; una base de 50.000 memorias se exporta e importa con menos de 100 MB de RAM.

### 8.2 · 0.19.0 · guardar y corregir con cabeza: secretos, veredictos con motivo, edición parcial y juntar ✅ (30-oct-2026)
**Hecho así**, en tres ramas en paralelo (un agente por rama, en worktrees) fusionadas en `master`:
- **Secretos** (`src/secretos.rs`, `tests/secretos.rs`): el detector a mano (prefijos de GitHub, OpenAI, Anthropic, AWS, Slack, GitLab, Google, Stripe, webhook de Slack, PEM, JWT, `usuario:clave@`, `<private>`, y cadenas largas con mayúsculas, minúsculas y cifras), redacción al guardar y al cambiar (solo escrituras locales), segunda capa al exportar a la sync, aviso `secret` en Revisar con «Redactar» (`POST /api/redact`, limpia también el historial) y recuento en `doctor`. **0 falsos positivos** en la base real, en el corpus y en 50.000 memorias; el único que salió (el id de un artifact en una URL) se quitó con «lo que va justo detrás de `/` es un nombre». 37 µs por memoria de 6.000 caracteres; el escaneo de Revisar con 50.000, 184 ms. `OMOIKANE_REDACT=0` lo apaga y `doctor` lo dice.
- **Veredictos con motivo y autor** (esquema 9: `verdict.reason`, `verdict.author`), que viajan por la sync y el export. Y la **cadena de `supersedes`** con `WITH RECURSIVE` (corta ciclos, tope 20): la nota de A lleva a C «(via N)». Es el patrón padre→hijo→nieto por el que se recomendó Datalog; SQLite lo hace sin dependencias. Recall medido intercalado: igual (55 ms de proceso entero).
- **`replace`, `append` y `absorb`** en `update` (`tests/update.rs`), y botón **Juntar** en Revisar.
- **Probado y quitado:** la activación de ACT-R (de los «engram» neurocientíficos) como desempate en la búsqueda y el recall: con los 300 mensajes reales, el primer resultado no cambió en ninguno. Se quedó lo que sí tiene sentido: `due` cuenta también desde `recalled_at`.
- **Por las revisiones independientes** (30-oct): la actualización automática viene **apagada** hasta que la persona diga que sí (antes, sin respuesta, encendida), el README dice qué agentes están probados a mano, y el BENCHMARK pone arriba que los duplicados están cubiertos al 100 % desde la 0.4.0.


**Secretos (punto 4).** engram quita lo marcado `<private>…</private>` en dos capas; nosotros solo filtramos las candidatas (`parece_secreto`).
- `remember` y `update` **redactan solos** antes de guardar: prefijos conocidos (`ghp_`, `sk-`, `AKIA`, `xox?-`, `github_pat_`), bloques PEM, JWT (`eyJ…`), URLs con `usuario:clave@`, y cadenas de ≥ 20 caracteres sin separadores con minúsculas, mayúsculas y cifras (el filtro de las candidatas, que ya pilló un token real). Cada hallazgo pasa a `[redacted:<tipo>]` y la respuesta dice `redacted: N` con los tipos, para que el agente lo sepa y lo diga. `<private>…</private>` también se respeta, por compatibilidad con quien venga de engram.
- **Segunda capa en la sync**, al exportar la línea: lo que se coló (una base de antes de la 0.19.0) no sale del equipo.
- **Auditoría de lo que ya hay:** aviso `secret` en Revisar y en `omoi review`, con *redactar* a un clic, y `omoi doctor` cuenta cuántas hay. engram no mira hacia atrás.
- Sin apagado por parámetro MCP (un agente no debe poder saltárselo); `OMOIKANE_REDACT=0` para quien de verdad lo quiera, y queda apuntado en `doctor`.
- Medida: cero falsos positivos sobre las 142 memorias reales y el corpus de 3.183 (los hashes de commits y los ULID no son secretos: se excluyen por forma), y los 3 secretos sembrados en el banco de pruebas, redactados.

**Veredictos con motivo y autor (punto 8).** engram guarda relación, motivo, evidencia, confianza, actor y modelo, con seis relaciones.
- Esquema 10: `verdict.reason` y `verdict.author`. El motivo sale del `reason` de `update` cuando la llamada solo trae veredictos (sin campo nuevo) y, en `remember`, del propio contenido; el autor es la persona de la sync (o `agent:<cliente>` si no hay). Viajan en la línea `verdict`.
- Dónde se ve: en la nota del recall y de `search` («superseded by X: <motivo>»), en Revisar (quién lo dijo y por qué) y en `omoi review`.
- Relaciones: se queda en **tres**. `related` ya lo cubren los `[[enlaces]]`; `scoped` y `not_conflict` son `compatible`. No hay confianza numérica: un agente se la inventa, y lo que de verdad protege es que el veredicto **caduca cuando cambia lo juzgado**, cosa que engram no hace.
- Mejor que engram: menos juicio (sin candidatas en cada guardado), más contexto (motivo y autor donde el agente lo lee), y veredictos que no se quedan viejos.

**Edición parcial (punto 9).** engram tiene find/replace literal en `mem_update`.
- `update` acepta `replace: [{find, with}]` (literal, cada `find` tiene que aparecer **exactamente una vez**; si no, error con el recuento y nada cambia) y `append: "texto"` (añade al final, para «un paso más» en una receta). Las dos son atómicas con el resto de campos, dejan la versión anterior en el historial, y la respuesta devuelve el fragmento cambiado con 80 caracteres de contexto.
- Mejor que engram: `append`, el «exactamente una vez» (evita cambiar la línea equivocada en una receta larga) y la respuesta que enseña lo cambiado sin pedir `get`.

**Juntar N memorias en una (punto 10).** engram tiene `mem_consolidate` con vista previa y confirmación.
- Sin herramienta nueva: `update(id, absorb: [ids])`. El agente escribe él el contenido fundido (es el modelo; Omoikane no redacta) y las absorbidas **van a la papelera** con un veredicto `superseded` hacia la que queda, sus etiquetas y rutas se suman, los `[[enlaces]]` que apuntaban a ellas pasan a la nueva, y su historial se cuelga de la nueva con la nota «absorbed from <id>». Todo en una transacción.
- Vista previa en la web: botón **Juntar** en una pareja de Revisar (estaba en el diseño de la 7.1), que enseña las dos y pide confirmar; la fusión del texto la hace el agente por `/omoikane juntar <id> <id>` o la persona a mano.
- Mejor que engram: lo absorbido se recupera de la papelera (8.1), ningún historial se pierde, y los enlaces no se rompen.

### 8.3 · 0.20.0 · proyectos de verdad: buscar donde toca y moverlos en bloque

**Esquema 10, hecho así** (30-oct-2026, rama `wt-esquema`, sin subir versión). Es el que congela la 1.0: proyectos y espacios como tablas, `kind` sin `CHECK`, veredictos por reloj lógico, `lector_minimo` y sitio para campos futuros.
- **Tablas.** `space(id, key UNIQUE, name, created_at)`; `project(id, key UNIQUE, name, path, space_id, created_at)` con la fila 0 para lo global (clave vacía), `path` es la carpeta principal (la que enseñan las memorias); `project_path(path PK, path_cmp UNIQUE, project_id, origin, is_foreign, last_seen)` con todas las carpetas de cada proyecto (`path_cmp` en minúsculas en Windows y macOS). La columna se llama `is_foreign` y no `foreign` porque `FOREIGN` es palabra reservada de SQL. `memory` pierde `project` y gana `project_id NOT NULL DEFAULT 0`, `space_id NULL`, `content_hlc` y `extra '{}'`, con `CHECK (project_id = 0 OR space_id IS NULL)`: una memoria es de un proyecto, de un espacio o global. `verdict.extra '{}'`. Locales, que no viajan: `recall_log.project_id` y `memory_use(memory_id, project_id, veces, ultima)`.
- **Índices.** `memory_scope(project_id, kind, updated_at)`, `memory_updated(updated_at)`, `memory_space(space_id)` parcial, `memory_always(project_id, updated_at) WHERE pinned = 1 OR kind = 'preference'`, `project_name`, `project_space`, `project_path_project`. Toda consulta de ámbito es `m.project_id IN (proyecto, 0) AND (…espacio…)`, que va por `memory_scope` (antes, `(?1 IS NULL OR project = ?1 OR project = '')` recorría la tabla).
- **Quién ve qué.** Desde un proyecto: las suyas, las de su espacio y las globales, nunca las de otro espacio (búsqueda, recall, `recent`, `context`, enlaces). «Todos los proyectos» desde un proyecto son los de su espacio (o los que no tienen espacio). Desde la carpeta de usuario, todo. Pedir por MCP o CLI un proyecto o un espacio de otro espacio es un error. Las preferencias sin `project` ni `space` se guardan globales.
- **La clave se calcula una vez**, al crear el proyecto. Una carpeta nueva con la misma clave `git:` (clon, worktree, carpeta movida) solo añade una fila en `project_path`; si la principal ya no existe, pasa a ser esta y el recall de inicio lo dice una vez. Con clave `dir:` no se junta sola (dos carpetas `web` pueden ser cosas distintas): la nueva lleva `~2`, y la vieja sale como «ya no está» en `omoi review` y `omoi doctor` con el `omoi projects move` que la arregla. Un `origin` que cambia se apunta en `project_path.origin` y Revisar lo avisa; la clave no cambia.
- **Migración 9 → 10**, en Rust y en una transacción: copia previa con `VACUUM INTO` (si falla, no se migra), `foreign_keys` apagadas antes del `BEGIN` (con ellas, el `DROP TABLE memory` borraba en cascada historial, rutas y vectores: lo pilló el test), sellos de 4 cifras al formato de 10, una clave por ruta (la de `remote_project` como ruta ajena; la de `sync::clave` si existe; la `dir:` y sin `last_seen` si no), `memory` rehecha con los 12 pasos conservando `n`, `content_hlc` sacado de `updated_at` (así los veredictos que valían siguen valiendo), `review` rehecha como caché, `remote_project` fuera, `foreign_key_check` antes del commit.
- **`lector_minimo`** en `meta`: una versión abre una base de esquema mayor si `lector_minimo` no pasa del suyo, sin tocarla. El 10 lo pone en 10; un cambio futuro que solo añada no lo sube.
- **Veredictos** (`veredictos.rs`): valen mientras `content_hlc <= verdict.hlc` (título, contenido o tipo; fijar, etiquetar o mover no los caducan). `poner` sella después del `content_hlc` de las dos memorias, así que vale aunque el cambio venga de un equipo con la hora adelantada.
- **Tipos desconocidos**: `kind` viaja como texto (`Linea::Memory.kind: String`), se guarda tal cual, se lee como `fact` (`Memoria::kind_text` lo conserva) y se reexporta igual.
- **Sync y export**: la línea lleva `project_key` (de la tabla), `project_path` (informativo), `space_key` y `content_hlc` (los dos con `serde(default)` y solo si hacen falta, así que las líneas de quien no usa espacios no cambian). `omoi sync init --space acme` es el repositorio propio del espacio, con su clave (`Config.space`): lleva solo lo de acme (sus proyectos y lo suyo), nunca globales, y solo acepta líneas de acme. El general no lleva nada de un espacio salvo que `Config.spaces` (opcional, sin opción en la CLI; los `sync.json` de hoy no lo llevan) lo nombre. Reglas en `sync::va_al_repo`. Desde la fusión, la línea de sync lleva también `versions` (como `omoi export`; campo opcional que las versiones de antes ignoran), para que quien se une a un repositorio reciba el historial; y cada olvido va solo al repositorio de su espacio (`tombstone.space_key`, ver «Aislamiento revisado»). Lo comprueba `tests/sync.rs::de_personal_a_empresa`.
- **API**: `remember` acepta `space` (`true` = el del proyecto, o su nombre); `search` y `recent`, `project` (carpeta o trozo de nombre, error si es ambiguo) y `space`; `update`, `space`; `context` devuelve `project` (ruta, clave, espacio, memorias) y `projects` (hasta 50 de su espacio, por actividad, con su `status`, `gone` y `origin_changed`). CLI: `omoi spaces add|list|assign|unassign`, `omoi projects [list]|move <p> [<carpeta>] [--space s]|merge <a> <b>|prune`, `--project`/`--space` en `search` y `recent`, `--space[=nombre]` en `remember` y `update`.
- **`candidate.project` y `session.project` siguen siendo la ruta**: son de este equipo, caducan en días y se buscan por la carpeta de la sesión (que es lo que llega en el hook); pasarlas a id no ahorra nada y añadiría una consulta en cada mensaje.
- **Medido** (release, dentro del proceso, 10 rondas intercaladas antes/después; copias de la base real, 147 memorias, y de la de 50.000 con el proyecto `/p0` en una carpeta real): recall en la base real 12,4 → 12,3 ms desde `D:\Omoikane` y 15,9 → 16,2 ms desde la carpeta de usuario; con 50.000, `recent` de un proyecto 113 → 86 ms, `recent` de todo 171 → 162, `search` de un proyecto 371 → 353, `search` de todo 897 → 924, el recall de un proyecto 594 → 515, y `context` 161 → 203 ms (ahora lleva la lista de proyectos con su `status`, 37 ms). Migración: base real 1,6 s y la de 50.000 4,8 s (proceso entero, con la copia previa); mismas cuentas de memorias, historial, rutas, vectores, veredictos, olvidos y FTS, `integrity_check` ok, y la misma búsqueda y el mismo recall antes y después.
- **Pendiente**: asignar espacio por carpeta padre; *mover a…* y *fusionar con…* como botones en la vista web (hoy el grupo da el comando).

**Formatos que congela la 1.0, hecho así** (31-oct-2026, rama `wt-formatos`, puntos A3, A5 y A6 de la auditoría; sin subir versión). Regla: en la 1.x todo lo que solo añade lo entienden (o al menos lo conservan) los equipos que sigan en la 1.0.
- **A3, campos desconocidos.** `Linea::Memory` pasa a `Memory(CuerpoMemoria)`, y `CuerpoMemoria`, `Tombstone`, `Verdict` y `Forgotten` llevan `#[serde(flatten)] extra` (`sync::Extra`). `flatten` dentro del `tag = "type"` funciona: el `type` no acaba en `extra` y las líneas viejas se leen igual. Al aplicar, `extra` va a `memory.extra` (por `Origen.extra`, en la misma transacción; un cambio de aquí no lo toca, uno que llega de fuera lo sustituye), a `verdict.extra` (`Veredicto.extra`) y, los de un olvido, a `meta('olvido_extra:<id>')`: la tabla `tombstone` no tiene columna y el esquema 10 no se toca por algo que casi nunca llega; se borra al restaurar. En la papelera van dentro del JSON de `forgotten.memory` (`extra`, `extra_forgotten`), que `Memoria` ignora al leerlo. Al exportar (sync y `omoi export`) se vuelven a escribir, sin los que esta versión ya conozca (`solo_desconocidos`, por si una versión vieja guardó uno que ahora es campo). Ejemplo: `{"type":"memory","id":"…","title":"…","hlc":"…","futuro":{"x":1}}` llega, se edita aquí y sale con otro sello y con `"futuro":{"x":1}`. Lo que no se conserva: campos nuevos dentro de `versions[]` (el historial no tiene columna `extra`).
- **A5, lo que se saltó se vuelve a leer.** `meta('sync_lector')` = `LECTOR_LINEAS` (1); si al sincronizar es menor (o no está), se borran `sync_commit*` y se relee todo. Sube cuando una versión aprende a leer un tipo de línea nuevo. Si una versión vieja se salta líneas nuevas en una base que ya usó una más nueva, baja `sync_lector` a la suya, para que aquella relea al volver. Las líneas de tipo desconocido (y las de etiqueta de cifrado que no es `k<n>:`) se cuentan en `Resumen.de_version_nueva` y en `Estado.ficheros_futuros`, aparte de las ilegibles: «N línea(s) de una versión más nueva: actualiza omoi (`omoi upgrade`)» en `omoi sync`, `sync status` y `doctor`. `omoikane.format` en la raíz del repositorio, una línea cifrada con la clave vigente como las de `omoikane.check`, con `{"min_reader":1}`: lo escribe la primera sincronización tras `init` (y la siguiente en los repositorios de antes), no se cambia si existe y tras una rotación se recifra igual. Si pide más que `LECTOR_REPO` (1), ese repositorio ni se lee ni se escribe: error en `omoi sync`, aviso en `status`, `doctor` y al empezar sesión («este repositorio necesita una versión más nueva de omoi»). Se lee del clon, sin red.
- **A6, export tolerante y completo.** Un `type` desconocido dentro de un export entra en el SHA, se cuenta (`Cuenta.unknown`, «unknown» en `--json`) y se salta; la versión del formato sigue en 2 y solo sube con lo que rompa. `Forgotten.memory` tiene la forma de una línea `memory` (`project_key`, `project_path`, `space_key`, `kind` como texto, `content_hlc`, sin `machine`), así que lo restaurado en otro equipo cae en su proyecto y espacio (`alcance_guardado` le da al proyecto el espacio que trae si aquí no tenía); lo olvidado antes del esquema 10 toma la clave de su ruta al exportar. Los de la 0.18/0.19 (el `Memoria` de la API, con `project`) se reconocen por no llevar `project_path`. Ejemplo: `{"type":"forgotten","id":"…","memory":{"id":"…","project_key":"git:github.com/acme/web","project_path":"D:\\web","space_key":"acme","kind":"decision",…},"versions":[],"hlc":"…","deleted_at":"…","by":"pc"}`. Se exportan todos los veredictos (`veredictos::todos` ya no filtra; la sync sigue mandando solo los de memorias que viajan) y van después de la papelera, para que al importar el de una memoria olvidada encuentre su memoria (`Aplicador` acepta los de memorias en la papelera).
- **Vista web.** `GET /api/projects` (proyectos con su espacio y `gone`, y espacios con su nombre). Cada memoria enseña su ámbito: el proyecto con su espacio detrás («webtaxi · Acme»), el espacio («◇ Acme», borde discontinuo) o «Global»; las de espacio tienen su página (`#/proyecto/@espacio:acme`) con sus proyectos. Filtro por espacio en la cronología (y la búsqueda pide `space` al servidor; si no le deja, filtra en la página). Proyectos agrupados por espacio, luego «Sin espacio» y al final lo global. Revisar tiene «Proyectos que ya no están» con `omoi projects move` y `omoi projects merge` listos para copiar. Capturas en `D:\omoi-capturas-020\`.
- **Tests**: `tests/sync.rs` (`un_campo_nuevo_sobrevive_al_pasar_por_este_equipo`, `las_lineas_de_una_version_nueva_se_cuentan_y_se_releen_al_actualizar`, `un_repositorio_que_pide_un_omoi_mas_nuevo_no_se_toca`) y `tests/copia.rs` (`un_campo_nuevo_sobrevive_a_export_e_import`, `la_papelera_viaja_con_su_proyecto_su_espacio_y_sus_veredictos`, `la_papelera_de_un_export_de_la_0_19_sigue_entrando`).

**Aislamiento revisado, hecho así** (31-oct-2026, rama `wt-aislamiento`, sin subir versión). Una revisión independiente rompió «nada de un cliente llega a otro» con cuatro bloqueantes (scripts en `D:\omoi-revision-100\scripts\`). Arreglados, con el esquema 10 aún sin publicar:
- **Persona local.** `meta('persona')` siempre está (`db::asegurar_persona`, al abrir y antes de la migración 9 → 10): la del `sync.json` general o de cualquier repositorio de espacio, o una nueva estable `u-<8 de un ULID>`. Las claves `dir:` son siempre `dir:<persona>/<carpeta>`; nunca `dir:/x` (una base 10 de antes que las tenga se arregla al abrirla). `sync init/join` sin `--another-person` adopta la persona de la sync (`sync::adoptar_persona`: las claves `dir:` de los proyectos con carpeta propia se recalculan con `proyecto::cambiar_persona` y sus memorias se vuelven a sellar); con `--another-person` se queda la local, que es la que va en su `sync.json`. `init` con otro repositorio ya configurado usa la que ya hay. Un `omoi export` lleva la persona en la cabecera, y un import en una base vacía y sin sync la adopta (restaurar en un equipo nuevo); en las demás, `de_clave` con `de_copia` reconoce la carpeta que existe en la ruta de origen si tendría esa clave con la persona de la clave.
- **Lo que entra.** Por un repositorio solo entra lo de su espacio (`va_al_repo` con el `space_key` de la línea); una memoria que aquí está en otro espacio, o cuyo proyecto (por su clave) aquí es de otro espacio, se rechaza. Nada cambia de espacio por algo que llega (el `UPDATE project SET space_id` solo queda para un proyecto recién creado y para `omoi import`), tampoco por una línea `project`: como mucho deja el proyecto en pausa (ver «Una línea `project` solo pausa»). Lo rechazado se cuenta en `Resumen.conflictos_de_espacio` y `Estado.ficheros_conflicto`, y `sync status` y `doctor` lo dicen.
- **Olvidos y veredictos.** `tombstone.space_key` (columna del esquema 10, `''` = de ninguno o de antes) y `space_key` en `Linea::Tombstone`: se rellena al olvidar con el espacio de la memoria, el olvido sale solo hacia el repositorio de su espacio (sin espacio o migrado, solo al general) y lo que llega solo se aplica si la memoria de aquí (viva o en la papelera) es de lo que lleva ese repositorio. Una memoria que llega por la sync solo la para un olvido de su mismo espacio. Un veredicto que llega, solo si sus dos memorias son de ese repositorio. `veredictos::poner` no junta memorias de espacios distintos (una global vale con cualquiera), ni desde la carpeta del usuario: eso cubre también `absorb`.
- **Export.** Sin `--all`, de la papelera, los olvidos (sin copia en la papelera: los de su espacio) y los veredictos (con las dos memorias dentro), solo lo del ámbito (`Ambito::incluye`).
- **Por id.** `get`, `history`, `forget` y `update` (con `supersedes`, `contradicts`, `compatible`, `absorb`, `restore` y `project`) dan «not in this scope» si la memoria es de otro espacio (o de un proyecto sin espacio desde uno con espacio); desde la carpeta del usuario, todo.
- **`projects merge` y `projects move <carpeta>`** entre proyectos de espacios distintos (cuando alguna memoria cambiaría de espacio): error «los proyectos están en espacios distintos («beta» → «acme»)» salvo `--force`.
- **Copia previa** con `VACUUM INTO` a `.tmp` y `rename`; una copia sin tablas (cortada) se rehace.
- **`omoi sync` sin cambios**: el historial de todas las memorias en una consulta (`memoria::historiales`) en vez de una por memoria. Medido (50.000 memorias, copia de `omoi-019\g.db` migrada, 8 rondas intercaladas): antes 3,40 s, ahora 1,98 s (mediana); la 0.19.1, en la misma sesión, 1,97 s.
- **Regla para la 1.x:** un ámbito nuevo (p. ej. equipo) en la 1.x tiene que subir `min_reader`, porque una 1.0 lo trataría como global.
- **Tests**: `tests/sync.rs` (`la_carpeta_sin_remoto_de_antes_de_la_sync_lleva_persona`, `lo_que_llega_por_un_espacio_no_cae_en_otro`, `el_general_no_mete_nada_en_un_espacio`, `olvidos_y_veredictos_no_cruzan_de_espacio`, `export_de_un_cliente_sin_nada_de_otro`, `fundir_proyectos_de_dos_clientes_pide_force`), `tests/esquema.rs` (`la_migracion_pone_persona_en_las_claves_dir`, `una_base_10_sin_persona_se_pone_al_dia`, `por_id_tampoco_se_cruza_de_espacio`, `fundir_entre_espacios_pide_forzar`) y `db::tests::rehace_una_copia_previa_a_medias`. `actualizar::tests::verifica_firma_y_suma` solo en depuración (la clave de pruebas no vale en release).

**El espacio de un proyecto viaja (31-oct-2026, rama `wt-mover`, sin subir versión).** Un revisor encontró un bloqueante en el caso más común de «pasar de personal a empresa» (`D:\omoi-revision-100\scripts\t7c_personal_a_empresa_dos_equipos.py`, `t7d_arreglo_a_mano.py`, `t7b_mover_con_copia.py`): A y C (la misma persona) comparten `web` por el general; A hace `projects move web --space acme` y C se queda con `web` sin espacio, rechaza lo que llega por acme y A lo que C sigue subiendo por el general; el general sigue llevando el proyecto del cliente y el aviso no dice qué proyecto. Al arreglarlo a mano, la versión vieja de C ganaba a la nueva de A porque `asignar_espacio` volvía a sellar todo el proyecto. La primera versión (commit 54af8df) aplicaba la línea `project` del general si era de la misma persona; la persona va en claro, así que se cambió por la pausa de abajo. Lo que queda de ella:
- **`Linea::Project { key, name, space_key, hlc, moved, machine, extra }`** (tipo `project`, carpeta `proyectos/` del repositorio). Cada equipo escribe una por proyecto con clave cuyo espacio cambió alguna vez (`project.hlc`, columna del esquema 10, `''` = nunca; una base 10 sin publicar que no la tenga la recibe al abrirse, `db::al_dia_10`), **solo en el repositorio que lleva ese espacio** (`va_al_repo`): la de un proyecto sin espacio, en el general; la de uno de acme, en el de acme. Ejemplo: `{"type":"project","key":"git:github.com/acme/web","name":"web","space_key":"acme","hlc":"1793450000000-0000000000-pc-a","machine":"pc-a"}`. En el general, la de un proyecto que está en un espacio que el general no lleva va **sin nombre**: `{"type":"project","key":"p-3f1c…","hlc":"…","moved":true,"machine":"pc-a"}`, con `key` = el nombre opaco del proyecto (`Cifra::carpeta`, el de su carpeta de memorias: un hash con la clave del repositorio), sin `name` ni `space_key`. Quien no tiene ese proyecto no sabe de cuál habla ni de qué cliente es; quien lo tiene lo reconoce (`Aplicador::con_opacos`, con todas las claves del repositorio). Sin `person` (ya no decide nada). Sus campos desconocidos van en `meta('proyecto_extra:<clave>')`. Las versiones de antes se la saltan (tipo desconocido); para la 1.0 es conocida desde el principio (`LECTOR_LINEAS` sigue en 1).
- **Por qué hace falta la línea sin nombre** (punto 4 del encargo). Con la línea solo en el repositorio del espacio, C (que aún no está en acme) no se entera nunca y sigue subiendo `web` al general: la fuga. Así que en el general va `moved` sin nombre ni espacio, y basta para pausar con «otro equipo lo pasó a un espacio (el general no dice a cuál)». Cuando C se une a acme, la línea con nombre (mismo `hlc`) completa la pausa con el espacio.
- **Mover no resella.** `proyecto::asignar_espacio` solo pone `space_id` y un `hlc` nuevo al proyecto; las memorias salen con su `space_key` nuevo y su mismo `hlc`: ninguna edición más vieja gana a una más nueva por haberse movido el proyecto (`tests/sync.rs::la_edicion_mas_nueva_gana_aunque_otro_equipo_mueva_el_proyecto`, en los dos órdenes). `fundir` solo resella las memorias que cambian de clave, y `cambiar_persona` sigue resellando (cambia la clave).
- **Avisos de choque.** Cada línea de memoria rechazada porque su proyecto aquí está en otro espacio apunta un `Choque { key, name, here, there, machine }` en `Estado.ficheros_choques`; esos ficheros **se vuelven a leer en cada sincronización**, y `Choque::texto`/`Choque::orden` dicen qué proyecto y qué orden lo arregla. Lo dicen `omoi sync`, `sync status`, `doctor`, `omoi review` (`project_space`) y Revisar («Proyectos en otro espacio en otro equipo»; `GET /api/projects` → `space_conflicts`).
- **`projects move <p> --space <otro>`** de un espacio a otro pide `--force` (como merge y move por carpeta); de ninguno a uno, no.
- **`export --encrypt`** cifra con la clave del repositorio del espacio del ámbito; sin ese repositorio, error con la orden que lo configura. `import` prueba todas las claves del equipo.

**Una línea `project` solo pausa (31-oct-2026, rama `wt-pausa`, sin subir versión).** Un revisor independiente encontró un bloqueante en la línea anterior (`D:\omoi-revision-100\scripts\t8_linea_project.py`, log `v3_t8.log`): se aplicaba si llegaba por el general y `person` era la de esta base, pero la persona es texto sin autenticar (va en claro en las líneas y en las claves `dir:`). Quien tuviera la clave del general podía, escribiendo una línea a mano, sacar un proyecto de un cliente (sus memorias iban al general), meter uno personal en un cliente o pasar uno de un cliente a otro, sin aviso; y un compañero que entra al general sin `--another-person` comparte la persona y movía los proyectos de los demás. Hecho así:
- **Una línea `project` nunca cambia el espacio de nada.** Si dice que un proyecto que aquí está en E1 (o en ninguno) está en E2 ≠ E1 (o, la `moved` del general, que se movió a un espacio y aquí no tiene ninguno), y es más nueva que lo que este equipo sabía (su `project.hlc` o el de la línea que ya lo pausó), el proyecto queda **en pausa**: vale venga por el general o por un repositorio de espacio, y sea de quien sea. Una que dice lo mismo que hay aquí no hace nada (ni apunta su sello, para que una línea escrita a mano no deje el proyecto sordo a los movimientos de después, ni saca de la pausa). Una de un proyecto que aquí no está solo se apunta (en su espacio) si llega por el repositorio de ese espacio. Una con el sello de más de una hora en el futuro no se aplica y se cuenta como no válida (`reloj::del_futuro`): con ese sello, `keep` dejaría el proyecto sordo a los movimientos de verdad.
- **La pausa es local**: `project.paused` (columna del esquema 10, `''` = no; JSON `proyecto::Pausa { space, unknown, hlc, machine, at }`: el espacio que dice la línea, si no se sabe cuál, su sello, el equipo de su sello y cuándo se pausó aquí; `db::al_dia_10` la añade a una base 10 sin publicar). En la tabla y no en `meta` porque es del proyecto: se lee en las mismas consultas que su espacio (exportar y aplicar), se borra con él y `asignar_espacio` la limpia en la misma sentencia que cambia el espacio. No viaja.
- **Qué hace la pausa.** Las memorias del proyecto no se suben a ningún repositorio (`exportar` las salta: el fichero de este equipo deja de llevarlas, ni general ni de espacio), y las líneas de memoria que llegan para él, de cualquier repositorio, se cuentan (`Resumen.en_pausa`, `Estado.ficheros_en_pausa`) y no se aplican; esos ficheros se vuelven a leer en cada sincronización, así que entran en cuanto se decide. Su línea `project` sigue saliendo (dice lo de aquí).
- **Se sale decidiendo aquí**: `omoi projects move <p> --space E2` (acepta: sale de la pausa en E2; el espacio se crea si falta; de un espacio a otro sigue pidiendo `--force`; para «ninguno», `omoi space unassign <p> --force`) o `omoi projects keep <p>` (subacción nueva: se queda donde está). Los dos pasan por `asignar_espacio`, que pone un sello posterior al de la línea que lo pausó (`sello_tras` de ese sello), así que esa línea ya no vuelve a pausarlo, y los demás equipos ven lo decidido aquí (y lo pausan si no coincide: la decisión la toma cada persona en su equipo).
- **Avisos que no se olvidan** (`sync::texto_pausa`, con las dos órdenes, y cómo unirse al repositorio del espacio si este equipo no lo tiene): `omoi sync` (`avisos`, y `en_pausa` en `--json`), `sync status` (`paused` en `--json`), `doctor`, `omoi review` (`project_paused`), Revisar en la vista web (grupo «Proyectos en pausa», con las dos órdenes para copiar; `GET /api/projects` → `paused`) y, al empezar la sesión, una línea para el agente en el recall de `SessionStart` («The project web is paused: another computer says it belongs to the space acme… Ask the user; if they don't know, run `omoi projects keep web`…»; desde `wt-reclamo`, con `keep` primero y `--force` en la otra orden).
- **El caso legítimo** (una persona con dos equipos, `tests/sync.rs::personal_a_empresa_con_dos_equipos`): C ya no converge solo. Tras la línea sin nombre del general, C pausa `web` (deja de subirlo al general, que era la fuga) y avisa; se une a acme, la pausa pasa a decir acme, y tras `omoi projects move web --space acme` converge y gana la edición más nueva. `mover_entre_espacios_con_otro_equipo` y `la_edicion_mas_nueva_gana_aunque_otro_equipo_mueva_el_proyecto` igual, con su `projects move`. Coste conocido: dos carpetas sin remoto de la misma persona con el mismo nombre en espacios distintos (misma clave `dir:`) se pausan la una a la otra; `keep` lo resuelve y desde entonces es el `Choque` de siempre (`lo_que_llega_por_un_espacio_no_cae_en_otro`).
- **El reloj, acotado.** `reloj::sello_tras` con un `previo` de más de una hora en el futuro no adelanta el reloj (como ya hacía `recibir`): el cambio sobre esa memoria (o ese proyecto) le gana, con un sello justo detrás del suyo que no se apunta como el último del reloj, y los sellos siguientes de este equipo siguen en la hora de aquí. Antes, editar una memoria que llegó con un sello de 2040 envenenaba el reloj (todos los cambios siguientes de este equipo, en 2040).
- **Tests**: `tests/sync.rs` (`una_linea_project_solo_pausa_sacar_de_un_cliente`, `…_meter_lo_personal_en_un_cliente`, `…_de_un_cliente_a_otro`: con la clave del general y la persona de A, nada cambia de espacio ni llega a otro repositorio, lo cambiado en pausa no sale, y `projects keep` lo devuelve a su repositorio sin que la misma línea lo vuelva a pausar; `una_linea_project_por_un_cliente_o_sin_nombre_solo_pausa`, por el repositorio de acme, con la `moved` del general y con una de 2040; `un_sello_del_futuro_no_envenena_el_reloj`; y los cuatro de antes, adaptados) y `reloj::tests::el_futuro_no_adelanta_el_reloj`.

**Un cliente no reclama un proyecto por adelantado (31-oct-2026, rama `wt-reclamo`, sin subir versión).** Un revisor independiente encontró el último bloqueante antes de la 1.0 (`D:\omoi-revision-100\scripts\t9_pausa.py`, pasos 2 y 3; log en `v4\t9.log`): el repositorio de acme puede apuntar en acme un proyecto que en este equipo aún no tiene carpeta, con una línea `project` («se apunta en su espacio si lo dice el repositorio de ese espacio») o con una línea `memory` de su clave (`de_clave` lo crea y toma el `space_key` de la línea). Cuando la persona abría después esa carpeta, `registrar` la unía por la clave `git:` a ese proyecto ya en acme y todo lo que guardaba iba a `acme.git`, sin aviso. Sin mala fe también pasa: una librería personal que un compañero usa dentro de acme. Hecho así:
- **La carpeta que se une a un proyecto de un espacio sin que este equipo tuviera ninguna suya deja el proyecto en pausa.** En `proyecto::registrar_avisando`, si `decidir` une la carpeta a un proyecto que está en un espacio y que solo tenía rutas `is_foreign` (o ninguna: lo creó algo que llegó de un repositorio), `pausar_por_reclamo` lo pausa con el mismo `project.paused` y un motivo nuevo, `Pausa::claimed` (`serde(default)`, solo se escribe si es `true`; `space` es el espacio que tiene). Lo mismo en la rama `Memory` de la sincronización si `de_clave` adopta una carpeta que ya existía aquí en la misma ruta y con la misma clave y le pone el espacio de la línea. Si ya estaba en pausa (por una línea `project`), se queda la que tenía. No hay excepciones: también pausa al compañero que se une a acme y abre la carpeta del cliente, o al mismo usuario en su segundo equipo; es una confirmación por proyecto y equipo, una vez.
- **Mientras tanto**, lo de siempre de la pausa: lo guardado en la carpeta se queda en este equipo (no va a acme ni al general) y lo que llega de acme para ese proyecto se cuenta y no se aplica hasta decidir.
- **Se sale decidiendo**: `omoi projects keep <p>` (es de acme: sale de la pausa en acme y lo suyo va a `acme.git`) u `omoi space unassign <p> --force` (es personal: sin espacio, va al general y nunca a acme). Los dos pasan por `asignar_espacio`, que pone un sello posterior, así que la línea de acme ya no lo vuelve a tocar.
- **Avisos**: donde salen las pausas (`omoi sync`, `sync status`, `doctor`, `review`, Revisar en la web y el recall de `SessionStart`), con un texto propio: «esta carpeta es del espacio acme solo porque lo dice el repositorio de acme (este equipo no tenía ninguna carpeta suya)… si es de acme, `omoi projects keep futuro`; si es tuya, `omoi space unassign futuro --force`». Al agente: «Ask the user whether it belongs to acme». Como el recall de `SessionStart` apunta la carpeta antes de escribir el aviso, sale ya en la primera sesión en esa carpeta. Sin la coletilla de cómo unirse al repositorio del espacio (este equipo ya lo tiene: por él llegó).
- **Los avisos de una pausa por línea `project` recomiendan `keep` primero** y no repiten lo que dice la línea como si fuera cierto: «web está en pausa: otro equipo (pc-x) dice que es del espacio acme; aquí está en ninguno. … Si no lo sabes, `omoi projects keep web` (se queda como está); para pasarlo a acme, `omoi projects move web --space acme --force`». La orden de aceptar lleva siempre `--force` cuando propone un espacio, y `projects move <p> --space X` (y `space assign X`) sobre un proyecto en pausa sin espacio aquí exige `--force` (`pausado_sin_espacio_exige_force` en `main.rs`): antes, «de ninguno a uno» no lo pedía y era justo lo que proponía una línea escrita a mano.
- **Tests** (`tests/sync.rs`): `un_cliente_no_reclama_un_proyecto_por_adelantado_con_project` y `…_con_memory` (los pasos 2 y 3 de `t9`: tras abrir la carpeta y guardar, nada llega a `acme.git` ni al general hasta decidir; con `keep` va a acme; con `unassign --force` va al general y nunca a acme; el aviso sale en la primera sesión, en `sync`, `sync status` y `review`) y `un_companero_que_se_une_a_un_cliente_confirma_el_proyecto` (el caso normal: pide confirmación y, tras `keep`, sincroniza en los dos sentidos). Adaptados: `personal_a_empresa_con_dos_equipos` y `la_edicion_mas_nueva_gana_aunque_otro_equipo_mueva_el_proyecto` aceptan el espacio con `--force` (el primero comprueba también que sin él falla), y los que miraban el texto del aviso, al texto nuevo. `de_personal_a_empresa` pasa sin cambios.

**Espacios: separar clientes (añadido el 30-oct-2026, a petición de Marc, pensando en una software factory).** Hoy una memoria es de un proyecto o global, y las globales se ven en todos los proyectos: lo que se guarda como global trabajando para el cliente A le llega al agente en los proyectos del cliente B, y un cliente con varios repos (web, app, backend) no tiene dónde guardar lo común sin hacerlo global. Con la tabla `project` de la auditoría (A1) se añade un nivel entre proyecto y global:
- Tabla `space(id, key, name, created_at)`; `project.space_id` (NULL = sin espacio). La memoria tiene tres alcances: proyecto (`project_id`), espacio (`space_id` sin proyecto) o global (ni uno ni otro). Las preferencias del usuario siguen siendo globales.
- Desde un proyecto se ven las suyas, las de su espacio y las globales; nunca las de otro espacio. Desde la carpeta de usuario se sigue viendo todo, pero `search`/`recent` aceptan `space` como aceptan `project`.
- `remember` acepta `space: true` (o el nombre) para guardar a nivel de espacio; `context` dice en qué espacio estás. Sin herramientas nuevas.
- La sincronización elige por espacio («este repo comparte todo el espacio cliente-a»), además de por proyecto: un repo de sync por cliente, con su propia clave, es el aislamiento entre clientes.
- Cómo se asigna: `omoi spaces add <nombre>` y `omoi projects move <proyecto> --space <nombre>`; opcionalmente por carpeta padre (todo lo que cuelga de `D:\clientes\acme` es del espacio acme).
- Se hace en la 0.20.0 y no después porque es una columna y un filtro ahora, y un cambio de esquema tras la 1.0.
- **Rotar la clave de la sincronización y una clave por espacio** (requisito de Marc para la 1.0, pensando en una software factory). Hoy quien tuvo la clave de un repo lo lee para siempre: no hay forma de sacar a alguien que deja la empresa o un cliente. Diseño: cada línea cifrada lleva el id de su clave (`k1:`, `k2:`…; las de hoy, sin prefijo, son la clave 1), el repo lleva en `omoikane.check` qué claves hay y cuál es la vigente, y `omoi sync rotate` crea una nueva, vuelve a cifrar todo lo de este equipo con ella, la marca como vigente y da el código nuevo para los demás equipos; los que siguen con la vieja dejan de poder leer lo nuevo. Cada espacio puede tener su propio repo y su propia clave (`omoi sync init --space acme`), y un equipo puede estar en varios a la vez. Va en la 0.20.0 porque cambia el formato de la línea de sync, que la 1.0 congela.
- **Para el aprendizaje entre proyectos de la 0.21.0** (que no rompa la 1.0): `recall_log` apunta también el proyecto desde el que se pidió cada memoria (`project_id`), y lo que hoy se borra a los 7 días se resume antes en una tabla local `memory_use(memory_id, project_id, veces, ultima)`. Son tablas locales: no viajan por la sincronización ni entran en el export, así que no tocan los formatos que se congelan.


**Proyecto en `search`, y saber dónde estás (punto 7).** engram tiene `project` en `mem_search`, `mem_current_project` y `mem_list_projects`.
- `search` y `recent` aceptan `project`: una carpeta, o un **nombre o trozo de nombre** («Balua») que se resuelve contra los proyectos conocidos; si encaja con más de uno, error con la lista. `context` devuelve `project` (ruta, clave de sync y cuántas memorias) y `projects` (hasta 50, por última actividad, con su `status` más reciente en una línea). Consultas con índice `memory_project`; a 500 proyectos sigue siendo instantáneo.
- Mejor que engram: resolución por nombre parcial, y todo dentro de las 8 herramientas.

**Mover o fusionar un proyecto en bloque (punto 5).** engram tiene alias, merge, consolidate, prune y rescue de proyectos. Nuestro proyecto es la ruta canónica de la raíz git, así que no hay variantes de nombre, pero si mueves la carpeta las memorias se quedan en la ruta vieja.
- **Detección sola:** al empezar sesión en una carpeta cuyo `origin` coincide con la clave de un proyecto cuya ruta ya no existe en disco, las memorias se mueven a la carpeta nueva (lo mismo que `sync::adoptar` hace hoy con lo que llega de fuera) y se avisa una vez. Sin `origin`, se pregunta en Revisar (grupo **Proyectos que ya no están**, con *mover a…* y *fusionar con…*).
- CLI: `omoi projects move <vieja> <nueva>`, `omoi projects merge <a> <b>` (b absorbe; las memorias conservan id, historial y veredictos; el `status` más nuevo se queda y el otro queda como `superseded`), `omoi projects prune` (proyectos sin memorias). `omoi doctor` lista los proyectos cuya carpeta no existe con el comando que lo arregla.
- Mejor que engram: automático por `origin`, sin ficheros de alias ni configuración; y lo que se mueve viaja ya con la clave de sync correcta.

### 8.4 · 0.21.0 · siempre al día: traer cambios a mitad de sesión y escuchar a los subagentes
**Pasa a la 1.2.0 (Fase 9, §9.2), 31-oct-2026:** la 0.20.0 salió como 1.0.0 y esto solo añade.

**Aprender entre proyectos (añadido el 30-oct-2026).** Lo que se aprende en un proyecto y sirve en otros (la trampa de bitsandbytes en la RTX 5070 Ti, aprendida en Balùa, vale para cualquier proyecto de visión) hoy solo llega si el agente lo guardó como global. Tres piezas, sin LLM y sin cruzar nunca de un espacio a otro:
1. **Recall entre proyectos:** si en un mensaje no pasa la puerta ninguna memoria del proyecto actual, se buscan también las de los demás proyectos del mismo espacio (o de la misma persona sin espacios), con un listón de cobertura más alto y como mucho una, marcada con su origen («de Balùa: …»). Se mide con los mensajes reales antes de dejarlo: cuántas veces trae algo útil y cuántas mete ruido.
2. **Proponer promociones en Revisar:** una memoria de proyecto que se ha recibido o buscado desde dos o más proyectos (`memory_use`) sale como «parece general: ¿hacerla global / de espacio?». «Déjala» es un veredicto `ok:promote`, que ya viaja.
3. **Aviso al guardar:** si una memoria nueva no nombra ningún fichero del proyecto y habla de herramientas (PowerShell, Gradle, una GPU, el sistema), `remember` sugiere guardarla como global. Solo una pista.
No copiar memorias entre proyectos (duplicados que se desincronizan) ni hacer global lo dudoso (es la fuga entre clientes que los espacios evitan).

**Traer cambios a mitad de sesión (punto 3).** Engram Cloud sincroniza en continuo (500 ms) contra su servidor. Nosotros bajamos al empezar sesión y al escribir.
- El hook de cada mensaje (`omoi recall`) comprueba, **fuera del camino caliente**, si hace más de `OMOIKANE_SYNC_PULL_MINUTES` (2) desde el último intento y, si la sync está configurada, lanza `omoi sync --background` desatendido: el recall sigue en 15-26 ms, y la bajada corre aparte con el `Cerrojo` que ya existe. `omoi serve` hace lo mismo con un temporizador mientras está abierto, y la web enseña lo que llega.
- Barato cuando no hay nada: `git fetch` y comparar `HEAD`; si no cambió, no se lee ningún fichero (`por_leer` ya va por diff). Medir el coste de una pasada sin cambios en el PC y en marc-server, intercalado, antes de fijar los 2 minutos.
- Lo que llega a mitad de sesión entra en el recall como cualquier memoria; nada se inyecta por el hecho de llegar. Si una llega marcada `[by Ana]` y tiene que ver con el mensaje, saldrá como siempre.
- Mejor que engram: sin servidor, cifrado, con cualquier remoto git, y con tope de frecuencia para no castigar la red ni el remoto.

**Subagentes (punto 6).** engram captura en `SubagentStop`.
- Hook `classic.SubagentStop` del mod (comprobar que el motor lo expone; si no, leer los transcripts de subagentes que Claude Code deja junto al de la sesión) → `omoi recall --session-end` sobre la conversación del subagente, con la misma puntuación de candidatas y una señal más: **error de herramienta seguido del mismo comando con éxito** en un fichero del proyecto, que en los subagentes sí es señal de trampa (en las sesiones de Marc lo eran casi todo scripts de un solo uso; en un subagente que toca el repo, no). Las candidatas llevan `kind: subagent` y se deduplican contra las del padre.
- Mismo tratamiento en los demás agentes que tengan evento de subagente (Codex, Gemini CLI: comprobar en su documentación).
- Medida: con 30 transcripts reales de subagentes de Marc, 6 de cada 10 candidatas que él guardaría, y nunca más de 5 por sesión en total.

### 1.0.0 · la 0.20.0 se publica como 1.0 (31-oct-2026)
Marc: «terminar la 0.20 y convertirla en la 1.0.0». La 0.20.0 no llegó a publicarse: todo lo de §8.3 (esquema 10 con proyectos y espacios, rotación de claves y un repo por espacio, formatos que no rompen, detector incremental, papelera y copias de antes) sale como **1.0.0**. Lo de §8.4 (pull a mitad de sesión, subagentes, aprender entre proyectos) pasa a la 1.x, porque solo añade.
- **Revisión independiente en cinco pasadas** (un agente que no escribió el código, con scripts que se ejecutan contra repos git locales, la 0.19.1 y la 0.15.0 de crates.io y WSL; todo en `D:\omoi-revision-100\`). Encontró, en este orden: (1) claves `dir:` sin persona y cuatro formas de que algo de un cliente llegara a otro (entrada cruzada, olvidos y veredictos cruzados, export con la papelera de otro); (2) un proyecto pasado a un espacio no llegaba al otro equipo de la misma persona, y mover volvía a sellar; (3) la línea `project`, que se aplicaba con una persona sin autenticar; (4) un cliente podía reclamar por adelantado un proyecto que aquí aún no existía; (5) nada bloqueante, y un aviso que proponía deshacer una decisión de la persona. Todo arreglado y con test; los scripts de cada pasada dejan de reproducir el fallo.
- **Lo que promete la 1.0** (README, «Estable desde la 1.0»): la base, la línea de sync y el export no cambian de forma incompatible en la 1.x; una 1.0 guarda y reenvía lo que no entiende (`extra`), relee lo que se saltó al actualizar (`sync_lector`), se aparta de un repo que pide un lector más nuevo (`omoikane.format`) y abre una base que solo añade (`lector_minimo`); las 8 herramientas no se rompen. Un ámbito nuevo (p. ej. equipo) en la 1.x sube `min_reader`.
- **Lo que no promete:** la 0.19.x y anteriores no se entienden con una base o un repo de la 1.0 tras una rotación de clave o con espacios (se dice en las notas de la versión).
- Medido a 50.000 memorias: `remember` 294 ms (0.19.1: 2,5 s), `update` 263 ms, `omoi sync` sin cambios 1,98 s (igual que la 0.19.1), recall igual que antes. 219 tests en Windows y Linux.

### 1.1.0 · software factory (después de la 1.0; solo añade, sin romper el esquema ni los formatos)
**Pasa a la 1.3.0 (Fase 9, §9.3), 31-oct-2026.**
Marc, 30-oct-2026: que Omoikane sirva igual a una persona con sus agentes que a una software factory con muchos clientes, sin servidor propio. La 1.0 deja el esquema y el formato preparados (espacios, rotación de claves y clave por espacio, en la 0.20.0); esto llega después:
- **Memoria de equipo para quien llega nuevo:** lo fijado del espacio le llega al agente de quien se une, como hoy las preferencias.
- **Agentes sin persona delante** (CI, servidores): `omoi` sin carpeta de usuario, con la base y la clave por variable de entorno, y sin preguntas.
- **Vista de responsable** en la web: espacios, quién ha escrito qué y qué está desfasado, por cliente.
- **Muchas escrituras a la vez:** medir 20 agentes contra la misma base (SQLite escribe de uno en uno; WAL y reintentos) y arreglar lo que salga.
- Lo que no se hace: un servidor propio con usuarios y permisos. El permiso es tener la clave del repo del cliente.

### Orden y por qué
8.1 primero porque todo lo demás se apoya en no perder nada (juntar y mover usan la papelera; la copia fiel es la red de seguridad de cada migración siguiente). 8.2 son cambios de API de `remember`/`update` que conviene sacar juntos para no marear a los agentes con tres versiones de esquema de herramientas. 8.3 toca el modelo de proyecto. 8.4 es lo único con red y procesos de fondo, y va al final para medirlo con calma.

Cada versión: tests nuevos (ida y vuelta, papelera con sync entre dos equipos, redacción con falsos positivos cero, `replace` que no encaja una sola vez, `absorb` con enlaces, mover proyecto por `origin`, pull con tope), migración sobre una copia de la base real, capturas de la web en oscuro, claro y 400 px, README y BENCHMARK al día, y la receta de publicación.


## Fase 9 · la 1.x: confianza, siempre al día y la software factory (plan, 31-oct-2026)

Con la 1.0.0 publicada, la pieza que falta no es una función: es la confianza de quien no conoce el proyecto. Las tres revisiones independientes del 30-oct (comparando con engram) coincidieron en lo mismo: benchmark del autor y no reproducible, demasiadas versiones en pocos días, un solo mantenedor, README enorme. Esta fase empieza por ahí y sigue con lo que quedó de la Fase 8.

**Reglas de la 1.x** (las de siempre, más las que fija la 1.0): solo se añade; un campo, un tipo de línea o una relación nuevos van en `extra` o con `#[serde(default)]` y una 1.0 los conserva; un ámbito nuevo (p. ej. equipo) o cualquier cosa que una 1.0 malinterpretaría sube `min_reader`/`lector_minimo` y es motivo de pensárselo dos veces; nada de LLM dentro; 8 herramientas; el recall por debajo de 30 ms; cada versión con una revisión independiente que intente romperla (como las cinco pasadas de la 1.0) cuando toque la sync, el esquema o el aislamiento.

### 9.1 · 1.1.0 · que se pueda comprobar
- **Benchmark público y reproducible, en un repo aparte** (`omoikane-bench`), enlazado desde el README y el BENCHMARK: un corpus público en inglés (los 3.055 párrafos de READMEs técnicos de la prueba de escala, con sus licencias, más preguntas escritas a ciegas por otro agente y publicadas), un script que instala Omoikane y engram 3.6 desde sus releases y los pasa con las mismas condiciones (intercalado, proceso nuevo por llamada), y resultados con top-1/top-3, **P50 y P95**, **tokens que añade el recall por mensaje**, y escalas de **1.000, 10.000 y 100.000** memorias. Cualquiera lo ejecuta con una orden y obtiene la misma tabla.
- **Test de compatibilidad en la CI**: un binario 1.0.0 fijo sincroniza por el mismo repo con el de cada commit (en los dos sentidos, con campos y líneas nuevas), y abre su base. Si una versión 1.x rompe a la 1.0, la CI lo dice antes de publicar.
- **README a la mitad**: la portada dice qué es, cómo se instala, lo que promete la 1.0 y el benchmark; «Todo lo que hace» pasa a `docs/` (un fichero por tema, plano: `docs/sync.md`, `docs/espacios.md`, `docs/agentes.md`…).
  - **Hecho así (31-oct-2026):** README de 271 a 86 líneas por idioma (de 33 KB a 6 KB): qué lo hace distinto, instalación, «Estable desde la 1.0», una tabla de números con enlace a BENCHMARK y el índice. Lo demás, en seis temas planos, cada uno en inglés y en español (`docs/recall`, `saving`, `memory-care`, `sync-and-spaces`, `agents`, `reference`, con `.md` y `.es.md`). Se añadió lo que faltaba en el README (`omoi sync rotate` y una tabla de variables de entorno), y BENCHMARK enlaza ahora a `docs/recall`.
- **macOS** probado de verdad (la CI ya lo compila; falta una pasada de la revisión con sync y espacios) y **notas de cada versión** en la Release (qué cambia, qué no rompe, con qué versiones se entiende).
- Hecho cuando: el benchmark lo reproduce alguien que no sea Marc, y la CI de compatibilidad está en verde.

### 9.2 · 1.2.0 · siempre al día (lo que era la 8.4)
- **Traer cambios a mitad de sesión**: pull en segundo plano desde el hook de cada mensaje, fuera del camino caliente, con tope (2 min) y `git fetch` barato cuando no hay nada; `omoi serve` lo mismo con un temporizador.
- **Subagentes**: candidatas también de lo que aprenden los subagentes (`SubagentStop`), con la señal de «error y luego el mismo comando con éxito» en ficheros del proyecto.
- **Aprender entre proyectos** (sin cruzar nunca de un espacio a otro): recall de otros proyectos del mismo espacio como último recurso y con listón alto; proponer en Revisar hacer global (o de espacio) lo que se usa desde varios proyectos (`memory_use`, ya en el esquema 10); pista al guardar algo que habla de herramientas y no del proyecto. Cada pieza, medida con los mensajes reales antes de dejarla.

### 9.3 · 1.3.0 · software factory (lo que era la 1.1.0)
- **Memoria para quien llega**: lo fijado del espacio le llega al agente de quien se une a un cliente, como hoy las preferencias.
- **Agentes sin persona delante** (CI, servidores): base, clave y persona por variables de entorno, sin preguntas y sin carpeta de usuario.
- **Vista de responsable** en la web: por cliente, qué hay, quién lo escribió, qué está desfasado o en pausa.
- **Muchas escrituras a la vez**: 20 agentes contra la misma base (WAL, reintentos, `BEGIN IMMEDIATE`), medido antes de prometer nada.
- **Recall sobre una wiki en solo lectura** (la LLM Wiki de Karpathy u otra documentación): que el recall traiga también el trozo de la wiki que toca, sin copiarla a la base. Es la idea con más valor para una factory que ya tiene su documentación.
- Lo que no se hace: un servidor propio con usuarios y permisos. El permiso sigue siendo tener la clave del repo del cliente.

### Deudas conocidas que entran donde encajen
- La TUI no conoce la papelera ni los espacios (9.1 o 9.3).
- Un campo nuevo dentro de `versions[]` se pierde: añadir `memory_version.extra` (solo añade).
- Dos carpetas sin `origin` con el mismo nombre en dos clientes, de la misma persona, se pausan la una a la otra; se resuelve con `keep` y queda como conflicto. Pensar una clave `dir:` que incluya el espacio sin romper las que ya hay.
- A 50.000 memorias, `review --all` tarda unos 2 s y `search` de todo unos 600 ms; recall, `remember` y `sync` ya están bien.
- `export --encrypt` desde un espacio cifra con la clave de ese espacio; una copia de varios espacios a la vez sigue usando la del general.
- Nombres mezclados en tablas y claves locales (`memory_use.veces`, `meta('persona')`): son locales y no se congelan, se dejan.

### Orden y por qué
9.1 primero porque es lo que frena que otros lo usen, y porque la CI de compatibilidad protege todo lo demás. 9.2 antes que 9.3 porque mejora a todos los usuarios, y la factory se apoya en ella (traer cambios a mitad de sesión importa más con varias personas). 9.3 al final, con datos de concurrencia antes de prometerla.

## 12. Pruebas

- Unitarias en cada módulo (`#[cfg(test)]`), integración en `tests/` con base en memoria y modelo `potion-base-8M` descargado a una caché de CI.
- Un test de **coherencia**: tras N operaciones aleatorias, `memory`, `memory_fts`, `memory_vec` y `memory_path` tienen las mismas filas.
- Un test de **concurrencia**: 4 procesos escribiendo a la vez sobre el mismo fichero durante 10 s, sin `SQLITE_BUSY` que escape.
- Un test de **latencia** del recall que falla si pasa de 50 ms.

## 13. Riesgos

| Riesgo | Mitigación |
|---|---|
| El modelo multilingüe tardaba 1,78 s en cargar | resuelto en la fase 0.5: formato compilado y mapeado, 2,7 ms |
| Nuestro troceado deja de coincidir con `tokenizers` si cambia el `tokenizer.json` del modelo | `cargo test` (`tests/equivalencia.rs`) en CI y antes de publicar cualquier modelo nuevo; `fase05 equivalencia` para pasarlo con un corpus grande |
| sqlite-vec sin índice ANN: fuerza bruta, ~1 µs por vector | 5k memorias (uso intenso real) = 6 ms, bien. A partir de 20k (24 ms) probar `int8[256]` en vec0 (4× menos datos) o prefiltrar por proyecto con una columna de partición de vec0 |
| Calidad de model2vec por debajo de MiniLM/E5 | va fusionado con BM25; la palabra exacta siempre cuenta. Backend intercambiable si hace falta ONNX |
| El agente no guarda nada | `SessionStart` le recuerda que existe; `instructions` del MCP dicen cuándo guardar; recordatorio a los 15 min y 10 mensajes. Fase 7.2: candidatas sacadas de la conversación al cerrar |
| Memorias que se quedan viejas y llevan al agente a lo que ya no es | Fase 7.1: avisos de desfase en el recall y pestaña Revisar en el visor |
| Windows (MSVC, rutas, hooks) | se desarrolla en Windows desde el día 1; Linux en el servidor casero; macOS por CI |
| Deriva de alcance (nube, sync, TUI…) | este documento. Lo que no está en §11 no se hace en v1 |
