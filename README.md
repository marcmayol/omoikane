<p align="center"><img src="assets/banner.png" alt="Omoikane" width="100%"></p>

# Omoikane

*Eternal memory, alive and lightning fast.*

**Local memory for AI coding agents. One binary, one SQLite file, and the agent doesn't have to remember to look things up.**

[Español](README.es.md)

> Omoikane (思兼, "the one who gathers thoughts") is the kami the other gods consult when a decision has to be made: he remembers everything that is known and puts it together. That is what this program does for an agent.

## What makes it different

Your agent learns things every session: a bug that only shows up on Windows, why the team chose ULIDs, that you want short commits in English. Then the session ends and all of it is gone. Omoikane keeps those as short, typed memories in a local SQLite file, and does the rest by itself.

### What it does every session

- **It recalls without being asked.** When what you type relates to something saved, a hook hands the agent that memory (at most 2, once per session) or the gotcha of the file you named, in about 20 ms. If nothing relates, it hands over nothing, as with 93 % of real prompts. The agent doesn't have to remember to search.
- **Search that works in your language**, accents or not, in 101 languages: on questions written blind it finds the right memory first 90 % of the time, in 43 ms, and stays as accurate at 3,183 memories.
- **It catches duplicates when saving.** If the agent tries to save something it already knows, it is told so and updates the memory that is there instead of adding another.
- **It works with 15 agents.** Claude Code, Codex, Gemini CLI, Qwen Code, Cursor, Copilot, Kiro, OpenCode, Grok and the rest share the same memory; `omoi setup <agent>` gets each one ready. Tested by hand: Claude Code and OpenCode; the rest, going by their documentation (the "Other agents" table says which, one by one).
- **The same memory on all your computers, and with teammates.** It syncs by itself through a git repository of yours, encrypted before it leaves your computer, with nobody's server in between.
- **History with reasons.** Changing a memory keeps the previous version and, if the agent says so, why it changed.
- **It doesn't keep secrets.** If a token, a private key or a password slips into a memory, it becomes `[redacted:<type>]` before it is saved, and the reply tells the agent. What was saved before shows in Review with a button to redact it, history included, and none of it leaves the computer when syncing.
- **Nothing gets lost.** Forgetting sends to the trash, history included, and `omoi restore` brings it back from any computer. `omoi export` is a faithful copy that `omoi import` rebuilds exactly, and a copy of the database is kept every day by itself.
- **You can see it and tidy it up.** A web view and a terminal one to search, read the history, pin, move or forget.
- **Private and small.** One binary, one file, 8 MCP tools and no server of its own.

### What keeps the memory alive

- **It saves even when the agent forgets to.** When a session ends, Omoikane reads the conversation and sets aside what you decided and asked for; the next session saves what is worth keeping, in your words.
- **It saves when you ask.** `/omoikane` goes over the session and saves what is worth keeping; `/omoikane <text>` saves that.
- **It notices when a memory is out of date.** If a newer memory contradicts an older one, the old one reaches the agent marked `⚠ may be outdated`, and the Review tab shows both side by side.
- **It remembers what the agent judges.** If it says one memory replaces another, or that two disagree, that's kept, travels to your other machines and reaches the agent flagged next time.
- **Recipes that come back whole.** A `procedure` memory holds the steps of something you repeat; "publish 0.14.1" brings the whole checklist, and the agent fixes a step when it changes.
- **Linked memories.** `[[links]]` between notes travel with them: recall one and the warning linked to it comes along.
- **It picks up where you left off.** Each project's latest `status` memory arrives at session start, so "let's carry on with X" needs no searching.
- **It keeps itself up to date if you ask it to**, and only installs releases signed with Omoikane's key. It comes off.

## Noise, stale memories and mistakes

A recall that injects too much is worse than none: it fills the context with noise and carries what no longer holds from one session to the next. That is why it is the most heavily calibrated part of Omoikane.

