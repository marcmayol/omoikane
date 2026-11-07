# Recall y búsqueda

[English](recall.md) · [Volver al README](../README.es.md)

Cómo le pasa Omoikane al agente lo que necesita sin que se lo pida, cómo calla el resto del tiempo y cómo busca.

## Recuerda solo

Al empezar la sesión le llegan al agente tus preferencias y lo fijado, en qué punto se quedó cada proyecto, lo último en lo que se trabajó y los avisos de lo que conviene revisar. En cada mensaje solo le llega algo si tiene que ver de verdad: una memoria tiene que cubrir bastantes palabras poco frecuentes del mensaje, y las trampas salen también cuando nombras sus ficheros. Cada memoria llega una vez por sesión. En 300 mensajes reales solo inyectó algo en un 7 % de ellos. Si en 15 minutos y 10 mensajes no se ha guardado nada, le recuerda que lo haga.

Retoma donde lo dejaste: al empezar la sesión llega la última memoria `status` de cada proyecto, así que «sigamos con X» no obliga a buscar.

Los nombres de proyecto se entienden aunque estén mal escritos. Donde se escribe uno (`project`, `--project`, `omoi projects`), da igual cómo vayan las mayúsculas, los acentos, los guiones, los guiones bajos, los espacios o los puntos (`cronicas` encuentra `Crónicas`), y si nada más encaja vale con una letra de diferencia (`Omoikae`). Si encaja con varios, pregunta cuál. En el mensaje, un nombre mal dictado («tokimicho» por Tokimichi, «onmoikane» por Omoikane) se busca como el nombre bueno, siempre que la palabra no esté en ninguna memoria, las dos tengan al menos 6 letras, la palabra no sea el nombre con una terminación normal («faltas» no es Faltis) y el proyecto se vea desde donde está la sesión (desde un proyecto de un cliente, solo los de ese cliente; nunca los de otro). Con 5 letras, palabras normales se volvían proyectos («tropa» Troba, «rodeo» Rodet, «balsa» Balùa), así que un nombre de 5 («trova» por Troba) ya no se adivina.

## El ruido, lo viejo y lo erróneo

Un recall que inyecta de más es peor que no tenerlo: llena el contexto de ruido y arrastra de una sesión a otra lo que ya no vale. Por eso es la parte de Omoikane con más horas de calibrado.

- **Casi siempre calla.** Un mensaje solo recibe algo si tiene al menos 3 palabras útiles y una memoria comparte con él 2 palabras poco frecuentes en tu base que sumen como mínimo la mitad del peso del mensaje. El canal por significado, para las paráfrasis, pide además que las palabras cubran un 25 % y no se usa en mensajes de más de 500 caracteres. En 300 mensajes reales inyectó algo en el 7 %; los demás no recibieron nada. «¿Cómo ha ido todo?» no trae nada.
- **Poco y una sola vez.** Como mucho 2 memorias por mensaje, y solo las que se acercan a la mejor: si una destaca, va sola. De cada una llegan 200 caracteres; si el agente quiere el resto, la pide con `get`. Puede sumarse una trampa o preferencia enlazada, y una receta solo si el mensaje la pide: una frase de su `when:`, o una de sus palabras y algo más que diga que es esa receta (una palabra de su título o de su proyecto, o estar en su proyecto), así que «publica un post» no trae «Publicar una versión». Cada memoria llega una vez por sesión.
- **Lo viejo llega marcado.** Si una memoria más nueva del mismo tema la contradice (otra versión de lo mismo, o hecho lo que estaba pendiente), la vieja llega con `⚠ may be outdated: newer <id> …` delante y el agente mira la nueva antes de fiarse. También avisa de estados de proyecto que se han quedado atrás, rutas que ya no existen y memorias que nadie usa. La pestaña Revisar pone las dos memorias lado a lado para olvidar la vieja o dejarla. Todo sin modelo: palabras, fechas y versiones.
- **Lo erróneo se corrige, no se arrastra.** En la memoria solo entra lo que guarda el agente. Lo que Omoikane aparta al cerrar una sesión son propuestas, y caducan a los 7 días si nadie las guarda. Corregir es `update` con el motivo, y la versión anterior queda en el historial. `forget` la borra, y tampoco vuelve desde tus otros equipos. Lo que guarda un compañero llega marcado `[by Ana]`.
- **Medido.** Cada umbral sale de preguntas escritas a ciegas por otro agente, 20 de ellas sin respuesta en las notas: con esas inyectó algo en 2. Los números, en [BENCHMARK.es.md](../BENCHMARK.es.md).

