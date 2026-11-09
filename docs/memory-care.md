# Keeping the memory right, and never losing it

[Español](memory-care.es.md) · [Back to the README](../README.md)

## It looks after itself

Omoikane notices when a memory has fallen behind: a newer one on the same subject says something different (a higher version of the same thing, or done what was pending), a `status` memory the project has moved past, paths that no longer exist, memories never used in a month, and notes too long for recall. When one of those reaches the agent it comes marked `⚠ may be outdated: newer <id> …`, and saving a memory that makes another outdated says so right away, so the agent fixes it on the spot. The **Review** tab of the web view lists them with the two memories side by side, *forget the old one* and *it's fine as it is*; `omoi review` does the same from the terminal. No model involved: words, dates and versions.

When a lot has piled up you don't need to open the tab: at the start of a session the agent gets a short notice with how many there are of each kind, and is told to look at them and propose what to do (update, merge with `absorb`, forget, mark `supersedes` or `compatible`, fix the links), changing nothing until you say yes. It comes from 20 pending items, or 10% of the memories if there are more, once a day, and again the same day only if the number grows by a fair amount. Agents without hooks get it as `review_notice` in the `context` tool. To keep the start fast, the notice counts without reading the contents or the disk: broken paths and possible secrets only show in the full review.

## Verdicts

What Omoikane can't know by itself, the agent says, with no new tools: when saving or changing a memory it can pass `supersedes` (this one replaces those), `contradicts` (they say different things and it's unclear which holds) or `compatible` (a warning paired them and both hold). The verdict is kept, travels with sync and shows wherever the memories show: in the recall (`⚠ superseded by …`), in `search`, in `context` and in Review. It holds while what it judged doesn't change, and for a pair the newest one wins. "It's fine as it is" is a verdict too, so what you approve on one machine holds on the others.

**Why and who.** Each verdict keeps its reason and who gave it (the sync person, or the agent and its client), and both show in the recall, in `search` and in Review. If A is superseded by B and B by C, A's note leads straight to C.

## Fixing a piece and merging