- **Mostly silent.** A prompt only gets something if it has at least 3 useful words and a memory shares 2 words that are rare in your base, adding up to at least half of the prompt's weight. The meaning channel, for paraphrases, also needs the words to cover 25 % and is skipped on prompts longer than 500 characters. Over 300 real prompts it injected something into 7 %; the rest got nothing. "How did it all go?" brings nothing.
- **Little, and once.** At most 2 memories per prompt, and only those close to the best one: if one stands out, it goes alone. Each arrives as 200 characters; if the agent wants the rest, it calls `get`. A linked gotcha or preference may join them, and a procedure only when the prompt says one of its `when:` words. Each memory arrives once per session.
- **Stale memories arrive flagged.** If a newer memory on the same subject contradicts it (another version of the same thing, or done what was pending), the old one arrives with `⚠ may be outdated: newer <id> …` in front, and the agent checks the new one before trusting it. It also flags project statuses left behind, paths that no longer exist and memories nobody uses. The Review tab puts both memories side by side to forget the old one or keep it. No model involved: words, dates and versions.
- **Mistakes get fixed, not carried.** Only what the agent saves goes into memory. What Omoikane sets aside when a session closes are proposals, and they expire after 7 days if nobody saves them. Fixing one is `update` with a reason, and the previous version stays in the history. `forget` deletes it, and it doesn't come back from your other machines. What a teammate saved arrives marked `[by Ana]`.
- **Measured.** Every threshold comes from questions written blind by another agent, 20 of them with no answer in the notes: on those it injected something twice. The numbers are in [BENCHMARK.md](BENCHMARK.md).

At session start more does arrive: your preferences, what you pinned, where each project stands and what was touched last. Preferences and pins are what you want the agent to always know.

engram, the alternative Omoikane is compared with, doesn't inject memories on each prompt. With stale ones it does something else: every save proposes similar memories and asks the agent to judge how they relate (`mem_judge`). Omoikane asks for nothing. It detects the contradiction itself, flags it in the recall with the reason, and lets the agent judge only when it knows something Omoikane can't see (`supersedes`, `contradicts`, `compatible`).

## What about CLAUDE.md?

For a small project with twenty things to remember, a good CLAUDE.md is enough. Omoikane pays off where CLAUDE.md stops reaching:

- CLAUDE.md is loaded whole every session, relevant or not, and the more it knows the more context it spends. Omoikane brings what fits the prompt.
- It's one file per project. What you learn in one doesn't reach another unless you copy it; in Omoikane a memory can apply everywhere.
- It doesn't know when something has gone stale, nor keep why it changed.
- You write it. Omoikane gets filled by the agent as it works.
- One agent reads it. Omoikane is shared by 15, and by all your machines.

They can live together: CLAUDE.md for the repository's fixed rules, Omoikane for what gets learned along the way.

## Status

**Before 1.0:** the database schema can still change. Each change migrates by itself and keeps a copy of the database first, but a migrated database can no longer be opened by an older version. 1.0 will freeze it.

Version 0.19.1,
 on crates.io and in the Releases, with CI on Windows, Linux and macOS. Used every day on Windows and Linux. The full design and every decision are in [PLAN.md](PLAN.md) (Spanish).

## Install

In Claude Code (2.1.310 or newer), two commands and nothing else:

```text
/plugin marketplace add marcmayol/drac-plugins
/plugin install omoikane@drac
```

Automatic updates come off. `omoi setup claude` and the one-line installers ask whether you want them (no answer leaves them off) and, if you say yes, turn on the plugin's update too; `omoi upgrade --on` turns them on any time. For the plugin's, if you only ran the two commands above: `/plugin` → **Marketplaces** → **drac** → **Enable auto-update** (Claude Code leaves it off for third-party marketplaces).

On the first session the plugin downloads the `omoi` binary for your system from this repo's Releases (checking its SHA-256) into `~/.omoikane/bin`, then the embedding model (~150 MB, once) in the background; until it arrives, search works by words. When the plugin updates, it fetches the matching binary. Prebuilt binaries: Windows x64, Linux x64/ARM64 (glibc 2.35 or newer), macOS Apple Silicon and Intel; anywhere else, `cargo install omoikane` and the plugin uses the `omoi` on your PATH (or set `OMOIKANE_BIN`). If you'd rather not run a binary you didn't build, do the same: `cargo install omoikane` builds it from the source published on crates.io.

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

## Everything it does

Every feature, one by one, with how it works.

### Getting started

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

### It recalls by itself

