# Agentes sin nadie delante

[English](unattended.md) · [Volver al README](../README.es.md)

Las CI, los servidores, los contenedores y los agentes en la nube pueden usar la misma memoria que las personas del equipo. Ahí nadie contesta preguntas y puede que no haya carpeta personal, así que `omoi` lo toma todo de variables de entorno.

## Las variables

| variable | qué hace |
|---|---|
| `OMOIKANE_HOME` | la carpeta de Omoikane entera: la base, los modelos, la elección y el servicio del modelo de significado y las actualizaciones. Por defecto, `~/.omoikane`. La configuración de la sincronización, sus claves y sus clones, las copias y el diario están junto a la base, así que van con ella. `OMOIKANE_DB` y `OMOIKANE_MODELS` siguen mandando en lo suyo |
| `OMOIKANE_NONINTERACTIVE=1` | no hay nadie: ninguna pregunta (lo que pide confirmación pide `--yes`), ninguna descarga del modelo que nadie haya pedido, ninguna actualización automática y ninguna sincronización de fondo que siga viva después de la orden (`OMOIKANE_SYNC_AUTO=1` la devuelve en un servidor) |
| `OMOIKANE_SYNC_CODE` | uno o varios códigos de `omoi sync code` (separados por espacios, comas o líneas: el del repositorio general y el de cada espacio). `omoi sync`, `omoi sync --pull` y `omoi sync join` sin código se unen solos, compartiendo lo de por defecto (sin preferencias) |
| `OMOIKANE_SYNC_URL` | otra URL para el repositorio del código, por ejemplo HTTPS con un token de la CI (solo con un código) |
| `OMOIKANE_AUTHOR` | quién firma lo que guarda este agente: el autor de cada memoria y de cada veredicto (por encima del que se eligió al configurar la sincronización) |
| `OMOIKANE_NO_MODEL=1` | además de los instaladores, el propio `omoi` no descarga el modelo de significado cuando crece la memoria |

Sin carpeta personal y sin `OMOIKANE_HOME` ni `OMOIKANE_DB`, `omoi` se para con un error en vez de crear una base en la carpeta desde la que se ejecuta, que podría acabar dentro del repositorio que se está probando.

El código lleva la clave del repositorio. `omoi` no lo escribe nunca: ni al unirse ni en los errores, y el usuario y la contraseña de una URL tampoco salen. Guárdalo en los secretos de la CI; si un agente intenta guardarlo como memoria, queda como `[redacted:omoikane-sync-code]`. El código de un espacio solo abre el repositorio de ese cliente.

Con `OMOIKANE_NONINTERACTIVE=1`, `omoi sync --pull` espera a la sincronización que haya en marcha, hace una completa y sale con error (código 1) si no llega al repositorio o la clave no lo abre. Sin ella, `--pull` es el que lanzan los hooks en silencio y vuelve a intentarlo más tarde.

## GitHub Actions

Un trabajo que se une al espacio de un cliente, trae sus memorias y les pregunta algo. Pon el código de `omoi sync code --space acme` en el secreto `OMOIKANE_SYNC_CODE`, y una deploy key con acceso al repositorio de memorias en `MEMORY_DEPLOY_KEY`.

```yaml
name: Project memory
on: pull_request

jobs:
  memory:
    runs-on: ubuntu-latest
    env:
      OMOIKANE_NONINTERACTIVE: "1"
      OMOIKANE_SYNC_CODE: ${{ secrets.OMOIKANE_SYNC_CODE }}
      OMOIKANE_AUTHOR: ci:${{ github.repository }}
    steps:
      - uses: actions/checkout@v4

      - name: Install omoi
        run: |
          echo "OMOIKANE_HOME=$RUNNER_TEMP/omoikane" >> "$GITHUB_ENV"
          curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh \
            | OMOIKANE_NO_SETUP=1 OMOIKANE_NO_MODEL=1 OMOIKANE_INSTALL_DIR="$RUNNER_TEMP/bin" sh
          echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"

      - uses: webfactory/ssh-agent@v0.9.0
        with:
          ssh-private-key: ${{ secrets.MEMORY_DEPLOY_KEY }}

      - name: Join the space and bring its memories
        run: omoi sync

      - name: Ask the memory
        run: |
          omoi search "deploy migrations"
          USER_PROMPT="What should I know before touching the deploy?" omoi recall --format plain
```

Por HTTPS en vez de SSH, quita la deploy key y añade a `env` `OMOIKANE_SYNC_URL: https://x-access-token:${{ secrets.MEMORY_REPO_TOKEN }}@github.com/acme/memory.git`. Si el trabajo también guarda memorias (`omoi remember`, o un agente con el MCP), termínalo con otro `omoi sync`: sin nadie delante no se sincroniza nada en segundo plano. Sin el modelo ligero la búsqueda va por palabras; `omoi model install` lo añade (unos 150 MB, conviene cachear `$OMOIKANE_HOME/models`).
