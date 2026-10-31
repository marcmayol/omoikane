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

Las memorias pertenecen al repositorio git desde el que ejecutas `omoi` (`--global` hace que una valga en todos; más en [sync-and-spaces.es.md](sync-and-spaces.es.md#proyectos)). La base está en `~/.omoikane/omoikane.db` (`OMOIKANE_DB` para cambiarla). Todos los comandos aceptan `--json`. Los mensajes salen en inglés; con `OMOIKANE_LANG=es`, en español.

## La ves y la ordenas tú

`omoi tui` abre un visor en la terminal: busca mientras escribes, lee una memoria y su historial, fíjala u olvídala.

`omoi serve --open` (o `/omoikane-web` en Claude Code) abre lo mismo en el navegador, en `http://127.0.0.1:7438/`. **Memorias** es una cronología por días de lo que se guardó o cambió (y, si quieres, de lo que el agente recordó), con un buscador que busca como el agente y filtros por tipo, proyecto o etiqueta; cada memoria se abre ahí mismo con su Markdown con formato, su último cambio como diff, el historial completo y botones para fijarla, moverla u olvidarla. **Proyectos** enseña en qué punto se quedó cada proyecto (su última memoria `status`) y abre una página por proyecto con sus memorias en una columna por tipo. Sigue el modo claro u oscuro del sistema y funciona sin internet: las fuentes (IBM Plex y Shippori Mincho, licencia SIL OFL, en `assets/fuentes`) van dentro del binario. Solo responde en tu propio equipo y rechaza las peticiones de otras webs.

## Por HTTP, para scripts y apps

`omoi serve` sirve las mismas herramientas por HTTP en `127.0.0.1:7438` para scripts y apps que no hablan MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<nombre>` con los argumentos JSON de la herramienta. Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar `Authorization: Bearer <token>`.

## Se mantiene al día, si se lo pides

Viene apagado: hasta que digas que sí (`omoi upgrade --on`, o la pregunta de `omoi setup claude`), `omoi` no se cambia a sí mismo. Encendido, una vez al día, la primera vez que se ejecuta, `omoi` mira en segundo plano si hay una Release más nueva de la misma versión mayor, la instala solo si está firmada con la clave de Omoikane (Ed25519) y añade a los agentes que ya lo tienen lo que traiga la versión nueva; la sesión siguiente te lo dice. `omoi setup claude` lo pregunta (y enciende también la actualización del plugin en Claude Code); `omoi upgrade --off` u `OMOIKANE_AUTO_UPDATE=0` lo apagan, `omoi upgrade` actualiza a mano, y un binario que gestiona cargo, Homebrew, Scoop o Nix se deja a ellos. Antes de pasar una base a un esquema nuevo guarda una copia.

## Variables de entorno

Todas salen en las páginas de arriba; aquí están juntas.

| variable | qué hace |
|---|---|
| `OMOIKANE_DB` | dónde está la base (por defecto, `~/.omoikane/omoikane.db`) |
| `OMOIKANE_LANG=es` | mensajes en español |
| `OMOIKANE_BIN` | qué `omoi` ejecuta el plugin de Claude Code |
| `OMOIKANE_TRASH_DAYS` | días en la papelera antes de borrar del todo (30; 0 = nunca) |
| `OMOIKANE_BACKUPS` | copias diarias que se guardan (7; 0 = apagado) |
| `OMOIKANE_AUTO_UPDATE=0` | apaga la actualización automática |
| `OMOIKANE_HTTP_TOKEN` | token que pide `omoi serve` por HTTP |
| `OMOIKANE_NO_SETUP=1` | los instaladores no añaden el plugin de Claude Code |
| `OMOIKANE_NO_MODEL=1` | los instaladores no bajan el modelo |
