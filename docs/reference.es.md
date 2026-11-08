# Terminal, vista web, HTTP y actualizaciones

[English](reference.md) · [Volver al README](../README.es.md)

## Desde la terminal

```sh
cd tu-proyecto
omoi remember -k gotcha -t "El token caduca antes de tiempo" --path src/auth.rs \
  "El middleware compara la hora local con la caducidad del token, que va en UTC."
omoi search "token caduca"
omoi recent
OMOIKANE_LANG=es omoi --help
```

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos; más en [sync-and-spaces.es.md](sync-and-spaces.es.md#proyectos)). La base está en `~/.omoikane/omoikane.db` (`OMOIKANE_DB` para cambiarla, u `OMOIKANE_HOME` para llevar a otro sitio toda la carpeta `~/.omoikane`; para CI y servidores, ver [unattended.es.md](unattended.es.md)). Todos los comandos aceptan `--json`. Los mensajes salen en inglés; con `OMOIKANE_LANG=es`, en español.

Varios agentes pueden usar la misma base a la vez (subagentes, varias sesiones, trabajos de CI): cada `omoi` espera su turno para escribir, hasta 5 s, y una base nueva que crean varios procesos en el mismo momento sale una sola. Lo comprueba `tests/concurrencia.rs` con procesos `omoi` de verdad; `cargo test --release --test concurrencia -- --ignored --nocapture` hace la medida entera (20 agentes durante 30 s).

## La ves y la ordenas tú

`omoi tui` abre un visor en la terminal: busca mientras escribes, lee una memoria y su historial, fíjala u olvídala.

`omoi serve --open` (o `/omoikane-web` en Claude Code) abre lo mismo en el navegador, en `http://127.0.0.1:7438/`. **Memorias** es una cronología por días de lo que se guardó o cambió (y, si quieres, de lo que el agente recordó), con un buscador que busca como el agente y filtros por tipo, proyecto o etiqueta; cada memoria se abre ahí mismo con su Markdown con formato, su último cambio como diff, el historial completo y botones para fijarla, moverla u olvidarla. **Proyectos** enseña en qué punto se quedó cada proyecto (su última memoria `status`) y abre una página por proyecto con sus memorias en una columna por tipo. **Panorama** enseña, por espacio, qué hay, quién lo escribió y qué está desfasado o en pausa ([detalles](sync-and-spaces.es.md#panorama-por-cliente)). Sigue el modo claro u oscuro del sistema y funciona sin internet: las fuentes (IBM Plex y Shippori Mincho, licencia SIL OFL, en `assets/fuentes`) van dentro del binario. Solo responde en tu propio equipo y rechaza las peticiones de otras webs.

## Por HTTP, para scripts y apps

`omoi serve` sirve las mismas herramientas por HTTP en `127.0.0.1:7438` para scripts y apps que no hablan MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `GET /api/overview?months=…` (el panorama por cliente, en JSON), `POST /tools/<nombre>` con los argumentos JSON de la herramienta. Verificar una memoria no es una herramienta: es el botón de la vista web (`POST /api/verify`, que pide el `Origin` de la vista y el token que `omoi serve` mete en la página, en `X-Omoikane-View`) u `omoi verify` ([saving.es.md](saving.es.md#verificada-por-una-persona)). Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar `Authorization: Bearer <token>`.

La vista web y la API ven lo mismo que la CLI y el MCP desde la carpeta en la que arrancó `omoi serve`. En un proyecto de un cliente, el espacio de ese cliente (todos sus proyectos y lo del espacio) y las memorias globales; en un proyecto sin espacio, los proyectos sin espacio y las globales; en la carpeta del usuario, todo. Vale para todas las páginas (Memorias, Proyectos, Panorama, Revisar, la papelera, lo apartado de las sesiones, la búsqueda) y para lo que escribe: fijar, mover, olvidar, decidir, restaurar o redactar una memoria de fuera da error (403 en `/api/*`, 400 en `/tools/*`, como por MCP). La cabecera dice desde dónde miras («Solo ◇ Acme»), así que compartir pantalla con un cliente no enseña a los demás. `?dir=<carpeta>` elige otro proyecto para una petición, pero solo dentro de lo que el servidor ya enseña (otro proyecto del mismo espacio; desde la carpeta del usuario, cualquiera): nunca amplía la vista (403). Una ruta de red ahí (`\\servidor\recurso`, `//host/x`, `\\?\UNC\…`, `\??\UNC\…`) se rechaza (400) sin mirarla: cualquier web puede pedir esa URL con un `<img>`, y en Windows solo con comprobar una ruta así se abre una conexión SMB que entrega tu hash NTLM. Lo mismo con el `project` que llega por MCP y con cualquier carpeta que se le dé a Omoikane (`--dir`, `export --okf`/`--obsidian`, `import --okf`, el fichero de `import`, las rutas de proyecto que llegan por la sincronización), con las rutas que nombra una memoria (también `~/` seguido de una ruta de red, como `~/\\host\recurso`, que `remember` desde la carpeta personal miraba), con el `gitdir:` del fichero `.git` de un worktree y con las carpetas que salen de variables de entorno (`--db`, `OMOIKANE_DB`, `OMOIKANE_HOME`, `OMOIKANE_MODELS`, `HOME`/`USERPROFILE`, `COPILOT_HOME`, `GROK_HOME`, `XDG_CONFIG_HOME`, `APPDATA`): una carpeta de red no se abre nunca; si la necesitas, asígnale una letra de unidad. Solo puede serlo la carpeta en la que ya estás.

## Se mantiene al día, si se lo pides

Viene apagado: hasta que digas que sí (`omoi upgrade --on`, o la pregunta de `omoi setup claude`), `omoi` no se cambia a sí mismo. Encendido, una vez al día, la primera vez que se ejecuta, `omoi` mira en segundo plano si hay una Release más nueva de la misma versión mayor, la instala solo si está firmada con la clave de Omoikane (Ed25519) y añade a los agentes que ya lo tienen lo que traiga la versión nueva; la sesión siguiente te lo dice. `omoi setup claude` lo pregunta (y enciende también la actualización del plugin en Claude Code); `omoi upgrade --off` u `OMOIKANE_AUTO_UPDATE=0` lo apagan, `omoi upgrade` actualiza a mano, y un binario que gestiona cargo, Homebrew, Scoop o Nix se deja a ellos. Antes de pasar una base a un esquema nuevo guarda una copia.

## Variables de entorno

Todas salen en las páginas de arriba; aquí están juntas.

| variable | qué hace |
|---|---|
| `OMOIKANE_DB` | dónde está la base (por defecto, `omoikane.db` en `OMOIKANE_HOME`) |
| `OMOIKANE_HOME` | la carpeta de Omoikane entera: base, modelos y preferencias (por defecto, `~/.omoikane`) |
| `OMOIKANE_NONINTERACTIVE=1` | no hay nadie delante: ni preguntas, ni descargas, ni actualizaciones, ni sincronización de fondo ([unattended.es.md](unattended.es.md)) |
| `OMOIKANE_SYNC_CODE` / `OMOIKANE_SYNC_URL` | unirse a la sincronización con un código (y otra URL) sin preguntar ([unattended.es.md](unattended.es.md)) |
| `OMOIKANE_AUTHOR` | quién firma lo que se guarda aquí |
| `OMOIKANE_SESSION` | la sesión del agente que se guarda con cada memoria nueva (si no, `CLAUDE_CODE_SESSION_ID` o la de los hooks) |
| `OMOIKANE_SYNC_AUTO=0` / `1` | apaga o enciende la sincronización de fondo (encendida por defecto, apagada con `OMOIKANE_NONINTERACTIVE=1`) |
| `OMOIKANE_LANG=es` | mensajes en español |
| `OMOIKANE_BIN` | qué `omoi` ejecuta el plugin de Claude Code |
| `OMOIKANE_TRASH_DAYS` | días en la papelera antes de borrar del todo (30; 0 = nunca) |
| `OMOIKANE_BACKUPS` | copias diarias que se guardan (7; 0 = apagado) |
| `OMOIKANE_AUTO_UPDATE=0` | apaga la actualización automática |
| `OMOIKANE_HTTP_TOKEN` | token que pide `omoi serve` por HTTP |
| `OMOIKANE_NO_SETUP=1` | los instaladores no añaden el plugin de Claude Code |
| `OMOIKANE_NO_MODEL=1` | los instaladores no bajan el modelo ligero (el de significado no lo bajan nunca), y `omoi` no baja solo el de significado |
| `OMOIKANE_SEMANTIC=0` / `1` | apaga o enciende el modelo de significado (por encima de `omoi model semantic` y del tamaño de la memoria) |
| `OMOIKANE_SEMANTIC_THRESHOLD` | desde cuántas memorias se enciende solo el modelo de significado (1000) |
| `OMOIKANE_TRAZA=1` | el recuerdo escribe cada paso en stderr: candidatas, puntuaciones y qué hizo el modelo de significado, o por qué no se le preguntó; y cada orden dice cuánto esperó a que otro proceso soltara la base |
| `OMOIKANE_SEMANTIC_IDLE` | segundos sin uso antes de que se cierre el servicio de significado (1800) |
| `OMOIKANE_SEMANTIC_RAM` | MB de RAM que puede usar el modelo de significado al indexar (por encima de `omoi model semantic --ram`; 128 por defecto, mínimo 80) |
| `OMOIKANE_SEMANTIC_MODEL` | una carpeta con EmbeddingGemma para usarla en vez de la instalada |
| `OMOIKANE_SEMANTIC_SOURCE` | de dónde descarga `omoi model install --semantic` (separadas por comas) |
