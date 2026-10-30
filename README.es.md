<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

*Memoria eterna, viva y ultrarrápida.*

**Memoria local para agentes de programación. Un binario, un fichero SQLite, y el agente no tiene que acordarse de buscar.**

[English](README.md)

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente.

## Qué lo hace distinto

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo. Omoikane lo guarda como memorias cortas y tipadas en un SQLite local, y hace lo demás solo.

### Lo que hace en cada sesión

- **Recuerda sin que se lo pidas.** En cada mensaje, un hook le da al agente las 3-5 memorias que importan para lo que acabas de escribir, más las trampas de los ficheros que has nombrado, en unos 20 ms. El agente no tiene que acordarse de buscar.
- **Busca bien en tu idioma**, con tildes o sin ellas y en 101 idiomas: con preguntas escritas a ciegas acierta la memoria a la primera el 90 % de las veces, en 43 ms, y sigue igual de preciso con 3.183 memorias.
- **Detecta duplicados al guardar.** Si el agente intenta guardar algo que ya sabe, se le dice y actualiza la memoria que hay en lugar de crear otra.
- **Funciona con 15 agentes.** Claude Code, Codex, Gemini CLI, Qwen Code, Cursor, Copilot, Kiro, OpenCode, Grok y los demás comparten la misma memoria; `omoi setup <agente>` deja cada uno listo.
- **La misma memoria en todos tus equipos, y con compañeros.** Se sincroniza sola por un repositorio git tuyo, cifrada antes de salir del equipo, y sin servidor de nadie.
- **Historial con motivo.** Cambiar una memoria guarda la versión anterior y, si el agente lo dice, por qué cambió.
- **La ves y la ordenas tú.** Una vista web y otra en la terminal para buscar, leer el historial, fijar, mover u olvidar.
- **Privado y pequeño.** Un binario, un fichero, 8 herramientas MCP y ningún servidor propio.

### Lo que mantiene viva la memoria

- **Guarda aunque el agente se olvide.** Al cerrar la sesión, Omoikane lee la conversación y aparta lo que decidiste y lo que pediste; la sesión siguiente guarda lo que valga, con tus palabras.
- **Guarda cuando se lo pides.** `/omoikane` repasa la sesión y guarda lo que valga; `/omoikane <texto>` guarda eso.
- **Se da cuenta de cuándo una memoria se ha quedado vieja.** Si una más nueva contradice a otra, la vieja le llega al agente marcada con `⚠ may be outdated`, y la pestaña Revisar enseña las dos una al lado de la otra.
- **Recetas que vuelven enteras.** Una memoria de tipo `procedure` guarda los pasos de algo que se repite; «publica la 0.14.1» trae la lista entera, y el agente corrige el paso que cambie.
- **Memorias conectadas.** Los `[[enlaces]]` entre notas viajan con ellas: al recordar una, llega también el aviso que lleva enlazado.
- **Retoma donde lo dejaste.** Al empezar la sesión llega la última memoria `status` de cada proyecto, así que «sigamos con X» no obliga a buscar.
- **Se mantiene al día solo**, y solo instala versiones firmadas con la clave de Omoikane.

## Estado

Versión 0.16.0, en crates.io y en las Releases, con integración continua en Windows, Linux y macOS. Se usa a diario en Windows y Linux. El diseño completo y cada decisión están en [PLAN.md](PLAN.md).

## Instalación

En Claude Code (2.1.310 o más nuevo), dos comandos y nada más:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

`omoi setup claude` y los instaladores de una línea preguntan si quieres que Omoikane se actualice solo y, si dices que sí, encienden también la actualización del plugin. Si solo has usado los dos comandos de arriba, actívala una vez: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code la deja apagada en los marketplaces de terceros).

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

## Todo lo que hace

Una por una, todas las funciones, con el detalle de cómo trabajan.

### Empezar

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

### Recuerda solo

