# Changelog

What changes in each version of Omoikane. The release workflow publishes the section of each version as the notes of its GitHub Release, and won't publish a version that has no section here. Every section ends with **Compatibility**: which versions it understands and which ones understand it.

The full design and every decision are in [PLAN.md](PLAN.md) (Spanish).

## Unreleased

## Unreleased

### Added
- **The meaning model, first piece** (EmbeddingGemma 300M, for questions asked with other words; see PLAN.md 9.2). It runs in pure Rust with candle, with no ONNX Runtime or anything else outside the binary, and gives the same vectors as Google's reference (cosine 1.00000). Loading it takes about 2 s, so it lives in a service of its own that Omoikane starts by itself the first time it's needed (`127.0.0.1` only, with a token) and that closes after 30 minutes without use. Whoever asks never waits for it to load: without the service, everything works as in 1.1. Search and recall don't use it yet.
- `omoi model semantic --off` turns it off and closes its service: Omoikane goes back to the light model only, as in 1.1, faster and with less memory. `--on` turns it back on; `OMOIKANE_SEMANTIC=0` overrides both.

### Compatibility
- Same as 1.1.0: the database, the sync lines and the export format don't change.

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
