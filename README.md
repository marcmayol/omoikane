<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

*Your agent's eternal memory. It recalls on its own, in 20 ms.*

**Local memory for AI coding agents. One binary, one SQLite file, and the agent doesn't have to remember to look things up.**

[Español](README.es.md)

> Omoikane (思兼, "the one who gathers thoughts") is the kami the other gods consult when a decision has to be made: he remembers everything that is known and puts it together. That is what this program does for an agent.

## What makes it different

Your agent learns things every session: a bug that only shows up on Windows, why the team chose ULIDs, that you want short commits in English. Then the session ends and all of it is gone. Omoikane keeps those as short, typed memories in a local SQLite file, and does the rest by itself:

- **It recalls without being asked.** On every prompt a hook gives the agent the 3-5 memories that matter for what you just typed, plus the gotchas of the files you mentioned, in about 20 ms. The agent doesn't have to remember to search.
- **It saves even when the agent forgets to.** When a session ends, Omoikane reads the conversation and sets aside what you decided and asked for; the next session saves what is worth keeping, in your words.
- **It notices when a memory is out of date.** If a newer memory contradicts an older one, the old one reaches the agent marked `⚠ may be outdated`, and the Review tab shows both side by side.
- **Recipes that come back whole.** A `procedure` memory holds the steps of something you repeat; "publish 0.14.1" brings the whole checklist, and the agent fixes a step when it changes.
- **Linked memories.** `[[links]]` between notes travel with them: recall one and the warning linked to it comes along.
- **It picks up where you left off.** Each project's latest `status` memory arrives at session start, so "let's carry on with X" needs no searching.
- **Search that works in your language**, accents or not: on questions written blind it finds the right memory first 90 % of the time, in 43 ms.
- **Private and small.** One binary, one file, 8 MCP tools and no server of its own. Optional sync goes through a git repository of yours, encrypted.
- **It keeps itself up to date**, and only installs releases signed with Omoikane's key.

## Status

Version 0.14.0, on crates.io and in the Releases, with CI on Windows, Linux and macOS. Used every day on Windows and Linux. The full design and every decision are in [PLAN.md](PLAN.md) (Spanish).

## Install

In Claude Code (2.1.310 or newer), two commands and nothing else:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

`omoi setup claude` and the one-line installers ask whether Omoikane should keep itself up to date, and if so turn on the plugin's update too. If you only ran the two commands above, turn it on once: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code leaves it off for third-party marketplaces).

On the first session the plugin downloads the `omoi` binary for your system from this repo's Releases (checking its SHA-256) into `~/.omoikane/bin`, then the embedding model (~150 MB, once) in the background; until it arrives, search works by words. When the plugin updates, it fetches the matching binary. Prebuilt binaries: Windows x64, Linux x64/ARM64 (glibc 2.35 or newer), macOS Apple Silicon and Intel; anywhere else, `cargo install omoikane` and the plugin uses the `omoi` on your PATH (or set `OMOIKANE_BIN`).

**Without Claude Code**, or to use `omoi` from the terminal or another MCP client, install the binary with one line:

**Windows** (PowerShell)
```powershell
irm https://raw.githubusercontent.com/marcmayol/omoikane/master/install.ps1 | iex
```

**Linux and macOS**
```sh
curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh | sh
```

Or `cargo install omoikane`. The installers also add the Claude Code plugin if Claude Code is there (`OMOIKANE_NO_SETUP=1` to skip it, `OMOIKANE_NO_MODEL=1` to skip the model).

## Use it

Once the plugin is in, Claude Code does it by itself: it gets 8 tools (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`), and on every prompt the memories that really relate to it are injected automatically (or nothing). The plugin is a Claude Code mod: it runs the binary directly, with no MCP server and no shell scripts, the same on Windows, macOS and Linux. Any other MCP client can use `omoi mcp` directly.

From the terminal:

```sh
cd your-project
omoi remember -k gotcha -t "Token expires early" --path src/auth.rs \
  "Auth middleware compares local time against the token's UTC expiry."
omoi search "auth token"
omoi recent
omoi --help
```

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere). If you open your sessions in your home folder instead of inside a repository, you see the memories of every project, and a new memory goes to the repository its absolute paths point to (or is global); `omoi update <id> --project <folder>` or `--global` moves one later. The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

`omoi tui` opens a terminal viewer: search as you type, read a memory and its history, pin or forget it.

`omoi serve --open` (or `/omoikane-web` in Claude Code) opens the same in your browser at `http://127.0.0.1:7438/`. **Memories** is a timeline by day of what was saved or changed (and, if you want, what the agent recalled), searchable the way the agent searches and filtered by kind, project or tag; a memory opens in place with its Markdown formatted, its last change as a diff, its full history, and buttons to pin, move or forget it. **Projects** shows where each project was left (its latest `status` memory) and opens a page per project with its memories in one column per kind. It follows the system's light or dark mode, and works offline: the fonts (IBM Plex and Shippori Mincho, SIL OFL, in `assets/fuentes`) are inside the binary. It only answers on your own computer, and refuses requests from other websites.

**The memory looks after itself.** Omoikane notices when a memory has fallen behind: a newer one on the same subject says something different (a higher version of the same thing, or done what was pending), a `status` memory the project has moved past, paths that no longer exist, memories never used in a month, and notes too long for recall. When one of those reaches the agent it comes marked `⚠ may be outdated: newer <id> …`, and saving a memory that makes another outdated says so right away, so the agent fixes it on the spot. The **Review** tab of the web view lists them with the two memories side by side, *forget the old one* and *it's fine as it is*; `omoi review` does the same from the terminal. No model involved: words, dates and versions.

**It saves even when the agent forgets to.** When a session ends or compacts, Omoikane reads the conversation and sets aside what may be worth keeping: your answers to the agent's questions (with the question), your preferences and corrections, and the last commit or release. The next session in that project gets them once, and the agent saves the ones worth it in your words; the Review tab lists them too. Nothing that looks like a secret, nothing already saved. It reads the conversation of Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro and OpenCode (`omoi setup <agent>` adds the end-of-session hooks); not Antigravity, whose conversations are stored in an undocumented binary format.

**It keeps itself up to date.** Once a day, the first time it runs, `omoi` checks in the background for a newer release of the same major version, installs it only if it is signed with Omoikane's key, and adds whatever the new version needs to the agents that already have it; the next session tells you. `omoi setup claude` asks first (and turns on the plugin's update in Claude Code too); `omoi upgrade --off` or `OMOIKANE_AUTO_UPDATE=0` turns it off, `omoi upgrade` updates by hand, and a binary managed by cargo, Homebrew, Scoop or Nix is left to them. Before moving a database to a newer schema it keeps a copy.