Al empezar la sesión le llegan al agente tus preferencias y lo fijado, en qué punto se quedó cada proyecto, lo último en lo que se trabajó y los avisos de lo que conviene revisar. En cada mensaje solo le llega algo si tiene que ver de verdad: una memoria tiene que cubrir bastantes palabras poco frecuentes del mensaje, y las trampas salen también cuando nombras sus ficheros. Cada memoria llega una vez por sesión. En 300 mensajes reales solo inyectó algo en un 7 % de ellos. Si en 15 minutos y 10 mensajes no se ha guardado nada, le recuerda que lo haga.

### Busca en tu idioma

La búsqueda empieza por palabras: le dan igual las tildes, mira también las rutas de fichero y da más peso a lo poco frecuente. Después, un modelo de embeddings local que abre en 2,7 ms rescata lo que se parece mucho aunque no comparta ninguna palabra. Dónde busca depende de dónde estés. Desde un proyecto, en él y en lo global; desde tu carpeta de usuario, en todo.

### Guarda sin duplicados

Cada memoria tiene un tipo que decide cuándo vuelve: `fact` y `decision` por relevancia, `gotcha` también por sus ficheros, `preference` siempre y `procedure` entera cuando la pides. Antes de guardar mira si ya existe algo casi igual. Si lo hay, no la guarda y dice cuál actualizar; si solo se parece, la guarda y enseña las relacionadas. Avisa cuando pasa de los 6.000 caracteres que cuentan para el recall y cuando deja anticuada a otra. Las rutas se guardan relativas al repositorio, así que valen en cualquier equipo.

### Guarda cuando se lo pides

`/omoikane` hace que el agente guarde en ese momento. Repasa la sesión, guarda lo que valga y aún no esté guardado (decisiones con su motivo, trampas con sus ficheros, preferencias, recetas) y pone al día la memoria `status` del proyecto. Con texto detrás, como en `/omoikane el deploy va por ssh`, guarda eso con tus palabras. Termina con una línea por memoria guardada o cambiada.

### Guarda aunque el agente se olvide

Al cerrar o compactar una sesión, Omoikane lee la conversación y aparta lo que quizá valga la pena guardar: lo que respondiste a las preguntas del agente (con la pregunta), tus preferencias y correcciones, y el último commit o publicación. La sesión siguiente en ese proyecto lo recibe una vez, y el agente guarda lo que valga con tus palabras; la pestaña Revisar también lo enseña. Nada que parezca un secreto ni lo que ya está guardado. Lee la conversación de Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro, OpenCode y Grok Build (`omoi setup <agente>` añade los hooks de cierre); no la de Antigravity, que la guarda en un formato binario sin documentar. En los agentes sin recall al empezar, como Grok, lo apartado llega con la herramienta `context`.

### Se cuida sola

Omoikane se da cuenta de cuándo una memoria se ha quedado atrás: otra más nueva del mismo tema dice algo distinto (una versión más alta de lo mismo, o hecho lo que estaba pendiente), una memoria `status` que el proyecto ya ha dejado atrás, rutas que ya no existen, memorias que en un mes no se han usado nunca y notas demasiado largas para el recall. Cuando una de esas le llega al agente, va marcada con `⚠ may be outdated: newer <id> …`, y al guardar una memoria que deja anticuada a otra se le dice en el momento, para que lo arregle ahí mismo. La pestaña **Revisar** de la vista web las enseña con las dos memorias una al lado de la otra, *olvidar la vieja* y *está bien así*; `omoi review` hace lo mismo desde la terminal. Sin modelo: palabras, fechas y versiones.

### Recetas y memorias conectadas

**Recetas.** Una memoria de tipo `procedure` es algo que se repite (publicar una versión, desplegar): pasos numerados y una línea `when: publica, release`. Cuando un mensaje la pide, vuelve entera, y el agente la corrige cuando un paso falla o cambia.

**Memorias conectadas.** `[[nombre]]` en una memoria la enlaza con la que lleva esa etiqueta. Cuando se recuerda una memoria, puede venir con ella una trampa o una preferencia enlazada; los enlaces rotos salen en Revisar y la página de cada proyecto dibuja sus conexiones.