Al empezar la sesión sí llega más: tus preferencias, lo que has fijado, en qué punto está cada proyecto y lo último que se tocó. Las preferencias y lo fijado son lo que tú quieres que el agente sepa siempre. La primera sesión en un proyecto de un cliente le cuenta además lo que tiene el resto del equipo de ese cliente ([Llega alguien nuevo a un cliente](sync-and-spaces.es.md#llega-alguien-nuevo-a-un-cliente)).

engram, la alternativa con la que se compara Omoikane, no inyecta memorias en cada mensaje. Con lo viejo hace otra cosa: cada vez que guarda, propone las memorias parecidas y le pide al agente que juzgue qué relación tienen (`mem_judge`). Omoikane no pide nada. Detecta él la contradicción, la marca en el recall con el motivo y deja al agente juzgar solo cuando sabe algo que Omoikane no puede ver (`supersedes`, `contradicts`, `compatible`). Cómo funcionan esos veredictos está en [memory-care.es.md](memory-care.es.md#veredictos).

## Entiende lo que preguntas con otras palabras

Las palabras encuentran una memoria cuando preguntas con las palabras que usa. Si preguntas lo mismo de otra forma («¿cómo recupero una versión que retiré?» para una memoria sobre `cargo yank --undo`), las palabras no la encuentran, y el modelo ligero casi nunca. Desde la 1.2 un segundo modelo, [EmbeddingGemma 300M](https://huggingface.co/google/embeddinggemma-300m), compara el significado. Solo entra cuando la memoria lo necesita:

- **Ligera por defecto.** Con pocas memorias, las palabras solas aciertan igual. Por debajo de 1.000, el benchmark apenas ve diferencia en el recuerdo, así que por debajo de **1.000 memorias** Omoikane va como la 1.1, unos 35 ms por mensaje, sin descargar el modelo ni tener un servicio abierto. Cuando la memoria pasa de 1.000, Omoikane lo descarga solo en segundo plano (unos 450 MB), calcula los vectores de las memorias y lo dice una vez al empezar una sesión. Cuentan todas las memorias de la base, no solo las del proyecto: el modelo, su servicio y sus vectores son de la base entera, así que la decisión es la misma desde cualquier carpeta.
- **Lo que eliges manda sobre el tamaño.** `omoi model semantic --on` lo usa aunque la memoria sea pequeña, `--off` nunca (y cierra su servicio), `--auto` deja que vuelva a decidir el tamaño. `omoi model semantic` y `omoi doctor` dicen si se usa y por qué.
- **En la búsqueda**, siempre (cuando está en uso): cada candidata suma su parecido en significado y una parte de su puntuación por palabras; con cinco palabras o menos (palabras clave) pesan más las palabras. En el benchmark público, con 3.000 memorias, la memoria buena sale la primera en el 55 % de las paráfrasis (9 % sin él, 6 % engram), el 89 % de las preguntas y el 96 % de las búsquedas por palabras clave. Lo que solo trae el significado, sin ninguna palabra en común, necesita un parecido de 0,35: una búsqueda puede volver vacía en vez de enseñar algo sin relación.
- **En el recuerdo, solo para callar**: se le pregunta solo cuando las palabras ya trajeron algo, en un mensaje de más de cinco palabras y de hasta 300 caracteres, y lo que traen las palabras se queda fuera si no está entre las tres memorias más parecidas en significado. Las preferencias nunca se quitan: una preferencia se aplica sin parecerse al mensaje. En las preguntas sin respuesta, el recuerdo mete algo el 7,5 % de las veces con 3.000 memorias (40 % sin él). Se le pregunta en pocos mensajes (60 de 1.161 mensajes del autor), así que el recuerdo sigue cerca de 35 ms.
- **Si no responde a tiempo** (250 ms en el recuerdo, por ejemplo con la CPU ocupada), el recuerdo sigue solo con palabras, como en la 1.1. No pasa en silencio: `omoi doctor` dice cuántas veces le preguntó el recuerdo y cuántas se quedaron sin respuesta, y `OMOIKANE_TRAZA=1` escribe cada paso de un recuerdo en stderr.
- **Lo que cuesta**, medido en el benchmark público: la búsqueda pasa de ~32 a ~64 ms; el recuerdo, de ~27-31 ms a ~32 ms con 3.000 memorias y ~54 ms con 10.000 (la mayoría de los mensajes no le preguntan). Unos 450 MB en disco y calcular una vez el vector de cada memoria en segundo plano (450 s para 3.000 notas cortas en 8 hilos de CPU; las memorias largas tardan más).
- **Cómo funciona**: en Rust puro dentro de `omoi`, en la CPU, con un motor propio (pesos en f16 leídos directamente del disco, AVX2; las CPU sin AVX2 usan candle). Carga en unos 26 ms y usa unos 13 MB de RAM propia, porque los pesos se quedan en el fichero. Lo mantiene abierto un servicio propio (uno por usuario, solo en `127.0.0.1` y con token) que se cierra tras 30 minutos sin uso. Nadie espera a que cargue: hasta que está, y hasta que casi todas las memorias tienen su vector, todo va por palabras como antes. `omoi model install --semantic` lo descarga ya y comprueba el SHA-256 de cada fichero.
- **RAM al indexar**: el servicio calcula los vectores de las memorias nuevas por lotes; `omoi model semantic --ram <MB>` pone tope a la RAM de esos lotes (con más va algo más rápido; `0` vuelve al de por defecto). `omoi model semantic` y `omoi doctor` enseñan el tope en uso.

## Busca en tu idioma

La búsqueda empieza por palabras: le dan igual las tildes, mira también las rutas de fichero y da más peso a lo poco frecuente. Después, un modelo de embeddings local que abre en 2,7 ms rescata lo que se parece mucho aunque no comparta ninguna palabra. Dónde busca depende de dónde estés. Desde un proyecto, en él y en lo global; desde tu carpeta de usuario, en todo.

Funciona en 101 idiomas, con tildes o sin ellas. Con preguntas escritas a ciegas acierta la memoria a la primera el 90 % de las veces y entre las tres primeras el 97 %, con una mediana de 43 ms, y sigue igual de precisa con 3.183 memorias (50 ms por búsqueda). Cuatro procesos comparten el modelo con 13 MB de memoria cada uno. El modelo es [potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M) (int8), y abre en 2,7 ms en lugar de 1,78 s. Hasta que se descarga (~150 MB, una vez), la búsqueda va por palabras.

## También lee tu documentación

Si el proyecto ya tiene su documentación en Markdown (una carpeta `docs/`, una wiki para LLM, una bóveda de Obsidian), dile a Omoikane dónde está:

```sh
omoi docs add docs                                   # la indexa en el momento
omoi docs list
omoi docs test "¿cómo vuelvo atrás un despliegue?"   # lo que traería el recall con eso
omoi docs remove docs
```

Desde entonces, junto a las memorias, el recall puede traer el apartado de esa documentación que toca con el mensaje: uno como mucho, unos 700 caracteres con su fichero y su título, y cada uno una vez por sesión. Si el agente necesita más, abre el fichero. La regla es la de las memorias: el apartado tiene que compartir dos palabras raras en la documentación y cubrir el 60 % del peso del mensaje (algo más que una memoria, a la que le basta la mitad, porque un apartado es más largo), así que un mensaje que no va de ella no recibe nada.

No se copia nada a la memoria ni se escribe en los ficheros. Omoikane guarda un índice, partido por títulos (`#`, `##`, `###`), solo de ficheros `.md` y `.markdown` normales (no sigue enlaces simbólicos y deja fuera los de más de 2 MB), y lo pone al día al empezar cada sesión leyendo solo los ficheros que cambiaron de fecha o de tamaño. Lo hace aparte, así que la sesión no lo espera, ni siquiera al cambiar a una rama donde han cambiado cientos de ficheros; mientras tanto, el recuerdo usa el índice que había y no empieza otro refresco del proyecto. No se puede añadir la raíz de una unidad ni tu carpeta personal: indica la carpeta de la documentación. Las carpetas son del proyecto desde el que haces `omoi docs add`, y tanto ellas como el índice se quedan en este equipo: no se sincronizan ni se exportan. Un proyecto sin documentación no paga nada por esto.

## ¿Y CLAUDE.md?

Con un proyecto pequeño y veinte cosas que recordar, un buen CLAUDE.md basta. Omoikane compensa donde CLAUDE.md deja de llegar:

- CLAUDE.md se carga entero en cada sesión, venga o no a cuento, y cuanto más sabe más contexto gasta. Omoikane trae lo que encaja con el mensaje.
- Es un fichero por proyecto. Lo que aprendes en uno no llega a otro si no lo copias; en Omoikane una memoria puede valer para todos.
- No sabe cuándo algo se ha quedado viejo ni guarda por qué cambió.
- Lo escribes tú. Omoikane lo va llenando el agente mientras trabaja.
- Lo lee un agente. Omoikane lo comparten 15, y todos tus equipos.

Pueden convivir: CLAUDE.md para las reglas fijas del repositorio y Omoikane para lo que se va aprendiendo.
