# Agentes

[English](agents.md) · [Volver al README](../README.es.md)

## Claude Code

Con el plugin, Claude Code lo hace solo: recibe 8 herramientas (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`) y, en cada mensaje, se le inyectan las memorias que de verdad tienen que ver (o ninguna). El plugin es un mod de Claude Code: ejecuta el binario directamente, sin servidor MCP ni scripts de shell, igual en Windows, macOS y Linux. Cualquier otro cliente MCP puede usar `omoi mcp` directamente.

## Otros agentes

`omoi setup <agente>` registra el servidor MCP, el recall automático donde el agente lo permite y un párrafo en su fichero de instrucciones. Antes enseña lo que va a cambiar (`--dry-run` para solo mirar), guarda una copia de cada fichero que toca (`<fichero>.antes-de-omoikane`) y no hace nada dos veces.

| Agente | Herramientas (MCP) | Recall automático | Guarda al cerrar | Probado |
|---|---|---|---|---|
| Claude Code | sí (el plugin) | al empezar y en cada mensaje | sí | sí |
| OpenCode | sí | en cada mensaje (un plugin pequeño) | sí (el plugin) | sí, 1.19.5 |
| Codex CLI | sí | al empezar y en cada mensaje (aprueba los hooks una vez con `/hooks`) | sí | según su documentación |
| Gemini CLI | sí | al empezar y en cada mensaje | sí | según su documentación |
| Qwen Code | sí | al empezar y en cada mensaje | sí | según su documentación |
| Kiro | sí | al empezar y en cada mensaje | sí | según su documentación |
| Cursor | sí | solo al empezar (su hook del mensaje no deja añadir contexto) | sí | según su documentación |
| GitHub Copilot (CLI y VS Code) | sí | solo al empezar (su documentación dice que la salida del hook del mensaje se descarta) | sí | según su documentación |
| Antigravity CLI | sí | solo al empezar (no tiene evento de mensaje) | no (guarda las conversaciones en un formato binario sin documentar) | según su documentación |
| Grok Build | sí | no (sus hooks de inicio y de mensaje descartan lo que devuelven; las instrucciones le piden llamar a `context` al empezar) | sí | según su documentación |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | sí | no (sus hooks no pueden añadir contexto, o no está documentado) | no | según su documentación |

## Órdenes en cada agente

`omoi setup` añade también dos órdenes en los agentes que admiten órdenes de usuario: `/omoikane`, que hace guardar al agente, y `/omoikane-web`, que abre la vista web. Son Gemini CLI, Qwen Code, Codex (`/prompts:omoikane` y `/prompts:omoikane-web`), Cursor, OpenCode, Kiro, Windsurf, Pi, Kilo Code y Grok Build. En esos agentes una orden es un texto para el modelo, así que es el agente quien ejecuta `omoi serve --open` y puede pedir permiso antes. Una `/omoikane` de antes, que abría la vista web, pasa a ser la que guarda.
