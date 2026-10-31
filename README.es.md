<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

*Memoria eterna, viva y ultrarrápida.*

**Memoria local para agentes de programación. Un binario, un fichero SQLite, y el agente no tiene que acordarse de buscar.**

[English](README.md)

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente.

## Qué lo hace distinto

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo. Omoikane lo guarda como memorias cortas y tipadas en un SQLite local, y hace lo demás solo.

- **Recuerda solo, y casi siempre calla.** Cuando lo que escribes tiene que ver con algo guardado, un hook le pasa al agente esa memoria en unos 20 ms. Si no tiene que ver, no le pasa nada, y así fue en el 93 % de los mensajes reales. Y al cerrar la sesión guarda lo que al agente se le olvidó.
- **Busca entendiendo la pregunta**, en 101 idiomas, con tildes o sin ellas: con preguntas escritas a ciegas acierta la memoria a la primera el 90 % de las veces.
- **Nada se pierde.** Sin duplicados, con historial y motivo, lo viejo marcado, los secretos tachados, una papelera desde la que todo vuelve y una copia de la base cada día.
- **De ti solo a una empresa entera.** La misma memoria en todos tus equipos y con compañeros, por un repositorio git tuyo y cifrada. Los espacios separan las memorias de cada cliente.
- **15 agentes, una sola memoria.** Claude Code, Codex, Gemini CLI, Cursor, Copilot, OpenCode y los demás; `omoi setup <agente>` deja cada uno listo. Dos están probados a mano, el resto según su documentación.
- **Un binario y ningún servidor.** 8 herramientas MCP, SQLite dentro, ni Python, ni Node, ni Docker. Funciona sin internet, y en Windows igual que en Linux y macOS.

## Instalación

En Claude Code (2.1.310 o más nuevo), dos comandos y nada más:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

La actualización automática viene apagada. `omoi setup claude` y los instaladores de una línea preguntan si la quieres (si no contestas, se queda apagada) y, si dices que sí, encienden también la del plugin; `omoi upgrade --on` la enciende cuando quieras. Para la del plugin, si solo has usado los dos comandos de arriba: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code la deja apagada en los marketplaces de terceros).

En la primera sesión el plugin descarga de las Releases de este repo el binario `omoi` de tu sistema (comprobando su SHA-256) en `~/.omoikane/bin` y después, en segundo plano, el modelo de embeddings (~150 MB, una vez); mientras llega, la búsqueda va por palabras. Cuando el plugin se actualiza, baja el binario de su misma versión. Binarios compilados: Windows x64, Linux x64/ARM64 (glibc 2.35 o más nueva), macOS Apple Silicon e Intel; en cualquier otro, `cargo install omoikane` y el plugin usa el `omoi` del PATH (o pon `OMOIKANE_BIN`). Si prefieres no ejecutar un binario que no has compilado tú, haz lo mismo: `cargo install omoikane` lo compila desde el código publicado en crates.io.

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

## Estable desde la 1.0

Durante toda la 1.x, la base, la línea de sincronización y el formato de `omoi export` no cambian de forma que rompa nada: una 1.3 y una 1.0 sincronizan entre sí, y un equipo con la 1.0 guarda y vuelve a enviar lo que no entiende de una versión más nueva. Las 8 herramientas y sus parámetros tampoco se rompen; solo se añaden cosas. Si algún día hace falta romper algo, será la 2.0, con migración y aviso.

Versión 1.0.0, en crates.io y en las Releases, con integración continua en Windows, Linux y macOS. Se usa a diario en Windows y Linux. El diseño completo y cada decisión están en [PLAN.md](PLAN.md).

## Medido

Con 137 notas reales y preguntas escritas a ciegas por otro agente, que nunca se usaron para ajustar:

| | |
|---|---|
| memoria correcta la primera / entre las tres primeras | 90 % / 97 % |
| búsqueda, mediana | 43 ms (50 ms con 3.183 memorias, igual de precisa) |
| mensajes reales a los que se les inyecta algo | 7 % de 300 |
| hook del recall | 15-26 ms |
| modelo de embeddings ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) | abre en 2,7 ms; 13 MB por proceso |

El método completo y todos los números: [BENCHMARK.es.md](BENCHMARK.es.md). Un benchmark público y reproducible está en preparación.

## Documentación

- [Recall y búsqueda](docs/recall.es.md): cuándo habla y cuándo calla, lo que se queda viejo, la búsqueda en 101 idiomas y qué tiene frente a CLAUDE.md.
- [Qué es una memoria y cómo se guarda](docs/saving.es.md): tipos, duplicados, `/omoikane`, guardar al cerrar la sesión, secretos, recetas y enlaces.
- [Que siga siendo cierta y no se pierda](docs/memory-care.es.md): lo desfasado, veredictos, juntar, fechas de revisión, historial, papelera, export, copia diaria e importar de engram.
- [Sincronización, proyectos y espacios](docs/sync-and-spaces.es.md): compartir entre equipos y con compañeros, el cifrado y un espacio por cliente.
- [Agentes](docs/agents.es.md): el plugin de Claude Code, `omoi setup` y qué funciona en cada uno de los 15 agentes.
- [Terminal, vista web, HTTP y actualizaciones](docs/reference.es.md): la CLI, la TUI y la vista web, la API HTTP, la actualización automática y las variables de entorno.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (SQLite + FTS5 incluidos) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## Licencia

MIT · Marc Mayol
