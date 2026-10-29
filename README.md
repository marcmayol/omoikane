<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

**Local memory for AI coding agents. One binary, one SQLite file, and the agent doesn't have to remember to look things up.**

[Español](README.es.md)

> Omoikane (思兼, "the one who gathers thoughts") is the kami the other gods consult when a decision has to be made: he remembers everything that is known and puts it together. That is what this program does for an agent.

## Status

Early development. See [PLAN.md](PLAN.md) (Spanish) for the full design and roadmap.

Working today: the memory store and its CLI (phase 1), search by words (plus meaning as a fallback) and duplicate detection when saving (phase 2), the MCP server for agents (phase 3), and automatic recall through Claude Code hooks, packaged as a plugin (phase 4). Phase 5 adds prebuilt binaries and one-line installers. CI runs on Windows, Linux and macOS. The 101-language model ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) opens in **2.7 ms** instead of 1.78 s, with identical token ids to `tokenizers` over 165,282 sentences, and four processes share it with 13 MB of private memory each. Tested on Windows and Linux.

## Install

In Claude Code (2.1.310 or newer), two commands and nothing else:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

To get updates automatically, turn on auto-update for the marketplace once: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code leaves it off for third-party marketplaces).

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

`omoi serve --open` (or `/omoikane` in Claude Code) opens the same in your browser at `http://127.0.0.1:7438/`. **Memories** is a timeline by day of what was saved or changed (and, if you want, what the agent recalled), searchable the way the agent searches and filtered by kind, project or tag; a memory opens in place with its Markdown formatted, its last change as a diff, its full history, and buttons to pin, move or forget it. **Projects** shows where each project was left (its latest `status` memory) and opens a page per project with its memories in one column per kind. It follows the system's light or dark mode, and works offline: the fonts (IBM Plex and Shippori Mincho, SIL OFL, in `assets/fuentes`) are inside the binary. It only answers on your own computer, and refuses requests from other websites.

`omoi serve` serves the same tools over HTTP on `127.0.0.1:7438` for scripts and apps that don't speak MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<name>` with the tool's JSON arguments. Set `OMOIKANE_HTTP_TOKEN` to require `Authorization: Bearer <token>`.

### Other agents

`omoi setup <agent>` registers the MCP server, the automatic recall where the agent allows it, and a short paragraph in its instructions file. It shows what it will change first (`--dry-run` to only look), keeps a copy of every file it touches (`<file>.antes-de-omoikane`) and does nothing twice.

| Agent | Tools (MCP) | Automatic recall | Tested |
|---|---|---|---|
| Claude Code | yes (the plugin) | session start and every prompt | yes |
| OpenCode | yes | every prompt (a small plugin) | yes, 1.19.5 |
| Codex CLI | yes | session start and every prompt (approve the hooks once with `/hooks`) | from its docs |
| Gemini CLI | yes | session start and every prompt | from its docs |
| Qwen Code | yes | session start and every prompt | from its docs |
| Kiro | yes | session start and every prompt | from its docs |
| Cursor | yes | session start only (its prompt hook can't add context) | from its docs |
| GitHub Copilot (CLI and VS Code) | yes | session start only (its docs say prompt-hook output is dropped) | from its docs |
| Antigravity CLI | yes | session start only (no prompt event) | from its docs |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | yes | no (their hooks can't add context, or it isn't documented) | from their docs |

### Coming from engram

`omoi setup` also adds a `/omoikane` command that opens the web view, in the agents that support user commands: Gemini CLI, Qwen Code, Codex (`/prompts:omoikane`), Cursor, OpenCode, Kiro, Windsurf, Pi and Kilo Code. In those agents a command is a prompt, so the agent runs `omoi serve --open` itself and may ask before doing it.

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

### Coming from engram

`engram export` and then `omoi import engram-export.json`. Types map to Omoikane's (bugfix → gotcha, decision, preference; the rest become facts tagged with their engram type), `topic_key` becomes a `topic:` tag, personal scope becomes global, dates are kept, deleted observations are skipped and anything already saved is not imported twice. `--dry-run` shows where each memory would go. `omoi import` also reads `omoi export` files.

## How it compares

Against engram, with the same 127 real notes and questions written blind (130 of them never used for tuning), and at 3,183 memories: search takes the question as typed (88 % top-1 on natural questions, where engram needs keywords to match it), 34 ms per search against 120-155 ms, on questions never used for tuning it finds the right note more often (86-92 % top-1 on natural and paraphrased questions against 83-84 %, 87 % against 67 % in English), duplicates are caught as often as engram's (100 %) while flagging a third to half as many non-duplicates, it stays as accurate at 3,183 memories (50 ms per search against 203 ms), and an agent answering with it cost 29-40 % less. Both sync between computers: engram through git exports or a server you host (Postgres), in plain text; Omoikane through a git repository of yours, encrypted and automatic. Both set up 14 agents. Full method and every number, including where engram wins: [BENCHMARK.md](BENCHMARK.md).

## What it does

Your agent learns things every session: a bug that only shows up on Windows, why the team chose ULIDs, that you want short commits in English. Then the session ends and all of it is gone.

Omoikane keeps those as short, typed memories in a local SQLite file and gives them back:

- **Search that works in your language.** Keywords (FTS5, accent-insensitive, file paths included) rank the results; a local static embedding model adds very close matches that share no words, and spots duplicates when you save. Measured on real notes, words alone find the right memory first 98 % of the time; the embeddings are there to catch duplicates, not to replace that.
- **Automatic recall.** On every prompt the plugin runs `omoi recall`. It is a compiled subcommand, no bash, no curl, no server, and it answers in milliseconds with the 3-5 memories that matter for what you just typed, plus any *gotcha* attached to the files you mentioned.
- **Small on purpose.** 8 MCP tools. No daemon and no server of its own. One `~/.omoikane/omoikane.db`; optional sync goes through a git repository of yours.
- **History with reasons.** Updating a memory keeps the previous version and, if the agent says so, why it changed.

## Memory model

A memory is one paragraph the agent would want to read before touching something again.

| field | who sets it |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference`) | the agent |
| `paths`, `tags`, `pinned` | the agent, optionally |
| `project`, `source`, timestamps, recall stats, version history | Omoikane |

Each `kind` changes how it is recalled: preferences and pinned memories are always injected; gotchas come up when the prompt or the files match; facts and decisions by relevance. A memory tagged `status` says where the work on a project was left: at session start the latest one of each project comes with part of its content, so "let's carry on with X" needs no searching.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (bundled SQLite + FTS5) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## License

MIT · Marc Mayol