**Procedures.** A memory of kind `procedure` is a recipe you repeat (publishing a release, a deploy): numbered steps and a line `when: publish, release`. When a message asks for it, it comes back whole, and the agent updates it when a step fails or changes.

**Linked memories.** `[[name]]` in a memory links it to the one tagged with that name. When a memory is recalled, a linked gotcha or preference can come along with it; broken links show up in Review, and each project page draws its connections.

`omoi serve` serves the same tools over HTTP on `127.0.0.1:7438` for scripts and apps that don't speak MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<name>` with the tool's JSON arguments. Set `OMOIKANE_HTTP_TOKEN` to require `Authorization: Bearer <token>`.

### Other agents

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
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | yes | no (their hooks can't add context, or it isn't documented) | no | from their docs |

`omoi setup` also adds a `/omoikane-web` command that opens the web view, in the agents that support user commands: Gemini CLI, Qwen Code, Codex (`/prompts:omoikane-web`), Cursor, OpenCode, Kiro, Windsurf, Pi and Kilo Code. In those agents a command is a prompt, so the agent runs `omoi serve --open` itself and may ask before doing it.

### Sharing memory between computers (and teammates)

Omoikane hosts nothing: it syncs through a git repository of yours, used only for this knowledge (never your code repository). Any remote works: a private repo on GitHub, GitLab, Codeberg, your company's Gitea, or a shared folder. It uses your own git credentials.

```
omoi sync init git@github.com:you/my-memory.git   # on the first computer: asks what to share
omoi sync code                                    # prints a code that carries the key
omoi sync join <code>                             # on another computer (or `--url` to reach it another way)
```

From then on it runs by itself, for every agent: after each saved, changed or forgotten memory, `omoi` syncs in the background (changes a few seconds apart go in one commit), and pulls when a session starts. `omoi sync status` and `omoi doctor` say how the last sync went, the web view shows it too, and a session warns if sync has been failing for a while.

- **What travels:** text, never the database. One encrypted line per memory, in one file per project and per computer, so a change is a one-line diff and nobody's file conflicts with anyone else's. Vectors and indexes are rebuilt on each computer.
- **Encrypted on the remote:** each line is encrypted (AES-SIV) before it leaves your computer; the repository only ever holds ciphertext and opaque folder names. The key is created by `init` and travels inside the join code, so **share the code only through a private channel**. Lose every copy of the key and the remote can't be read, though each computer keeps its own memories.
- **What is shared** is chosen in `init`/`join`: kinds (preferences stay personal unless you say so), global memories, which projects, and whether each memory carries its author (your git name, on by default). With teammates, their memories show up marked `[by Ana]` so the agent treats them with care.
- **Which version wins:** the newest change by a hybrid logical clock, so a computer with the wrong time can't overwrite newer work. The older local version stays in the memory's history. Forgotten memories don't come back from another computer.
- **Projects** are matched by their git `origin` remote, so the same repo in different folders on different computers is the same project. Projects without a remote are matched by folder name, only between the same person's computers.

`omoi sync off` stops syncing on a computer; its memories stay.

### Importing from other tools

`omoi import` reads exports from engram (`engram export`) and from `omoi export`: it maps the kinds, keeps the dates, skips what was deleted and imports nothing twice. `--dry-run` shows where each memory would go.

## Measured

With 137 real notes and questions written blind by another agent, never used for tuning: search finds the right memory first 90 % of the time and within the top three 97 %, with a median of 43 ms. At 3,183 memories it stays as accurate, at 50 ms per search. Over 300 real messages it only injects something when it relates (in 7 % of them), and the recall hook takes 15-26 ms. The 101-language embedding model ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) opens in 2.7 ms instead of 1.78 s. Full method and every number: [BENCHMARK.md](BENCHMARK.md).

## Memory model

A memory is one paragraph the agent would want to read before touching something again.

| field | who sets it |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | the agent |
| `paths`, `tags`, `pinned` | the agent, optionally |
| `project`, `source`, timestamps, recall stats, version history | Omoikane |

Each `kind` changes how it is recalled: preferences and pinned memories are always injected; gotchas come up when the prompt or the files match; facts and decisions by relevance; procedures, whole, when the message says one of their `when:` words. A memory tagged `status` says where the work on a project was left: at session start the latest one of each project comes with part of its content, so "let's carry on with X" needs no searching.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (bundled SQLite + FTS5) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## License

MIT · Marc Mayol
