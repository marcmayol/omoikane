# Changelog

What changes in each version of Omoikane. The release workflow publishes the section of each version as the notes of its GitHub Release, and won't publish a version that has no section here. Every section ends with **Compatibility**: which versions it understands and which ones understand it.

The full design and every decision are in [PLAN.md](PLAN.md) (Spanish).

## Unreleased

Omoikane is light again until the memory needs more. In 1.2 the meaning model (EmbeddingGemma, ~450 MB) came with every install and was asked on most prompts, taking recall from ~35 ms to ~100 ms. With a small memory it barely helped: below about 1,000 memories recall injected the same noise with it and without it.

- **The meaning model comes in with the size of the memory.** Below 1,500 memories (every memory in the database counts, so the decision is the same from every folder) Omoikane works as 1.1 did: words and the light model, no download, no service. Once the memory passes 1,500, Omoikane downloads the model in the background, indexes the memories and says so once at the start of a session. The Claude Code plugin and the one-line installers no longer download it. `omoi model semantic --on` uses it with a small memory, `--off` never, `--auto` lets the size decide again; `omoi model semantic` and `omoi doctor` say whether it is in use and why. 1,500 is provisional: it will be calibrated with the final measurements, between 1,000 and 3,000 (`OMOIKANE_SEMANTIC_THRESHOLD` changes it).
- **Recall asks the meaning model only to keep quiet.** Only when the words already brought something, on a prompt of more than five words and at most 300 characters, and only to take out what isn't among the three memories closest in meaning. On 1,161 of the author's real prompts it is asked on 60; on the public benchmark (3,000 memories) recall injects something on 7.5 % of the questions with no answer, as in 1.2. It no longer adds a memory the words didn't find (search still finds paraphrases).
- **Preferences are never vetoed**: a preference applies without resembling the prompt ("open it in Creality Print" and the preference of not using Orca). On the author's real prompts, a quarter of the vetoed preferences were useful.
- **When the model doesn't answer in time**, recall goes on with words, as before, but it is no longer silent: `omoi doctor` shows how many times recall asked it and how many got no answer, with the last one. `OMOIKANE_TRAZA=1` writes every step of a recall to stderr.
- **Search can come back empty.** What only meaning brings, with no word in common with the query, needs a similarity of 0.35. With 1.2 a search never came back empty and often showed something unrelated first; now that happens on 6 % of the author's prompts instead of 21 %, with the same first results on the public benchmark.
- **A procedure needs more than one loose word.** "publish a post on LinkedIn" brought "Publish a release" because `publish` is in its `when:` line. Now a single word only brings it inside its own project, or with a word of its title or project ("publish the release", "publish omoikane"); a phrase from `when:` or two of its words still bring it anywhere.

**Compatibility**: same as 1.2.0. The database schema, the sync lines and the export format don't change; the recall counters go in the local `meta` table.


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
