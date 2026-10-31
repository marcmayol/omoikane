# Contributing to Omoikane

[Español](CONTRIBUTING.es.md)

Thanks for wanting to help. Omoikane is a memory that people trust with what they learned over months, on every computer they own and, in a company, shared with a whole team. That shapes everything below: a change that loses a memory, leaks one across clients or breaks a sync with an older version is worse than a missing feature.

## Before you write code

- **Small fixes** (a typo, a clearer message, a bug with an obvious cause): open the pull request directly.
- **Anything else** (a new feature, a new command or MCP tool, a change to search or recall, anything that touches the database, sync or export formats): open an issue first and say what problem you want to solve. It saves you work that might not fit the rules below.
- **Security problems**: don't open a public issue. See [SECURITY.md](SECURITY.md).
- Questions on how something works are welcome as issues too; [docs/](docs/) and [PLAN.md](PLAN.md) may already answer them.

## Rules that don't bend

These come from the 1.0 promise ("Stable since 1.0" in the README) and from what the project is. A pull request that breaks one won't be merged, however good it is otherwise.

1. **1.x only adds.** The database (schema 10), the sync lines and the `omoi export` format stay readable by 1.0. A new field goes in `extra` or with `#[serde(default)]`, and 1.0 must keep it and pass it on. Anything 1.0 would misread raises `min_reader` / `lector_minimo` and needs an issue first.
2. **Nothing crosses between spaces.** A memory, verdict, tombstone or project line of one client never reaches another client's repository, search or recall. Changes here get an extra review that tries to break them.
3. **No LLM inside Omoikane.** It runs offline, fast and the same on every machine. The agent is the one with the language model; Omoikane gives it tools.
4. **Eight MCP tools.** Every extra tool costs every user context on every session. New behaviour goes into an existing tool's parameters, or into the CLI.
5. **Recall stays fast.** The hook runs on every message: under 30 ms on a normal memory, measured, not estimated.
6. **Local and private.** No telemetry, no accounts, no server of ours. The network is used only to download the model, to update when the user said yes, and to sync with the user's own git repository.
7. **Windows, Linux and macOS alike.** CI runs on all three; paths, line endings and file locking are the usual traps.

## Build and test

You need Rust stable and git.

```bash
cargo build --release
cargo clippy --release --all-targets -- -D warnings
cargo test --release
```

- Tests never touch your real `~/.omoikane`: they set `OMOIKANE_DB` and `OMOIKANE_HOME` to a temporary folder. Do the same when you try things by hand, so your own memory stays out of it.
- `tests/equivalencia.rs` needs the int8 model (about 150 MB). Without `OMOI_MODELO_INT8` pointing to it, it skips itself. CI downloads it.
- `tests/compat.rs` checks against a real `omoi` 1.0.0. Point `OMOIKANE_COMPAT_BIN` to it (from the [v1.0.0 Release](https://github.com/marcmayol/omoikane/releases/tag/v1.0.0)); without it, it skips itself. CI always runs it, on the three systems.
- Fake secrets in tests (tokens, keys) must not look real as a whole in the source, or GitHub's push protection blocks the push. Look at `tests/secretos.rs` for how they're split.

## Changes to search, recall or ranking

These are judged by numbers, not by examples that happen to work.

- Run the public benchmark ([omoikane-bench](https://github.com/marcmayol/omoikane-bench)) before and after, and put both tables in the pull request.
- Compare the two versions interleaved (A, B, A, B…) on the same machine, never one block after the other: caches and thermal throttling make the second block look better or worse than it is.
- Report what got worse too. A change that helps paraphrases and adds noise to questions with no answer has to say so.

## Code style

- The code is written in Spanish: names, comments and test names. Follow the code around yours. If you don't write Spanish, write your comments in English and we'll sort it out in review; don't rename existing code.
- Comment why, not what, and only when the reason isn't obvious.
- No new dependency without a reason in the pull request. Versions of crypto and network crates are pinned (`=x.y.z`) on purpose.
- `cargo clippy -- -D warnings` must pass.

## Commits and pull requests

- Commit messages in English, one line saying what changes for the user ("Recall and search that scale: from 0.7 s to 70-110 ms with 100,000 memories, same results"). Details go in the body.
- One topic per pull request. A fix with a test that fails without it.
- Add your change to `CHANGELOG.md`, under **Unreleased**, in the right section. If it touches the database, sync or export, update **Compatibility** too.
- User-facing text and docs in both languages: when you change `README.md` or a file in `docs/`, change its `.es.md` too (or say in the pull request that you need help with the Spanish).
- Versions and releases are done by the maintainer. Fixes bump the third number (1.0.1); the second number is for new features.

## AI-assisted contributions

Welcome: this is a tool for coding agents, after all. You are still the author: read what you send, run the tests yourself, and be ready to explain any line in review. A pull request that clearly nobody read will be closed.

## License

Omoikane is under the [MIT license](LICENSE). By sending a contribution you agree that it is published under the same license.