`update` takes `replace` (changes a text that must appear exactly once; otherwise nothing changes), `append` (adds at the end, for a procedure's new step) and `absorb` (merges several memories into one: the agent writes the merged text, and the absorbed ones go to the trash with their history hung on the one that stays and their tags and links moved to it). The Review tab has a **Merge** button to do it by hand.

## Duplicated projects

Sometimes one project ends up split in two: `Omoikane` and `omoikane`, a folder renamed without an `origin` (`Narrador`, now gone, and `Aloud`, which mentions it), or a "project" that is just the folder a file was uploaded to (`.claude\uploads\<uuid>`, `%TEMP%`). The review spots them and proposes them, but never merges anything by itself, because a `merge` reaches every computer through sync. At the start of a session, once a day, the agent gets each case with the command already written (`omoi projects merge A B`) and asks you before running it. If they aren't the same, `omoi projects distinct A B` keeps it from coming back. `omoi review`, `context` (`projects_review`) and the Review tab show them too.

It counts as a duplicate when the name is the same ignoring case, accents, hyphens, underscores, spaces and dots (`social-publish` = `Social Publish`). One letter apart (`Omoikane`/`Omoikae`, from 6 letters) only counts with another sign: one of the folders no longer exists, the newer one has hardly any memories, or both share their `origin`. A look-alike name alone is not enough: `Tokimichi` and `tokimichi-web` are different projects.

## Review date

Each kind has its own: decisions and procedures after 120 days untouched, facts and gotchas after 180, preferences never, counting from its last change or the last time the agent got it (what gets used and nobody corrects is reviewed). When it comes, the memory shows in Review and in `search` with "not reviewed in N days"; not in the recall, which only carries what says a memory may be wrong. "It's fine as it is" restarts the clock.

Some memories stop holding on a known day: a token that expires, an offer, a plan with a date. Save them with the tag `review:YYYY-MM-DD` (`omoi remember … --tag review:2026-11-25`, or in `tags` from the MCP) and that day the review flags them as *review date reached*, in Review, in `search` and also in the recall (it may no longer hold). The notice at the start of the session comes even with a single one. Touching the memory or "it's fine as it is" clears it; a tag that isn't a date is refused when saving.

## Code that changed under it

A memory that talks about files (its `paths`, and the ones it names with their folder, like `src/sync.rs`) can fall behind when those files change a lot after it was written. The review flags it as *code changed under them* (`code_drift`) when at least half of the lines of those files changed since it was written or last checked, according to git (and at least 20 lines), or, for files outside git, when they were edited in 5 different sessions of the [session diary](#session-diary). What changes in the first 6 hours doesn't count: saving the memory and then committing the work it describes is the normal thing. It only proposes a check and never changes the memory. "It's fine as it is" (`omoi review --dismiss <id> --kind code_drift`) marks it checked, and it comes back only if the files change a lot again. A file that no longer exists is a broken path. Paths that aren't on this computer (`\\server\share\…`, `//host/…`) are never looked at, here or for broken paths: on Windows, just checking one opens a network connection. The first time, `context` doesn't wait for it (it can take seconds with git): it is worked out in the background and shows up in the next session; each `git log` gets 10 seconds at most.

It never runs in each prompt's recall. It is worked out once a day in the background (one `git log` per repository, only with the files modified since), the result is kept until the memory changes, and the notice at the start of the session only reads what is kept.

## History and trash

Changing a memory keeps the previous version with the reason, and `omoi history` shows them all.

**Trash.** Forgetting a memory (`forget`, from the agent, the CLI or the web view) sends it to the trash with its history, and a forget doesn't come back from another computer when you sync either. `omoi restore <id>` brings it back, history included, and wins on the other computers at the next sync: a `forget` made by mistake on one machine is undone from any of them. The agent does it with `update` and `restore: true`, with no new tool; the Review tab has a **Trash** group with a button for each one. After 30 days (`OMOIKANE_TRASH_DAYS`; 0 = never) they are deleted for good; `forget --purge` skips the trash and `omoi trash --empty` empties it.

## Session diary

Besides the memories, Omoikane keeps one line per session: when it started and ended, in which project, which files were edited or written (relative to the repository, up to 50), which memories reached it through recall and which ones were saved or changed in it. It doesn't keep the conversation. It answers what the memories can't, since the `status` memory only keeps the latest picture: "what did I do on Tuesday in the website?" or "when did we last touch `sync.rs`?".

`omoi diary` lists them newest first, with `--since 2026-11-03` (or `7d`), `--until`, `--file sync.rs`, `--project`, `--all` and `--json`. Dates are in UTC, and a session counts for every day it spans. The agent asks for them with `recent` and `sessions: true` (with `since`, `until` and `file`), with no new tool.

The diary also feeds the recall: when a prompt names a file changed in at least 3 sessions of the project, the agent gets one line with how many sessions changed it and when last, the files that usually change with it (in at least 3 of those sessions and 40 % of them) and how many gotchas mention it, once per session. For example: ``History of `src/recall.rs`: changed in 30 sessions (last 2026-11-04); it usually changes together with `src/main.rs` (18 of 30), `Cargo.toml` (12) and `src/mcp.rs` (12).``

`omoi file-history` gives the same numbers in full, for code: a risk gate that decides from a diff whether a change ships by itself or goes to a person reads them, and no agent decides anything. It takes paths relative to the repository or absolute, or one per line on standard input (the output of `git diff --name-only`), and answers for the project of the current folder or of `--dir`. It has no thresholds: per file, how many sessions changed it and when last, up to 10 files that changed in the same sessions with how many, and the gotchas (of the project, its space and global) whose paths name it. It also counts sessions opened in another folder, such as your home folder, that edited files of this repository (those are stored with the absolute path), but only their files inside it. The top `sessions` is how many sessions touched files of the project, to put the rest in proportion. A file the diary has never seen comes with `sessions: 0` and `last: null`.

```sh
git diff --name-only origin/master... | omoi file-history --json
```

```json
{
  "project": "/home/ana/work/acme-web",
  "sessions": 48,
  "files": [
    {
      "file": "src/sync.rs",
      "sessions": 14,
      "last": "2026-11-04",
      "changes_with": [
        { "file": "tests/sync.rs", "sessions": 9 },
        { "file": "CHANGELOG.md", "sessions": 7 }
      ],
      "gotchas": 2,
      "gotcha_ids": ["01M64WZATAVWKSYRMVG4FCV64V", "01M666RGFB3JWVYHTPYVPFD282"]
    }
  ]
}
```

The risk gate of Kumi, an agent factory that uses Omoikane as its memory, might read it like this: a file with gotchas raises the level, and so does a diff that leaves out a file which nearly always changes with one it touches.

```python
diff = subprocess.run(["git", "diff", "--name-only", "origin/master..."], capture_output=True, text=True).stdout
h = json.loads(subprocess.run(["omoi", "file-history", "--json"], input=diff, capture_output=True, text=True).stdout)
touched = set(diff.split())
level = 0
for f in h["files"]:
    if f["gotchas"]:
        level = max(level, 1)
    if any(j["file"] not in touched and j["sessions"] >= 0.6 * f["sessions"] >= 3 for j in f["changes_with"]):
        level = max(level, 1)
```

It only reads, and like the diary it knows only this computer's sessions.

It is written when the session ends or compacts, by the same hook that sets candidates aside, and never in the recall of each prompt. When a session compacts, its line is completed, not repeated. A memory counts as saved when `remember` or `update` returned its id in that conversation. Looking at which memories changed between the start and the end of the session would claim the ones another session in the same project was saving at the same time. Preferences and pinned memories don't count as recalled: they reach every session.

It stays on this computer: it travels neither with sync nor with `omoi export`, because the paths of your files say a lot about what you do and the history belongs to each computer. It is in the daily copy, which is the whole database. It weighs little: on real conversations, 142 bytes per session at the median and 716 on average (about 2 MB a year at ten sessions a day). Lines are kept for a year.

## Faithful copy

`omoi export` writes a JSONL with everything needed to rebuild the database: each memory with its history, clock stamp and author, the forget marks, the verdicts and the trash, with an end line carrying the count and the SHA-256 (a cut or tampered file is not imported). `omoi import` puts it back with the same rules as sync: same ids, dates and stamps, newest wins and what was forgotten doesn't come back, so repeating it changes nothing and it serves both to restore a database and to merge two. Run inside a coding agent's shell, the memories come in without their "verified by a person" mark (anyone can write the file) and the import says so. Both stream, one line in memory at a time: 50,000 memories export and import without loading the RAM. With `--encrypt`, every line is encrypted with the sync key (a client's project, with its space's key; with `--all`, each line with its own space's key), to keep the copy anywhere. What is in the trash goes with its project and its space, and with its verdicts: restored on another computer, it goes back where it was, `superseded` included. Files in the previous format and engram's still import, and so does one from a newer omoi: what this version doesn't understand is skipped and counted, and the fields it doesn't know are kept and come out again in the next export.

## Seeing it in Obsidian

`omoi export --obsidian <vault>` writes one Markdown note per memory in `<vault>/omoikane/`, to look at it in Obsidian's graph view. Each note is named after its memory, for example `We use UUID.md`, because that is what Obsidian shows as the title; if two would have the same name, the end of their id goes after it. A project's memories go in its folder and link to the project's note. Global ones don't. With almost everything global, one shared note for them turned the graph into a huge star that hid the links that really matter. Each memory also links to the ones it names with `[[name]]`, to the ones it supersedes or contradicts, and to a note for each file cited twice or more. The kind goes as a tag, and if the vault has no graph settings yet, it gets one colour per kind. It is a mirror of the database: every export rebuilds it, writes only what changed and deletes what no longer exists. A note you edited there and haven't imported yet is left alone (see below), and anything without Omoikane's mark in its header stays where it is. By default it exports the personal projects and the global memories; `--space acme` exports only that client, without the global ones, and is best sent to another vault. With `--watch` it stays open and exports again when the database changes (it checks every `--interval` seconds, 60 by default). With 173 memories it takes under a second.

### Bringing edits back

`omoi import --obsidian <vault>` brings back what you changed in the vault (with `--space acme` for a client's vault, the space it was exported from; `--dry-run` only says what it would do). Afterwards it exports again, so the vault matches the database.

- **An edited note updates its memory**, with history (`omoi history <id>` shows the previous version, with the reason `obsidian import`). What comes back: the text above the line (lines you didn't touch keep their exact original, links included; in the ones you changed, the export's `[[note|name]]` goes back to `[[name]]`, a `[[note]]` of another memory becomes `[[<its id>]]`, and the `\#` the export adds goes back to `#`), the title (the note's name, which is what Obsidian changes when you retitle it, or `aliases`), `kind`, the plain tags and `paths`. Changing the text of a memory a person verified removes the verification, as `update` does, and a trade memory's change goes through the same leak check as `update`.
- **A note nobody touched does nothing**: each note carries in its header a fingerprint of what can be edited as it was exported (`base`). Exporting and importing without changing anything changes nothing, and importing twice is the same as once. Obsidian rewriting the header its own way when you touch a property doesn't count as an edit.
- **A new note** in a project's folder of the vault (or with a `project:` that is one of the vault's projects) becomes a new memory of that project (in `_<space>/`, of that space) if it says its kind: `kind: decision`, or a tag that is a kind. Without one, or outside a project's folder, it isn't imported and the import says why; nothing is guessed. Its file becomes Omoikane's copy of the memory. Importing it again updates the same memory (it is saved with the key `obsidian:<its path>`) instead of making another.
- **The group and file notes**, anything outside `omoikane/` and notes you delete do nothing. Deleting a note doesn't forget its memory (`omoi forget` does); the next export writes it again.
- **Conflicts**: a note edited in Obsidian whose memory also changed in Omoikane after the export (by hand, an agent or a sync) isn't applied, and Omoikane's version isn't touched either. The import lists them, and the export doesn't overwrite them (with `--watch` too). Compare them, then import again with `--prefer obsidian` (the note wins; Omoikane's version stays in the history) or `--prefer omoikane` (the note is written again from the memory and its change is lost). Two edited copies of the same memory are a conflict as well.
- **Nothing from the vault grants anything.** A vault is plain text anyone with access to the folder can edit, so only text comes back: a note never moves its memory to another project or space, never makes it global, pinned or verified, and tags with `:` (`trade:`, `topic:`, `review:`…) are neither added nor removed from the vault. A new note never becomes global (one in `_global/` is skipped: save it with `omoi remember --global`) and comes in unpinned. Everything goes through the secret redaction, as any write.
- **Only notes this database exported touch a memory.** A memory's id is not a secret (it travels in syncs, OKF bundles and other vaults), so every exported note is signed (`sig`, an HMAC with the database's own key, the one OKF uses, which never leaves this computer) over which memory it is, the scope it was exported from, the memory's version and the fingerprint. A note with someone else's id, one from another database or from a version before 1.9.0, or one whose `omoikane:` was pointed at another memory doesn't touch any memory; the import says so. Export again to refresh old notes, or remove the `omoikane:` line to bring one in as a new memory.

## OKF bundles

`omoi export --okf <folder>` writes the memory as an [OKF v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md) bundle (Open Knowledge Format, the one okf-agent-memory uses): one Markdown file with a YAML header per memory in `<group>/<kind>/<title>.md`, an `index.md` in every folder (the root one says `okf_version: "0.2"`), a `log.md` by date and an `overview.md` per project that links to its memories. The kind becomes `type` (`Decision`, `Gotcha`…), the paths `code_refs`, `review:YYYY-MM-DD` becomes `stale_after`, a memory you verified (`omoi verify` or the web view, with its text unchanged since) a `verified` entry by `human:<you>` with the date you verified it (a space in the name goes as `%20`), and a superseded memory `status: deprecated`, with a link to the one that replaced it. The rest (id, key, pin, dates, data) goes in an `omoikane:` key. Like the Obsidian export, it is a mirror with the same scope: personal projects and global memories, or only `--space acme`, and it won't write into a folder that holds someone else's bundle. It passes `okf validate --strict`.

`omoi import --okf <folder>` reads a bundle (or a repository that has one in `knowledge/`) into this folder's project, into a space with `--space`, or into the global memories with `--global`. From your home folder it refuses without one of those (or `--dir <project>`): everything would become global. Into a client's space (`--space`, whose sync repository other people share), it warns when the bundle would bring pinned or verified memories or Omoikane memories from outside that space (personal ones or another client's) and imports them only with `--yes` (in a terminal it asks). Each concept is saved by its key, so importing it again updates it instead of duplicating it. What was exported from this same database comes back with its own id, key and verdicts, to the memory it came from, wherever that memory is now (if you moved a global memory into a project after exporting it, it is updated there, not duplicated), except a memory you forgot here (in the trash or not): it doesn't come back and the import counts it, as sync does with a forget (`omoi restore` brings it back). Any other bundle, also one from another Omoikane database, is someone else's: its concepts get new ids under the key `okf:<concept path>` in the destination you asked for, and nothing in their `omoikane:` key (id, key, `project_key`) decides which memory of yours they update or which project they go to.

Only a bundle exported from this same database is trusted with what gives a concept weight: being global, being pinned and a person's verification. Each concept carries a signature (`omoikane.sig`, an HMAC with a random key that never leaves this database) of everything the import applies from it: its path, id, key, project, scope, pin, kind, title and content, paths, tags (also the ones `status` and `stale_after` become), data, dates, source, its `superseded_by` and `contradicts` and its verification. From any other bundle (someone else's, one you exported on another computer, or yours with anything edited) the concepts go to the destination as new memories, unpinned, and the import says how many asked for more: editing any field of a concept in the Markdown makes it someone else's concept, so a copied concept can't change the tags, paths, data or verdicts of your memory. A verdict never joins a concept of this database with one without its signature. A concept of this database older than its memory here (its `updated` is earlier: a bundle exported before you changed the memory) leaves that memory as it is, with no text, verification or verdicts from the bundle, and the import counts it (`skipped_older`). A concept that would become a trade memory (global with `trade:`) goes through the same leak check as `remember --trade`, without `allow_leaks`: if it names a project or a client, that concept fails and nothing of it is written. Besides the 1 MB per concept, `index.md` has the same limits (no symbolic links, 1 MB), and a folder with over 20,000 files and folders, 50 MB of concepts, 16 MB of YAML headers or 2 million values in them, or a concept whose YAML header is over 256 KB, is refused whole, with an error saying so. `--dry-run` tells, before writing anything, how many would come in global, pinned or verified. Symbolic links inside the bundle aren't followed, a concept over 1 MB is skipped with a warning, and data over 64 KB fails that concept before creating its memory. Other tools' types are recognised by name (`Runbook` is a procedure, `Convention` a preference, the rest facts), and what OKF has and Omoikane doesn't (`sources`, `generated`, process verifications, `governance`…) is kept in the memory's data and comes out again on export. Secrets are masked as in any `remember`. A folder that isn't OKF (a `.md` without a header or without `type`) is rejected before anything is written, naming the files at fault.

Verification only ever comes from a person. A `verified` tag is just a tag, on export and on import: agents can set tags. Importing is your own action, so a person's `verified` entry (`human:<who>`) of a bundle exported from this database verifies the memory with its original date, as if you had run `omoi verify`, as long as the text is still the one verified (the bundle carries its fingerprint, `omoikane.verified_sha`): edit the Markdown and the verification stays behind. Someone else's bundle verifies only with `--trust-verified`, when you trust whoever made it. Never when it is older than the concept's `generated.at` (OKF counts it as superseded), when you unverified that memory here afterwards, or when its date is in the future. Inside a coding agent's shell the memory always comes in unverified and the import tells you; run it again from your own terminal to keep the verification. Process verifications (`process:…`, `<producer>/<version>`) never verify anything: they stay in the memory's data.

## Daily copy

At the first session start of each day, Omoikane copies the database to `~/.omoikane/backups/omoikane-<date>.db` and keeps the last 7 (`OMOIKANE_BACKUPS`; 0 = off). `omoi backup` does it now, and `omoi doctor` says when the last one was.

## Importing from other tools

`omoi import` reads exports from engram (`engram export`) and from `omoi export`: it maps the kinds, keeps the dates, skips what was deleted and imports nothing twice. `--dry-run` shows where each memory would go.