When a session starts, the agent gets your preferences and pinned memories, where each project was left, what was worked on last and anything worth reviewing. On each prompt it only gets something that really matters: a memory has to cover enough of the prompt's rarer words, and gotchas also come up when you name their files. Each memory arrives once per session. Over 300 real prompts it injected something into 7 % of them. If 15 minutes and 10 prompts go by with nothing saved, it reminds the agent to save.

### It searches in your language

Search starts with words: accents don't matter, file paths count too, and rare words weigh more. Then a local embedding model, which opens in 2.7 ms, finds what is very close in meaning even with no word in common. Where it searches depends on where you are: from a project, that project and the global memories; from your home folder, everything.

### It saves without duplicates

Every memory has a kind that decides when it comes back: `fact` and `decision` by relevance, `gotcha` also by its files, `preference` always, and `procedure` whole when you ask for it. Before saving it checks whether something almost the same already exists. If so, it doesn't save and says which one to update; if it is only similar, it saves and shows the related ones. It warns when a memory goes past the 6,000 characters that count for recall and when it makes another one outdated. Paths are stored relative to the repository, so they work on any computer.

### It saves when you ask

`/omoikane` makes the agent save right then. It goes over the session, saves what is worth keeping and isn't saved yet (decisions with their reason, gotchas with their files, preferences, procedures) and brings the project's `status` memory up to date. With text after it, as in `/omoikane deploys go over ssh`, it saves that, in your words. It ends with one line per memory saved or changed.

### It saves even when the agent forgets to

When a session ends or compacts, Omoikane reads the conversation and sets aside what may be worth keeping: your answers to the agent's questions (with the question), your preferences and corrections, and the last commit or release. The next session in that project gets them once, and the agent saves the ones worth it in your words; the Review tab lists them too. Nothing that looks like a secret, nothing already saved. It reads the conversation of Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro, OpenCode and Grok Build (`omoi setup <agent>` adds the end-of-session hooks); not Antigravity, whose conversations are stored in an undocumented binary format. In agents with no recall at session start, such as Grok, what was set aside arrives through the `context` tool.

### It looks after itself

Omoikane notices when a memory has fallen behind: a newer one on the same subject says something different (a higher version of the same thing, or done what was pending), a `status` memory the project has moved past, paths that no longer exist, memories never used in a month, and notes too long for recall. When one of those reaches the agent it comes marked `⚠ may be outdated: newer <id> …`, and saving a memory that makes another outdated says so right away, so the agent fixes it on the spot. The **Review** tab of the web view lists them with the two memories side by side, *forget the old one* and *it's fine as it is*; `omoi review` does the same from the terminal. No model involved: words, dates and versions.

**Verdicts.** What Omoikane can't know by itself, the agent says, with no new tools: when saving or changing a memory it can pass `supersedes` (this one replaces those), `contradicts` (they say different things and it's unclear which holds) or `compatible` (a warning paired them and both hold). The verdict is kept, travels with sync and shows wherever the memories show: in the recall (`⚠ superseded by …`), in `search`, in `context` and in Review. It holds while what it judged doesn't change, and for a pair the newest one wins. "It's fine as it is" is a verdict too, so what you approve on one machine holds on the others.

**Why and who.** Each verdict keeps its reason and who gave it (the sync person, or the agent and its client), and both show in the recall, in `search` and in Review. If A is superseded by B and B by C, A's note leads straight to C.

