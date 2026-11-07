# Qué es una memoria y cómo se guarda

[English](saving.md) · [Volver al README](../README.es.md)

## Modelo de memoria

Una memoria es un párrafo que el agente querría leer antes de volver a tocar algo.

| campo | quién lo pone |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | el agente |
| `paths`, `tags`, `pinned` | el agente, si quiere |
| `project`, `source`, fechas, estadísticas de recall, historial de versiones | Omoikane |

Cada `kind` cambia cómo se recupera: las preferencias y lo fijado se inyectan siempre; los gotcha salen cuando el prompt o los ficheros coinciden; hechos y decisiones, por relevancia; las recetas, enteras, cuando el mensaje dice una de las palabras de su `when:`. Una memoria con la etiqueta `status` dice en qué punto se quedó el trabajo de un proyecto: al empezar sesión sale la última de cada proyecto con parte de su contenido, para que «sigamos con X» no obligue a buscar.

## Guarda sin duplicados

Cada memoria tiene un tipo que decide cuándo vuelve: `fact` y `decision` por relevancia, `gotcha` también por sus ficheros, `preference` siempre y `procedure` entera cuando la pides. Antes de guardar mira si ya existe algo casi igual. Si lo hay, no la guarda y dice cuál actualizar; si solo se parece, la guarda y enseña las relacionadas. Avisa cuando pasa de los 6.000 caracteres que cuentan para el recall y cuando deja anticuada a otra. Las rutas se guardan relativas al repositorio, así que valen en cualquier equipo.

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
