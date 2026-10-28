# Omoikane

**Memoria local para agentes de programación. Un binario, un fichero SQLite, y el agente no tiene que acordarse de buscar.**

[English](README.md)

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente.

## Estado

En desarrollo. El diseño completo y las fases están en [PLAN.md](PLAN.md).

Funciona ya: el almacén de memorias y su CLI (fase 1), la búsqueda por palabras (y por significado como rescate) con aviso de duplicados al guardar (fase 2) el servidor MCP para agentes (fase 3) y el recall automático con hooks de Claude Code, empaquetado como plugin (fase 4). La fase 5 añade binarios compilados e instaladores de una línea. La CI corre en Windows, Linux y macOS. El modelo de 101 idiomas ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) abre en **2,7 ms** en vez de 1,78 s, con los mismos ids que `tokenizers` en 165.282 frases, y cuatro procesos lo comparten con 13 MB de memoria privada cada uno. Probado en Windows y Linux.

## Instalación

En Claude Code (2.1.310 o más nuevo), dos comandos y nada más:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

En la primera sesión el plugin descarga de las Releases de este repo el binario `omoi` de tu sistema (comprobando su SHA-256) en `~/.omoikane/bin` y después, en segundo plano, el modelo de embeddings (~150 MB, una vez); mientras llega, la búsqueda va por palabras. Cuando el plugin se actualiza, baja el binario de su misma versión. Binarios compilados: Windows x64, Linux x64/ARM64 (glibc 2.35 o más nueva), macOS Apple Silicon e Intel; en cualquier otro, `cargo install omoikane` y el plugin usa el `omoi` del PATH (o pon `OMOIKANE_BIN`).

**Sin Claude Code**, o para usar `omoi` desde la terminal u otro cliente MCP, instala el binario con una línea:

**Windows** (PowerShell)
```powershell
irm https://raw.githubusercontent.com/marcmayol/omoikane/master/install.ps1 | iex
```

**Linux y macOS**
```sh
curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh | sh
```

O `cargo install omoikane`. Los instaladores también añaden el plugin si Claude Code está instalado (`OMOIKANE_NO_SETUP=1` para no hacerlo, `OMOIKANE_NO_MODEL=1` para no bajar el modelo).

## Uso

Con el plugin, Claude Code lo hace solo: recibe 8 herramientas (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`) y, en cada mensaje, se le inyectan las memorias que de verdad tienen que ver (o ninguna). El plugin es un mod de Claude Code: ejecuta el binario directamente, sin servidor MCP ni scripts de shell, igual en Windows, macOS y Linux. Cualquier otro cliente MCP puede usar `omoi mcp` directamente.

Desde la terminal:

```sh
cd tu-proyecto
omoi remember -k gotcha -t "El token caduca antes de tiempo" --path src/auth.rs \
  "El middleware compara la hora local con la caducidad del token, que va en UTC."
omoi search "token caduca"
omoi recent
OMOIKANE_LANG=es omoi --help
```

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos). La base está en `~/.omoikane/omoikane.db` (`OMOIKANE_DB` para cambiarla). Todos los comandos aceptan `--json`. Los mensajes salen en inglés; con `OMOIKANE_LANG=es`, en español.

## Qué hace

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo.

Omoikane guarda eso como memorias cortas y tipadas en un SQLite local, y las devuelve:

- **Búsqueda que funciona en tu idioma.** Las palabras (FTS5, sin importar las tildes, rutas de fichero incluidas) ordenan los resultados; un modelo de embeddings estático y local añade lo muy parecido que no comparte palabras y detecta duplicados al guardar. Medido con notas reales, las palabras solas sacan primero la memoria correcta el 98 % de las veces; los embeddings están para pillar duplicados, no para sustituir eso.
- **Recall automático.** En cada mensaje el plugin ejecuta `omoi recall`. Es un subcomando compilado, sin bash, sin curl, sin servidor, y responde en milisegundos con las 3-5 memorias que importan para lo que acabas de escribir, más los *gotcha* de los ficheros que has mencionado.
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
