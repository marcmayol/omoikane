# What a memory is and how it gets saved

[Español](saving.es.md) · [Back to the README](../README.md)

## Memory model

A memory is one paragraph the agent would want to read before touching something again.

| field | who sets it |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | the agent |
| `paths`, `tags`, `pinned` | the agent, optionally |
| `project`, `source`, timestamps, recall stats, version history | Omoikane |

Each `kind` changes how it is recalled: preferences and pinned memories are always injected; gotchas come up when the prompt or the files match; facts and decisions by relevance; procedures, whole, when the message says one of their `when:` words. A memory tagged `status` says where the work on a project was left: at session start the latest one of each project comes with part of its content, so "let's carry on with X" needs no searching.

## It saves without duplicates

Every memory has a kind that decides when it comes back: `fact` and `decision` by relevance, `gotcha` also by its files, `preference` always, and `procedure` whole when you ask for it. Before saving it checks whether something almost the same already exists. If so, it doesn't save and says which one to update; if it is only similar, it saves and shows the related ones. It warns when a memory goes past the 6,000 characters that count for recall and when it makes another one outdated. Paths are stored relative to the repository, so they work on any computer.

## It saves when you ask

`/omoikane` makes the agent save right then. It goes over the session, saves what is worth keeping and isn't saved yet (decisions with their reason, gotchas with their files, preferences, procedures) and brings the project's `status` memory up to date. With text after it, as in `/omoikane deploys go over ssh`, it saves that, in your words. It ends with one line per memory saved or changed.

## It saves even when the agent forgets to

When a session ends or compacts, Omoikane reads the conversation and sets aside what may be worth keeping: your answers to the agent's questions (with the question), your preferences and corrections, and the last commit or release. The next session in that project gets them once, and the agent saves the ones worth it in your words; the Review tab lists them too. Nothing that looks like a secret, nothing already saved. They are proposals, and they expire after 7 days if nobody saves them.

It reads the conversation of Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro, OpenCode and Grok Build (`omoi setup <agent>` adds the end-of-session hooks); not Antigravity, whose conversations are stored in an undocumented binary format. In agents with no recall at session start, such as Grok, what was set aside arrives through the `context` tool.

## It doesn't keep secrets

If a token, a private key or a password slips into a memory, it becomes `[redacted:<type>]` before it is saved, and the reply tells the agent. What was saved before shows in Review with a button to redact it, history included, and none of it leaves the computer when syncing.

## History with reasons

Changing a memory keeps the previous version and, if the agent says so, why it changed. `omoi history` shows them all.

## Procedures and linked memories

**Procedures.** A memory of kind `procedure` is a recipe you repeat (publishing a release, a deploy): numbered steps and a line `when: publish, release`. When a message asks for it, it comes back whole ("publish 0.14.1" brings the whole checklist), and the agent updates it when a step fails or changes.

**Linked memories.** `[[name]]` in a memory links it to the one tagged with that name, and the links travel with the notes. When a memory is recalled, a linked gotcha or preference can come along with it; broken links show up in Review, and each project page draws its connections.