### Historial, olvido y exportación

Cambiar una memoria guarda la versión anterior con el motivo, y `omoi history` las enseña todas. Olvidar la borra con su historial. Un olvido tampoco vuelve desde otro equipo cuando sincronizas, y `omoi export` saca todas las memorias en JSONL, cada una con su historial.

### La ves y la ordenas tú

`omoi tui` abre un visor en la terminal: busca mientras escribes, lee una memoria y su historial, fíjala u olvídala.

`omoi serve --open` (o `/omoikane-web` en Claude Code) abre lo mismo en el navegador, en `http://127.0.0.1:7438/`. **Memorias** es una cronología por días de lo que se guardó o cambió (y, si quieres, de lo que el agente recordó), con un buscador que busca como el agente y filtros por tipo, proyecto o etiqueta; cada memoria se abre ahí mismo con su Markdown con formato, su último cambio como diff, el historial completo y botones para fijarla, moverla u olvidarla. **Proyectos** enseña en qué punto se quedó cada proyecto (su última memoria `status`) y abre una página por proyecto con sus memorias en una columna por tipo. Sigue el modo claro u oscuro del sistema y funciona sin internet: las fuentes (IBM Plex y Shippori Mincho, licencia SIL OFL, en `assets/fuentes`) van dentro del binario. Solo responde en tu propio equipo y rechaza las peticiones de otras webs.

### Otros agentes

`omoi setup <agente>` registra el servidor MCP, el recall automático donde el agente lo permite y un párrafo en su fichero de instrucciones. Antes enseña lo que va a cambiar (`--dry-run` para solo mirar), guarda una copia de cada fichero que toca (`<fichero>.antes-de-omoikane`) y no hace nada dos veces.

| Agente | Herramientas (MCP) | Recall automático | Guarda al cerrar | Probado |
|---|---|---|---|---|
| Claude Code | sí (el plugin) | al empezar y en cada mensaje | sí | sí |
| OpenCode | sí | en cada mensaje (un plugin pequeño) | sí (el plugin) | sí, 1.19.5 |
| Codex CLI | sí | al empezar y en cada mensaje (aprueba los hooks una vez con `/hooks`) | sí | según su documentación |
| Gemini CLI | sí | al empezar y en cada mensaje | sí | según su documentación |
| Qwen Code | sí | al empezar y en cada mensaje | sí | según su documentación |
| Kiro | sí | al empezar y en cada mensaje | sí | según su documentación |
| Cursor | sí | solo al empezar (su hook del mensaje no deja añadir contexto) | sí | según su documentación |
| GitHub Copilot (CLI y VS Code) | sí | solo al empezar (su documentación dice que la salida del hook del mensaje se descarta) | sí | según su documentación |
| Antigravity CLI | sí | solo al empezar (no tiene evento de mensaje) | no (guarda las conversaciones en un formato binario sin documentar) | según su documentación |
| Grok Build | sí | no (sus hooks de inicio y de mensaje descartan lo que devuelven; las instrucciones le piden llamar a `context` al empezar) | sí | según su documentación |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | sí | no (sus hooks no pueden añadir contexto, o no está documentado) | no | según su documentación |

`omoi setup` añade también dos órdenes en los agentes que admiten órdenes de usuario: `/omoikane`, que hace guardar al agente, y `/omoikane-web`, que abre la vista web. Son Gemini CLI, Qwen Code, Codex (`/prompts:omoikane` y `/prompts:omoikane-web`), Cursor, OpenCode, Kiro, Windsurf, Pi, Kilo Code y Grok Build. En esos agentes una orden es un texto para el modelo, así que es el agente quien ejecuta `omoi serve --open` y puede pedir permiso antes. Una `/omoikane` de antes, que abría la vista web, pasa a ser la que guarda.

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

### Se mantiene al día solo

