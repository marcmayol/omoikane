# Que la memoria siga siendo cierta, y que no se pierda

[English](memory-care.md) · [Volver al README](../README.es.md)

## Se cuida sola

Omoikane se da cuenta de cuándo una memoria se ha quedado atrás: otra más nueva del mismo tema dice algo distinto (una versión más alta de lo mismo, o hecho lo que estaba pendiente), una memoria `status` que el proyecto ya ha dejado atrás, rutas que ya no existen, memorias que en un mes no se han usado nunca y notas demasiado largas para el recall. Cuando una de esas le llega al agente, va marcada con `⚠ may be outdated: newer <id> …`, y al guardar una memoria que deja anticuada a otra se le dice en el momento, para que lo arregle ahí mismo. La pestaña **Revisar** de la vista web las enseña con las dos memorias una al lado de la otra, *olvidar la vieja* y *está bien así*; `omoi review` hace lo mismo desde la terminal. Sin modelo: palabras, fechas y versiones.

## Veredictos

Lo que Omoikane no puede saber solo lo dice el agente, sin herramientas nuevas: al guardar o cambiar una memoria puede pasar `supersedes` (esta sustituye a aquellas), `contradicts` (dicen cosas distintas y no se sabe cuál vale) o `compatible` (un aviso las emparejó y valen las dos). El veredicto se guarda, viaja con la sincronización y sale donde salen las memorias: en el recall (`⚠ superseded by …`), en `search`, en `context` y en Revisar. Vale mientras no cambie lo que juzgó, y de una pareja manda el más nuevo. «Está bien así» también es un veredicto, así que lo que das por bueno en un equipo vale en los demás.

**Por qué y quién.** Cada veredicto guarda el motivo y quién lo dio (la persona de la sincronización, o el agente y su cliente), y los dos salen en el recall, en `search` y en Revisar. Si A está sustituida por B y B por C, la nota de A lleva directamente a C.

## Corregir un trozo y juntar

`update` acepta `replace` (cambia un texto que tiene que aparecer exactamente una vez; si no, no toca nada), `append` (añade al final, para el paso nuevo de una receta) y `absorb` (junta varias memorias en una: el agente escribe el texto fundido, y las absorbidas van a la papelera con su historial colgado de la que queda y sus etiquetas y enlaces pasados a ella). La pestaña Revisar tiene un botón **Juntar** para hacerlo a mano.

## Fecha de revisión

Cada tipo tiene la suya: decisiones y recetas a los 120 días sin tocar, hechos y trampas a los 180, y las preferencias nunca, contando desde su último cambio o desde la última vez que el agente la recibió (lo que se usa y nadie corrige, está repasado). Cuando llega, la memoria sale en Revisar y en `search` con «not reviewed in N days»; en el recall no, porque ahí solo va lo que dice que una memoria puede estar mal. «Está bien así» vuelve a poner el reloj a cero.

## Historial y papelera

Cambiar una memoria guarda la versión anterior con el motivo, y `omoi history` las enseña todas.

**Papelera.** Olvidar una memoria (`forget`, desde el agente, la CLI o la vista web) la manda a la papelera con su historial, y un olvido tampoco vuelve desde otro equipo cuando sincronizas. `omoi restore <id>` la devuelve, con su historial, y gana en los demás equipos en la siguiente sincronización: un `forget` hecho por error en una máquina se deshace desde cualquiera. El agente lo hace con `update` y `restore: true`, sin herramienta nueva; la pestaña Revisar tiene el grupo **Papelera** con un botón para cada una. A los 30 días (`OMOIKANE_TRASH_DAYS`; 0 = nunca) se borran del todo; `forget --purge` no pasa por la papelera y `omoi trash --empty` la vacía.

## Copia fiel

`omoi export` escribe un JSONL con todo lo que hace falta para reconstruir la base: cada memoria con su historial, sello y autor, las marcas de olvido, los veredictos y la papelera, con una línea de cierre que lleva el recuento y el SHA-256 (un fichero cortado o tocado no se importa). `omoi import` lo vuelve a meter con las mismas reglas que la sincronización: los mismos ids, fechas y sellos, gana el más nuevo y lo olvidado no resucita, así que repetirlo no cambia nada y sirve tanto para restaurar una base como para juntar dos. Las dos van en streaming, una línea en memoria cada vez: 50.000 memorias se exportan e importan sin cargar la RAM. Con `--encrypt`, cada línea va cifrada con la clave de la sincronización (la de un proyecto de un cliente, con la de su espacio), para guardar la copia donde quieras. Lo de la papelera va con su proyecto y su espacio, y con sus veredictos: restaurado en otro equipo, vuelve a su sitio y con su `superseded`. Los ficheros del formato anterior y los de engram siguen entrando, y uno de un omoi más nuevo también: lo que esta versión no entiende se salta y se cuenta, y los campos que no conoce se guardan y vuelven a salir en el siguiente export.

## Copia diaria

Al empezar la primera sesión de cada día, Omoikane copia la base a `~/.omoikane/backups/omoikane-<fecha>.db` y conserva las 7 últimas (`OMOIKANE_BACKUPS`; 0 = apagado). `omoi backup` la hace ahora y `omoi doctor` dice cuándo fue la última.

## Importar de otras herramientas

`omoi import` lee las exportaciones de engram (`engram export`) y las de `omoi export`: adapta los tipos, conserva las fechas, se salta lo borrado y no importa nada dos veces. `--dry-run` enseña adónde iría cada memoria.
