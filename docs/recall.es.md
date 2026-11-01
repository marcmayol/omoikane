# Recall y búsqueda

[English](recall.md) · [Volver al README](../README.es.md)

Cómo le pasa Omoikane al agente lo que necesita sin que se lo pida, cómo calla el resto del tiempo y cómo busca.

## Recuerda solo

Al empezar la sesión le llegan al agente tus preferencias y lo fijado, en qué punto se quedó cada proyecto, lo último en lo que se trabajó y los avisos de lo que conviene revisar. En cada mensaje solo le llega algo si tiene que ver de verdad: una memoria tiene que cubrir bastantes palabras poco frecuentes del mensaje, y las trampas salen también cuando nombras sus ficheros. Cada memoria llega una vez por sesión. En 300 mensajes reales solo inyectó algo en un 7 % de ellos. Si en 15 minutos y 10 mensajes no se ha guardado nada, le recuerda que lo haga.

Retoma donde lo dejaste: al empezar la sesión llega la última memoria `status` de cada proyecto, así que «sigamos con X» no obliga a buscar.

## El ruido, lo viejo y lo erróneo

Un recall que inyecta de más es peor que no tenerlo: llena el contexto de ruido y arrastra de una sesión a otra lo que ya no vale. Por eso es la parte de Omoikane con más horas de calibrado.

- **Casi siempre calla.** Un mensaje solo recibe algo si tiene al menos 3 palabras útiles y una memoria comparte con él 2 palabras poco frecuentes en tu base que sumen como mínimo la mitad del peso del mensaje. El canal por significado, para las paráfrasis, pide además que las palabras cubran un 25 % y no se usa en mensajes de más de 500 caracteres. En 300 mensajes reales inyectó algo en el 7 %; los demás no recibieron nada. «¿Cómo ha ido todo?» no trae nada.
- **Poco y una sola vez.** Como mucho 2 memorias por mensaje, y solo las que se acercan a la mejor: si una destaca, va sola. De cada una llegan 200 caracteres; si el agente quiere el resto, la pide con `get`. Puede sumarse una trampa o preferencia enlazada, y una receta solo si el mensaje dice una de sus palabras `when:`. Cada memoria llega una vez por sesión.
- **Lo viejo llega marcado.** Si una memoria más nueva del mismo tema la contradice (otra versión de lo mismo, o hecho lo que estaba pendiente), la vieja llega con `⚠ may be outdated: newer <id> …` delante y el agente mira la nueva antes de fiarse. También avisa de estados de proyecto que se han quedado atrás, rutas que ya no existen y memorias que nadie usa. La pestaña Revisar pone las dos memorias lado a lado para olvidar la vieja o dejarla. Todo sin modelo: palabras, fechas y versiones.
- **Lo erróneo se corrige, no se arrastra.** En la memoria solo entra lo que guarda el agente. Lo que Omoikane aparta al cerrar una sesión son propuestas, y caducan a los 7 días si nadie las guarda. Corregir es `update` con el motivo, y la versión anterior queda en el historial. `forget` la borra, y tampoco vuelve desde tus otros equipos. Lo que guarda un compañero llega marcado `[by Ana]`.
- **Medido.** Cada umbral sale de preguntas escritas a ciegas por otro agente, 20 de ellas sin respuesta en las notas: con esas inyectó algo en 2. Los números, en [BENCHMARK.es.md](../BENCHMARK.es.md).

Al empezar la sesión sí llega más: tus preferencias, lo que has fijado, en qué punto está cada proyecto y lo último que se tocó. Las preferencias y lo fijado son lo que tú quieres que el agente sepa siempre.

