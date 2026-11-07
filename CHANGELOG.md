# Changelog

What changes in each version of Omoikane. The release workflow publishes the section of each version as the notes of its GitHub Release, and won't publish a version that has no section here. Every section ends with **Compatibility**: which versions it understands and which ones understand it.

How it works is in [docs/](docs/).

## 1.8.0 (unreleased)

### Trades: memory for each stack
- A new level between the project and the global memories: the **trade** (android, web, wordpress, rust, python…). What one stack teaches ("don't kill the Android emulator with `taskkill`") is saved once with `remember --trade android` (`trade: "android"` over MCP) and reaches every project of that trade, of any client, and no other: a client's web project never gets the Android trap. In search, recall, `recent`, `context`, the web view and `get` by id. From the home folder, all of them. `update` with `trade` turns a memory into a trade memory (and `trade: ""` turns it back); moving it to a project or a space drops the trade.
- Each project's trades come from its files: the root and one level of subfolders, never the whole tree (Gradle with Android, `AndroidManifest.xml`, `astro.config.*`, `next.config.*`, a `package.json` with a web framework, `wp-config.php`, a theme's `style.css`, `Cargo.toml`, `pyproject.toml`, `.claude-plugin/`, Electron or Tauri, `skill-package/`… 27 trades, several per project, all listed). The names are those of Kumi's production lines (`android`, `web`, `rust`, `plugin-claude`, `python`, `escritorio-node`, `alexa`); `seo-geo` can't be told from the files and is set by hand. They're worked out at the start of each session, in `context` and the first time a project is used, and only written when they change. On Marc's 33 real projects it found what each one is (Android apps, Astro webs, Rust tools, Claude Code plugins, Python tools, an Alexa skill), with nothing for the two that are only Markdown.
- `omoi trades` shows a project's trades and where they come from; `add`, `remove`, `set` and `auto` correct them; `list --all` shows every project's and how many trade memories each trade has. `omoi projects`, `context` and `/api/projects` show them too, and the session start tells the agent the project's trades and when a trap belongs to the trade.
- A trade memory reaches every client, so before saving one Omoikane looks for what belongs to someone: a project's name, a space's (a client), an absolute path of yours, the URL of one of your repositories (or any SSH one), a client's git server, or a domain that only shows up in a client's memories. If it finds any, nothing is saved and the agent gets `trade_leaks` with what it found (exit code 2 in the terminal), to rewrite it or save it in the project; `force` only for false positives. `update` checks what's new in a trade memory, and the whole text when a memory becomes one.
- Any kind can be a trade memory. Trade preferences and pinned ones are injected at the start of every session of a project of that trade, next to the global preferences ("new screens go in a `Scaffold` and use its padding" reaches the next Android app from its first session); the rest come with recall.
- `--for client|personal` (`for` over MCP) keeps a trade memory for the projects in a space (a client's) or in none (yours); both by default. "Review the landing's SEO and GEO" as a `web` preference `--for client` reaches every client's web and not a personal one. Nothing new on the project: the space says it. In session start, recall, search, `get` and the web view.
- `omoi update <id> --trade android` (`update` with `trade`) promotes a memory of a project to its trade with the same id, history and verdicts, after checking its whole text (a client's has to be rewritten in that same update). When it comes from a client, or its previous text or any old version names a project or a client, its history stays on this computer and doesn't travel with it through the general repository (`history_kept_local`).
- Sync: trade memories go only through the general repository (with global sharing on, like global memories), never through a client's, and a client's repository can't bring one in (the line is refused).
- Measured on a copy of the real database plus 20 trade memories (233), release, whole process, 30 interleaved rounds against the published 1.7.2: a prompt's recall P50 129 ms against 150 from a project with trades and 123 against 142 from the home folder; the session start (which now looks at the project's files) 111 against 145 and 128 against 135. The machine was busy and the noise is large: what it says is that it costs no more. Hidden trade memories go through a partial index, read once per query. Of the 134 real global memories, 71 would be refused as trade memories, all for naming Marc's projects or his paths: none of them is stack knowledge.

### Compatibility
- The database schema stays at 10 and `lector_minimo` doesn't move. A project's trades live in two local tables (`project_trade`, `project_trade_seen`) and trade memories have a partial index (`memory_oficio`), all created on open; who a trade memory is for travels as a tag too (`trade-for:client`, `trade-for:personal`), and the memories whose history doesn't travel are noted in `meta` (`historial_local:<id>`); 1.0 to 1.7 ignore them, and the trade tables empty themselves when a project is deleted.
- The trade travels as a tag (`trade:android`) on a global memory: the sync lines and the export format don't change. 1.0 to 1.7 read, keep and send them on unchanged, but see them as ordinary global memories, in every project. A `trade:` tag on a memory of a project or a space means nothing.
- MCP: `remember` and `update` take `trade` and `for` (and `update` takes `force`); compact results carry `trade` and `for` (and then no `global`); a promotion answers `history_kept_local`; a refused save answers `saved: false` (`updated: false` in `update`) with `trade_leaks`. `context`, `omoi projects --json` and `/api/projects` add `trades` to each project. Clients that don't know these fields can ignore them.
- A global memory that has a `trade:` tag from before (nobody should) becomes a trade memory: it stops showing up in projects without that trade.

## 1.7.2 (2026-11-07)

### Fixed
- Two projects whose names only differ in case (a repository `Omoikane` and a folder `omoikane` for its campaign, say) made `project: "Omoikane"` fail with «matches several projects» in `search`, `recent`, `remember` and the CLI. A name written exactly as one of them now picks that one; written any other way (`OMOIKANE`), it still asks which.

### Development
- `examples/eval.rs` compares the recall of two binaries or two settings over a battery of prompts, deterministically and with no model as judge: the hit rate of each side and the list of prompts the change fixes and the ones it breaks. `tests/recall_casos.tsv` is a public synthetic battery (30 memories, 52 prompts, in both languages). With it a project glossary was tried and left out: on 280 real prompts it fixed none and broke none.

### Compatibility
- Same as 1.7.1: the database schema (10), the sync lines and the export format don't change.

## 1.7.1 (2026-11-06)

### Fixed
- `ratatui` goes from 0.29 to 0.30.2 (and `crossterm` from 0.28 to 0.29), which brings `lru` 0.18.5 instead of 0.12.5 and closes the Dependabot alert about `lru`'s `IterMut` (low severity; `omoi tui` didn't use it). `omoi tui` needed no changes and was checked on screen: list, search as you type, whole memory, history, all projects, forget with confirmation.
- The web view's sync indicator (`GET /api/sync`) showed the general repository's URL and author (the personal one) even when `omoi serve` was started in a client's project. Now, from a client's project, it shows only that space's repository, or nothing if the space has none here; from the home folder or a project with no space, the general one as before.
- With `OMOIKANE_NONINTERACTIVE=1`, saving, searching or syncing no longer starts the meaning model's service, which lived on for 30 minutes after the job. A service already running is still used.
- The tests left one folder per test in the temporary folder (30 per `cargo test` on Windows, almost 500 after a few releases). Each test now deletes its own, also when it fails; a full run leaves none.

