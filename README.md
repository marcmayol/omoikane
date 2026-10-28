# Omoikane

**Local memory for AI coding agents. One binary, one SQLite file, and the agent doesn't have to remember to look things up.**

[Español](README.es.md)

> Omoikane (思兼, "the one who gathers thoughts") is the kami the other gods consult when a decision has to be made: he remembers everything that is known and puts it together. That is what this program does for an agent.

## Status

Early development. See [PLAN.md](PLAN.md) (Spanish) for the full design and roadmap.

Working today: the memory store and its CLI (phase 1) with search by words (plus meaning as a fallback) and duplicate detection when saving (phase 2). Coming next: the MCP server (phase 3) and automatic recall through hooks (phase 4). CI runs on Windows, Linux and macOS. The 101-language model ([potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M), int8) opens in **2.7 ms** instead of 1.78 s, with identical token ids to `tokenizers` over 165,282 sentences, and four processes share it with 13 MB of private memory each. Tested on Windows and Linux.

## Try it

You need [Rust](https://rustup.rs). Then:

```sh
cargo install --git https://github.com/marcmayol/omoikane
cd your-project
omoi remember -k gotcha -t "Token expires early" --path src/auth.rs \
  "Auth middleware compares local time against the token's UTC expiry."
omoi search "auth token"
omoi model install     # optional, ~150 MB once: adds search by meaning and duplicate detection
omoi recent
omoi --help
```

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere). The database lives in `~/.omoikane/omoikane.db` (`OMOIKANE_DB` to change it). Every command takes `--json`. Messages are in English; `OMOIKANE_LANG=es` switches them to Spanish.

## What it will do

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
