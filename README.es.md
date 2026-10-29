<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

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

Para recibir las actualizaciones solo, activa una vez la actualización automática del marketplace: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code la deja apagada en los marketplaces de terceros).

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

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos). Si abres las sesiones en tu carpeta de usuario y no dentro de un repositorio, ves las memorias de todos los proyectos, y una memoria nueva va al repositorio al que apuntan sus rutas absolutas (o es global); `omoi update <id> --project <carpeta>` o `--global` la cambia de sitio después. La base está en `~/.omoikane/omoikane.db` (`OMOIKANE_DB` para cambiarla). Todos los comandos aceptan `--json`. Los mensajes salen en inglés; con `OMOIKANE_LANG=es`, en español.

`omoi tui` abre un visor en la terminal: busca mientras escribes, lee una memoria y su historial, fíjala u olvídala.

`omoi serve --open` (o `/omoikane` en Claude Code) abre lo mismo en el navegador, en `http://127.0.0.1:7438/`: todas las memorias, con buscador, filtros por proyecto, tema (etiqueta) o tipo, su historial y botones para fijarlas, llevarlas a otro proyecto u olvidarlas. Solo responde en tu propio equipo y rechaza las peticiones de otras webs.

`omoi serve` sirve las mismas herramientas por HTTP en `127.0.0.1:7438` para scripts y apps que no hablan MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<nombre>` con los argumentos JSON de la herramienta. Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar `Authorization: Bearer <token>`.

### Otros agentes

`omoi setup <agente>` registra el servidor MCP, el recall automático donde el agente lo permite y un párrafo en su fichero de instrucciones. Antes enseña lo que va a cambiar (`--dry-run` para solo mirar), guarda una copia de cada fichero que toca (`<fichero>.antes-de-omoikane`) y no hace nada dos veces.

| Agente | Herramientas (MCP) | Recall automático | Probado |
|---|---|---|---|
| Claude Code | sí (el plugin) | al empezar y en cada mensaje | sí |
| OpenCode | sí | en cada mensaje (un plugin pequeño) | sí, 1.19.5 |
| Codex CLI | sí | al empezar y en cada mensaje (aprueba los hooks una vez con `/hooks`) | según su documentación |
| Gemini CLI | sí | al empezar y en cada mensaje | según su documentación |
| Qwen Code | sí | al empezar y en cada mensaje | según su documentación |
| Kiro | sí | al empezar y en cada mensaje | según su documentación |
| Cursor | sí | solo al empezar (su hook del mensaje no deja añadir contexto) | según su documentación |
| GitHub Copilot (CLI y VS Code) | sí | solo al empezar (su documentación dice que la salida del hook del mensaje se descarta) | según su documentación |
| Antigravity CLI | sí | solo al empezar (no tiene evento de mensaje) | según su documentación |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | sí | no (sus hooks no pueden añadir contexto, o no está documentado) | según su documentación |

`omoi setup` añade también una orden `/omoikane` que abre la vista web, en los agentes que admiten órdenes de usuario: Gemini CLI, Qwen Code, Codex (`/prompts:omoikane`), Cursor, OpenCode, Kiro, Windsurf, Pi y Kilo Code. En esos agentes una orden es un texto para el modelo, así que es el agente quien ejecuta `omoi serve --open` y puede pedir permiso antes.

### Compartir la memoria entre equipos (y con compañeros)

Omoikane no aloja nada: sincroniza a través de un repositorio git tuyo, dedicado solo a este conocimiento (nunca el repositorio del código). Vale cualquier remoto: un repositorio privado en GitHub, GitLab o Codeberg, el Gitea de tu empresa o una carpeta compartida. Usa tus propias credenciales de git.

```
omoi sync init git@github.com:tu-usuario/mi-memoria.git   # en el primer equipo: pregunta qué compartir
omoi sync code                                            # escribe un código que lleva la clave
omoi sync join <código>                                   # en otro equipo (o `--url` para llegar por otra vía)
```

Desde entonces funciona solo y con todos los agentes: después de guardar, cambiar u olvidar una memoria, `omoi` sincroniza en segundo plano (espera 3 segundos y junta en un commit lo que llegue mientras), y baja lo nuevo al empezar cada sesión. `omoi sync status` y `omoi doctor` dicen cómo fue la última sincronización, la vista web también lo enseña, y si lleva más de una hora fallando, la sesión lo avisa al empezar.

- **Qué viaja:** texto, nunca la base de datos. Una línea cifrada por memoria, en un fichero por proyecto y por equipo, así que un cambio es una línea en el diff y el fichero de nadie choca con el de otro. Los vectores y los índices se rehacen en cada equipo.
- **Cifrado en el remoto:** cada línea se cifra (AES-SIV) antes de salir de tu equipo; el repositorio solo guarda texto cifrado y nombres de carpeta opacos. La clave la crea `init` y viaja dentro del código para unirse, así que **pasa el código solo por un canal privado**. Si pierdes todas las copias de la clave, lo del remoto no se puede leer, aunque cada equipo conserva sus memorias.
- **Qué se comparte** se elige en `init`/`join`: los tipos (las preferencias se quedan en tu equipo salvo que digas lo contrario), las globales, qué proyectos y si cada memoria lleva su autor (tu nombre de git, sí por defecto). Con compañeros, sus memorias aparecen marcadas `[by Ana]` para que el agente las trate con cuidado.
- **Qué versión gana:** el cambio más nuevo según un reloj lógico híbrido, así que un equipo con la hora mal no pisa trabajo más reciente. La versión local anterior queda en el historial de la memoria. Lo olvidado no vuelve desde otro equipo.
- **Los proyectos** se reconocen por su remoto `origin` de git: el mismo repositorio en carpetas distintas de equipos distintos es el mismo proyecto. Los que no tienen remoto se reconocen por el nombre de la carpeta, y solo entre los equipos de una misma persona.

`omoi sync off` deja de sincronizar en un equipo; sus memorias se quedan.

### Si vienes de engram

`engram export` y después `omoi import engram-export.json`. Los tipos pasan a los de Omoikane (bugfix → gotcha, decision, preference; el resto, facts con su tipo de engram como etiqueta), `topic_key` pasa a una etiqueta `topic:`, el ámbito personal pasa a global, se conservan las fechas, se saltan las observaciones borradas y lo que ya estaba guardado no se importa dos veces. `--dry-run` enseña adónde iría cada memoria. `omoi import` también lee los ficheros de `omoi export`.

## Comparado con engram

Con las mismas 127 notas reales y preguntas escritas a ciegas (130 de ellas nunca usadas para ajustar), y con 3.183 memorias: la búsqueda entiende la pregunta tal como se escribe (88 % de top-1 con preguntas naturales, cuando engram necesita palabras clave para lo mismo), tarda 34 ms frente a 120-155 ms, con preguntas nunca usadas para ajustar encuentra más veces la nota buena (86-92 % de top-1 con naturales y paráfrasis frente a 83-84 %, 87 % frente a 67 % en inglés), caza los duplicados tanto como engram (100 %) señalando entre un tercio y la mitad de no duplicados, sigue igual de preciso con 3.183 memorias (50 ms por búsqueda frente a 203 ms), y un agente respondiendo con él gastó entre un 29 % y un 40 % menos. Los dos sincronizan entre equipos: engram con exportaciones a git o un servidor que montas tú (Postgres), en claro; Omoikane con un repositorio git tuyo, cifrado y automático. Los dos preparan 14 agentes. El método completo y todos los números, también donde gana engram: [BENCHMARK.es.md](BENCHMARK.es.md).

## Qué hace

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo.

Omoikane guarda eso como memorias cortas y tipadas en un SQLite local, y las devuelve:

- **Búsqueda que funciona en tu idioma.** Las palabras (FTS5, sin importar las tildes, rutas de fichero incluidas) ordenan los resultados; un modelo de embeddings estático y local añade lo muy parecido que no comparte palabras y detecta duplicados al guardar. Medido con notas reales, las palabras solas sacan primero la memoria correcta el 98 % de las veces; los embeddings están para pillar duplicados, no para sustituir eso.
- **Recall automático.** En cada mensaje el plugin ejecuta `omoi recall`. Es un subcomando compilado, sin bash, sin curl, sin servidor, y responde en milisegundos con las 3-5 memorias que importan para lo que acabas de escribir, más los *gotcha* de los ficheros que has mencionado.
- **Pequeño a propósito.** 8 herramientas MCP. Sin demonio ni servidor propio. Un `~/.omoikane/omoikane.db`; la sincronización, opcional, va por un repositorio git tuyo.
- **Historial con motivo.** Actualizar una memoria conserva la versión anterior y, si el agente lo dice, por qué cambió.

## Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

| campo | quién lo pone |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference`) | el agente |
| `paths`, `tags`, `pinned` | el agente, si quiere |
| `project`, `source`, fechas, estadísticas de recall, historial de versiones | Omoikane |

Cada `kind` cambia cómo se recupera: las preferencias y lo fijado se inyectan siempre; los gotcha salen cuando el prompt o los ficheros coinciden; hechos y decisiones, por relevancia. Una memoria con la etiqueta `status` dice en qué punto se quedó el trabajo de un proyecto: al empezar sesión sale la última de cada proyecto con parte de su contenido, para que «sigamos con X» no obligue a buscar.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (SQLite + FTS5 incluidos) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## Licencia

MIT · Marc Mayol
