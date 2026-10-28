# Omoikane

**Local memory for AI coding agents. One binary, one SQLite file, and the agent doesn't have to remember to look things up.**

[Español](README.es.md)

> Omoikane (思兼, "the one who gathers thoughts") is the kami the other gods consult when a decision has to be made: he remembers everything that is known and puts it together. That is what this program does for an agent.

## Status

Early development. See [PLAN.md](PLAN.md) (Spanish) for the full design and roadmap.

Working today: the memory store and its CLI (phase 1), search by words (plus meaning as a fallback) and duplicate detection when saving (phase 2), the MCP server for agents (phase 3), and automatic recall through Claude Code hooks, packaged as a plugin (phase 4). Phase 5 adds prebuilt binaries and one-line installers. CI runs on Windows, Linux and macOS. The 101-language model ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) opens in **2.7 ms** instead of 1.78 s, with identical token ids to `tokenizers` over 165,282 sentences, and four processes share it with 13 MB of private memory each. Tested on Windows and Linux.

## Install

One line installs the `omoi` binary, the embedding model (~150 MB, once) and the Claude Code plugin (MCP server + automatic recall):

**Windows** (PowerShell)
```powershell
irm https://raw.githubusercontent.com/marcmayol/omoikane/master/install.ps1 | iex
```

**Linux and macOS**
```sh
curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh | sh
```

The installer checks the SHA-256 of the binary before installing it. `OMOIKANE_NO_MODEL=1` skips the model (search by words only) and `OMOIKANE_NO_SETUP=1` leaves Claude Code alone; you can run `omoi setup claude` later. Prebuilt binaries: Windows x64, Linux x64/ARM64 (glibc 2.35 or newer), macOS Apple Silicon and Intel. Anything else: `cargo install omoikane`.

Already have `omoi`? In Claude Code: `/plugin marketplace add marcmayol/drac-plugins` and then `/plugin install omoikane@drac`, or `omoi setup claude`.

## Use it

Once the plugin is in, Claude Code does it by itself: it gets 8 tools (`remember`, `search`, `get`, `update`, `forget`, `history`, `context`, `recent`), and on every prompt the memories that really relate to it are injected automatically (or nothing). Any other MCP client can use `omoi mcp` directly.

From the terminal:

```sh
cd your-project
omoi remember -k gotcha -t "Token expires early" --path src/auth.rs \
  "Auth middleware compares local time against the token's UTC expiry."
omoi search "auth token"
omoi recent
omoi --help
```

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere). The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

## What it does

Your agent learns things every session: a bug that only shows up on Windows, why the team chose ULIDs, that you want short commits in English. Then the session ends and all of it is gone.

Omoikane keeps those as short, typed memories in a local SQLite file and gives them back:

- **Search that works in your language.** Keywords (FTS5, accent-insensitive, file paths included) rank the results; a local static embedding model adds very close matches that share no words, and spots duplicates when you save. Measured on real notes, words alone find the right memory first 98 % of the time; the embeddings are there to catch duplicates, not to replace that.
- **Automatic recall.** A Claude Code hook runs `omoi recall` on every prompt. It is a compiled subcommand, no bash, no curl, no server, and it answers in milliseconds with the 3-5 memories that matter for what you just typed, plus any *gotcha* attached to the files you mentioned.
- **Small on purpose.** 8 MCP tools. No daemon, no port, no cloud, no sync. One `~/.omoikane/omoikane.db`.
- **History with reasons.** Updating a memory keeps the previous version and, if the agent says so, why it changed.

## Memory model

A memory is one paragraph the agent would want to read before touching something again.

| field | who sets it |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference`) | the agent |
| `paths`, `tags`, `pinned` | the agent, optionally |
| `project`, `source`, timestamps, recall stats, version history | Omoikane |

Each `kind` changes how it is recalled: preferences and pinned memories are always injected; gotchas come up when the prompt or the files match; facts and decisions by relevance.

## Stack

Rust · [rusqlite](https://crates.io/crates/rusqlite) (bundled SQLite + FTS5) · [sqlite-vec](https://github.com/asg017/sqlite-vec) · [model2vec-rs](https://github.com/MinishLab/model2vec-rs) · [rmcp](https://github.com/modelcontextprotocol/rust-sdk)

## License

MIT · Marc Mayol
