<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

*Memoria eterna, viva y ultrarrápida.*

**Memoria local para agentes de programación. Un binario, un fichero SQLite, y el agente no tiene que acordarse de buscar.**

[English](README.md)

> Omoikane (思兼, «el que reúne pensamientos») es el kami al que los demás dioses consultan cuando hay que decidir: recuerda todo lo que se sabe y lo junta. Eso es lo que hace este programa por un agente.

## Qué lo hace distinto

Tu agente aprende cosas en cada sesión: un bug que solo sale en Windows, por qué se eligieron ULID, que quieres los commits cortos y en inglés. Se cierra la sesión y se pierde todo. Omoikane lo guarda como memorias cortas y tipadas en un SQLite local, y hace lo demás solo.

### Lo que hace en cada sesión

- **Recuerda sin que se lo pidas.** Cuando lo que escribes tiene que ver con algo guardado, un hook le pasa al agente esa memoria (como mucho 2, una vez por sesión) o la trampa del fichero que has nombrado, en unos 20 ms. Si no tiene que ver, no le pasa nada, y así fue en el 93 % de los mensajes reales. El agente no tiene que acordarse de buscar.
- **Busca bien en tu idioma**, con tildes o sin ellas y en 101 idiomas: con preguntas escritas a ciegas acierta la memoria a la primera el 90 % de las veces, en 43 ms, y sigue igual de preciso con 3.183 memorias.
- **Detecta duplicados al guardar.** Si el agente intenta guardar algo que ya sabe, se le dice y actualiza la memoria que hay en lugar de crear otra.
- **Funciona con 15 agentes.** Claude Code, Codex, Gemini CLI, Qwen Code, Cursor, Copilot, Kiro, OpenCode, Grok y los demás comparten la misma memoria; `omoi setup <agente>` deja cada uno listo. Probados a mano: Claude Code y OpenCode; los demás, según su documentación (la tabla de «Otros agentes» lo dice uno por uno).
- **La misma memoria en todos tus equipos, y con compañeros.** Se sincroniza sola por un repositorio git tuyo, cifrada antes de salir del equipo, y sin servidor de nadie.
- **Historial con motivo.** Cambiar una memoria guarda la versión anterior y, si el agente lo dice, por qué cambió.
- **No guarda secretos.** Si un token, una clave privada o una contraseña se cuela en una memoria, se cambia por `[redacted:<tipo>]` antes de guardarla, y la respuesta se lo dice al agente. Lo que se guardó antes sale en Revisar con un botón para redactarlo, historial incluido, y nada de eso sale del equipo al sincronizar.
- **Nada se pierde.** Olvidar manda a la papelera, con el historial, y `omoi restore` lo devuelve desde cualquier equipo. `omoi export` es una copia fiel que `omoi import` reconstruye exacta, y cada día se guarda sola una copia de la base.
- **La ves y la ordenas tú.** Una vista web y otra en la terminal para buscar, leer el historial, fijar, mover u olvidar.
- **Privado y pequeño.** Un binario, un fichero, 8 herramientas MCP y ningún servidor propio.

### Lo que mantiene viva la memoria

- **Guarda aunque el agente se olvide.** Al cerrar la sesión, Omoikane lee la conversación y aparta lo que decidiste y lo que pediste; la sesión siguiente guarda lo que valga, con tus palabras.
- **Guarda cuando se lo pides.** `/omoikane` repasa la sesión y guarda lo que valga; `/omoikane <texto>` guarda eso.
- **Se da cuenta de cuándo una memoria se ha quedado vieja.** Si una más nueva contradice a otra, la vieja le llega al agente marcada con `⚠ may be outdated`, y la pestaña Revisar enseña las dos una al lado de la otra.
- **Recuerda lo que juzga el agente.** Si dice que una memoria sustituye a otra, o que dos se contradicen, queda apuntado, viaja a tus otros equipos y le llega marcado la próxima vez.
- **Recetas que vuelven enteras.** Una memoria de tipo `procedure` guarda los pasos de algo que se repite; «publica la 0.14.1» trae la lista entera, y el agente corrige el paso que cambie.
- **Memorias conectadas.** Los `[[enlaces]]` entre notas viajan con ellas: al recordar una, llega también el aviso que lleva enlazado.
- **Retoma donde lo dejaste.** Al empezar la sesión llega la última memoria `status` de cada proyecto, así que «sigamos con X» no obliga a buscar.
- **Se mantiene al día si se lo pides**, y solo instala versiones firmadas con la clave de Omoikane. Viene apagado.

