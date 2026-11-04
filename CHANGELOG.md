# Changelog

What changes in each version of Omoikane. The release workflow publishes the section of each version as the notes of its GitHub Release, and won't publish a version that has no section here. Every section ends with **Compatibility**: which versions it understands and which ones understand it.

How it works is in [docs/](docs/).

## Unreleased

### Maintenance that speaks up
- When the review piles up (20 pending items, or 10% of the memories in scope if that is more), the session starts with a three-line notice for the agent: how many there are of each kind and an instruction to look at them with `omoi review` and propose concrete actions (update, `absorb`, `forget`, `supersedes`/`compatible`, fix the links), never acting without the user's OK. Omoikane still carries no language model: the agent decides with the user. It shows once a day per database, and again the same day only if the number grows by at least 5 (or 10%). Agents without session hooks get the same text as `review_notice` in the `context` tool.
- The count at session start reads the cached review, the verdicts and the small columns of each memory, not its contents or the disk, and is cached per day until something is written. Broken paths and possible secrets stay in the full review. On the real 178-memory database the start is as fast as before (P50 22.8 ms against 22.2 ms).

### Compatibility
- The database schema (10), the sync lines and the export format don't change. The notice keeps its state in the local `meta` table (`revision_aviso`, `revision_aviso_cifras`), which doesn't travel.

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
