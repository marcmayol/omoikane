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

### 7.1 · 0.11.0 · memoria que se cuida sola
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

### 7.2 · 0.12.0 · guardar aunque el agente no lo haga
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

### 7.3 · 0.13.0 · recetas: memorias que se ejecutan
- **Tipo nuevo `procedure`**: título («Publicar una versión de Omoikane») y pasos numerados en el contenido; opcionalmente, una línea `when:` con las palabras que lo disparan («publica», «release», «sacar versión»). Es el único cambio en el modelo de memoria (§3): cinco tipos en lugar de cuatro.
- **Recall:** cuando el mensaje coincide con su `when:` o su título, sale **entera**, no un fragmento de 200 caracteres, con la instrucción de seguirla como lista de comprobación.
- **Aprende:** si un paso falla o cambia, el agente actualiza la receta con `update` y el motivo (por ejemplo, «en Windows renombrar antes omoi.exe, está bloqueado»). El historial guarda cada versión, y el visor la enseña como lista con lo que cambió la última vez.
- **Primera receta, de verdad:** la publicación de Omoikane tal como se hizo con la 0.9.2 y la 0.10.0.

**Hecho cuando:** «publica la 0.10.1» en una sesión nueva trae la receta y el agente la sigue sin explorar; «qué tal la publicación de ayer» no la trae (no es una petición de hacerlo).

### 7.4 · 0.14.0 · memorias conectadas
- **Enlaces:** los `[[nombre]]` del contenido, resueltos contra las etiquetas y los títulos (las notas importadas usan su nombre de fichero como etiqueta), en una tabla `memory_link(from_id, to_id)` que se actualiza al guardar. Los enlaces rotos salen en **Revisar**.
- **Recall:** cuando se inyecta una memoria, puede llegar también una vecina, solo si es `gotcha` o `preference` y supera el mismo listón de relevancia; nunca más de una por mensaje.
- **Visor:** en el detalle, «Enlaza con» y «La enlazan»; en la página del proyecto, un grafo sencillo (SVG propio, sin librerías).

**Hecho cuando:** un 8.º conjunto de preguntas a ciegas (escrito por otro agente sin ver nada) no empeora en top-1 ni en inyecciones de más frente a la 0.13.0, y en los mensajes reales de Marc la vecina aporta algo en al menos la mitad de las veces que sale.

### Para cada versión
Probar en el PC y en marc-server; el visor, con capturas en oscuro, en claro y a 400 px; tests y clippy en los tres sistemas; publicar con la receta de 7.3 en cuanto exista; PLAN y README al día.

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
