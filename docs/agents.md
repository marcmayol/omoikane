# Agents

[Español](agents.es.md) · [Back to the README](../README.md)

## Claude Code

Once the plugin is in, Claude Code does it by itself: it gets 8 tools (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`), and on every prompt the memories that really relate to it are injected automatically (or nothing). The plugin is a Claude Code mod: it runs the binary directly, with no MCP server and no shell scripts, the same on Windows, macOS and Linux. Any other MCP client can use `omoi mcp` directly.

## Other agents

`omoi setup <agent>` registers the MCP server, the automatic recall where the agent allows it, and a short paragraph in its instructions file. It shows what it will change first (`--dry-run` to only look), keeps a copy of every file it touches (`<file>.antes-de-omoikane`) and does nothing twice.

| Agent | Tools (MCP) | Automatic recall | Saves at session end | Tested |
|---|---|---|---|---|
| Claude Code | yes (the plugin) | session start and every prompt | yes | yes |
| OpenCode | yes | every prompt (a small plugin) | yes (the plugin) | yes, 1.19.5 |
| Codex CLI | yes | session start and every prompt (approve the hooks once with `/hooks`) | yes | from its docs |
| Gemini CLI | yes | session start and every prompt | yes | from its docs |
| Qwen Code | yes | session start and every prompt | yes | from its docs |
| Kiro | yes | session start and every prompt | yes | from its docs |
| Cursor | yes | session start only (its prompt hook can't add context) | yes | from its docs |
| GitHub Copilot (CLI and VS Code) | yes | session start only (its docs say prompt-hook output is dropped) | yes | from its docs |
| Antigravity CLI | yes | session start only (no prompt event) | no (its conversations are in an undocumented binary format) | from its docs |
| Grok Build | yes | no (its session-start and prompt hooks discard what they return; the instructions ask it to call `context` at the start) | yes | from its docs |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | yes | no (their hooks can't add context, or it isn't documented) | no | from their docs |

## Commands in each agent

`omoi setup` also adds two commands in the agents that support user commands: `/omoikane`, which makes the agent save, and `/omoikane-web`, which opens the web view. Those are Gemini CLI, Qwen Code, Codex (`/prompts:omoikane` and `/prompts:omoikane-web`), Cursor, OpenCode, Kiro, Windsurf, Pi, Kilo Code and Grok Build. In those agents a command is a prompt, so the agent runs `omoi serve --open` itself and may ask before doing it. An older `/omoikane`, which opened the web view, becomes the one that saves.