## El ruido, lo viejo y lo erróneo

Un recall que inyecta de más es peor que no tenerlo: llena el contexto de ruido y arrastra de una sesión a otra lo que ya no vale. Por eso es la parte de Omoikane con más horas de calibrado.

- **Casi siempre calla.** Un mensaje solo recibe algo si tiene al menos 3 palabras útiles y una memoria comparte con él 2 palabras poco frecuentes en tu base que sumen como mínimo la mitad del peso del mensaje. El canal por significado, para las paráfrasis, pide además que las palabras cubran un 25 % y no se usa en mensajes de más de 500 caracteres. En 300 mensajes reales inyectó algo en el 7 %; los demás no recibieron nada. «¿Cómo ha ido todo?» no trae nada.
- **Poco y una sola vez.** Como mucho 2 memorias por mensaje, y solo las que se acercan a la mejor: si una destaca, va sola. De cada una llegan 200 caracteres; si el agente quiere el resto, la pide con `get`. Puede sumarse una trampa o preferencia enlazada, y una receta solo si el mensaje dice una de sus palabras `when:`. Cada memoria llega una vez por sesión.
- **Lo viejo llega marcado.** Si una memoria más nueva del mismo tema la contradice (otra versión de lo mismo, o hecho lo que estaba pendiente), la vieja llega con `⚠ may be outdated: newer <id> …` delante y el agente mira la nueva antes de fiarse. También avisa de estados de proyecto que se han quedado atrás, rutas que ya no existen y memorias que nadie usa. La pestaña Revisar pone las dos memorias lado a lado para olvidar la vieja o dejarla. Todo sin modelo: palabras, fechas y versiones.
- **Lo erróneo se corrige, no se arrastra.** En la memoria solo entra lo que guarda el agente. Lo que Omoikane aparta al cerrar una sesión son propuestas, y caducan a los 7 días si nadie las guarda. Corregir es `update` con el motivo, y la versión anterior queda en el historial. `forget` la borra, y tampoco vuelve desde tus otros equipos. Lo que guarda un compañero llega marcado `[by Ana]`.
- **Medido.** Cada umbral sale de preguntas escritas a ciegas por otro agente, 20 de ellas sin respuesta en las notas: con esas inyectó algo en 2. Los números, en [BENCHMARK.es.md](BENCHMARK.es.md).

Al empezar la sesión sí llega más: tus preferencias, lo que has fijado, en qué punto está cada proyecto y lo último que se tocó. Las preferencias y lo fijado son lo que tú quieres que el agente sepa siempre.

engram, la alternativa con la que se compara Omoikane, no inyecta memorias en cada mensaje. Con lo viejo hace otra cosa: cada vez que guarda, propone las memorias parecidas y le pide al agente que juzgue qué relación tienen (`mem_judge`). Omoikane no pide nada. Detecta él la contradicción, la marca en el recall con el motivo y deja al agente juzgar solo cuando sabe algo que Omoikane no puede ver (`supersedes`, `contradicts`, `compatible`).

## ¿Y CLAUDE.md?

Con un proyecto pequeño y veinte cosas que recordar, un buen CLAUDE.md basta. Omoikane compensa donde CLAUDE.md deja de llegar:

- CLAUDE.md se carga entero en cada sesión, venga o no a cuento, y cuanto más sabe más contexto gasta. Omoikane trae lo que encaja con el mensaje.
- Es un fichero por proyecto. Lo que aprendes en uno no llega a otro si no lo copias; en Omoikane una memoria puede valer para todos.
- No sabe cuándo algo se ha quedado viejo ni guarda por qué cambió.
- Lo escribes tú. Omoikane lo va llenando el agente mientras trabaja.
- Lo lee un agente. Omoikane lo comparten 15, y todos tus equipos.

Pueden convivir: CLAUDE.md para las reglas fijas del repositorio y Omoikane para lo que se va aprendiendo.

## Estado

