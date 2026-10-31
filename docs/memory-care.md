# Keeping the memory right, and never losing it

[Español](memory-care.es.md) · [Back to the README](../README.md)

## It looks after itself

Omoikane notices when a memory has fallen behind: a newer one on the same subject says something different (a higher version of the same thing, or done what was pending), a `status` memory the project has moved past, paths that no longer exist, memories never used in a month, and notes too long for recall. When one of those reaches the agent it comes marked `⚠ may be outdated: newer <id> …`, and saving a memory that makes another outdated says so right away, so the agent fixes it on the spot. The **Review** tab of the web view lists them with the two memories side by side, *forget the old one* and *it's fine as it is*; `omoi review` does the same from the terminal. No model involved: words, dates and versions.

## Verdicts

What Omoikane can't know by itself, the agent says, with no new tools: when saving or changing a memory it can pass `supersedes` (this one replaces those), `contradicts` (they say different things and it's unclear which holds) or `compatible` (a warning paired them and both hold). The verdict is kept, travels with sync and shows wherever the memories show: in the recall (`⚠ superseded by …`), in `search`, in `context` and in Review. It holds while what it judged doesn't change, and for a pair the newest one wins. "It's fine as it is" is a verdict too, so what you approve on one machine holds on the others.

**Why and who.** Each verdict keeps its reason and who gave it (the sync person, or the agent and its client), and both show in the recall, in `search` and in Review. If A is superseded by B and B by C, A's note leads straight to C.

## Fixing a piece and merging

`update` takes `replace` (changes a text that must appear exactly once; otherwise nothing changes), `append` (adds at the end, for a procedure's new step) and `absorb` (merges several memories into one: the agent writes the merged text, and the absorbed ones go to the trash with their history hung on the one that stays and their tags and links moved to it). The Review tab has a **Merge** button to do it by hand.

## Review date

Each kind has its own: decisions and procedures after 120 days untouched, facts and gotchas after 180, preferences never, counting from its last change or the last time the agent got it (what gets used and nobody corrects is reviewed). When it comes, the memory shows in Review and in `search` with "not reviewed in N days"; not in the recall, which only carries what says a memory may be wrong. "It's fine as it is" restarts the clock.

## History and trash

Changing a memory keeps the previous version with the reason, and `omoi history` shows them all.

**Trash.** Forgetting a memory (`forget`, from the agent, the CLI or the web view) sends it to the trash with its history, and a forget doesn't come back from another computer when you sync either. `omoi restore <id>` brings it back, history included, and wins on the other computers at the next sync: a `forget` made by mistake on one machine is undone from any of them. The agent does it with `update` and `restore: true`, with no new tool; the Review tab has a **Trash** group with a button for each one. After 30 days (`OMOIKANE_TRASH_DAYS`; 0 = never) they are deleted for good; `forget --purge` skips the trash and `omoi trash --empty` empties it.

## Faithful copy

`omoi export` writes a JSONL with everything needed to rebuild the database: each memory with its history, clock stamp and author, the forget marks, the verdicts and the trash, with an end line carrying the count and the SHA-256 (a cut or tampered file is not imported). `omoi import` puts it back with the same rules as sync: same ids, dates and stamps, newest wins and what was forgotten doesn't come back, so repeating it changes nothing and it serves both to restore a database and to merge two. Both stream, one line in memory at a time: 50,000 memories export and import without loading the RAM. With `--encrypt`, every line is encrypted with the sync key (a client's project, with its space's key), to keep the copy anywhere. What is in the trash goes with its project and its space, and with its verdicts: restored on another computer, it goes back where it was, `superseded` included. Files in the previous format and engram's still import, and so does one from a newer omoi: what this version doesn't understand is skipped and counted, and the fields it doesn't know are kept and come out again in the next export.

## Daily copy

At the first session start of each day, Omoikane copies the database to `~/.omoikane/backups/omoikane-<date>.db` and keeps the last 7 (`OMOIKANE_BACKUPS`; 0 = off). `omoi backup` does it now, and `omoi doctor` says when the last one was.

## Importing from other tools

`omoi import` reads exports from engram (`engram export`) and from `omoi export`: it maps the kinds, keeps the dates, skips what was deleted and imports nothing twice. `--dry-run` shows where each memory would go.