**Fixing a piece and merging.** `update` takes `replace` (changes a text that must appear exactly once; otherwise nothing changes), `append` (adds at the end, for a procedure's new step) and `absorb` (merges several memories into one: the agent writes the merged text, and the absorbed ones go to the trash with their history hung on the one that stays and their tags and links moved to it). The Review tab has a **Merge** button to do it by hand.

**Review date.** Each kind has its own: decisions and procedures after 120 days untouched, facts and gotchas after 180, preferences never, counting from its last change or the last time the agent got it (what gets used and nobody corrects is reviewed). When it comes, the memory shows in Review and in `search` with "not reviewed in N days"; not in the recall, which only carries what says a memory may be wrong. "It's fine as it is" restarts the clock.

### Procedures and linked memories

**Procedures.** A memory of kind `procedure` is a recipe you repeat (publishing a release, a deploy): numbered steps and a line `when: publish, release`. When a message asks for it, it comes back whole, and the agent updates it when a step fails or changes.

**Linked memories.** `[[name]]` in a memory links it to the one tagged with that name. When a memory is recalled, a linked gotcha or preference can come along with it; broken links show up in Review, and each project page draws its connections.

### History, trash and copies

Changing a memory keeps the previous version with the reason, and `omoi history` shows them all.

**Trash.** Forgetting a memory (`forget`, from the agent, the CLI or the web view) sends it to the trash with its history, and a forget doesn't come back from another computer when you sync either. `omoi restore <id>` brings it back, history included, and wins on the other computers at the next sync: a `forget` made by mistake on one machine is undone from any of them. The agent does it with `update` and `restore: true`, with no new tool; the Review tab has a **Trash** group with a button for each one. After 30 days (`OMOIKANE_TRASH_DAYS`; 0 = never) they are deleted for good; `forget --purge` skips the trash and `omoi trash --empty` empties it.

**Faithful copy.** `omoi export` writes a JSONL with everything needed to rebuild the database: each memory with its history, clock stamp and author, the forget marks, the verdicts and the trash, with an end line carrying the count and the SHA-256 (a cut or tampered file is not imported). `omoi import` puts it back with the same rules as sync: same ids, dates and stamps, newest wins and what was forgotten doesn't come back, so repeating it changes nothing and it serves both to restore a database and to merge two. Both stream, one line in memory at a time: 50,000 memories export and import without loading the RAM. With `--encrypt`, every line is encrypted with the sync key, to keep the copy anywhere. What is in the trash goes with its project and its space, and with its verdicts: restored on another computer, it goes back where it was, `superseded` included. Files in the previous format and engram's still import, and so does one from a newer omoi: what this version doesn't understand is skipped and counted, and the fields it doesn't know are kept and come out again in the next export.

**Daily copy.** At the first session start of each day, Omoikane copies the database to `~/.omoikane/backups/omoikane-<date>.db` and keeps the last 7 (`OMOIKANE_BACKUPS`; 0 = off). `omoi backup` does it now, and `omoi doctor` says when the last one was.

### See it and tidy it up

`omoi tui` opens a terminal viewer: search as you type, read a memory and its history, pin or forget it.

`omoi serve --open` (or `/omoikane-web` in Claude Code) opens the same in your browser at `http://127.0.0.1:7438/`. **Memories** is a timeline by day of what was saved or changed (and, if you want, what the agent recalled), searchable the way the agent searches and filtered by kind, project or tag; a memory opens in place with its Markdown formatted, its last change as a diff, its full history, and buttons to pin, move or forget it. **Projects** shows where each project was left (its latest `status` memory) and opens a page per project with its memories in one column per kind. It follows the system's light or dark mode, and works offline: the fonts (IBM Plex and Shippori Mincho, SIL OFL, in `assets/fuentes`) are inside the binary. It only answers on your own computer, and refuses requests from other websites.

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
| Grok Build | yes | no (its session-start and prompt hooks discard what they return; the instructions ask it to call `context` at the start) | yes | from its docs |
| Windsurf / Devin Desktop, Kimi Code, Pi, Amp, Kilo Code | yes | no (their hooks can't add context, or it isn't documented) | no | from their docs |

`omoi setup` also adds two commands in the agents that support user commands: `/omoikane`, which makes the agent save, and `/omoikane-web`, which opens the web view. Those are Gemini CLI, Qwen Code, Codex (`/prompts:omoikane` and `/prompts:omoikane-web`), Cursor, OpenCode, Kiro, Windsurf, Pi, Kilo Code and Grok Build. In those agents a command is a prompt, so the agent runs `omoi serve --open` itself and may ask before doing it. An older `/omoikane`, which opened the web view, becomes the one that saves.

### Projects and spaces

Each project has its key, worked out once: its git `origin`, or, without one, your name and the folder's. It also keeps every folder it lives in, so two clones of the same repository, or a worktree, are the same project. Move the folder of a repository with an `origin` and its memories follow it by themselves at the next session (you're told once); without an `origin`, `omoi projects move <project> <folder>`, and until then `omoi review` and `omoi doctor` point it out. `omoi projects` lists them. `omoi projects merge <a> <b>` joins two without touching ids, history or verdicts, keeping the newer of their two `status` memories; `omoi projects prune` forgets the ones left without memories.

