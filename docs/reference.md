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

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere; more in [sync-and-spaces.md](sync-and-spaces.md#projects)). The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it, or `OMOIKANE_HOME` to move the whole `~/.omoikane` folder; for CI and servers, see [unattended.md](unattended.md)). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

Many agents can share one database at once (subagents, several sessions, CI jobs): each `omoi` waits its turn to write, up to 5 s, and a brand-new database created by several processes at the same moment comes out as one. `tests/concurrencia.rs` checks it with real `omoi` processes; `cargo test --release --test concurrencia -- --ignored --nocapture` runs the full measurement (20 agents for 30 s).

## See it and tidy it up

`omoi tui` opens a terminal viewer: search as you type, read a memory and its history, pin or forget it. It sees what the web view sees from the same folder: from a client's project, only that client's space and the global memories (`a` widens the list to the whole space, never to other clients); from the home folder, everything, and `e` filters by space. Each memory says its project and space. `t` opens the trash: `Enter` or `r` restores, `x` deletes one for good and `E` empties what is listed, both after asking. It fits an 80×24 terminal; in a narrower one the memory goes below the list, or opens with `Enter`.

`omoi serve --open` (or `/omoikane-web` in Claude Code) opens the same in your browser at `http://127.0.0.1:7438/`. **Memories** is a timeline by day of what was saved or changed (and, if you want, what the agent recalled), searchable the way the agent searches and filtered by kind, project or tag; a memory opens in place with its Markdown formatted, its last change as a diff, its full history, and buttons to pin, move or forget it. **Projects** shows where each project was left (its latest `status` memory) and opens a page per project with its memories in one column per kind. **Overview** shows, per space, what there is, who wrote it and what is outdated or paused ([details](sync-and-spaces.md#overview-by-client)). It follows the system's light or dark mode, and works offline: the fonts (IBM Plex and Shippori Mincho, SIL OFL, in `assets/fuentes`) are inside the binary. It only answers on your own computer, and refuses requests from other websites.

A memory has its own link: `http://127.0.0.1:7438/#/memoria/<id>` opens its project with that memory in the side panel, where the **Verify** button is (the beginning of the id is enough, as in the CLI). `omoi serve --open --memory <id>` opens the browser right there, and `omoi serve --memory <id>` just prints the link; both accept the beginning of the id and fail with a clear error, before starting anything, if no memory has it or if the view started from that folder wouldn't show it (another client's space). That is how a notice asks the person to look at a memory: Kumi sends that link when an approval went through an agent, and the person verifies it by hand. With `OMOIKANE_HTTP_TOKEN` the link carries the token first: `#token=<token>&to=/memoria/<id>` (what `--open` opens).

## Over HTTP, for scripts and apps

`omoi serve` serves the same tools over HTTP on `127.0.0.1:7438` for scripts and apps that don't speak MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `GET /api/overview?months=…` (the overview by client, as JSON), `POST /tools/<name>` with the tool's JSON arguments. Verifying a memory is not a tool: it is the web view's button (`POST /api/verify`, which needs the view's `Origin` and the token `omoi serve` puts inside the page, sent as `X-Omoikane-View`) or `omoi verify` ([saving.md](saving.md#verified-by-a-person)). Set `OMOIKANE_HTTP_TOKEN` to require `Authorization: Bearer <token>`.

The web view and the API see what the CLI and the MCP see from the folder where `omoi serve` started. In a client's project, that client's space (all its projects, its own memories) and the global memories; in a project with no space, the projects with no space and the global memories; in the home folder, everything. That holds for every page (Memories, Projects, Overview, Review, the trash, what was set aside from sessions, search) and for every write: pinning, moving, forgetting, deciding, restoring or redacting a memory from outside is an error (403 from `/api/*`, 400 from `/tools/*`, as over MCP). The header says where you are looking from ("Only ◇ Acme"), so sharing your screen with one client doesn't show the others. `?dir=<folder>` picks another project for a request, but only inside what the server already shows (another project of the same space; from the home folder, any): it never widens the view (403). A network path there (`\\server\share`, `//host/x`, `\\?\UNC\…`, `\??\UNC\…`) is refused (400) without being looked at: any web page can request that URL with an `<img>`, and on Windows just checking such a path opens an SMB connection that hands over your NTLM hash. The same holds for a `project` given over MCP and for every folder Omoikane is handed (`--dir`, `export --okf`/`--obsidian`, `import --okf`, the file of `import`, the project paths that arrive by sync), for the paths a memory names (also `~/` followed by a network path, such as `~/\\host\share`, which `remember` from the home folder used to look up), for the `gitdir:` of a worktree's `.git` file, and for the folders that come from environment variables (`--db`, `OMOIKANE_DB`, `OMOIKANE_HOME`, `OMOIKANE_MODELS`, `HOME`/`USERPROFILE`, `COPILOT_HOME`, `GROK_HOME`, `XDG_CONFIG_HOME`, `APPDATA`): a network folder is never opened; map it to a drive letter if you need it. Only the folder you work in can be one: the folder `omoi` (or the agent's MCP server) runs in, and the `cwd` the agent's own hook sends, which is its session's folder. There Omoikane reads the repository as it does on a local disk (its `origin` for the project's key, `HEAD` for the provenance, worktrees), also when the repository starts higher up in the same share (`\\server\share`), because the system is already connected to it. That folder (or its repository's root) can also be given back as `project` over MCP, written any way (`\\?\UNC\…`, forward slashes, another case): it is already being read. Another folder of that share, a subfolder of it, another share, even on the same server, still counts as from outside. A local path with the extended-length prefix (`\\?\C:\…`, what Windows' `canonicalize` gives) is local like `C:\…`, also in `--db` and `OMOIKANE_HOME`; `\\?\UNC\…`, `\\?\GLOBALROOT\…`, `\\.\…` and `\??\…` aren't. WSL paths (`\\wsl$\<distro>\…`, `\\wsl.localhost\<distro>\…`) are not SMB to another machine: when WSL is installed they count as local everywhere. Without WSL (its network provider, P9NP, ahead of the SMB client in the provider order), those names would be looked up on the network like any other, so they are refused like any other network path.

## It keeps itself up to date, if you ask it to

It comes off: until you say yes (`omoi upgrade --on`, or the question in `omoi setup claude`), `omoi` doesn't change itself. When on, once a day, the first time it runs, `omoi` checks in the background for a newer release of the same major version, installs it only if it is signed with Omoikane's key (Ed25519), and adds whatever the new version needs to the agents that already have it; the next session tells you. `omoi setup claude` asks (and turns on the plugin's update in Claude Code too); `omoi upgrade --off` or `OMOIKANE_AUTO_UPDATE=0` turns it off, `omoi upgrade` updates by hand, and a binary managed by cargo, Homebrew, Scoop or Nix is left to them. Before moving a database to a newer schema it keeps a copy.

## Environment variables

All of them appear on the pages above; here they are together.

| variable | what it does |
|---|---|
| `OMOIKANE_DB` | where the database lives (default `omoikane.db` in `OMOIKANE_HOME`) |
| `OMOIKANE_HOME` | the whole Omoikane folder: database, models, preferences (default `~/.omoikane`) |
| `OMOIKANE_NONINTERACTIVE=1` | nobody at the keyboard: no prompts, downloads, updates or background sync ([unattended.md](unattended.md)) |
| `OMOIKANE_SYNC_CODE` / `OMOIKANE_SYNC_URL` | join sync from a code (and another URL) without asking ([unattended.md](unattended.md)) |
| `OMOIKANE_AUTHOR` | who signs what is saved here |
| `OMOIKANE_SESSION` | the agent session saved with each new memory (otherwise `CLAUDE_CODE_SESSION_ID`, or the hooks') |
| `OMOIKANE_SYNC_AUTO=0` / `1` | background sync off or on (on by default, off with `OMOIKANE_NONINTERACTIVE=1`) |
| `OMOIKANE_LANG=es` | messages in Spanish |
| `OMOIKANE_BIN` | which `omoi` the Claude Code plugin runs |
| `OMOIKANE_TRASH_DAYS` | days in the trash before deleting for good (30; 0 = never) |
| `OMOIKANE_BACKUPS` | daily copies kept (7; 0 = off) |
| `OMOIKANE_AUTO_UPDATE=0` | turns automatic updates off |
| `OMOIKANE_HTTP_TOKEN` | token that `omoi serve` requires over HTTP |
| `OMOIKANE_NO_SETUP=1` | the installers don't add the Claude Code plugin |
| `OMOIKANE_NO_MODEL=1` | the installers don't download the light model (the meaning model is never downloaded by them), and `omoi` doesn't download the meaning model by itself |
| `OMOIKANE_SEMANTIC=0` / `1` | turns the meaning model off or on (over `omoi model semantic` and the size of the memory) |
| `OMOIKANE_SEMANTIC_THRESHOLD` | from how many memories the meaning model turns on by itself (1000) |
| `OMOIKANE_TRAZA=1` | recall writes each step to stderr: candidates, scores and what the meaning model did, or why it wasn't asked; every command also says how long it waited for another process to release the database |
| `OMOIKANE_SEMANTIC_IDLE` | seconds without use before the meaning service closes (1800) |
| `OMOIKANE_SEMANTIC_RAM` | MB of RAM the meaning model may use while indexing (over `omoi model semantic --ram`; default 128, minimum 80) |
| `OMOIKANE_SEMANTIC_MODEL` | a folder with EmbeddingGemma to use instead of the installed one |
| `OMOIKANE_SEMANTIC_SOURCE` | where `omoi model install --semantic` downloads from (comma-separated) |
