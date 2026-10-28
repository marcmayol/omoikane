# Omoikane

**Memoria local para agentes de programación. Un binario, un fichero SQLite, y el agente no tiene que acordarse de buscar.**

[English](README.md)

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente.

## Estado

En desarrollo. El diseño completo y las fases están en [PLAN.md](PLAN.md).

Funciona ya: el almacén de memorias y su CLI (fase 1), la búsqueda por palabras (y por significado como rescate) con aviso de duplicados al guardar (fase 2) y el servidor MCP para agentes (fase 3). Lo siguiente: el recall automático con hooks (fase 4). La CI corre en Windows, Linux y macOS. El modelo de 101 idiomas ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) abre en **2,7 ms** en vez de 1,78 s, con los mismos ids que `tokenizers` en 165.282 frases, y cuatro procesos lo comparten con 13 MB de memoria privada cada uno. Probado en Windows y Linux.

## Probarlo

Hace falta [Rust](https://rustup.rs). Después:

```sh
cargo install --git https://github.com/marcmayol/omoikane
cd tu-proyecto
omoi remember -k gotcha -t "El token caduca antes de tiempo" --path src/auth.rs \
  "El middleware compara la hora local con la caducidad del token, que va en UTC."
omoi search "token caduca"
omoi model install     # opcional, ~150 MB una vez: añade búsqueda por significado y aviso de duplicados
omoi recent
OMOIKANE_LANG=es omoi --help
```

Para darle la memoria a Claude Code (o a cualquier cliente MCP):

```sh
claude mcp add omoikane --scope user -- omoi mcp
```

El agente recibe 8 herramientas (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`) e instrucciones de cuándo usarlas.

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos). La base está en `~/.omoikane/omoikane.db` (`OMOIKANE_DB` para cambiarla). Todos los comandos aceptan `--json`. Los mensajes salen en inglés; con `OMOIKANE_LANG=es`, en español.

## Qué va a hacer

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo.

Omoikane guarda eso como memorias cortas y tipadas en un SQLite local, y las devuelve:

- **Búsqueda que funciona en tu idioma.** Las palabras (FTS5, sin importar las tildes, rutas de fichero incluidas) ordenan los resultados; un modelo de embeddings estático y local añade lo muy parecido que no comparte palabras y detecta duplicados al guardar. Medido con notas reales, las palabras solas sacan primero la memoria correcta el 98 % de las veces; los embeddings están para pillar duplicados, no para sustituir eso.
- **Recall automático.** Un hook de Claude Code ejecuta `omoi recall` en cada mensaje. Es un subcomando compilado, sin bash, sin curl, sin servidor, y responde en milisegundos con las 3-5 memorias que importan para lo que acabas de escribir, más los *gotcha* de los ficheros que has mencionado.
- **Pequeño a propósito.** 8 herramientas MCP. Sin demonio, sin puerto, sin nube, sin sincronización. Un `~/.omoikane/omoikane.db`.
- **Historial con motivo.** Actualizar una memoria conserva la versión anterior y, si el agente lo dice, por qué cambió.

## Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

| campo | quién lo pone |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference`) | el agente |
| `paths`, `tags`, `pinned` | el agente, si quiere |
| `project`, `source`, fechas, estadísticas de recall, historial de versiones | Omoikane |

Cada `kind` cambia cómo se recupera: las preferencias y lo fijado se inyectan siempre; los gotcha salen cuando el prompt o los ficheros coinciden; hechos y decisiones, por relevancia.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (SQLite + FTS5 incluidos) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## Licencia

MIT · Marc Mayol