Una vez al día, la primera vez que se ejecuta, `omoi` mira en segundo plano si hay una Release más nueva de la misma versión mayor, la instala solo si está firmada con la clave de Omoikane y añade a los agentes que ya lo tienen lo que traiga la versión nueva; la sesión siguiente te lo dice. `omoi setup claude` lo pregunta antes (y enciende también la actualización del plugin en Claude Code); `omoi upgrade --off` u `OMOIKANE_AUTO_UPDATE=0` lo apagan, `omoi upgrade` actualiza a mano, y un binario que gestiona cargo, Homebrew, Scoop o Nix se deja a ellos. Antes de pasar una base a un esquema nuevo guarda una copia.

### Por HTTP, para scripts y apps

`omoi serve` sirve las mismas herramientas por HTTP en `127.0.0.1:7438` para scripts y apps que no hablan MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<nombre>` con los argumentos JSON de la herramienta. Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar `Authorization: Bearer <token>`.

### Importar de otras herramientas

`omoi import` lee las exportaciones de engram (`engram export`) y las de `omoi export`: adapta los tipos, conserva las fechas, se salta lo borrado y no importa nada dos veces. `--dry-run` enseña adónde iría cada memoria.

## Puntos fuertes

- **Recuerda sola.** El agente no tiene que acordarse de buscar: lo que importa le llega en cada mensaje, en 15-26 ms.
- **Acierta.** El 90 % de las veces, la memoria correcta sale la primera; el 97 %, entre las tres primeras. En 101 idiomas, con tildes o sin ellas.
- **Rápida a cualquier tamaño.** 43 ms por búsqueda, y 50 ms con 3.183 memorias. Cuatro procesos comparten el modelo con 13 MB de memoria cada uno.
- **Guarda aunque el agente se olvide**, y no guarda dos veces lo mismo.
- **No se queda vieja.** Ve lo que está desfasado, lo marca y te lo pone delante para decidir.
- **Un binario sin nada más:** ni Python, ni Node, ni Docker, ni un servidor que dejar encendido. SQLite va dentro.
- **Funciona sin internet.** Todo es local, y la vista web lleva dentro del binario hasta sus fuentes, IBM Plex y Shippori Mincho.
- **Windows igual que Linux y macOS:** sin bash ni scripts, y probado en los tres en cada cambio.
- **15 agentes, una sola memoria**, y `omoi setup <agente>` lo deja todo listo.
- **Tus memorias son tuyas:** un fichero SQLite que se exporta entero, y una sincronización que solo pasa por tu repositorio git, cifrada.
- **Se actualiza con cuidado:** solo instala versiones firmadas con Ed25519 y, antes de cambiar el esquema de la base, guarda una copia.

## Medido

Con 137 notas reales y preguntas escritas a ciegas por otro agente, que nunca se usaron para ajustar: la búsqueda acierta la memoria a la primera el 90 % de las veces y entre las tres primeras el 97 %, con una mediana de 43 ms. Con 3.183 memorias sigue igual de precisa, a 50 ms por búsqueda. En 300 mensajes reales solo inyecta algo cuando tiene que ver (en un 7 % de ellos), y el hook del recall tarda entre 15 y 26 ms. El modelo de embeddings de 101 idiomas ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) abre en 2,7 ms en lugar de 1,78 s. El método completo y todos los números: [BENCHMARK.es.md](BENCHMARK.es.md).

## Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

| campo | quién lo pone |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | el agente |
| `paths`, `tags`, `pinned` | el agente, si quiere |
| `project`, `source`, fechas, estadísticas de recall, historial de versiones | Omoikane |

Cada `kind` cambia cómo se recupera: las preferencias y lo fijado se inyectan siempre; los gotcha salen cuando el prompt o los ficheros coinciden; hechos y decisiones, por relevancia; las recetas, enteras, cuando el mensaje dice una de las palabras de su `when:`. Una memoria con la etiqueta `status` dice en qué punto se quedó el trabajo de un proyecto: al empezar sesión sale la última de cada proyecto con parte de su contenido, para que «sigamos con X» no obligue a buscar.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (SQLite + FTS5 incluidos) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## Licencia

MIT · Marc Mayol