engram, la alternativa con la que se compara Omoikane, no inyecta memorias en cada mensaje. Con lo viejo hace otra cosa: cada vez que guarda, propone las memorias parecidas y le pide al agente que juzgue qué relación tienen (`mem_judge`). Omoikane no pide nada. Detecta él la contradicción, la marca en el recall con el motivo y deja al agente juzgar solo cuando sabe algo que Omoikane no puede ver (`supersedes`, `contradicts`, `compatible`). Cómo funcionan esos veredictos está en [memory-care.es.md](memory-care.es.md#veredictos).

## Entiende lo que preguntas con otras palabras

Las palabras encuentran una memoria cuando preguntas con las palabras que usa. Si preguntas lo mismo de otra forma («¿cómo recupero una versión que retiré?» para una memoria sobre `cargo yank --undo`), las palabras no la encuentran, y el modelo ligero casi nunca. Desde la 1.2 un segundo modelo, [EmbeddingGemma 300M](https://huggingface.co/google/embeddinggemma-300m), compara el significado:

- **En la búsqueda**, cada candidata suma su parecido en significado y una parte de su puntuación por palabras; con cinco palabras o menos (palabras clave) pesan más las palabras. En el benchmark público, con 3.000 memorias, la memoria buena sale la primera en el 55 % de las paráfrasis (9 % sin él, 6 % engram), el 89 % de las preguntas y el 96 % de las búsquedas por palabras clave.
- **En el recuerdo sirve sobre todo para callar**: lo que traen las palabras se queda fuera si no está entre las tres memorias más parecidas en significado, y la más parecida entra cuando destaca claramente. En las preguntas sin respuesta, el recuerdo mete algo el 8 % de las veces con 3.000 memorias y el 15 % con 10.000 (40 % y 70 % sin él). Las dos reglas son relativas, un puesto y una ventaja, porque el parecido depende del idioma y de cómo escribe cada uno.
- **Lo que cuesta**: unos 60 ms más por búsqueda y por mensaje (nunca más de 250 ms en el recuerdo: pasado eso, sigue sin él), unos 450 MB en disco y calcular una vez el vector de cada memoria, un cuarto de segundo cada una, en segundo plano.
- **Cómo funciona**: en Rust puro dentro de `omoi`, en la CPU. Cargarlo tarda 1,4 s, así que lo mantiene abierto un servicio propio (uno por usuario, solo en `127.0.0.1` y con token) que se cierra tras 30 minutos sin uso. Nadie espera a que cargue: hasta que está, y hasta que casi todas las memorias tienen su vector, todo va por palabras como antes. `omoi doctor` dice cuántas lo tienen.
- **Instalarlo y apagarlo**: `omoi model install --semantic` lo descarga y comprueba el SHA-256 de cada fichero. `omoi model semantic --off` vuelve a usar solo el modelo ligero (más rápido y con menos memoria) y cierra el servicio; `--on` lo vuelve a encender.
- **RAM al indexar**: el servicio calcula los vectores de las memorias nuevas por lotes; `omoi model semantic --ram <MB>` pone tope a la RAM de esos lotes (con más va algo más rápido; `0` vuelve al de por defecto). `omoi model semantic` y `omoi doctor` enseñan el tope en uso.

## Busca en tu idioma

La búsqueda empieza por palabras: le dan igual las tildes, mira también las rutas de fichero y da más peso a lo poco frecuente. Después, un modelo de embeddings local que abre en 2,7 ms rescata lo que se parece mucho aunque no comparta ninguna palabra. Dónde busca depende de dónde estés. Desde un proyecto, en él y en lo global; desde tu carpeta de usuario, en todo.

Funciona en 101 idiomas, con tildes o sin ellas. Con preguntas escritas a ciegas acierta la memoria a la primera el 90 % de las veces y entre las tres primeras el 97 %, con una mediana de 43 ms, y sigue igual de precisa con 3.183 memorias (50 ms por búsqueda). Cuatro procesos comparten el modelo con 13 MB de memoria cada uno. El modelo es [potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M) (int8), y abre en 2,7 ms en lugar de 1,78 s. Hasta que se descarga (~150 MB, una vez), la búsqueda va por palabras.

## ¿Y CLAUDE.md?

Con un proyecto pequeño y veinte cosas que recordar, un buen CLAUDE.md basta. Omoikane compensa donde CLAUDE.md deja de llegar:

- CLAUDE.md se carga entero en cada sesión, venga o no a cuento, y cuanto más sabe más contexto gasta. Omoikane trae lo que encaja con el mensaje.
- Es un fichero por proyecto. Lo que aprendes en uno no llega a otro si no lo copias; en Omoikane una memoria puede valer para todos.
- No sabe cuándo algo se ha quedado viejo ni guarda por qué cambió.
- Lo escribes tú. Omoikane lo va llenando el agente mientras trabaja.
- Lo lee un agente. Omoikane lo comparten 15, y todos tus equipos.

Pueden convivir: CLAUDE.md para las reglas fijas del repositorio y Omoikane para lo que se va aprendiendo.
