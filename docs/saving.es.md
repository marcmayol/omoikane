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

## Guarda cuando se lo pides

`/omoikane` hace que el agente guarde en ese momento. Repasa la sesión, guarda lo que valga y aún no esté guardado (decisiones con su motivo, trampas con sus ficheros, preferencias, recetas) y pone al día la memoria `status` del proyecto. Con texto detrás, como en `/omoikane el deploy va por ssh`, guarda eso con tus palabras. Termina con una línea por memoria guardada o cambiada.

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
