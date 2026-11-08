# Roadmap

Where Omoikane is going, what it won't do and why. There are no dates: a version ships when it is measured and tested on Windows, Linux and macOS. What each version actually changed is in the [CHANGELOG](CHANGELOG.md). The same items are on the [project board](https://github.com/users/marcmayol/projects/1).

[Leer en español](ROADMAP.es.md)

## How things get in

- An idea gets measured before it ships. Several were dropped because, on real conversations, they fixed nothing (see [Not planned](#not-planned)).
- 1.x doesn't break: every version reads the databases and syncs with every earlier 1.x. The CI checks this on each commit against a fixed 1.0.0 binary.
- Recall runs on every prompt, so it has a budget (around 30 ms). Anything that would cost more there goes elsewhere: the review, a background job, or an explicit command.
- Want something? Open an issue saying what you tried and what was missing. [CONTRIBUTING.md](CONTRIBUTING.md) explains how to test a change.

## Next

Known gaps, in no fixed order:

- The terminal UI doesn't know about the trash or spaces yet.
- Two folders without a git remote, with the same name, in two different clients' spaces, pause each other. It needs a project key that includes the space without breaking the existing ones.
- At 50,000 memories, `review --all` takes about 2 s and a search across everything about 600 ms. Recall, `remember` and `sync` are already fine there.
- `export --encrypt` of several spaces at once uses the general key; each space should keep its own.
- A field added inside a memory's version history is lost on older versions; it needs an `extra` field there too.
- A performance check in the CI that fails if recall or search get slower than the previous version.
- Going back from an Obsidian vault to Omoikane, if people ask for it (today the export only goes one way).

## Later, maybe

- A Docker image, only if Omoikane ever gets a server side. Today there is no server: sync goes through your own git repository, and the hooks and MCP run next to the agent, so a container adds nothing. A Dockerfile to reproduce [omoikane-bench](https://github.com/marcmayol/omoikane-bench) could come earlier.

## Not planned

Decided against, with the reason:

- **A server, a daemon or a cloud.** One binary and one SQLite file; sync goes through a git repository you own.
- **External runtimes** (Python, Node, ONNX Runtime). Zero dependencies is a principle: the meaning model runs in pure Rust.
- **Agents verifying memories.** Only a person can mark a memory as verified, from their own terminal or the web view. There is no MCP tool for it, on purpose.
- **A glossary per project** (synonyms to widen the query). On 280 real prompts it fixed 0 and broke 0.
- **Learning across projects** (recalling other projects' memories). On 168 real prompts it helped once and added noise the rest.
- **Saving the user's prompts.** Over 30 days of real conversations, 0 questions needed an earlier prompt that the memories didn't already answer.
- **A summary per session.** In 151 real status updates, the items dropped from summaries were never needed again.

## Shipped

| Version | Date | What it brought |
|---|---|---|
| 1.8.6 | 2026-11-08 | A direct link to verify a memory, verifications that survive key changes, trade duplicates, hand-written passwords redacted, download time limits |
| 1.8.5 | 2026-11-08 | Memories verified by a person, OKF export and import, code that changed under a memory, provenance |
| 1.8.0 | 2026-11-07 | Trades (memory per stack), project names that tolerate typos, review dates, key-based memories |
| 1.7.0 | 2026-11-06 | Agents with nobody at the keyboard (CI, servers), overview by client, many agents on one database |
| 1.6.0 | 2026-11-06 | Open decisions, file history, recall from your own docs |
| 1.5.0 | 2026-11-05 | Proposed recipes from repeated steps |
| 1.4.0 | 2026-11-04 | Changes arrive mid-session, subagents, session diary, maintenance that speaks up |
| 1.3.0 | 2026-11-02 | Fast again: own engine for the meaning model, loaded only when it helps |
| 1.2.0 | 2026-11-01 | Understands questions asked with other words (EmbeddingGemma 300M, pure Rust) |
| 1.1.0 | 2026-10-31 | Public benchmark, compatibility check against 1.0 on every commit |
| 1.0.0 | 2026-10-31 | First stable version |
