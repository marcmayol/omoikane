# Que la memoria siga siendo cierta, y que no se pierda

[English](memory-care.md) · [Volver al README](../README.es.md)

## Se cuida sola

Omoikane se da cuenta de cuándo una memoria se ha quedado atrás: otra más nueva del mismo tema dice algo distinto (una versión más alta de lo mismo, o hecho lo que estaba pendiente), una memoria `status` que el proyecto ya ha dejado atrás, rutas que ya no existen, memorias que en un mes no se han usado nunca y notas demasiado largas para el recall. Cuando una de esas le llega al agente, va marcada con `⚠ may be outdated: newer <id> …`, y al guardar una memoria que deja anticuada a otra se le dice en el momento, para que lo arregle ahí mismo. La pestaña **Revisar** de la vista web las enseña con las dos memorias una al lado de la otra, *olvidar la vieja* y *está bien así*; `omoi review` hace lo mismo desde la terminal. Sin modelo: palabras, fechas y versiones.

Cuando hay mucho por revisar, no hace falta abrir la pestaña: al empezar la sesión el agente recibe un aviso corto con cuántas hay de cada tipo y la indicación de mirarlas y proponerte qué hacer (actualizar, juntar con `absorb`, olvidar, marcar `supersedes` o `compatible`, arreglar los enlaces), sin tocar nada hasta que digas que sí. Sale a partir de 20 pendientes, o del 10 % de las memorias si son más, una vez al día, y otra vez el mismo día solo si la cifra crece bastante. Los agentes sin hooks lo reciben en `review_notice` de la herramienta `context`. Para que el arranque siga siendo rápido, el aviso cuenta sin leer el contenido ni el disco: las rutas rotas y los posibles secretos solo salen en la revisión entera.

## Veredictos

Lo que Omoikane no puede saber solo lo dice el agente, sin herramientas nuevas: al guardar o cambiar una memoria puede pasar `supersedes` (esta sustituye a aquellas), `contradicts` (dicen cosas distintas y no se sabe cuál vale) o `compatible` (un aviso las emparejó y valen las dos). El veredicto se guarda, viaja con la sincronización y sale donde salen las memorias: en el recall (`⚠ superseded by …`), en `search`, en `context` y en Revisar. Vale mientras no cambie lo que juzgó, y de una pareja manda el más nuevo. «Está bien así» también es un veredicto, así que lo que das por bueno en un equipo vale en los demás.

**Por qué y quién.** Cada veredicto guarda el motivo y quién lo dio (la persona de la sincronización, o el agente y su cliente), y los dos salen en el recall, en `search` y en Revisar. Si A está sustituida por B y B por C, la nota de A lleva directamente a C.

## Corregir un trozo y juntar

`update` acepta `replace` (cambia un texto que tiene que aparecer exactamente una vez; si no, no toca nada), `append` (añade al final, para el paso nuevo de una receta) y `absorb` (junta varias memorias en una: el agente escribe el texto fundido, y las absorbidas van a la papelera con su historial colgado de la que queda y sus etiquetas y enlaces pasados a ella). La pestaña Revisar tiene un botón **Juntar** para hacerlo a mano.

## Proyectos repetidos

A veces el mismo proyecto acaba partido en dos: `Omoikane` y `omoikane`, una carpeta que se renombró sin `origin` (`Narrador`, que ya no está, y `Aloud`, que lo nombra) o un «proyecto» que es la carpeta donde se subió un fichero (`.claude\uploads\<uuid>`, `%TEMP%`). La revisión lo detecta y lo propone, pero nunca junta nada sola, porque un `merge` llega a todos los equipos por la sincronización. Al empezar la sesión, una vez al día, el agente recibe cada caso con la orden ya escrita (`omoi projects merge A B`) y te pregunta antes de lanzarla. Si no son el mismo, `omoi projects distinct A B` hace que no vuelva a salir. Lo enseñan también `omoi review`, `context` (`projects_review`) y la pestaña Revisar.

Cuenta como repetido el mismo nombre sin mayúsculas, acentos, guiones, guiones bajos, espacios ni puntos (`social-publish` = `Social Publish`). A una letra de distancia (`Omoikane`/`Omoikae`, desde 6 letras) solo cuenta si hay otra señal: una de las carpetas ya no existe, el más nuevo casi no tiene memorias o los dos comparten `origin`. Un parecido a secas no basta: `Tokimichi` y `tokimichi-web` son proyectos distintos.

## Fecha de revisión