### Compatibility
- Nothing changes in the database (schema 10), the sync lines, the export format or the HTTP API's fields: 1.7.0 and 1.7.1 can use the same database at once.
- A program that read `/api/sync` from a client's project to get the general repository's URL now gets that space's (or `configured: false`); start it from the home folder to see the general one.

## 1.7.0 (2026-11-06)

### Agents with nobody at the keyboard
- CI jobs, servers, containers and cloud agents can use the team's memory configured only by environment variables ([docs/unattended.md](docs/unattended.md), with a GitHub Actions example).
- `OMOIKANE_HOME` moves the whole Omoikane folder: the database, the models, the meaning model's choice and service, and updates (before, only the last two followed it). Sync, backups and the diary already lived next to the database. Without a home folder and without `OMOIKANE_HOME` or `OMOIKANE_DB`, `omoi` stops with a clear error instead of creating `.omoikane/omoikane.db` in the folder it runs in.
- `OMOIKANE_SYNC_CODE` (one or more codes from `omoi sync code`) and `OMOIKANE_SYNC_URL`: `omoi sync`, `omoi sync --pull` and `omoi sync join` with no code join those repositories by themselves, without prompts, before syncing. A rotated key is added like `join` does. The code is never printed, not even in errors; a code saved in a memory is redacted (`omoikane-sync-code`), and git errors no longer show a URL's user and password in the command they quote.
- `OMOIKANE_AUTHOR` signs what is saved here (each memory's and each verdict's author), over the author chosen when sync was set up.
- `OMOIKANE_NONINTERACTIVE=1`: no prompts even with a terminal (what needs confirming asks for `--yes`), no automatic download of the meaning model, no automatic update and no background sync after each change (`OMOIKANE_SYNC_AUTO=1` brings it back). `omoi sync --pull` then waits for any other sync, does a full one and exits with code 1 if it fails. `OMOIKANE_NO_MODEL=1` also stops `omoi` from downloading the meaning model by itself.
- A CI that joins a client's space with `OMOIKANE_SYNC_CODE` starts from a new database every run, so its checkout was a project the space's repository claims and this computer had never seen: it was paused (on a person's computer that asks them to confirm it), brought nothing and shared nothing. With `OMOIKANE_NONINTERACTIVE=1` the variable is the decision: a checkout whose git `origin` is exactly that of a project of the space joins the space without the pause. Only when this computer joined that space's repository with a code in `OMOIKANE_SYNC_CODE` (the key it holds for it is the code's), only for projects with a git remote (a folder name says nothing about whose it is), and only if the project had no memories here yet (they could have come from another repository). Anything else is still paused, and the agent's notice no longer tells it to ask a user who isn't there.
- `OMOIKANE_SYNC_URL` was ignored once joined; now a different one (a new CI token) replaces the repository's URL in its settings and in the clone's `origin`, without printing the token. If the code turns out to be for another repository, the old URL stays.
- With `OMOIKANE_NONINTERACTIVE=1` and `OMOIKANE_SYNC_AUTO=1` (a server), the `--pull` that `omoi` launches by itself in the background is again the cheap one; the strict one (wait, full sync, exit code 1) is only for a `--pull` a person or a CI asks for.
- A relative `OMOIKANE_HOME` is an error that says so (it would change with the folder each command runs in); `~` and `~/…` are expanded. `omoi setup` and `omoi upgrade` no longer need a home folder. URLs in messages also hide credential-looking query values (`?access_token=…`, `&private_token=…`, `?sig=…`).
- The `/api/sync` endpoint of `omoi serve` and the meaning model's service don't yet follow `OMOIKANE_NONINTERACTIVE`; that comes in 1.7.1.
- With one agent, interleaved against the previous commit (two passes of 60 on a 150-memory space project, whole process): session start P50 27.3/27.0 ms against 27.1/26.7, a prompt's recall 23.5/23.1 against 23.5/23.0. Noise.

### Overview by client
- The web view (`omoi serve`) has a new **Overview** page for whoever runs a client or the whole factory. Per space it shows what there is (projects with their memory count, pinned ones and latest `status`, open decisions and pinned memories), who wrote it (memories per author, how many in the last 30 days, the agents they used and the latest activity) and what is outdated or paused (what Review flags, projects on hold with both commands, and memories untouched for over 3, 6, 12 or 24 months).
- It keeps to the space isolation: started in a client's project, the server shows only that space; in a project with no space, only the projects with no space; in the home folder, every space. Global memories aren't shown per space.
- The same data comes as JSON from `GET /api/overview?months=6`, with the same protection as the rest of the API (local Host only and `OMOIKANE_HTTP_TOKEN`).

### Space isolation in the web view
- Until now only Overview kept to the spaces: Memories, Projects, Review, the trash, what was set aside from sessions and their APIs (`/api/memories`, `/api/projects`, `/api/review`, `/api/trash`, `/api/candidates`) showed every client, whatever folder `omoi serve` started in. Now every page sees what the CLI and the MCP see from that folder: in a client's project, that space and the global memories; in a project with no space, the projects with no space and the global memories; in the home folder, everything. Links (`links`) to memories outside are left out too.
- Writes keep to it as well: redacting, restoring from the trash, dismissing a Review warning or a set-aside note of a memory outside answers 403, and the tools (`/tools/update`, `/tools/forget`, `get`…) already refused it.
- `?dir=<folder>` can no longer widen the view: from a client's project, pointing it at the home folder, another client or a project with no space answers 403. It still picks another project inside what the server shows (and, from the home folder, narrows to one client).
- The header says where you are looking from: "All spaces", "Only ◇ Acme" or "Only no space", with the explanation on hover.

### Memory for whoever joins a client
- The first session in a project of a space on a computer (a new teammate after `omoi sync join`, or anyone opening one of the client's projects there for the first time) brings a short welcome, once: the space's other projects with where their work was left (their latest `status`, up to 8), the decisions still open in them (up to 5) and who writes in the space, with how many memories each. Before, the agent only saw what belonged to the project it was opened in, so the state of the client's other repositories and their open questions didn't reach it. It comes again after compacting that first session, not in later ones; not at all if everything in the space was written by the same person, and never from projects without a space. Only that space: nothing of another client.
- `omoi sync join <code>` for a space ends by saying what arrived (memories, projects, pinned, open decisions) and that the agent will have it at its next session in those projects.
- Preferences saved for a whole space (`remember --space`) now travel through that space's repository even when preferences aren't shared: they were the team's, and a newcomer didn't get them. Personal (global) preferences still don't.
- Each memory is signed with the author of its own space's repository: someone who syncs a client's repository with `--author "Ana (Acme)"` gets `[by Ana (Acme)]` on that client's memories, so teammates and the welcome can say who writes. That name never signs another client's memories nor the ones with no space, which keep the general repository's author; a space whose repository was joined without an author signs nothing. Before, only the general repository's author counted (in the 1.7.0 previews, a client's author could end up signing everything). `OMOIKANE_AUTHOR` still signs everything.
- A project that arrived through sync without a folder on this computer was named `api]` in some lines; now `api`.
- Measured against 1.6.0, interleaved, three passes of 30 session starts on a base with a two-project space (158 memories, whole process): later sessions pay one more query, a P50 of 28.8 against 28.3 ms (28.1/27.6, 28.9/28.2), noise; the first session in a project takes 1 to 3 ms more (30.6 against 29.4, 32.2/29.0, 30.2/27.9) and its block grows by about 600 bytes.

### Many agents on one database
- Measured first, with a new harness (`tests/concurrencia.rs`): real `omoi` processes at once against one database of 3,000 memories with the model installed, each agent doing what its hooks and plugin do (session start, the recall of each prompt, `remember`, `omoi tool remember`, `update`, `search`, `omoi tool search`, session end with its conversation) and one of them running `omoi sync` against a local repository. At the end it checks that every write `omoi` reported as saved is there with its last content, and that `memory`, `memory_fts`, `memory_path` and `memory_vec` match. 1.6.0 already held up: with 20 agents for 30 s (about 1,000 to 1,500 processes per run) and with 5, no `database is locked`, no lost write and nothing out of step, in any of its runs (four with 20 agents, three with 5).
- What it did find: when several processes create a brand-new database at the same moment (a CI job starting from scratch), SQLite can answer `SQLITE_BUSY` without waiting at all. It happened twice in about 1,500 such openings, and once a `remember` failed with `database is locked`. Opening now starts over when that happens, for up to 5 s. The steps that bring an older database up to date when opening it (the local person, two columns of the unreleased schema 10) now check and write inside one `IMMEDIATE` transaction: two processes opening such a database at once could both try to add the same column (`duplicate column`).
- Waiting for the database is now Omoikane's own: it tries again every 0.1 to 2 ms instead of SQLite's 1, 2, 5… up to 100 ms, still up to 5 s in all. Same binary, both ways, interleaved, 20 agents: the average wait of a `remember` went from 200-300 ms to 150, that of each prompt's recall from 95-170 ms to 50, and that recall's P99 from 1.4-2.1 s to 0.8-1.2 s (its slowest went from 5.2 s, at the edge of the error, to 1.7 s). With 5 agents the wait is 1 to 8 ms either way. Most of the time with 20 agents is the machine, not the database: on the same runs, `omoi --version`, which doesn't open it, took a P50 of 55 to 450 ms.
- The two transactions of the review that read before they write (`revisar`) are now `IMMEDIATE`, like every other one that writes: deferred, a write by another agent in between made them fail at once (`BUSY_SNAPSHOT`) and the review stayed out of date. The recall of a prompt writes once instead of twice (what it injected and what counts as recalled, in one transaction).
- With one agent nothing changes: interleaved against 1.6.0, the recall of a prompt took a P50 of 42, 98 and 82 ms against 39, 76 and 98 (the machine was busy; `--version` moved as much).
- `OMOIKANE_TRAZA=1` now also says, for every command, how long it waited for another process (`TRAZA espera_base`). The full measurement is `cargo test --release --test concurrencia -- --ignored --nocapture` (`OMOI_CONC_AGENTES`, `OMOI_CONC_SEGUNDOS`, `OMOI_CONC_MEMORIAS`, `OMOI_CONC_SYNC`, and `OMOI_CONC_BIN` to measure another `omoi`); CI runs a short one, 6 agents for 5 s, that fails on any error, lost write or mismatch.

### Compatibility
- The database schema (10), the sync lines and the export format don't change, and 1.6.0 and 1.7.0 can use the same database at once. The overview only reads.
- If you had `OMOIKANE_HOME` set without `OMOIKANE_DB`, the database is now looked for in `OMOIKANE_HOME/omoikane.db` instead of `~/.omoikane/omoikane.db`, and the light model in `OMOIKANE_HOME/models` (without `OMOIKANE_MODELS`). Move them, or set `OMOIKANE_DB`.
- Preferences saved for a whole space with `remember --space` in earlier versions stayed on the computer unless preferences were shared. On the first sync with 1.7.0 they are uploaded to that space's (the client's) repository, where everyone in the space gets them. Look at them first (`omoi recent --space <space> --kind preference -n 100`) and forget the ones that shouldn't go.
- A space preference uploaded from 1.7.0 is dropped by a 1.6.0 computer that doesn't share preferences, as before; once it updates, it gets the ones changed after that.

## 1.6.0 (2026-11-06)

### Open decisions
- When the agent runs into a product or business decision that is the user's to make and can't ask right then, it saves it as a `decision` memory tagged `open` (the question, the options and what it assumed meanwhile) and keeps working. The instructions for the agent (MCP, session start and `/omoikane`) say so, and ask it to tell the user which ones it left when it finishes a long piece of work.
- Every session starts with the open decisions of the project, its space and the global ones (from the home folder, which is not a project, those of every project), newest first, one line each with a short excerpt (up to 10, and how many more there are), until the user decides: then the agent `update`s the memory with the answer and drops the `open` tag. They don't count as injected, so recall can still bring them in full.
- In `omoi serve`, the projects page lists the open decisions above the projects; writing the answer and pressing Decide appends it to the memory (with the date) and drops the `open` tag. `omoi search … --tag open` already finds them from the terminal.

### File history
- When a prompt names a file that shows up in the project's session diary (the whole path, or the file name if it has at least 5 characters, as with the gotchas found by path), the recall of that prompt adds one line about it: in how many sessions it changed and when last, which files usually change together with it, and how many gotchas mention it. For example: ``History of `src/recall.rs`: changed in 30 sessions (last 2026-11-04); it usually changes together with `src/main.rs` (18 of 30), `Cargo.toml` (12) and `src/mcp.rs` (12).`` It comes from this computer's sessions, not from a model's guess, so the agent knows how much a file moves before touching it.
- Only for files changed in at least 3 sessions. A file counts as changing together when it shares at least 3 of those sessions and at least 40 % of them; at most 3 are named, and at most 2 files per prompt. When only the name is given and several files share it, the one that changed most. Each file once per session (again after compacting), noted in a new local table, `diary_hint`, kept 7 days: it doesn't count as a recalled memory.
- Most prompts name no file, and that is decided from the text alone, without reading the database: those prompts cost nothing more. Names without an extension (`Makefile`) count only with a folder in front. Measured against 1.5.0, interleaved, on a copy of a real database with a year-sized diary (3,766 sessions, 1.7 MB of file lists): the recall of a prompt that names no file doesn't change (P50 38.5 against 40.2 ms, and 44.3 against 46.4 in a second pass), and one that names a file takes about 5 ms more (44.5 against 40.1, and 49.2 against 43.9).

### Recall from your docs
- A project that already keeps its documentation in Markdown (a `docs/` folder, an LLM wiki, an Obsidian vault) can tell Omoikane where it is: `omoi docs add <folder>` indexes it at once, `omoi docs list` shows the folders with how many files and sections are indexed, `omoi docs remove <folder>` drops one, `omoi docs refresh` brings the index up to date now and `omoi docs test "<prompt>"` shows what recall would bring for that prompt. The folders belong to the project you run it from (or the one given with `--dir`, which a relative folder is also taken from). A drive root or the home folder is refused: it would index everything in it.
- From then on, next to the memories, recall can bring the section of that documentation that fits the prompt: one at most, with its file (relative to the project) and its heading, about 700 characters, each section once per session. It comes marked as quoted from the files, data and not instructions. It is not copied into memory and the files are never written. The rule is the one memories follow (useful words, two rare ones, a share of the prompt's weight), with the bar a little higher (60 % instead of 50 %) because a section is longer than a memory. With Omoikane's own docs and 785 more Markdown files (14,548 sections), on 302 real prompts it brought something once (a borderline case); on hand-written prompts about the docs it brought the right section, and on four that weren't about them, nothing.
- The index splits each `.md` or `.markdown` file by its `#`, `##` and `###` headings (not inside code blocks; Obsidian's YAML header left out) into sections of at most 2,000 characters, in an FTS5 table with the same tokenizer as the memories. Hidden folders (`.git`, `.obsidian`), `node_modules`, `target`, `vendor` and `__pycache__` are skipped, and so are files over 2 MB and anything that isn't a regular file: symbolic links, to files or folders, aren't followed. It is brought up to date when a session starts, reading only the files whose date or size changed and dropping those that are gone, in a separate `omoi docs refresh` that the session start launches and doesn't wait for (at most once a minute per project, like the sync pull, and not while the previous one is still running); until it ends, recall uses the index as it was. An error never stops the session from starting.
- Measured against 1.5.0, interleaved, on a copy of a real database (about 200 memories): the recall of each prompt without docs doesn't change (P50 39.6 against 39.0 ms, noise; the machine was busy); with the 12 files of Omoikane's docs, +0.6 ms; with 797 files, +2.6 ms. The session start no longer walks the folders itself: with 797 files and all of them to index again, it took a P50 of 54 ms against 44 ms for 1.5.0 without docs, and the refresh it launched on the side had the index up to date 1.7 s later. Doing it inside the start, as first written, took about 0.9 s in that case. `omoi docs add` with 785 files took 3 to 7 s.

### Subagent names
- The Claude Code plugin now has subagents named "Project: task" (`Billing app: review login`, `Docs site: fix broken links`), so with several sessions open you can tell which one each agent belongs to. It was the separate `agent-names` plugin; it comes with Omoikane now. The rule goes in the description of the `Agent` tool, and a name that doesn't fit, or runs over 60 characters, is refused with the rule so the model tries again. It only checks the shape, and if the check itself fails the subagent still starts. The `agent-names` plugin is retired from the drac marketplace; if you had it installed, uninstall it (`claude plugin uninstall agent-names@drac`): with both, the model reads the rule twice.

### Compatibility
- Same as 1.5.0: the database schema (10), the sync lines and the export format don't change; `open` is a tag like any other, so older versions sync and show these memories as plain decisions.
- `diary_hint`, kept by the file history, is a new local table created with the first line given, like `diary`, which older versions ignore.
- The docs live in four new local tables (`docs_folder`, `docs_file`, `docs_fts`, `docs_seen`), created by the first `omoi docs add` and not when the database opens, which 1.0 ignores; they aren't synced or exported (the documentation has its own repository and the paths are this computer's).

## 1.5.0 (2026-11-05)

### Proposed recipes
- When a session ends, Omoikane notes the steps that worked (program and subcommand, like `cargo test` or `git tag`; read from the conversation of any of the agents it already reads, leaving out what only looks around, such as `git status` or `git log`, and, in Claude Code, what failed) in a new local table, `session_steps`, kept 90 days. When a sequence of 3 to 6 steps with at least two different programs has come up in 5 sessions, no saved recipe names all its steps and it wasn't proposed before, it is set aside (kind `recipe`) for the next session, and the agent asks the user whether to save it as a recipe. Only sequences of the session that is ending are considered, counted over the 500 most recent sessions (on a synthetic worst case of 1,000 sessions of 300 steps, the whole proposal took 127 ms; with 500, about half), so the cost doesn't grow with the history and the proposal lands in the project where it happens. One each time a session ends or compacts at most, each sequence once. From the web view, saving one creates a recipe with the numbered steps; its `when:` line is left for the user to write.
- Replaying 115 real sessions in order, it would have proposed two: `git fetch → gh issue → gh pr` and `git fetch → git tag → gh release`, in under 1 ms at each session end. Without leaving out the looking-around steps and asking for two programs, the top ones were `git add → git commit → git push`, which everybody does. The conversation is read once for the proposals and the steps: ending the largest real session takes at most 46 ms more than in 1.4.1 (681 against 635 ms, measured before the proposal got cheaper).
- The other idea planned with it, recall from other projects of the same space as a last resort, stays out: on 168 real prompts written inside a project with nothing recalled, only one would have got a memory from another project, and it was noise.

### Fixed
- On Windows the plugin couldn't install `omoi` on networks where `curl.exe` can't check whether GitHub's certificate was revoked, which is common behind company proxies (`CRYPT_E_NO_REVOCATION_CHECK`): the download failed and Omoikane didn't start, after installing and after every plugin update. It now tries again with `--ssl-revoke-best-effort`, which still checks but doesn't block when the check can't be done, and last with PowerShell. The SHA-256 is still checked.
- `omoi`'s own downloads (the model and its updates) now trust the system's certificate store on Windows, macOS and Linux, not only the Mozilla list built into the binary. Behind a company proxy that inspects HTTPS with its own root certificate (installed in the system, which is why Git and PowerShell work there), they failed on every system. Updates are still checked by their Ed25519 signature. If the system has no certificate store (a minimal Linux without `ca-certificates`), the download tries again with the built-in list, as before.

### Compatibility
- Same as 1.4.1: the database schema (10), the sync lines and the export format don't change. `session_steps` is a new local table created with the first session that has steps, which 1.0 ignores; the proposals that were made are kept in the local `meta` table (`receta_propuesta:…`).

## 1.4.1 (2026-11-04)

### Fixed
- The session diary kept the whole path of a file when the folder reached it written another way, for example with its short Windows name (`C:/Users/RUNNER~1/…`) or through a link. When the text doesn't match, it now compares the real paths on disk, so the file is listed relative to its project. It made the Windows CI fail for 1.4.0, which therefore has no Release with binaries; it is on crates.io.

### Compatibility
- Same as 1.4.0: the database schema (10), the sync lines and the export format don't change.

## 1.4.0 (2026-11-04)

Always up to date: what other computers save arrives in the middle of a session, what subagents find isn't lost, the memory says when it needs a look, and each session leaves one line in a local diary.

### Subagents
- When a Claude Code session ends or compacts, Omoikane also reads its subagents' conversations (`<session>/subagents/`) and sets aside, for the next session, the final report of each one that finished (its first 700 characters), with what it was asked to do. A subagent still working, one with something that looks like a secret, and one whose report a saved memory already covers are skipped; at most the 5 newest each time the session ends or compacts. Only the start and the end of each conversation are read: some are over 50 MB. The other signal considered, a command that fails and then works, didn't show up once in 142 real subagent conversations, so it isn't used.
- Measured against 1.3.2, interleaved: the recall of each prompt (P50 29.0 against 28.4 ms) and the start of a session (22.8 against 22.3 ms) don't change. Ending the largest real session (a 73 MB conversation with 74 subagents), with the diary and the subagents, takes 590 ms against 374 ms, within the hook's 1.2 s.

### Sync
- What other computers push now arrives in the middle of a session, not only when one starts. With each message, at most every 2 minutes, `omoi recall` starts `omoi sync --pull` on the side and doesn't wait for it: a `git fetch` and a comparison when there is nothing new, a full sync only when there is. Several sessions at once start one pull between them (the time of the last one is kept in the local `meta` table), and it never runs alongside another sync. `omoi serve` does the same on a 2-minute timer. On most messages it costs one `meta` read, too little to measure; starting the pull adds about 8-10 ms (P50) once every 2 minutes. Without sync set up nothing runs. `OMOIKANE_SYNC_EVERY` (seconds) changes the interval; `OMOIKANE_SYNC_AUTO=0` turns it off with the rest of the automatic sync.
- A pull that fails for a reason that doesn't fix itself (the repository asks for a newer version, a conflict) isn't retried every 2 minutes: only when the remote moves, when something new here is waiting to go up, or after half an hour.

### Session diary
- **One line per session**, written when the session ends or compacts (`omoi recall --session-end`, which already reads the conversation): when it started and ended, the project, the files edited or written (Edit, Write, MultiEdit and NotebookEdit in Claude Code, and the equivalents in Codex, Gemini CLI, Qwen Code, Copilot CLI, Cursor and Grok Build; relative to the repository, up to 50), the memories recall brought into it and the ones saved or changed in it, counting what its Claude Code subagents did (the 10 newest, up to 16 MB each and 32 MB in all). Not the conversation. It answers "what did I do on Tuesday in X?" or "when did we last touch `sync.rs`?", which the `status` memory can't, since it only keeps the latest picture.
- **Saved means its id came back from `remember` or `update` in that conversation**, and the memory still exists. A time window would claim what another session in the same project saved meanwhile. Preferences and pinned memories don't count as recalled: they reach every session.
- **`omoi diary`** reads it, newest first, with `--since` (a date or `7d`), `--until`, `--file <part of a path>`, `--project`, `--space`, `--all` and `--json`. The agent gets the same through `recent` with `sessions: true` (and `since`, `until`, `file`): optional parameters of an existing tool, still 8 tools.
- **Measured on 60 real Claude Code conversations**: 142 bytes per session at the median, 716 on average, 4.6 KB at most. Interleaved over 25 rounds, the diary adds no measurable time to `omoi recall --session-end` on a 1.4 MB conversation (median difference −0.4 ms, noise) and 97 ms on a 73 MB one (577 → 676 ms, within the hook's 1.2 s) (it reads the file once more; only the lines that call a file-writing tool or Omoikane are parsed). Reading its 10 newest subagents (15.8 MB) adds 22 ms more (diary write alone, median of 12 interleaved rounds: 89 → 111 ms). Nothing is added to the recall of each prompt: the table isn't even created when the database opens.
- **Local, kept for a year.** The file paths say a lot about what you do and the history belongs to each computer, so the diary is neither synced nor in `omoi export`; it is in the daily backup, which copies the whole database. At this size a year is about 2 MB at ten sessions a day; older lines are deleted when a new one is written.

### Maintenance that speaks up
- When the review piles up (20 pending items, or 10% of the memories in scope if that is more), the session starts with a three-line notice for the agent: how many there are of each kind and an instruction to look at them with `omoi review` and propose concrete actions (update, `absorb`, `forget`, `supersedes`/`compatible`, fix the links), never acting without the user's OK. Omoikane still carries no language model: the agent decides with the user. It shows once a day per database, and again the same day only if the number grows by at least 5 (or 10%). Agents without session hooks get the same text as `review_notice` in the `context` tool.
- The count at session start reads the cached review, the verdicts and the small columns of each memory, not its contents or the disk, and is cached per day until something is written. Broken paths and possible secrets stay in the full review. On the real 178-memory database the start is as fast as before (P50 22.8 ms against 22.2 ms).

### Compatibility
- The database schema stays at 10 and `lector_minimo` doesn't change: the diary is a new local table (`diary`), created with the first line, which 1.0 ignores. The sync lines and the export format don't change.
- The notice and the mid-session pull keep their state in the local `meta` table (`revision_aviso`, `revision_aviso_cifras`, `sync_traido`, `sync_intentado`, `sync_pendiente_intentado`), which doesn't travel.

## 1.3.2 (2026-11-03)

### Obsidian
- Each note is now named after its memory's title, which is what Obsidian shows as the note's title, instead of a lowercase slug with part of the id. The id goes after the title only when two notes would have the same name. The title is no longer repeated as a heading in the body.
- Global memories no longer hang from a single "global" note. For anyone who works from their home folder almost everything is global, so that note became the centre of a star that hid the real links between memories.

### Compatibility
- Same as 1.3.1: the database schema (10), the sync lines and the export format don't change. The next export renames the notes in the vault and deletes the old ones (only Omoikane's own notes).

## 1.3.1 (2026-11-03)

### Seeing the memory in Obsidian
- `omoi export --obsidian <vault>` writes one Markdown note per memory in `<vault>/omoikane/`, linked to its project, to the memories it names with `[[name]]`, to the ones it supersedes or contradicts and to the files it shares with others, so that Obsidian's graph view shows how everything connects. It is a mirror: rebuilt on every export, it only touches its own notes. By default, the personal projects and the global memories; `--space` exports one client alone, without the global ones. `--watch` keeps it up to date.

### Compatibility
- Same as 1.3.0: the database schema (10), the sync lines and the export format don't change. The Obsidian notes are a new output, not a format anything reads back. Checked against 1.0.0 on every commit.

## 1.3.0 (2026-11-02)

Fast and light again, and still understanding what you ask with other words. 1.2 brought the meaning model to every install and asked it on most prompts; a review in five parts (speed, when to use it, weight, real value, GPU) showed it paid off only with large memories and cost too much in speed and weight. 1.3 keeps the quality and takes the cost back.

### Measured
On the public benchmark ([omoikane-bench](https://github.com/marcmayol/omoikane-bench), 250 questions written blind), interleaved, with 3,000 / 10,000 memories:

| | 1.3 | 1.2 | 1.3 with the meaning model off | engram 3.6 (best mode) |
|---|---|---|---|---|
| recall, P50 | **32 / 54 ms** | 99 / 126 ms | 27 / 31 ms | – |
| search, P50 | **64 / 64 ms** | 101 / 126 ms | 31 / 34 ms | 236 / 347 ms |
| right memory first, paraphrases (search) | 55 % / 53 % | 55 % / 53 % | 9 % / 8 % | 6 % / 4 % |
| right memory first, questions (search) | 89 % / 88 % | 89 % / 88 % | 85 % / 84 % | 71 % / 71 % |
| recall injects something on a question with no answer | 7.5 % / 15 % | 7.5 % / 15 % | 40 % / 70 % | – |
| recall brings the right memory, questions | 94 % / 94 % | 94 % / 94 % | 93 % / 94 % | – |
| computing the meaning vectors | 453 / 1,827 s | 730 / 2,437 s | – | – |

What got worse: recall no longer adds by itself a memory the words didn't find, so paraphrases in recall go from 29 % to 9-11 % (search still finds 53-55 % of them first). The first recall right after the meaning service starts takes ~0.2 s.

### Faster for everyone
- **Every `omoi` call no longer flushes the database's write-ahead log on exit.** On Windows, when the recall hook was the last process with the database open (the normal case without the meaning service), SQLite moved the log into the database and synced the disk on every prompt: recall took ~170-270 ms in 1.1 and 1.2 with the meaning model off, not the ~35 ms measured with another connection open. Now it takes ~30 ms. About once every ~240 prompts one call pays the flush (~0.5 s). The `-wal` file (up to ~4 MB) stays next to the database: Omoikane's own copies (`omoi backup`, the daily copy, `omoi export`) are unaffected, but a database copied by hand needs its `-wal` too.
- Faster process start on Windows (17.4 → 13.5 ms): the home folder comes from the standard library, and the TUI's libraries load only when the TUI opens.

### The meaning model: light and only when it helps
- **The meaning model comes in with the size of the memory.** Below 1,000 memories (every memory in the database counts, so the decision is the same from every folder) Omoikane works as 1.1 did: words and the light model, no download, no service. Once the memory passes 1,000, Omoikane downloads the model in the background, indexes the memories and says so once at the start of a session. The Claude Code plugin and the one-line installers no longer download it. `omoi model semantic --on` uses it with a small memory, `--off` never, `--auto` lets the size decide again; `omoi model semantic` and `omoi doctor` say whether it is in use and why. 1,000 comes from the measurements: with 1,000 memories the model already finds 36 % of paraphrases first in search (8 % without it) and takes recall noise from 30 % to 22.5 %; below that, recall doesn't gain and the download isn't worth it (`OMOIKANE_SEMANTIC_THRESHOLD` changes it).
- **Recall asks the meaning model only to keep quiet.** Only when the words already brought something, on a prompt of more than five words and at most 300 characters, and only to take out what isn't among the three memories closest in meaning. On 1,161 of the author's real prompts it is asked on 60; on the public benchmark (3,000 memories) recall injects something on 7.5 % of the questions with no answer, as in 1.2. It no longer adds a memory the words didn't find (search still finds paraphrases).
- **Preferences are never vetoed**: a preference applies without resembling the prompt ("open it in Creality Print" and the preference of not using Orca). On the author's real prompts, a quarter of the vetoed preferences were useful.
- **When the model doesn't answer in time**, recall goes on with words, as before, but it is no longer silent: `omoi doctor` shows how many times recall asked it and how many got no answer, with the last one. `OMOIKANE_TRAZA=1` writes every step of a recall to stderr.
- **Search can come back empty.** What only meaning brings, with no word in common with the query, needs a similarity of 0.35. With 1.2 a search never came back empty and often showed something unrelated first; now that happens on 6 % of the author's prompts instead of 21 %, with the same first results on the public benchmark.
- **A procedure needs more than one loose word.** "publish a post on LinkedIn" brought "Publish a release" because `publish` is in its `when:` line. Now a single word only brings it inside its own project, or with a word of its title or project ("publish the release", "publish omoikane"); a phrase from `when:` or two of its words still bring it anywhere.
- **Its own engine**: the meaning model runs on f16 weights read straight from the mapped file (AVX2), with a compiled tokenizer. The service is ready in ~26 ms instead of 1.4 s and uses ~13 MB of its own RAM instead of ~490 MB; a vector takes ~21 ms instead of ~42 (16 tokens). Same vectors (cosine 1.000000). CPUs without AVX2 keep using candle.
- **Indexing in batches that give way**: vectors are computed in batches that pause while a query is being served (a query during indexing: 66 ms instead of ~300), up to 3.8× faster per memory, with a RAM cap: `omoi model semantic --ram <MB>` (default 128 MB, minimum 80; `OMOIKANE_SEMANTIC_RAM` overrides it), shown by `omoi model semantic` and `omoi doctor`.
- **The service compares the vectors itself**, kept in memory, so recall and search no longer read every vector from the database on each call.

### Compatibility
- Same as 1.2.0: the database schema (10), the sync lines and the export format don't change. The recall counters go in the local `meta` table, and the compiled tokenizer (`tokenizer.omoi`) is generated locally next to the model; the published model files and their SHA-256 don't change. Checked against 1.0.0 on every commit.

## 1.2.0 (2026-11-01)

Omoikane understands what you ask with other words. Until 1.1, a question that didn't share words with the memory that answers it almost never found it, and engram 3.6 doesn't either. 1.2 adds a second model, EmbeddingGemma 300M, for meaning.

### Measured
On the public benchmark ([omoikane-bench](https://github.com/marcmayol/omoikane-bench), 250 questions written blind), with 3,000 and 10,000 memories:

| | 1.2 | 1.2 with the meaning model off (as 1.1) | engram 3.6, best mode |
|---|---|---|---|
| right memory first, paraphrases | **55 % / 53 %** | 9 % / 8 % | 6 % / 4 % |
| right memory first, questions | **89 % / 88 %** | 85 % / 84 % | 71 % / 71 % |
| right memory first, keywords | 96 % / 94 % | 94 % / 92 % | 88 % / 86 % |
| recall injects something on a question with no answer | **8 % / 15 %** | 40 % / 70 % | – |
| recall brings the right memory, questions | 94 % / 94 % | 93 % / 94 % | – |
| search, P50 | 101 / 126 ms | 37 / 45 ms | 205 / 337 ms |
| recall, P50 | 99 / 126 ms | 35 / 40 ms | – |

What it costs: about 60 ms more per search and per prompt, about 450 MB on disk, and computing each memory's vector once (about 0.25 s each on 8 CPU threads, in the background). Not measured yet at 100,000 memories.

### Added
- **The meaning model**: EmbeddingGemma 300M in pure Rust (candle), with no ONNX Runtime or anything else outside the binary. It gives the same vectors as Google's reference (cosine 0.999 or better, up to 2,048 tokens). It is installed in a compact format of about 450 MB, with its 262,144-token table in int8 read straight from disk, so it loads in 1.4 s. `omoi model install --semantic` downloads it (from [Hugging Face](https://huggingface.co/natzx94/embeddinggemma-300m-omoikane), or this repo's Releases if that fails) and checks every file's SHA-256; `--from` takes it from a folder, and converts the one sentence-transformers leaves. The Claude Code plugin and the one-line installers download it by themselves, once, unless it was turned off.
- **It lives in a service of its own**, one per user, that Omoikane starts by itself the first time it's needed (`127.0.0.1` only, with a token) and that closes after 30 minutes without use. Whoever asks never waits for it to load: without the service, everything works as in 1.1.
- **Vectors computed in the background**: the service computes the vectors a database is missing when it is idle, and every change tells it. They live in a local table that is not synced and that 1.0 ignores. `omoi doctor` shows how many memories have one.
- **Search joins words and meaning**: each candidate scores its similarity in meaning plus a share of its word score. Short queries (five words or fewer, keywords) give words more weight.
- **Recall uses meaning to keep quiet**: what the words bring stays out if it isn't among the three memories closest in meaning, and the closest one comes in when it clearly stands out from the second. Both thresholds are relative (a position and a lead, not a fixed similarity), because similarity changes with the language and with how people write: in the benchmark the right memory sits around 0.7; in real prompts in Spanish with typos, around 0.35. Checked by hand on the author's last 300 real prompts: 5 change, all for the better.
- `omoi model semantic --off` turns it off and closes its service: Omoikane goes back to the light model only, as in 1.1, faster and with less memory. `--on` turns it back on; `OMOIKANE_SEMANTIC=0` overrides both.

### Compatibility
- Same as 1.1.0: the database schema (10), the sync lines and the export format don't change. The meaning vectors are a new local table (`memory_sig`) that 1.0 opens and ignores; checked against 1.0.0 on every commit.

## 1.1.0 (2026-10-31)

Recall and search that hold up at 100,000 memories, a compatibility test against 1.0.0 on every commit, a public benchmark, and what a project needs to take contributions.

### Faster
- **Recall and search that scale**, measured interleaved, P50 of the whole process (about 12 ms of it is starting the process): recall 55 / 104 / 742 ms → 38 / 37 / 81 ms with 3,000 / 10,000 / 100,000 memories; search 54 / 100 / 691 → 40 / 46 / 112 ms; session start 48 / 92 / 731 → 27 / 27 / 67 ms. Same results as 1.0.0 on the benchmark corpus at the three scales, on the 250 blind questions and on a copy of a real database.
- How: indexes and a count per project and space so a scope is not read row by row, and an int8 copy of every vector with its exact error bound, so search only scores in full the memories that can make the top. They are local objects that 1.0 ignores and keeps up to date when it writes.

### Added
- **Public benchmark** in its own repository, [omoikane-bench](https://github.com/marcmayol/omoikane-bench): 3,000 notes from 15 openly licensed projects, 250 questions written blind, Omoikane and engram installed from their Releases, top-1/top-3, P50/P95, recall tokens per message, at 3,000, 10,000 and 100,000 memories. One command reproduces it.
- **CONTRIBUTING.md** (English and Spanish), **SECURITY.md** with private reporting, and issue and pull request templates.

### Changed
- README cut to a third; everything else moved to `docs/`, one file per topic, in English and Spanish.
- When another computer says it moved a project to a space and the general repository doesn't say which one, the notice now explains what to do if the project does belong to a client: join that client's repository first (`omoi sync join --space <space> <code>`), and then the notice names the space and the command that moves it.

### Checked on every commit
- **Compatibility CI** (`compat.yml`, on Windows, Linux and macOS): downloads `omoi` 1.0.0 from the v1.0.0 Release, checks its SHA-256, and runs `tests/compat.rs` against the binary of the commit. Both sync through the same git repository in both directions (memories, history, verdicts with their reason, trash and forgets, a space with its own repository, key rotation), 1.0 keeps and passes on fields and line types it doesn't know, 1.0 opens a database made by the commit, and each one imports the other's `omoi export`. The release workflow runs it too, before publishing.
- Release notes come from this file.
- Two tests that failed on some machines (directory order on ext4 vs NTFS, and an id prefix that wasn't unique on fast machines).

### Compatibility
- Same as 1.0.0: the database (schema 10), the sync lines and the `omoi export` format (version 2) don't change. Syncs and swaps exports with any 1.x, both ways; checked against 1.0.0 on every commit.

## 1.0.0 (2026-10-31)

The 0.20.0 work, published as 1.0. From here on, 1.x only adds.

### Added
- **Projects and spaces.** Schema 10 turns projects and spaces into tables. A project is found by its git `origin`, so clones, worktrees and moved folders are the same project. A space groups the projects of one client: from a project you see its memories, its space's and the global ones, never another space's. `omoi space add|list|assign|unassign` and `omoi projects list|move|merge|keep|prune`; `project` and `space` in `search`, `recent` and `remember`, still with the same 8 tools.
- **One repository per client.** `omoi sync init --space acme <url>` gives a space its own repository and key: it carries only that space, and accepts only lines of that space. Nothing of one client reaches another's repository or the general one.
- **Key rotation.** `omoi sync rotate` creates a new key, re-encrypts this computer's lines and gives the new code. Whoever only has the old key stops reading what's new. Every line carries the id of its key (`k2:…`; lines without a prefix are key 1, as before).
- **The space of a project travels.** Moving a project to a space reaches the other computers of the same person. A line that says a project is in another space never moves it: at most it pauses the project until the person decides here (`omoi projects keep` or `omoi projects move`), with a notice in `sync`, `sync status`, `doctor`, `review`, the web view and the session start.
- **Formats that 1.x can extend.** Unknown fields in sync and export lines are kept and passed on (`extra`). Lines of an unknown type are counted apart from unreadable ones and read again after updating. A repository can ask for a newer reader (`omoikane.format`, encrypted), and an older `omoi` then leaves it alone. A database records the oldest schema that can read it (`lector_minimo`).
- Copy of the database before migrating (`VACUUM INTO`), and an incremental detector of outdated memories.

### Fixed
- Isolation between people and between clients, reviewed in five independent passes with scripts against local git repositories: `dir:` keys always carry the person, forgets, verdicts and exports never cross from one space to another, a `project` line from someone else only pauses, and a client's repository can't claim a project in advance.

### Measured
- With 50,000 memories: `remember` 294 ms (0.19.1: 2.5 s), `update` 263 ms, `omoi sync` with no changes 1.98 s (same as 0.19.1), recall as before. 219 tests on Windows and Linux.

### Compatibility
- **Promise for all of 1.x:** the database, the sync lines and the `omoi export` format don't change in any way that breaks a 1.0. A 1.0 keeps and passes on what it doesn't understand, steps away from a repository that asks for a newer reader, and opens a database that only adds. The 8 tools and their parameters don't break. Anything a 1.0 would misread (a new scope, for example) raises `min_reader` and needs the newer version.
- **Opens** databases from any 0.x and migrates them to schema 10 on first use, after a copy. **Reads** sync repositories and exports from 0.x.
- **0.19.x and earlier** can't open a 1.0 database, and don't understand a repository after a key rotation or one with spaces. Update every computer that shares a repository.
