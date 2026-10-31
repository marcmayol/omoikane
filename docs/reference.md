# Terminal, web view, HTTP and updates

[Español](reference.es.md) · [Back to the README](../README.md)

## From the terminal

```sh
cd your-project
omoi remember -k gotcha -t "Token expires early" --path src/auth.rs \
  "Auth middleware compares local time against the token's UTC expiry."
omoi search "auth token"
omoi recent
omoi --help
```

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere; more in [sync-and-spaces.md](sync-and-spaces.md#projects)). The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

## See it and tidy it up

`omoi tui` opens a terminal viewer: search as you type, read a memory and its history, pin or forget it.

`omoi serve --open` (or `/omoikane-web` in Claude Code) opens the same in your browser at `http://127.0.0.1:7438/`. **Memories** is a timeline by day of what was saved or changed (and, if you want, what the agent recalled), searchable the way the agent searches and filtered by kind, project or tag; a memory opens in place with its Markdown formatted, its last change as a diff, its full history, and buttons to pin, move or forget it. **Projects** shows where each project was left (its latest `status` memory) and opens a page per project with its memories in one column per kind. It follows the system's light or dark mode, and works offline: the fonts (IBM Plex and Shippori Mincho, SIL OFL, in `assets/fuentes`) are inside the binary. It only answers on your own computer, and refuses requests from other websites.

## Over HTTP, for scripts and apps

`omoi serve` serves the same tools over HTTP on `127.0.0.1:7438` for scripts and apps that don't speak MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<name>` with the tool's JSON arguments. Set `OMOIKANE_HTTP_TOKEN` to require `Authorization: Bearer <token>`.

## It keeps itself up to date, if you ask it to

It comes off: until you say yes (`omoi upgrade --on`, or the question in `omoi setup claude`), `omoi` doesn't change itself. When on, once a day, the first time it runs, `omoi` checks in the background for a newer release of the same major version, installs it only if it is signed with Omoikane's key (Ed25519), and adds whatever the new version needs to the agents that already have it; the next session tells you. `omoi setup claude` asks (and turns on the plugin's update in Claude Code too); `omoi upgrade --off` or `OMOIKANE_AUTO_UPDATE=0` turns it off, `omoi upgrade` updates by hand, and a binary managed by cargo, Homebrew, Scoop or Nix is left to them. Before moving a database to a newer schema it keeps a copy.

## Environment variables

All of them appear on the pages above; here they are together.

| variable | what it does |
|---|---|
| `OMOIKANE_DB` | where the database lives (default `~/.omoikane/omoikane.db`) |
| `OMOIKANE_LANG=es` | messages in Spanish |
| `OMOIKANE_BIN` | which `omoi` the Claude Code plugin runs |
| `OMOIKANE_TRASH_DAYS` | days in the trash before deleting for good (30; 0 = never) |
| `OMOIKANE_BACKUPS` | daily copies kept (7; 0 = off) |
| `OMOIKANE_AUTO_UPDATE=0` | turns automatic updates off |
| `OMOIKANE_HTTP_TOKEN` | token that `omoi serve` requires over HTTP |
| `OMOIKANE_NO_SETUP=1` | the installers don't add the Claude Code plugin |
| `OMOIKANE_NO_MODEL=1` | the installers don't download the model |
