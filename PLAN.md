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

**Duplicados**: al guardar, si existe una memoria del mismo ámbito con similitud coseno ≥ 0,92 (umbral a calibrar en la fase 2), `save` no crea: devuelve la candidata y propone `update`. El agente decide.

## 4. Esquema SQL

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE memory (
  id           TEXT PRIMARY KEY,
  project      TEXT NOT NULL DEFAULT '',
  kind         TEXT NOT NULL CHECK (kind IN ('fact','decision','gotcha','preference')),
  title        TEXT NOT NULL,
  content      TEXT NOT NULL,
  paths        TEXT NOT NULL DEFAULT '[]',
  tags         TEXT NOT NULL DEFAULT '[]',
  pinned       INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  recalled_at  TEXT,
  recall_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX memory_project ON memory(project, kind);
CREATE INDEX memory_pinned  ON memory(project) WHERE pinned = 1;

CREATE TABLE memory_version (
  memory_id  TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  version    INTEGER NOT NULL,
  title      TEXT NOT NULL,
  content    TEXT NOT NULL,
  kind       TEXT NOT NULL,
  paths      TEXT NOT NULL,
  tags       TEXT NOT NULL,
  changed_at TEXT NOT NULL,
  reason     TEXT,
  PRIMARY KEY (memory_id, version)
);

-- rowid entero para enlazar FTS y vec con el id ULID
CREATE TABLE memory_rowid (
  rowid INTEGER PRIMARY KEY,
  id    TEXT NOT NULL UNIQUE REFERENCES memory(id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags,
  content='', tokenize='unicode61 remove_diacritics 2'
);
-- índice externo (content=''): se mantiene a mano en las mismas transacciones que memory

CREATE VIRTUAL TABLE memory_vec USING vec0(
  embedding float[256] distance_metric=cosine
);

-- paths desplegados para el recall por fichero
CREATE TABLE memory_path (
  memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  path      TEXT NOT NULL,
  PRIMARY KEY (memory_id, path)
);
CREATE INDEX memory_path_path ON memory_path(path);

CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
-- meta: schema_version, embedding_model, embedding_dim
```

Notas:
- `remove_diacritics 2` hace que «autenticación» y «autenticacion» sean la misma palabra. Sin stemming en v1: el vector cubre las variantes morfológicas.
- La dimensión del vector se fija al crear la base según el modelo (`meta.embedding_dim`). Cambiar de modelo implica `omoi reindex`.
- FTS, vec, paths y memory se escriben en **una sola transacción**. Nunca hay memoria sin índice.

## 5. Búsqueda y ranking

```
candidatos_fts  = top 40 por bm25(memory_fts, 3.0, 1.0, 2.0)   -- título pesa 3, tags 2
candidatos_vec  = top 40 por distancia coseno en memory_vec
fusion          = RRF con k = 60:  score = Σ 1 / (k + rango_i)
boosts          = ×1.5 si pinned · ×(1 + 0.3·recencia) con recencia = 1/(1 + días/90)
resultado       = top N tras filtrar por project/kind/tags
```

- Sin embedding disponible (modelo no descargado, `--no-vec`), la búsqueda es FTS sola. Nunca falla por falta de modelo.
- Términos de menos de 3 letras van igual por FTS (unicode61 no tiene el límite del trigram).
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

**Caché fría** (primer mensaje tras reiniciar). En el PC (NVMe, Windows): abrir 4,7 ms, primer vector ~15 ms. En marc-server (SSD SATA barato, Linux): abrir ~6-10 ms, primer vector **~45 ms**, así que el primer mensaje del día en un disco así se pasa algo del presupuesto (≈ 55-60 ms en total). Pasa una vez por arranque del equipo; después todo es caché. `madvise(RANDOM)` en la tabla y los pesos ayuda poco pero ayuda (A/B intercalado, 6 pasadas: primer vector ~50 → ~44 ms, los 10 siguientes ~170 → ~130 ms); precargar el trie (`WILLNEED`) no ayudó. Una medida anterior que daba ~21 ms estaba sesgada por el orden de las pruebas (la caché interna del SSD): por eso las comparaciones se hacen siempre intercaladas.

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
  - Se publicará en Hugging Face bajo `marcmayol/` en la fase 5. Hasta entonces, `omoi model` puede generarla desde el f32 oficial (es el código de `examples/fase0.rs`).
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
- Alternativa ligera para quien solo escriba en inglés: `potion-base-8M` (30 MB, 256 dims). Se elige con `omoi model`.
- Vector = `title + "\n" + content`, normalizado (coseno).
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
- [x] Linux (marc-server, Ubuntu 24.04): compila sin avisos, `doctor` y `cargo test` pasan, int8 bajado de Hugging Face; RAM: PSS ~23 MB y ~2 MB privados por proceso con 4 a la vez.
- [x] Guardia de compilación para plataformas big-endian.
- [ ] macOS: sin máquina; en la CI de la fase 5.
- [x] RAM: 13 MB privados por proceso con el modelo compartido (§6).
- [x] Test de equivalencia en `cargo test` (`tests/equivalencia.rs` + `tests/corpus.txt`, 1.840 frases públicas). Se salta si no está el modelo. Comprobado que falla: cambiar un `>` por `>=` en el desempate del Viterbi rompe 1 frase.
- [x] Diseño de versiones del modelo en disco para Windows (§9).

Similitud semántica de muestra (int8), con «fallo de login en la pantalla de acceso»: «login failure on the sign-in screen» **0,558** · «bug en auth: el token caduca antes de tiempo» **0,245** · «el middleware de autenticación…» 0,142 · temas ajenos entre −0,04 y 0,12. Ordena bien y cruza idiomas, pero con márgenes cortos en español: por eso va siempre fusionado con BM25 y nunca solo.

### Fase 1 · núcleo (2 días)
- `db.rs`, `memoria.rs`, `proyecto.rs`, `buscar.rs` (solo FTS).
- CLI: remember, search, get, update, forget, history, recent, projects, export.
- Tests de integración con base en memoria: guardar/buscar en español con acentos, update crea versión, forget borra todo, FTS y tabla siempre coherentes.
- Hecho cuando: `omoi remember` + `omoi search "fallo de autenticación"` encuentra «bug en auth» por FTS con la palabra y los tests pasan.

### Fase 2 · vectores (1 día)
- `embeber.rs`, tabla `memory_vec`, RRF, dedupe por similitud, `omoi model`, `omoi reindex`.
- Calibrar umbral de duplicado con 30 pares reales.
- Hecho cuando: «fallo de login» encuentra «bug en auth» sin compartir palabras, y guardar dos veces lo mismo con otras palabras propone `update`.

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