**Antes de la 1.0:** el esquema de la base todavía puede cambiar. Cada cambio migra solo y guarda antes una copia de la base, pero una base migrada ya no la abre una versión anterior. La 1.0 lo congelará.

Versión 0.19.1,
 en crates.io y en las Releases, con integración continua en Windows, Linux y macOS. Se usa a diario en Windows y Linux. El diseño completo y cada decisión están en [PLAN.md](PLAN.md).

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

**Veredictos.** Lo que Omoikane no puede saber solo lo dice el agente, sin herramientas nuevas: al guardar o cambiar una memoria puede pasar `supersedes` (esta sustituye a aquellas), `contradicts` (dicen cosas distintas y no se sabe cuál vale) o `compatible` (un aviso las emparejó y valen las dos). El veredicto se guarda, viaja con la sincronización y sale donde salen las memorias: en el recall (`⚠ superseded by …`), en `search`, en `context` y en Revisar. Vale mientras no cambie lo que juzgó, y de una pareja manda el más nuevo. «Está bien así» también es un veredicto, así que lo que das por bueno en un equipo vale en los demás.

**Por qué y quién.** Cada veredicto guarda el motivo y quién lo dio (la persona de la sincronización, o el agente y su cliente), y los dos salen en el recall, en `search` y en Revisar. Si A está sustituida por B y B por C, la nota de A lleva directamente a C.

**Corregir un trozo y juntar.** `update` acepta `replace` (cambia un texto que tiene que aparecer exactamente una vez; si no, no toca nada), `append` (añade al final, para el paso nuevo de una receta) y `absorb` (junta varias memorias en una: el agente escribe el texto fundido, y las absorbidas van a la papelera con su historial colgado de la que queda y sus etiquetas y enlaces pasados a ella). La pestaña Revisar tiene un botón **Juntar** para hacerlo a mano.

**Fecha de revisión.** Cada tipo tiene la suya: decisiones y recetas a los 120 días sin tocar, hechos y trampas a los 180, y las preferencias nunca, contando desde su último cambio o desde la última vez que el agente la recibió (lo que se usa y nadie corrige, está repasado). Cuando llega, la memoria sale en Revisar y en `search` con «not reviewed in N days»; en el recall no, porque ahí solo va lo que dice que una memoria puede estar mal. «Está bien así» vuelve a poner el reloj a cero.

### Recetas y memorias conectadas

**Recetas.** Una memoria de tipo `procedure` es algo que se repite (publicar una versión, desplegar): pasos numerados y una línea `when: publica, release`. Cuando un mensaje la pide, vuelve entera, y el agente la corrige cuando un paso falla o cambia.

**Memorias conectadas.** `[[nombre]]` en una memoria la enlaza con la que lleva esa etiqueta. Cuando se recuerda una memoria, puede venir con ella una trampa o una preferencia enlazada; los enlaces rotos salen en Revisar y la página de cada proyecto dibuja sus conexiones.

### Historial, papelera y copias

Cambiar una memoria guarda la versión anterior con el motivo, y `omoi history` las enseña todas.

**Papelera.** Olvidar una memoria (`forget`, desde el agente, la CLI o la vista web) la manda a la papelera con su historial, y un olvido tampoco vuelve desde otro equipo cuando sincronizas. `omoi restore <id>` la devuelve, con su historial, y gana en los demás equipos en la siguiente sincronización: un `forget` hecho por error en una máquina se deshace desde cualquiera. El agente lo hace con `update` y `restore: true`, sin herramienta nueva; la pestaña Revisar tiene el grupo **Papelera** con un botón para cada una. A los 30 días (`OMOIKANE_TRASH_DAYS`; 0 = nunca) se borran del todo; `forget --purge` no pasa por la papelera y `omoi trash --empty` la vacía.