Cada tipo tiene la suya: decisiones y recetas a los 120 días sin tocar, hechos y trampas a los 180, y las preferencias nunca, contando desde su último cambio o desde la última vez que el agente la recibió (lo que se usa y nadie corrige, está repasado). Cuando llega, la memoria sale en Revisar y en `search` con «not reviewed in N days»; en el recall no, porque ahí solo va lo que dice que una memoria puede estar mal. «Está bien así» vuelve a poner el reloj a cero.

## Historial y papelera

Cambiar una memoria guarda la versión anterior con el motivo, y `omoi history` las enseña todas.

**Papelera.** Olvidar una memoria (`forget`, desde el agente, la CLI o la vista web) la manda a la papelera con su historial, y un olvido tampoco vuelve desde otro equipo cuando sincronizas. `omoi restore <id>` la devuelve, con su historial, y gana en los demás equipos en la siguiente sincronización: un `forget` hecho por error en una máquina se deshace desde cualquiera. El agente lo hace con `update` y `restore: true`, sin herramienta nueva; la pestaña Revisar tiene el grupo **Papelera** con un botón para cada una. A los 30 días (`OMOIKANE_TRASH_DAYS`; 0 = nunca) se borran del todo; `forget --purge` no pasa por la papelera y `omoi trash --empty` la vacía.

## Diario de sesiones

Además de las memorias, Omoikane apunta una línea por sesión: cuándo empezó y acabó, en qué proyecto, qué ficheros se editaron o escribieron (relativos al repositorio, hasta 50), qué memorias le llegaron por el recall y cuáles se guardaron o cambiaron en ella. No guarda la conversación. Sirve para preguntas que las memorias no responden, porque la de estado solo guarda la última foto: «¿qué hice el martes en la web?» o «¿cuándo tocamos `sync.rs`?».

`omoi diary` las enseña de la más reciente a la más vieja, con `--since 2026-11-03` (o `7d`), `--until`, `--file sync.rs`, `--project`, `--all` y `--json`. Las fechas van en UTC, y una sesión cuenta para todos los días que dura. El agente las pide con `recent` y `sessions: true` (con `since`, `until` y `file`), sin herramienta nueva.

El diario también alimenta el recall: cuando un mensaje nombra un fichero que cambió en al menos 3 sesiones del proyecto, al agente le llega una línea con en cuántas sesiones cambió y cuándo fue la última, los ficheros que suelen cambiar con él (en al menos 3 de esas sesiones y el 40 %) y cuántos gotcha lo mencionan, una vez por sesión. Por ejemplo: ``History of `src/recall.rs`: changed in 30 sessions (last 2026-11-04); it usually changes together with `src/main.rs` (18 of 30), `Cargo.toml` (12) and `src/mcp.rs` (12).``

`omoi file-history` da esas mismas cifras enteras, para código: las lee una puerta de riesgo que decide sobre un diff si un cambio se publica solo o lo revisa una persona, y no decide ningún agente. Acepta rutas relativas al repositorio o absolutas, o una por línea por la entrada estándar (la salida de `git diff --name-only`), y responde por el proyecto de la carpeta actual o de `--dir`. No tiene umbrales: por fichero, en cuántas sesiones cambió y cuándo fue la última, hasta 10 ficheros que cambiaron en las mismas sesiones con cuántas, y los gotcha (del proyecto, de su espacio y globales) que lo nombran en sus rutas. También cuenta las sesiones abiertas en otra carpeta, como la del usuario, que editaron ficheros de este repositorio (esas los guardan con la ruta absoluta), pero solo sus ficheros de dentro. El `sessions` de arriba es cuántas sesiones tocaron ficheros del proyecto, para poner lo demás en proporción. Un fichero que el diario no ha visto nunca sale con `sessions: 0` y `last: null`.

```sh
git diff --name-only origin/master... | omoi file-history --json
```

```json
{
  "project": "/home/ana/trabajo/acme-web",
  "sessions": 48,
  "files": [
    {
      "file": "src/sync.rs",
      "sessions": 14,
      "last": "2026-11-04",
      "changes_with": [
        { "file": "tests/sync.rs", "sessions": 9 },
        { "file": "CHANGELOG.md", "sessions": 7 }
      ],
      "gotchas": 2,
      "gotcha_ids": ["01M64WZATAVWKSYRMVG4FCV64V", "01M666RGFB3JWVYHTPYVPFD282"]
    }
  ]
}
```

La puerta de riesgo de Kumi, una factoría de agentes que usa Omoikane como memoria, podría leerlo así: un fichero con gotchas sube el nivel, y también un diff que deja fuera un fichero que casi siempre cambia con uno que sí toca.

