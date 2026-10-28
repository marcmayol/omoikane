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

Es la pieza que justifica el proyecto. Dos hooks de Claude Code, los dos llamando al binario directamente:

| Hook | Comando | Qué inyecta |
|---|---|---|
| `SessionStart` | `omoi recall --session-start` | `preference` + `pinned` del proyecto y globales. Y una línea: «Tienes memoria: usa `remember` para guardar lo que aprendas» |
| `UserPromptSubmit` | `omoi recall` (lee el JSON del hook por stdin) | top 3-5 por relevancia con el prompt, más `gotcha` cuyos `paths` aparezcan mencionados en el prompt |

Formato de salida: JSON `hookSpecificOutput.additionalContext` con un bloque breve:

```
<omoikane>
[gotcha] src/auth.rs: el middleware no se ejecuta en rutas /health (#01J…)
[decision] Usamos ULID y no UUID por orden temporal (#01J…)
</omoikane>
```

Presupuesto: ≤ 600 tokens por inyección; cada memoria truncada a su `title` + primeras 200 letras. El agente pide `get` si quiere el resto.

**Latencia**: el hook debe responder en < 50 ms en frío. En la fase 0 el modelo tardaba 1,65 s en cargar y el hook iba a quedarse sin semántica; **la fase 0.5 lo resolvió: el modelo abre en 2,7 ms** (ver §9). Así que **el hook hace búsqueda híbrida completa** (FTS + vector + `paths` + pinned) en cada mensaje. Presupuesto estimado: arrancar el proceso + abrir base (1,7 ms) + abrir modelo (2,7 ms) + vector de la consulta (0,4 ms) + KNN y FTS (≈ 7 ms con 5k memorias) ≈ 15 ms.

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

### Fase 3 · MCP (1 día)
- `mcp.rs` con las 8 tools, `instructions`, resolución del proyecto por cwd.
- Probar con Claude Code real (`claude mcp add`) y con el inspector MCP.
- Hecho cuando: una sesión de Claude Code guarda y recupera memorias en este mismo repo.

### Fase 4 · recall automático (1 día)
- `recall.rs`, `hook.rs`, `setup claude` con confirmación.
- Medir latencia del hook en Windows y Linux (< 50 ms).
- Hecho cuando: abres Claude Code en un proyecto y, sin pedirlo, el agente sabe las preferencias y, al mencionar un fichero, le salen los `gotcha` de ese fichero.

### Fase 5 · publicar (½ día)
- `publicar_release.py` (árbol limpio, tag, binarios Windows/Linux/macOS por GitHub Actions).
- `cargo publish` en crates.io.
- README final con GIF del recall funcionando.
- Hecho cuando: `cargo install omoikane` + `omoi setup claude` funciona en una máquina limpia.

Después, si los datos lo piden: hooks para otros agentes, importación (JSON/JSONL/Markdown y convertidor desde otros sistemas), relaciones entre memorias, limpieza asistida de lo que nunca se recupera.

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
| El agente no guarda nada | `SessionStart` le recuerda que existe; `instructions` del MCP dicen cuándo guardar. Medir con `recent` cuántas memorias salen por sesión real |
| Windows (MSVC, rutas, hooks) | se desarrolla en Windows desde el día 1; Linux en el servidor casero; macOS por CI |
| Deriva de alcance (nube, sync, TUI…) | este documento. Lo que no está en §11 no se hace en v1 |