**Copia fiel.** `omoi export` escribe un JSONL con todo lo que hace falta para reconstruir la base: cada memoria con su historial, sello y autor, las marcas de olvido, los veredictos y la papelera, con una línea de cierre que lleva el recuento y el SHA-256 (un fichero cortado o tocado no se importa). `omoi import` lo vuelve a meter con las mismas reglas que la sincronización: los mismos ids, fechas y sellos, gana el más nuevo y lo olvidado no resucita, así que repetirlo no cambia nada y sirve tanto para restaurar una base como para juntar dos. Las dos van en streaming, una línea en memoria cada vez: 50.000 memorias se exportan e importan sin cargar la RAM. Con `--encrypt`, cada línea va cifrada con la clave de la sincronización (la de un proyecto de un cliente, con la de su espacio), para guardar la copia donde quieras. Lo de la papelera va con su proyecto y su espacio, y con sus veredictos: restaurado en otro equipo, vuelve a su sitio y con su `superseded`. Los ficheros del formato anterior y los de engram siguen entrando, y uno de un omoi más nuevo también: lo que esta versión no entiende se salta y se cuenta, y los campos que no conoce se guardan y vuelven a salir en el siguiente export.

**Copia diaria.** Al empezar la primera sesión de cada día, Omoikane copia la base a `~/.omoikane/backups/omoikane-<fecha>.db` y conserva las 7 últimas (`OMOIKANE_BACKUPS`; 0 = apagado). `omoi backup` la hace ahora y `omoi doctor` dice cuándo fue la última.

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

### Proyectos y espacios

Cada proyecto tiene su clave, que se calcula una vez: el `origin` de git o, si no tiene, tu nombre y el de la carpeta. Y guarda todas las carpetas donde vive. Dos clones del mismo repositorio, o un worktree, son el mismo proyecto. Si mueves la carpeta de un repositorio con `origin`, las memorias la siguen solas al empezar la sesión y te lo dice una vez; sin `origin`, `omoi projects move <proyecto> <carpeta>`, y hasta entonces lo avisan `omoi review` y `omoi doctor`. `omoi projects` los lista. `omoi projects merge <a> <b>` junta dos sin tocar ids, historial ni veredictos, y de sus dos `status` se queda el más nuevo; `omoi projects prune` olvida los que se quedaron sin memorias.

Los espacios separan clientes. `omoi spaces add acme` y luego `omoi space assign acme` en cada repositorio del cliente (o `omoi projects move <proyecto> --space acme`). Desde un proyecto de Acme se ven sus memorias, las del espacio y las globales, nunca las de otro cliente: ni en la búsqueda, ni en el recall, ni en `context`. Lo común a todos los repositorios de un cliente se guarda con `remember --space` (o `space: true` por MCP); tus preferencias siguen siendo tuyas, globales. Desde tu carpeta de usuario se ve todo. `search` y `recent` aceptan `project` (una carpeta o un trozo del nombre) y `space`. Para compartirlo, un repositorio por cliente, con su propia clave: `omoi sync init --space acme <url>` solo lleva lo de ese espacio, sin tus globales, y el repositorio general no lleva lo del espacio. Meter un proyecto en un espacio nunca lo mueve solo en otro sitio: esa línea la puede escribir cualquiera que tenga la clave de un repositorio. Tus otros equipos se enteran en su siguiente sincronización (el repositorio general solo dice que un proyecto pasó a un espacio, no a cuál ni lo que tiene dentro; el del espacio dice cuál) y dejan el proyecto **en pausa**: no se sube nada suyo a ningún repositorio ni se aplica lo que llegue de él hasta que decidas en ese equipo. Si no lo sabes, `omoi projects keep web` lo deja donde estaba; `omoi projects move web --space acme --force` acepta el cambio. En los dos casos vuelve a sincronizarse y gana la edición más nueva. El repositorio de un espacio también puede nombrar un proyecto que aún no has abierto en este equipo (el de un compañero, o una librería tuya que alguien usa en el cliente): la primera vez que abres esa carpeta, el proyecto también queda en pausa, y lo que guardes en ella se queda en tu equipo hasta que digas de quién es: `omoi projects keep <proyecto>` si es del cliente, `omoi space unassign <proyecto> --force` si es tuyo (va a tu repositorio general, nunca al del cliente). Lo dicen `omoi sync`, `omoi sync status`, `omoi doctor`, `omoi review`, Revisar en la vista web (con las dos órdenes para copiar) y el agente al empezar cada sesión, hasta que decidas. Así, lo peor que puede hacer una línea falsa es pausar un proyecto, nunca mandar lo de un cliente, ni lo tuyo, a otro repositorio. `omoi export --encrypt` desde un proyecto de un cliente cifra con la clave de su espacio; con `--all`, con la del general.

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
- **Los proyectos** se reconocen por su remoto `origin` de git: el mismo repositorio en carpetas distintas de equipos distintos es el mismo proyecto. Los que no tienen remoto se reconocen por el nombre de la carpeta, y solo entre los equipos de una misma persona. El espacio de un proyecto viaja en su propia línea y gana el cambio más nuevo; `--space` deja un repositorio para un solo cliente.
- **Equipos con versiones distintas:** desde la 1.0, lo que una versión nueva añada no rompe a las viejas. Un campo que un equipo no conoce se guarda con la memoria y vuelve a subir aunque la cambie, así que no se pierde por pasar por él. Una línea de un tipo que no conoce se salta, `omoi sync status` dice «N líneas de una versión más nueva: actualiza omoi», y al actualizar se vuelve a leer todo. Y si un día una versión cambiara algo que las viejas no pueden leer bien, el repositorio lo dice en `omoikane.format` y las viejas no escriben en él hasta que se actualicen (lo avisan `status`, `doctor` y la sesión al empezar).

