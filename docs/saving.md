# What a memory is and how it gets saved

[Español](saving.es.md) · [Back to the README](../README.md)

## Memory model

A memory is one paragraph the agent would want to read before touching something again.

| field | who sets it |
|---|---|
| `title`, `content`, `kind` (`fact` · `decision` · `gotcha` · `preference` · `procedure`) | the agent |
| `paths`, `tags`, `pinned`, `space` or `trade` | the agent, optionally |
| `project`, `source`, timestamps, recall stats, version history | Omoikane |

Each `kind` changes how it is recalled: preferences and pinned memories are always injected; gotchas come up when the prompt or the files match; facts and decisions by relevance; procedures, whole, when the message says one of their `when:` words. A memory tagged `status` says where the work on a project was left: at session start the latest one of each project comes with part of its content, so "let's carry on with X" needs no searching.

A memory belongs to its project, to a space (all of one client's projects), to a trade (every project of one stack, of any client: see [sync-and-spaces.md](sync-and-spaces.md#trades-what-one-stack-teaches-for-every-project-of-that-stack)) or is global.

## It saves without duplicates

Every memory has a kind that decides when it comes back: `fact` and `decision` by relevance, `gotcha` also by its files, `preference` always, and `procedure` whole when you ask for it. Before saving it checks whether something almost the same already exists. If so, it doesn't save and says which one to update; if it is only similar, it saves and shows the related ones. It warns when a memory goes past the 6,000 characters that count for recall and when it makes another one outdated. Paths are stored relative to the repository, so they work on any computer.

When a program keeps one memory about something and rewrites it (a project's profile, the state of a tool), it can save it with a key: `omoi remember … --key kumi-profile` (or `key` in the MCP `remember`). If a memory with that key already exists in the same project, space or global scope, it is updated, keeping the previous text in its history; if not, it is created. There is no duplicate check, and two processes saving the same key at once still leave a single memory. Two computers that each create the same key before syncing do end up with two memories with that key at the same level: reading the key gives the older one (by creation date), and the other one stays, in search and in `--resolve`, until you merge them (`update … --absorb`). `omoi get --key kumi-profile` (or `get` with `key`) reads it: the project's, else its space's, else the global one. The key is a tag, `topic:<key>`; a new key that looks like an existing one (`kumi_profile` next to `kumi-profile`) gets a warning.

A key can live at four levels, one memory per level: the project (the default), its space (`--space`), a trade (`--trade web`, a global memory tagged `trade:web`) and global (`--global`). `omoi get --key <key> --resolve --json` (or `get` with `key` and `resolve`) returns every one this project sees, most specific first (project, space, trade, global), each with its `scope` and its `data`, without merging them: whoever reads them combines them. Data goes with `--data '<JSON>'` (or `data` in the MCP), kept apart from the text like any memory's data: out of search and recall, it travels with sync and Omoikane doesn't interpret it. Saving the same text and data again changes nothing; changing only the data creates no new version, and saving the key without `--data` keeps the data it had. With `--trade`, `--for client|personal` makes its own level too. A `preference` saved without `--space`, `--trade`, `--global` or `--dir` is global, like any preference: `omoi remember -k preference --key x …` run inside a project creates or updates the global `x`, not the project's (use another kind, or `--dir`, for a project-level one). A trade key goes through the leak check like any trade memory, with its key and its data.

Kumi's route through its factories is the example. The web factory's default route is a trade memory, client X always adds seo-geo after web, and one of X's projects skips SEO:

```
omoi remember -k fact -t "Web factory route" --trade web --key kumi-route --data '{"route":["web"],"tests":["web","cyber"]}'
omoi remember -k fact -t "Client X route" --space=client-x --key kumi-route --data '{"route":["web","seo-geo"]}'
omoi remember -k fact -t "disney-countdown route" --key kumi-route --data '{"route":["web"]}' "No SEO: a family countdown."
omoi get --key kumi-route --resolve --json
```

From disney-countdown the last command returns three layers, `project`, `space` and `trade:web`, in that order. From another project of client X, `space` and `trade:web`; from a project of client Y, never client X's.

## It saves when you ask

`/omoikane` makes the agent save right then. It goes over the session, saves what is worth keeping and isn't saved yet (decisions with their reason, gotchas with their files, preferences, procedures) and brings the project's `status` memory up to date. With text after it, as in `/omoikane deploys go over ssh`, it saves that, in your words. It ends with one line per memory saved or changed.

## Decisions that wait for you

When a decision is yours and the agent can't ask you right then, it saves it as kind `decision` with the tag `open` (the question, the options, what it assumed meanwhile) and keeps working. If the work can't go on without your answer, because there is no safe assumption or a wrong guess would be costly to undo, it also adds the tag `blocking` and stops that part. Without it, the decision is for later. Every session starts with the open ones, the blocking ones first and marked. In the web view, **Projects** and **Overview** say which are blocking, and answering one there drops both tags.

`omoi decisions` lists them, blocking first (`--blocking` for only those; `--project`, `--space`, `--all` and `--json` as in `recent`), and agents have the `decisions` tool. From a client's project it never shows another client's. A program that hands out work, such as the dispatcher of an agent factory, can check before starting a task on a repository:

```sh
omoi --dir ~/work/acme-web decisions --blocking --json
```

```json
[
  {
    "id": "01M6B2Q8XKTE4R7N0VZC3HJ5WD",
    "title": "Delete accounts inactive for 12 months?",
    "content": "Options: delete, or only anonymise. Assumed meanwhile: nothing is deleted.",
    "priority": "blocking",
    "project": "/home/ana/work/acme-web",
    "project_key": "github.com/acme/web",
    "space": "acme",
    "global": false,
    "tags": ["blocking", "open"],
    "paths": ["src/accounts/cleanup.ts"],
    "source": "claude-code",
    "created_at": "2026-11-07T09:12:40.120Z",
    "updated_at": "2026-11-07T09:12:40.120Z"
  }
]
```

`priority` is `blocking` or `later`. It is a tag, so neither the schema nor sync change, and an older version sees an ordinary open decision.

A decision has three states, all in its tags, so the text stays clean: open and blocking (`open` + `blocking`), open for later (`open`), and resolved (`resolved`, with the answer at the end of its content). `omoi decisions --resolve <id> --answer "…"` resolves one: it appends `Answer (2026-11-07): …`, drops `open` and `blocking`, adds `resolved` and keeps the previous version in its history (the `decisions` tool takes `resolve` and `answer`, and the web view does the same). `--tag` narrows the list to the decisions that carry other tags too, such as the task that opened them.

## Listing by tag, and data for programs

`omoi list --tag <tag>` returns every memory with that tag in the project, its space and the global ones, newest first, with no search, ranking or threshold (`--tag` again for several, all required; `--kind`, `-n`, `--project`, `--space`, `--all`, `--json`). A program that saves something under a tag finds it every time; `search` ranks by words and could leave it out. Agents get the same with `recent` and `tags`.

A memory can also carry data for programs: any JSON up to 64 KB, with `--data '<json>'` in `remember` and `update` (`--no-data` removes it) or `data` in the tools. `get`, `list --json` and `decisions --json` return it as `data`. It is never searched or recalled, so it doesn't fill the agent's context, and what looks like a secret in it is redacted as in the content. Changing only the data doesn't add a version. It travels with sync and `omoi export` as a `data` field of the memory's line: versions from 1.0 to 1.7 don't know it, but they keep it and send it on unchanged, even after editing the memory.

This is how Kumi's dispatcher (`kumi.py`) uses it, with the text free of markers:

```python
def omoi(*args, entrada=None):
    r = subprocess.run(["omoi", *args], input=entrada, capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None

# The project profile: a memory with a tag and its data, found every time.
def perfil_leer(raiz):
    ms = omoi("--dir", raiz, "list", "--tag", "kumi-perfil", "--kind", "decision", "--json") or []
    return {**ms[0]["data"], "id": ms[0]["id"]} if ms and ms[0].get("data") else None

def perfil_guardar(raiz, perfil, previa_id=None):
    if previa_id:
        omoi("--dir", raiz, "update", previa_id, "--data", json.dumps(perfil), "--json")
    else:
        omoi("--dir", raiz, "remember", "--json", "-k", "decision", "-t", "Kumi: project profile",
             "--tag", "kumi", "--tag", "kumi-perfil", "--data", json.dumps(perfil), "--force",
             "Marc's decisions for how Kumi works on this project.")
# `--force` only skips the duplicate check. It never lets a trade memory (`--trade`) through the leak
# check: that takes `--allow-leaks`, for a false positive someone has looked at, never by default.

# The queue: a blocking decision for a task, the risk gate's check, and the approval.
def decision(raiz, tarea, titulo, texto, bloqueante=True):
    tags = ["--tag", "kumi", "--tag", f"tarea:{tarea}", "--tag", "open"] + (["--tag", "blocking"] if bloqueante else [])
    return (omoi("--dir", raiz, "remember", "--json", "-k", "decision", "-t", titulo, *tags, "--force", texto) or {}).get("id")

def bloqueantes_abiertas(raiz, tarea):
    return omoi("--dir", raiz, "decisions", "--blocking", "--tag", f"tarea:{tarea}", "--json") or []

def cerrar(raiz, mem_id, respuesta):
    omoi("--dir", raiz, "decisions", "--resolve", mem_id, "--answer", respuesta, "--json")
```

## It saves even when the agent forgets to

When a session ends or compacts, Omoikane reads the conversation and sets aside what may be worth keeping: your answers to the agent's questions (with the question), your preferences and corrections, and the last commit or release. The next session in that project gets them once, and the agent saves the ones worth it in your words; the Review tab lists them too. Nothing that looks like a secret, nothing already saved. They are proposals, and they expire after 7 days if nobody saves them.

In Claude Code it also reads the session's subagents. For each one that finished, it sets aside its final report with what it was asked to do, because the main agent reads it once and hardly ever saves it. One still working is skipped, as is one with something that looks like a secret, and what is already saved isn't proposed again.

It reads the conversation of Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor, Kiro, OpenCode and Grok Build (`omoi setup <agent>` adds the end-of-session hooks); not Antigravity, whose conversations are stored in an undocumented binary format. In agents with no recall at session start, such as Grok, what was set aside arrives through the `context` tool.

## Verified by a person

Agents write almost everything in the memory. When you check one and it is right, mark it: `omoi verify <id>` (it shows the memory and asks), or **Verify** on the memory in the web view. It keeps who and when (`--by <name>`; by default your sync name, `OMOIKANE_AUTHOR`, or the system user). The recall marks it `[decision, verified]`, and search, `get`, `list --json`, `recent` and `context` carry `verified: {by, at}`. The agent is told to trust a verified memory over an unverified one that disagrees. `omoi verify <id> --unverify` removes it.

An agent can't verify anything. There is no MCP tool for it, `remember` and `update` ignore a `verified` field, and the HTTP tools (`/tools/…`) are the MCP ones. `omoi verify` asks in a terminal and refuses elsewhere without `--yes`, and it always refuses inside a coding agent's shell (`CLAUDECODE`, `AI_AGENT`, `GEMINI_CLI`, `CODEX_SANDBOX`… are set). The web button goes through its own route, `POST /api/verify`, which only takes a browser's request from the view itself. An agent that wants to get around this on purpose can, as it could edit the database: the point is that nothing an agent normally does ends in "verified".

Changing the text removes the verification: you vouched for that text, not the next one. `update` says so (`unverified` in the answer) so the agent can tell you. Tags, paths, pinning or moving it don't touch it. The mark carries a fingerprint of the kind, title and content, so a change made by a version from before 1.8.5 (which keeps the mark without understanding it) also leaves it unverified.

When it's time to review (Review, `omoi review`), unverified decisions and gotchas come first in each group, and verified memories last.

**In Kumi.** What the person approves is verified; what the apprentice proposes isn't. `kumi.py` runs, after the person approves in Kumi and never from inside the apprentice's session:

```python
def aprobar(raiz, mem_id, respuesta, quien="Marc"):
    omoi("--dir", raiz, "decisions", "--resolve", mem_id, "--answer", respuesta, "--json")
    omoi("--dir", raiz, "verify", mem_id, "--by", quien, "--yes", "--json")
```

The same for an approved rule (a `preference` or a `procedure`): `verify` once it is saved with the person's wording. If `kumi.py` itself runs inside an agent's shell, it has to start `omoi` without those variables, on purpose: that is the person's path.

## Where each memory came from

Each new memory keeps the agent session and the commit it was written in: `get` shows `written in session … · commit … (branch)`, `history` too, and the JSON carries `provenance: {session, commit, branch}`. The session comes from `OMOIKANE_SESSION`, or from `CLAUDE_CODE_SESSION_ID`, which Claude Code sets for its shell and its MCP servers; otherwise, the only session the hooks registered in that project in the last 12 hours (with two at once it doesn't guess). The commit is the project's HEAD, read from `.git` without running `git` (worktrees and `packed-refs` included). It is the provenance of when the memory was written: later changes don't touch it. Trade memories don't carry it (nothing of a project goes to every client). It costs 0.4 to 0.5 ms per save.

## It doesn't keep secrets

If a token, a private key or a password slips into a memory, it becomes `[redacted:<type>]` before it is saved, and the reply tells the agent. What was saved before shows in Review with a button to redact it, history included, and none of it leaves the computer when syncing.

## History with reasons

Changing a memory keeps the previous version and, if the agent says so, why it changed. `omoi history` shows them all.

**What stays out.** An agent fills the gaps in what you ask for: "make the login secure" can end with a second factor nobody asked for. When you decide what goes in and what doesn't, save it as a `decision`, with what stays out written plainly: "Login is email and password, with password recovery and sign-in with Google. No second factor for now: nobody asks for it and it makes signing up harder." Decisions come back by relevance: when a message is about the login, the agent has it in front of it before it starts adding things. Write it with the words you would use when asking ("login security", "second factor"): those are what bring it back. If you change your mind, `update` it with the reason; the previous version stays in its history.

## Procedures and linked memories

**Procedures.** A memory of kind `procedure` is a recipe you repeat (publishing a release, a deploy): numbered steps and a line `when: publish, release`. When a message asks for it, it comes back whole ("publish 0.14.1" brings the whole checklist), and the agent updates it when a step fails or changes.

**Proposed recipes.** Omoikane also proposes recipes. When each session ends it notes, in a local table, the steps that worked: the program and its subcommand, like `cargo test` or `git tag`, leaving out what only looks around (and, in Claude Code, what failed). When a sequence of three or more steps, with at least two different programs, has come up in five sessions and none of your recipes names it, it is set aside for the next session. The agent asks you whether it is a procedure you follow and, if you say yes, saves it as a recipe with its steps and its `when:` line. Each sequence is proposed once, and at most one per session. Replaying 115 real sessions it came up with two, `git fetch → git tag → gh release` and `git fetch → gh issue → gh pr`: checking whether anyone answered on your PRs.

**Linked memories.** `[[name]]` in a memory links it to the one tagged with that name, and the links travel with the notes. When a memory is recalled, a linked gotcha or preference can come along with it; broken links show up in Review, and each project page draws its connections.
