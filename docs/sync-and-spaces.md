# Sync, projects and spaces

[Español](sync-and-spaces.es.md) · [Back to the README](../README.md)

## Sharing memory between computers (and teammates)

Omoikane hosts nothing: it syncs through a git repository of yours, used only for this knowledge (never your code repository). Any remote works: a private repo on GitHub, GitLab, Codeberg, your company's Gitea, or a shared folder. It uses your own git credentials.

```
omoi sync init git@github.com:you/my-memory.git   # on the first computer: asks what to share
omoi sync code                                    # prints a code that carries the key
omoi sync join <code>                             # on another computer (or `--url` to reach it another way)
```

From then on it runs by itself, for every agent: after each saved, changed or forgotten memory, `omoi` syncs in the background (changes a few seconds apart go in one commit), and pulls when a session starts. `omoi sync status` and `omoi doctor` say how the last sync went, the web view shows it too, and a session warns if sync has been failing for a while.

- **What travels:** text, never the database. One encrypted line per memory, in one file per project and per computer, so a change is a one-line diff and nobody's file conflicts with anyone else's. Vectors and indexes are rebuilt on each computer.
- **Encrypted on the remote:** each line is encrypted (AES-SIV) before it leaves your computer; the repository only ever holds ciphertext and opaque folder names. The key is created by `init` and travels inside the join code, so **share the code only through a private channel**. Lose every copy of the key and the remote can't be read, though each computer keeps its own memories.
- **Changing the key:** `omoi sync rotate` (`--space <name>` for a space's repository) re-encrypts this computer's memories with a new key and prints the new code. Computers that only have the old key stop reading what is written from then on: that is how someone who leaves the team or the client stops getting anything new.
- **What is shared** is chosen in `init`/`join`: kinds (preferences stay personal unless you say so), global memories, which projects, and whether each memory carries its author (your git name, on by default). With teammates, their memories show up marked `[by Ana]` so the agent treats them with care.
- **Which version wins:** the newest change by a hybrid logical clock, so a computer with the wrong time can't overwrite newer work. The older local version stays in the memory's history. Forgotten memories don't come back from another computer.
- **Projects** are matched by their git `origin` remote, so the same repo in different folders on different computers is the same project. Projects without a remote are matched by folder name, only between the same person's computers. A project's space travels on its own line, and the newest move wins; `--space` keeps a repository to one client.
- **Computers on different versions:** from 1.0 on, what a new version adds doesn't break the old ones. A field a computer doesn't know is kept with the memory and goes back up even if that computer changes it, so it isn't lost by passing through it. A line of a kind it doesn't know is skipped, `omoi sync status` says "N line(s) from a newer version: update omoi", and after updating everything is read again. And if a version ever changed something the old ones can't read right, the repository says so in `omoikane.format` and the old ones don't write to it until they're updated (`status`, `doctor` and the session start say so).

`omoi sync off` stops syncing on a computer; its memories stay.

## Projects

Memories belong to the git repository you run `omoi` in (`--global` makes one apply everywhere). If you open your sessions in your home folder instead of inside a repository, you see the memories of every project, and a new memory goes to the repository its absolute paths point to (or is global); `omoi update <id> --project <folder>` or `--global` moves one later.

Each project has its key, worked out once: its git `origin`, or, without one, your name and the folder's. It also keeps every folder it lives in, so two clones of the same repository, or a worktree, are the same project. Move the folder of a repository with an `origin` and its memories follow it by themselves at the next session (you're told once); without an `origin`, `omoi projects move <project> <folder>`, and until then `omoi review` and `omoi doctor` point it out. `omoi projects` lists them. `omoi projects merge <a> <b>` joins two without touching ids, history or verdicts, keeping the newer of their two `status` memories; `omoi projects prune` forgets the ones left without memories.

## Spaces: one per client

Spaces keep clients apart. `omoi spaces add acme`, then `omoi space assign acme` in each of the client's repositories (or `omoi projects move <project> --space acme`). From an Acme project you see its memories, the space's and the global ones, never another client's: not in search, not in recall, not in `context`. What is common to all of a client's repositories is saved with `remember --space` (`space: true` over MCP); your preferences stay yours, global. From your home folder you see everything. `search` and `recent` take `project` (a folder or part of a name) and `space`.

To share it, one repository per client, with its own key: `omoi sync init --space acme <url>` carries only that space, without your global memories, and the general repository doesn't carry the space's memories.

### Projects on hold

Moving a project into a space never moves it anywhere else by itself: anyone with a repository's key could write that line. Your other computers find out at their next sync (the general repository only says that a project went into a space, not which one, nor what's in it; the space's repository says which) and put the project **on hold**: nothing of it is uploaded to any repository, and nothing that arrives for it is applied, until you decide on that computer. If you're not sure, `omoi projects keep web` leaves it where it was; `omoi projects move web --space acme --force` accepts the move. Either way it syncs again, and the newest edit wins.

A space's repository can also name a project you haven't opened on this computer yet (a coworker's, or a library of yours that someone uses at the client): the first time you open that folder, the project goes on hold too, and what you save there stays on your computer until you say whose it is: `omoi projects keep <project>` if it is the client's, `omoi space unassign <project> --force` if it is yours (it goes to your general repository, never to the client's). `omoi sync`, `omoi sync status`, `omoi doctor`, `omoi review`, the web view's Review (with both commands to copy) and the agent at the start of each session say so until you decide. So the worst a forged line can do is put a project on hold, never send a client's memories, or yours, to another repository.

`omoi export --encrypt` from a client's project uses that space's key; with `--all`, the general one.
