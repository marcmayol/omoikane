# Qué es una memoria y cómo se guarda

[English](saving.md) · [Volver al README](../README.es.md)

## Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

| campo | quién lo pone |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | el agente |
| `paths`, `tags`, `pinned`, `space` o `trade` | el agente, si quiere |
| `project`, `source`, fechas, estadísticas de recall, historial de versiones | Omoikane |

Cada `kind` cambia cómo se recupera: las preferencias y lo fijado se inyectan siempre; los gotcha salen cuando el prompt o los ficheros coinciden; hechos y decisiones, por relevancia; las recetas, enteras, cuando el mensaje dice una de las palabras de su `when:`. Una memoria con la etiqueta `status` dice en qué punto se quedó el trabajo de un proyecto: al empezar sesión sale la última de cada proyecto con parte de su contenido, para que «sigamos con X» no obligue a buscar.

Una memoria es de su proyecto, de un espacio (todos los proyectos de un cliente), de un oficio (todos los proyectos de un stack, de cualquier cliente: ver [sync-and-spaces.es.md](sync-and-spaces.es.md#oficios-lo-que-enseña-un-stack-para-todos-los-proyectos-de-ese-stack)) o global.

## Guarda sin duplicados

Cada memoria tiene un tipo que decide cuándo vuelve: `fact` y `decision` por relevancia, `gotcha` también por sus ficheros, `preference` siempre y `procedure` entera cuando la pides. Antes de guardar mira si ya existe algo casi igual. Si lo hay, no la guarda y dice cuál actualizar; si solo se parece, la guarda y enseña las relacionadas. Avisa cuando pasa de los 6.000 caracteres que cuentan para el recall y cuando deja anticuada a otra. Las rutas se guardan relativas al repositorio, así que valen en cualquier equipo.

Cuando un programa lleva una sola memoria sobre algo y la reescribe (el perfil de un proyecto, el estado de una herramienta), puede guardarla con una clave: `omoi remember … --key kumi-perfil` (o `key` en el `remember` del MCP). Si en el mismo proyecto, espacio o ámbito global ya hay una con esa clave, la actualiza y deja el texto anterior en su historial; si no, la crea. No busca duplicadas, y dos procesos que guardan la misma clave a la vez dejan una sola memoria. Dos equipos que crean la misma clave antes de sincronizar sí acaban con dos memorias con esa clave en el mismo nivel: al leer la clave sale la más antigua (por fecha de creación), y la otra se queda, en la búsqueda y en `--resolve`, hasta que las juntes (`update … --absorb`). `omoi get --key kumi-perfil` (o `get` con `key`) la lee: la del proyecto, si no la de su espacio, y si no la global. La clave es una etiqueta, `topic:<clave>`; si una clave nueva se parece a otra que ya hay (`kumi_perfil` junto a `kumi-perfil`), avisa.

Una clave puede vivir en cuatro niveles, una memoria por nivel: el proyecto (por defecto), su espacio (`--space`), un oficio (`--trade web`, una global con la etiqueta `trade:web`) y global (`--global`). `omoi get --key <clave> --resolve --json` (o `get` con `key` y `resolve`) devuelve todas las que ve este proyecto, de la más concreta a la más general (proyecto, espacio, oficio, global), cada una con su `scope` y sus `data`, sin juntarlas: las combina quien las lee. Los datos van con `--data '<JSON>'` (o `data` en el MCP), aparte del texto como los de cualquier memoria: fuera de la búsqueda y del recall, viajan con la sincronización y Omoikane no los interpreta. Guardar otra vez el mismo texto y los mismos datos no cambia nada; cambiar solo los datos no crea versión, y guardar la clave sin `--data` conserva los que tenía. Con `--trade`, `--for client|personal` hace también su propio nivel. Una `preference` guardada sin `--space`, `--trade`, `--global` ni `--dir` es global, como toda preferencia: `omoi remember -k preference --key x …` desde un proyecto crea o actualiza la `x` global, no la del proyecto (usa otro tipo, o `--dir`, para una del proyecto). Una clave de oficio pasa por el aviso de fugas como cualquier memoria de oficio, con su clave y sus datos.

El ejemplo es la ruta de Kumi por sus factorías. La ruta por defecto de la factoría web es una memoria de oficio, el cliente X siempre añade seo-geo tras web, y un proyecto de X va sin SEO:

```
omoi remember -k fact -t "Ruta de la factoría web" --trade web --key kumi-ruta --data '{"ruta":["web"],"tests":["web","cyber"]}'
omoi remember -k fact -t "Ruta del cliente X" --space=cliente-x --key kumi-ruta --data '{"ruta":["web","seo-geo"]}'
omoi remember -k fact -t "Ruta de disney-countdown" --key kumi-ruta --data '{"ruta":["web"]}' "Sin SEO: es una cuenta atrás familiar."
omoi get --key kumi-ruta --resolve --json
```

Desde disney-countdown, la última orden devuelve tres capas, `project`, `space` y `trade:web`, en ese orden. Desde otro proyecto del cliente X, `space` y `trade:web`; desde uno del cliente Y, nunca la del cliente X.

## Guarda cuando se lo pides

`/omoikane` hace que el agente guarde en ese momento. Repasa la sesión, guarda lo que valga y aún no esté guardado (decisiones con su motivo, trampas con sus ficheros, preferencias, recetas) y pone al día la memoria `status` del proyecto. Con texto detrás, como en `/omoikane el deploy va por ssh`, guarda eso con tus palabras. Termina con una línea por memoria guardada o cambiada.

## Decisiones que te esperan

Cuando una decisión es tuya y el agente no puede preguntarte en ese momento, la guarda como `decision` con la etiqueta `open` (la pregunta, las opciones, lo que asumió mientras tanto) y sigue trabajando. Si el trabajo no puede seguir sin tu respuesta, porque no hay nada seguro que asumir o equivocarse costaría deshacerlo, añade también la etiqueta `blocking` y para esa parte. Sin ella, la decisión es para después. Cada sesión empieza con las abiertas, primero las bloqueantes y marcadas. En la vista web, **Proyectos** y **Panorama** dicen cuáles son bloqueantes, y al contestar una ahí se quitan las dos etiquetas.

`omoi decisions` las enseña, las bloqueantes primero (`--blocking` para solo esas; `--project`, `--space`, `--all` y `--json` como en `recent`), y los agentes tienen la herramienta `decisions`. Desde el proyecto de un cliente nunca enseña las de otro. Un programa que reparte trabajo, como la central de una factoría de agentes, puede mirarlo antes de empezar una tarea en un repositorio:

```sh
omoi --dir ~/trabajo/acme-web decisions --blocking --json
```

```json
[
  {
    "id": "01M6B2Q8XKTE4R7N0VZC3HJ5WD",
    "title": "¿Borrar las cuentas sin uso en 12 meses?",
    "content": "Opciones: borrarlas o solo anonimizarlas. Asumido mientras tanto: no se borra nada.",
    "priority": "blocking",
    "project": "/home/ana/trabajo/acme-web",
    "project_key": "github.com/acme/web",
    "space": "acme",
    "global": false,
    "tags": ["blocking", "open"],
    "paths": ["src/accounts/cleanup.ts"],
    "source": "claude-code",
    "created_at": "2026-11-07T09:12:40.120Z",
    "updated_at": "2026-11-07T09:12:40.120Z"
  }
]
```

`priority` es `blocking` o `later`. Es una etiqueta, así que no cambian ni el esquema ni la sincronización, y una versión anterior ve una decisión abierta de las de siempre.

Una decisión tiene tres estados, todos en sus etiquetas, así que el texto queda limpio: abierta y bloqueante (`open` + `blocking`), abierta para después (`open`) y resuelta (`resolved`, con la respuesta al final del contenido). `omoi decisions --resolve <id> --answer "…"` resuelve una: añade `Answer (2026-11-07): …`, quita `open` y `blocking`, pone `resolved` y guarda la versión anterior en su historial (la herramienta `decisions` acepta `resolve` y `answer`, y la vista web hace lo mismo). `--tag` deja en la lista solo las decisiones que lleven además otras etiquetas, como la de la tarea que las abrió.

## Listar por etiqueta, y datos para programas

`omoi list --tag <etiqueta>` devuelve todas las memorias con esa etiqueta del proyecto, de su espacio y globales, de la más nueva a la más vieja, sin búsqueda, ranking ni umbral (`--tag` otra vez para varias, todas obligatorias; `--kind`, `-n`, `--project`, `--space`, `--all`, `--json`). Un programa que guarda algo con una etiqueta lo encuentra siempre; `search` ordena por palabras y podía dejarlo fuera. Los agentes tienen lo mismo con `recent` y `tags`.

Una memoria también puede llevar datos para programas: cualquier JSON de hasta 64 KB, con `--data '<json>'` en `remember` y `update` (`--no-data` los quita) o `data` en las herramientas. `get`, `list --json` y `decisions --json` los devuelven como `data`. Nunca se buscan ni entran en el recall, así que no llenan el contexto del agente, y lo que parezca un secreto se tapa como en el contenido. Cambiar solo los datos no añade una versión. Viajan con la sincronización y con `omoi export` como un campo `data` de la línea de la memoria: de la 1.0 a la 1.7 no lo conocen, pero lo guardan y lo reenvían tal cual, también después de editar la memoria.

Así lo usa la central de Kumi (`kumi.py`), con el texto sin marcas:

```python
def omoi(*args, entrada=None):
    r = subprocess.run(["omoi", *args], input=entrada, capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None

# El perfil del proyecto: una memoria con su etiqueta y sus datos, que se encuentra siempre.
def perfil_leer(raiz):
    ms = omoi("--dir", raiz, "list", "--tag", "kumi-perfil", "--kind", "decision", "--json") or []
    return {**ms[0]["data"], "id": ms[0]["id"]} if ms and ms[0].get("data") else None

def perfil_guardar(raiz, perfil, previa_id=None):
    if previa_id:
        omoi("--dir", raiz, "update", previa_id, "--data", json.dumps(perfil), "--json")
    else:
        omoi("--dir", raiz, "remember", "--json", "-k", "decision", "-t", "Kumi: perfil del proyecto",
             "--tag", "kumi", "--tag", "kumi-perfil", "--data", json.dumps(perfil), "--force",
             "Decisiones de Marc para que Kumi trabaje este proyecto.")
# `--force` solo se salta la búsqueda de duplicadas. Nunca deja pasar una memoria de oficio (`--trade`)
# por el aviso de fugas: para eso está `--allow-leaks`, para un falso positivo que alguien ha mirado,
# nunca por defecto.

# La cola: una decisión bloqueante de una tarea, lo que mira la puerta y la aprobación.
def decision(raiz, tarea, titulo, texto, bloqueante=True):
    tags = ["--tag", "kumi", "--tag", f"tarea:{tarea}", "--tag", "open"] + (["--tag", "blocking"] if bloqueante else [])
    return (omoi("--dir", raiz, "remember", "--json", "-k", "decision", "-t", titulo, *tags, "--force", texto) or {}).get("id")

def bloqueantes_abiertas(raiz, tarea):
    return omoi("--dir", raiz, "decisions", "--blocking", "--tag", f"tarea:{tarea}", "--json") or []

def cerrar(raiz, mem_id, respuesta):
    omoi("--dir", raiz, "decisions", "--resolve", mem_id, "--answer", respuesta, "--json")
```

## Guarda aunque el agente se olvide

Al cerrar o compactar una sesión, Omoikane lee la conversación y aparta lo que quizá valga la pena guardar: lo que respondiste a las preguntas del agente (con la pregunta), tus preferencias y correcciones, y el último commit o publicación. La sesión siguiente en ese proyecto lo recibe una vez, y el agente guarda lo que valga con tus palabras; la pestaña Revisar también lo enseña. Nada que parezca un secreto ni lo que ya está guardado. Son propuestas, y caducan a los 7 días si nadie las guarda.

En Claude Code lee también los subagentes de la sesión. De cada uno que haya terminado aparta su informe final con lo que se le encargó, porque el agente principal lo lee una vez y casi nunca lo guarda. Uno que sigue trabajando se salta, igual que uno con algo que parezca un secreto, y lo ya guardado no se vuelve a proponer.

Lee la conversación de Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro, OpenCode y Grok Build (`omoi setup <agente>` añade los hooks de cierre); no la de Antigravity, que la guarda en un formato binario sin documentar. En los agentes sin recall al empezar, como Grok, lo apartado llega con la herramienta `context`.

## No guarda secretos

Si un token, una clave privada o una contraseña se cuela en una memoria, se cambia por `[redacted:<tipo>]` antes de guardarla, y la respuesta se lo dice al agente. Lo que se guardó antes sale en Revisar con un botón para redactarlo, historial incluido, y nada de eso sale del equipo al sincronizar.

## Historial con motivo

Cambiar una memoria guarda la versión anterior y, si el agente lo dice, por qué cambió. `omoi history` las enseña todas.

**Lo que queda fuera.** Un agente rellena los huecos de lo que le pides: «que el login sea seguro» puede acabar con un doble factor que nadie pidió. Cuando decidas qué entra y qué no, guárdalo como `decision`, con lo que queda fuera escrito tal cual: «El login es email y contraseña, con recuperar contraseña y entrar con Google. Sin doble factor por ahora: no lo pide nadie y complica el alta». Las decisiones vuelven por relevancia: cuando un mensaje hable del login, el agente la tendrá delante antes de ponerse a añadir cosas. Escríbela con las palabras que usarías al pedirlo («seguridad del login», «doble factor»): son las que la traen de vuelta. Si cambias de idea, `update` con el motivo y la versión anterior se queda en el historial.

## Recetas y memorias conectadas

**Recetas.** Una memoria de tipo `procedure` es algo que se repite (publicar una versión, desplegar): pasos numerados y una línea `when: publica, release`. Cuando un mensaje la pide, vuelve entera («publica la 0.14.1» trae la lista entera), y el agente la corrige cuando un paso falla o cambia.

**Recetas propuestas.** Omoikane también propone recetas. Al cerrar cada sesión apunta, en una tabla local, los pasos que funcionaron: el programa y su subcomando, como `cargo test` o `git tag`, sin lo que solo mira (y, en Claude Code, sin lo que falló). Cuando una secuencia de tres pasos o más, con dos programas distintos al menos, ya ha salido en cinco sesiones y ninguna receta tuya la nombra, la aparta para la sesión siguiente. El agente te pregunta si es un procedimiento que sigues y, si le dices que sí, la guarda como receta con sus pasos y su línea `when:`. Cada secuencia se propone una sola vez, y como mucho una por sesión. Repasando 115 sesiones reales salieron dos, `git fetch → git tag → gh release` y `git fetch → gh issue → gh pr`, que es mirar si te han contestado en tus PR.

**Memorias conectadas.** `[[nombre]]` en una memoria la enlaza con la que lleva esa etiqueta, y los enlaces viajan con las notas. Cuando se recuerda una memoria, puede venir con ella una trampa o una preferencia enlazada; los enlaces rotos salen en Revisar y la página de cada proyecto dibuja sus conexiones.