```python
diff = subprocess.run(["git", "diff", "--name-only", "origin/master..."], capture_output=True, text=True).stdout
h = json.loads(subprocess.run(["omoi", "file-history", "--json"], input=diff, capture_output=True, text=True).stdout)
tocados = set(diff.split())
nivel = 0
for f in h["files"]:
    if f["gotchas"]:
        nivel = max(nivel, 1)
    if any(j["file"] not in tocados and j["sessions"] >= 0.6 * f["sessions"] >= 3 for j in f["changes_with"]):
        nivel = max(nivel, 1)
```

Solo lee y, como el diario, solo conoce las sesiones de este equipo.

Se escribe al cerrar o compactar la sesión, con el mismo hook que aparta las candidatas, y nunca en el recall de cada mensaje. Si la sesión se compacta, la línea se completa y no se repite. Como guardadas cuentan las memorias cuyo id devolvió `remember` o `update` en esa conversación. Mirar qué memorias cambiaron entre el principio y el final de la sesión daría por suyas las que guardaba a la vez otra sesión en el mismo proyecto. Las preferencias y las fijadas no cuentan como recibidas, porque llegan en todas las sesiones.

Se queda en este equipo: no viaja con la sincronización ni con `omoi export`, porque las rutas de tus ficheros dicen mucho de lo que haces y el historial es de cada equipo. Sí va en la copia diaria, que es la base entera. Pesa poco: con conversaciones reales, 142 bytes por sesión de mediana y 716 de media (unos 2 MB al año con diez sesiones al día). Se guarda un año.

## Copia fiel

`omoi export` escribe un JSONL con todo lo que hace falta para reconstruir la base: cada memoria con su historial, sello y autor, las marcas de olvido, los veredictos y la papelera, con una línea de cierre que lleva el recuento y el SHA-256 (un fichero cortado o tocado no se importa). `omoi import` lo vuelve a meter con las mismas reglas que la sincronización: los mismos ids, fechas y sellos, gana el más nuevo y lo olvidado no resucita, así que repetirlo no cambia nada y sirve tanto para restaurar una base como para juntar dos. Las dos van en streaming, una línea en memoria cada vez: 50.000 memorias se exportan e importan sin cargar la RAM. Con `--encrypt`, cada línea va cifrada con la clave de la sincronización (la de un proyecto de un cliente, con la de su espacio), para guardar la copia donde quieras. Lo de la papelera va con su proyecto y su espacio, y con sus veredictos: restaurado en otro equipo, vuelve a su sitio y con su `superseded`. Los ficheros del formato anterior y los de engram siguen entrando, y uno de un omoi más nuevo también: lo que esta versión no entiende se salta y se cuenta, y los campos que no conoce se guardan y vuelven a salir en el siguiente export.

## Verla en Obsidian

`omoi export --obsidian <vault>` escribe una nota Markdown por memoria en `<vault>/omoikane/` para verla en la vista de grafo de Obsidian. Cada nota se llama como su memoria, por ejemplo `Usamos UUID.md`, porque eso es lo que Obsidian enseña como título; si dos se llamarían igual, llevan detrás el final de su id. Las de un proyecto van en su carpeta y enlazan a la nota del proyecto. Las globales, no. Con casi todo global, una nota común para ellas convertía el grafo en una estrella enorme que tapaba los enlaces que de verdad importan. Además, cada memoria enlaza a las que nombra con `[[nombre]]`, a las que sustituye o contradice y a una nota por fichero citado dos veces o más. El tipo va como etiqueta, y si el vault aún no tiene configuración del grafo, se le pone un color por tipo. Es un espejo de la base: cada exportación lo rehace, solo escribe lo que ha cambiado y borra lo que ya no existe. Lo que edites allí se pierde, y lo que no lleva la marca de Omoikane en la cabecera se queda donde está. Por defecto exporta lo personal y lo global; `--space acme` saca solo lo de ese cliente, sin lo global, y conviene llevarlo a otro vault. Con `--watch` sigue abierto y vuelve a exportar cuando cambia la base (comprueba cada `--interval` segundos, 60 por defecto). Con 173 memorias tarda menos de un segundo.

## Copia diaria

Al empezar la primera sesión de cada día, Omoikane copia la base a `~/.omoikane/backups/omoikane-<fecha>.db` y conserva las 7 últimas (`OMOIKANE_BACKUPS`; 0 = apagado). `omoi backup` la hace ahora y `omoi doctor` dice cuándo fue la última.

## Importar de otras herramientas

`omoi import` lee las exportaciones de engram (`engram export`) y las de `omoi export`: adapta los tipos, conserva las fechas, se salta lo borrado y no importa nada dos veces. `--dry-run` enseña adónde iría cada memoria.