`omoi sync off` deja de sincronizar en un equipo; sus memorias se quedan.

### Se mantiene al día, si se lo pides

Viene apagado: hasta que digas que sí (`omoi upgrade --on`, o la pregunta de `omoi setup claude`), `omoi` no se cambia a sí mismo. Encendido, una vez al día, la primera vez que se ejecuta, `omoi` mira en segundo plano si hay una Release más nueva de la misma versión mayor, la instala solo si está firmada con la clave de Omoikane y añade a los agentes que ya lo tienen lo que traiga la versión nueva; la sesión siguiente te lo dice. `omoi setup claude` lo pregunta (y enciende también la actualización del plugin en Claude Code); `omoi upgrade --off` u `OMOIKANE_AUTO_UPDATE=0` lo apagan, `omoi upgrade` actualiza a mano, y un binario que gestiona cargo, Homebrew, Scoop o Nix se deja a ellos. Antes de pasar una base a un esquema nuevo guarda una copia.

### Por HTTP, para scripts y apps

`omoi serve` sirve las mismas herramientas por HTTP en `127.0.0.1:7438` para scripts y apps que no hablan MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<nombre>` con los argumentos JSON de la herramienta. Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar `Authorization: Bearer <token>`.

### Importar de otras herramientas

`omoi import` lee las exportaciones de engram (`engram export`) y las de `omoi export`: adapta los tipos, conserva las fechas, se salta lo borrado y no importa nada dos veces. `--dry-run` enseña adónde iría cada memoria.

## Puntos fuertes

- **Recuerda sola, y solo cuando hace falta.** El agente no tiene que acordarse de buscar: si un mensaje tiene que ver con algo guardado, le llega en 15-26 ms; si no, no le llega nada.
- **Acierta.** El 90 % de las veces, la memoria correcta sale la primera; el 97 %, entre las tres primeras. En 101 idiomas, con tildes o sin ellas.
- **Rápida a cualquier tamaño.** 43 ms por búsqueda, y 50 ms con 3.183 memorias. Cuatro procesos comparten el modelo con 13 MB de memoria cada uno.
- **Guarda aunque el agente se olvide**, y no guarda dos veces lo mismo.
- **No se queda vieja.** Ve lo que está desfasado, lo marca y te lo pone delante para decidir.
- **Un binario sin nada más:** ni Python, ni Node, ni Docker, ni un servidor que dejar encendido. SQLite va dentro.
- **Funciona sin internet.** Todo es local, y la vista web lleva dentro del binario hasta sus fuentes, IBM Plex y Shippori Mincho.
- **Windows igual que Linux y macOS:** sin bash ni scripts, y probado en los tres en cada cambio.
- **15 agentes, una sola memoria** (2 probados a mano, el resto según su documentación), y `omoi setup <agente>` lo deja todo listo.
- **Tus memorias son tuyas, y no se pierden:** un fichero SQLite con copia diaria, un export que lo reconstruye exacto, una papelera desde la que todo vuelve, y una sincronización que solo pasa por tu repositorio git, cifrada.
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
