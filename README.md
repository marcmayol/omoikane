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

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere). The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

## How it compares

Against engram, with the same 127 real notes and 80 questions written blind: search takes the question as typed (88 % top-1 on natural questions, where engram needs keywords to match it), 34 ms per search against 120-155 ms, and the duplicate warning is right 99 % of the times it fires; engram catches more duplicates by asking the agent to review half of its saves. Full method and every number, including where engram wins: [BENCHMARK.md](BENCHMARK.md).

## What it does

Your agent learns things every session: a bug that only shows up on Windows, why the team chose ULIDs, that you want short commits in English. Then the session ends and all of it is gone.

Omoikane keeps those as short, typed memories in a local SQLite file and gives them back:

- **Search that works in your language.** Keywords (FTS5, accent-insensitive, file paths included) rank the results; a local static embedding model adds very close matches that share no words, and spots duplicates when you save. Measured on real notes, words alone find the right memory first 98 % of the time; the embeddings are there to catch duplicates, not to replace that.
- **Automatic recall.** On every prompt the plugin runs `omoi recall`. It is a compiled subcommand, no bash, no curl, no server, and it answers in milliseconds with the 3-5 memories that matter for what you just typed, plus any *gotcha* attached to the files you mentioned.
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
