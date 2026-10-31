# Sincronización, proyectos y espacios

[English](sync-and-spaces.md) · [Volver al README](../README.es.md)

## Compartir la memoria entre equipos (y con compañeros)

Omoikane no aloja nada: sincroniza a través de un repositorio git tuyo, dedicado solo a este conocimiento (nunca el repositorio del código). Vale cualquier remoto: un repositorio privado en GitHub, GitLab o Codeberg, el Gitea de tu empresa o una carpeta compartida. Usa tus propias credenciales de git.

```
omoi sync init git@github.com:tu-usuario/mi-memoria.git   # en el primer equipo: pregunta qué compartir
omoi sync code                                            # escribe un código que lleva la clave
omoi sync join <código>                                   # en otro equipo (o `--url` para llegar por otra vía)
```

Desde entonces funciona solo y con todos los agentes: después de guardar, cambiar u olvidar una memoria, `omoi` sincroniza en segundo plano (espera 3 segundos y junta en un commit lo que llegue mientras), y baja lo nuevo al empezar cada sesión. `omoi sync status` y `omoi doctor` dicen cómo fue la última sincronización, la vista web también lo enseña, y si lleva más de una hora fallando, la sesión lo avisa al empezar.

- **Qué viaja:** texto, nunca la base de datos. Una línea cifrada por memoria, en un fichero por proyecto y por equipo, así que un cambio es una línea en el diff y el fichero de nadie choca con el de otro. Los vectores y los índices se rehacen en cada equipo.
- **Cifrado en el remoto:** cada línea se cifra (AES-SIV) antes de salir de tu equipo; el repositorio solo guarda texto cifrado y nombres de carpeta opacos. La clave la crea `init` y viaja dentro del código para unirse, así que **pasa el código solo por un canal privado**. Si pierdes todas las copias de la clave, lo del remoto no se puede leer, aunque cada equipo conserva sus memorias.
- **Cambiar la clave:** `omoi sync rotate` (`--space <nombre>` para el repositorio de un espacio) vuelve a cifrar las memorias de este equipo con una clave nueva y escribe el código nuevo. Los equipos que solo tienen la vieja dejan de leer lo que se escriba desde entonces: así deja de recibir nada quien se va del equipo o del cliente.
- **Qué se comparte** se elige en `init`/`join`: los tipos (las preferencias se quedan en tu equipo salvo que digas lo contrario), las globales, qué proyectos y si cada memoria lleva su autor (tu nombre de git, sí por defecto). Con compañeros, sus memorias aparecen marcadas `[by Ana]` para que el agente las trate con cuidado.
- **Qué versión gana:** el cambio más nuevo según un reloj lógico híbrido, así que un equipo con la hora mal no pisa trabajo más reciente. La versión local anterior queda en el historial de la memoria. Lo olvidado no vuelve desde otro equipo.
- **Los proyectos** se reconocen por su remoto `origin` de git: el mismo repositorio en carpetas distintas de equipos distintos es el mismo proyecto. Los que no tienen remoto se reconocen por el nombre de la carpeta, y solo entre los equipos de una misma persona. El espacio de un proyecto viaja en su propia línea y gana el cambio más nuevo; `--space` deja un repositorio para un solo cliente.
- **Equipos con versiones distintas:** desde la 1.0, lo que una versión nueva añada no rompe a las viejas. Un campo que un equipo no conoce se guarda con la memoria y vuelve a subir aunque la cambie, así que no se pierde por pasar por él. Una línea de un tipo que no conoce se salta, `omoi sync status` dice «N líneas de una versión más nueva: actualiza omoi», y al actualizar se vuelve a leer todo. Y si un día una versión cambiara algo que las viejas no pueden leer bien, el repositorio lo dice en `omoikane.format` y las viejas no escriben en él hasta que se actualicen (lo avisan `status`, `doctor` y la sesión al empezar).

`omoi sync off` deja de sincronizar en un equipo; sus memorias se quedan.

## Proyectos

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos). Si abres las sesiones en tu carpeta de usuario y no dentro de un repositorio, ves las memorias de todos los proyectos, y una memoria nueva va al repositorio al que apuntan sus rutas absolutas (o es global); `omoi update <id> --project <carpeta>` o `--global` la cambia de sitio después.

Cada proyecto tiene su clave, que se calcula una vez: el `origin` de git o, si no tiene, tu nombre y el de la carpeta. Y guarda todas las carpetas donde vive. Dos clones del mismo repositorio, o un worktree, son el mismo proyecto. Si mueves la carpeta de un repositorio con `origin`, las memorias la siguen solas al empezar la sesión y te lo dice una vez; sin `origin`, `omoi projects move <proyecto> <carpeta>`, y hasta entonces lo avisan `omoi review` y `omoi doctor`. `omoi projects` los lista. `omoi projects merge <a> <b>` junta dos sin tocar ids, historial ni veredictos, y de sus dos `status` se queda el más nuevo; `omoi projects prune` olvida los que se quedaron sin memorias.

## Espacios: uno por cliente

Los espacios separan clientes. `omoi spaces add acme` y luego `omoi space assign acme` en cada repositorio del cliente (o `omoi projects move <proyecto> --space acme`). Desde un proyecto de Acme se ven sus memorias, las del espacio y las globales, nunca las de otro cliente: ni en la búsqueda, ni en el recall, ni en `context`. Lo común a todos los repositorios de un cliente se guarda con `remember --space` (o `space: true` por MCP); tus preferencias siguen siendo tuyas, globales. Desde tu carpeta de usuario se ve todo. `search` y `recent` aceptan `project` (una carpeta o un trozo del nombre) y `space`.

Para compartirlo, un repositorio por cliente, con su propia clave: `omoi sync init --space acme <url>` solo lleva lo de ese espacio, sin tus globales, y el repositorio general no lleva lo del espacio.

### Proyectos en pausa

Meter un proyecto en un espacio nunca lo mueve solo en otro sitio: esa línea la puede escribir cualquiera que tenga la clave de un repositorio. Tus otros equipos se enteran en su siguiente sincronización (el repositorio general solo dice que un proyecto pasó a un espacio, no a cuál ni lo que tiene dentro; el del espacio dice cuál) y dejan el proyecto **en pausa**: no se sube nada suyo a ningún repositorio ni se aplica lo que llegue de él hasta que decidas en ese equipo. Si no lo sabes, `omoi projects keep web` lo deja donde estaba; `omoi projects move web --space acme --force` acepta el cambio. En los dos casos vuelve a sincronizarse y gana la edición más nueva.

El repositorio de un espacio también puede nombrar un proyecto que aún no has abierto en este equipo (el de un compañero, o una librería tuya que alguien usa en el cliente): la primera vez que abres esa carpeta, el proyecto también queda en pausa, y lo que guardes en ella se queda en tu equipo hasta que digas de quién es: `omoi projects keep <proyecto>` si es del cliente, `omoi space unassign <proyecto> --force` si es tuyo (va a tu repositorio general, nunca al del cliente). Lo dicen `omoi sync`, `omoi sync status`, `omoi doctor`, `omoi review`, Revisar en la vista web (con las dos órdenes para copiar) y el agente al empezar cada sesión, hasta que decidas. Así, lo peor que puede hacer una línea falsa es pausar un proyecto, nunca mandar lo de un cliente, ni lo tuyo, a otro repositorio.

`omoi export --encrypt` desde un proyecto de un cliente cifra con la clave de su espacio; con `--all`, con la del general.