Spaces keep clients apart. `omoi spaces add acme`, then `omoi space assign acme` in each of the client's repositories (or `omoi projects move <project> --space acme`). From an Acme project you see its memories, the space's and the global ones, never another client's: not in search, not in recall, not in `context`. What is common to all of a client's repositories is saved with `remember --space` (`space: true` over MCP); your preferences stay yours, global. From your home folder you see everything. `search` and `recent` take `project` (a folder or part of a name) and `space`. To share it, one repository per client, with its own key: `omoi sync init --space acme <url>` carries only that space, without your global memories, and the general repository doesn't carry the space's memories.

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
- **Projects** are matched by their git `origin` remote, so the same repo in different folders on different computers is the same project. Projects without a remote are matched by folder name, only between the same person's computers. A project's space travels with its memories, and `--space` keeps a repository to one client.
- **Computers on different versions:** from 1.0 on, what a new version adds doesn't break the old ones. A field a computer doesn't know is kept with the memory and goes back up even if that computer changes it, so it isn't lost by passing through it. A line of a kind it doesn't know is skipped, `omoi sync status` says "N line(s) from a newer version: update omoi", and after updating everything is read again. And if a version ever changed something the old ones can't read right, the repository says so in `omoikane.format` and the old ones don't write to it until they're updated (`status`, `doctor` and the session start say so).

`omoi sync off` stops syncing on a computer; its memories stay.

### It keeps itself up to date, if you ask it to

It comes off: until you say yes (`omoi upgrade --on`, or the question in `omoi setup claude`), `omoi` doesn't change itself. When on, once a day, the first time it runs, `omoi` checks in the background for a newer release of the same major version, installs it only if it is signed with Omoikane's key, and adds whatever the new version needs to the agents that already have it; the next session tells you. `omoi setup claude` asks (and turns on the plugin's update in Claude Code too); `omoi upgrade --off` or `OMOIKANE_AUTO_UPDATE=0` turns it off, `omoi upgrade` updates by hand, and a binary managed by cargo, Homebrew, Scoop or Nix is left to them. Before moving a database to a newer schema it keeps a copy.

### Over HTTP, for scripts and apps

`omoi serve` serves the same tools over HTTP on `127.0.0.1:7438` for scripts and apps that don't speak MCP: `GET /health`, `GET /tools`, `GET /search?q=…`, `POST /tools/<name>` with the tool's JSON arguments. Set `OMOIKANE_HTTP_TOKEN` to require `Authorization: Bearer <token>`.

### Importing from other tools

`omoi import` reads exports from engram (`engram export`) and from `omoi export`: it maps the kinds, keeps the dates, skips what was deleted and imports nothing twice. `--dry-run` shows where each memory would go.

## Strengths

- **It recalls by itself, and only when it matters.** The agent doesn't have to remember to search: if a prompt relates to something saved, it arrives in 15-26 ms; if not, nothing arrives.
- **It gets it right.** 90 % of the time the right memory comes first; 97 % of the time, in the top three. In 101 languages, accents or not.
- **Fast at any size.** 43 ms per search, and 50 ms with 3,183 memories. Four processes share the model at 13 MB of memory each.
- **It saves even when the agent forgets to**, and never saves the same thing twice.
- **It doesn't go stale.** It sees what is out of date, flags it and puts it in front of you to decide.
- **One binary, nothing else:** no Python, no Node, no Docker, no server to keep running. SQLite is built in.
- **It works offline.** Everything is local, and the web view carries even its fonts, IBM Plex and Shippori Mincho, inside the binary.
- **Windows just like Linux and macOS:** no bash, no scripts, and tested on all three on every change.
- **15 agents, one memory** (2 tested by hand, the rest going by their documentation), and `omoi setup <agent>` gets each one ready.
- **Your memories are yours, and they don't get lost:** one SQLite file with a daily copy, an export that rebuilds it exactly, a trash everything comes back from, and a sync that only goes through your own git repository, encrypted.
- **It updates carefully:** it only installs releases signed with Ed25519 and, before changing the database schema, keeps a copy.

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
